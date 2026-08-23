use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{LazyLock, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::gstreamer_encoder::{APPSRC_MAX_BUFFERS, CeilingUpdate};
use crate::{CaptureConfig, NativeTelemetry};

const COMMAND_CAPACITY: usize = 32;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const TELEMETRY_TIMEOUT: Duration = Duration::from_millis(500);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const RECONNECT_DELAY_BASE: Duration = Duration::from_secs(1);
const RECONNECT_DELAY_MAX: Duration = Duration::from_secs(15);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const RATE_BACKPRESSURE_INTERVAL: Duration = Duration::from_millis(200);
const RATE_ADAPT_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) const RATE_LOSS_HIGH: f64 = 0.03;
pub(crate) const RATE_LOSS_LOW: f64 = 0.005;
pub(crate) const RATE_RECOVER_TICKS: u32 = 10;
const RATE_STEP_DOWN: f64 = 0.75;
const RATE_STEP_UP: f64 = 1.15;
const RATE_STEP_BACKPRESSURE: f64 = 0.85;
pub(crate) const RATE_QUEUE_FULL_TICKS: u32 = 3;
pub(crate) const RATE_QUEUE_FULL_COOLDOWN_TICKS: u32 = 10;
pub(crate) const RATE_FLOOR_KBPS: u32 = 500;
const DEFAULT_VIDEO_BITRATE_BPS: f64 = 20_000_000.0;

static ACTIVE_SESSION: LazyLock<Mutex<Option<PublisherSession>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionPhase {
    Dormant,
    Connected,
    Recovery,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigOutcome {
    Applied,
    Queued,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VideoIntent {
    config: CaptureConfig,
}

impl VideoIntent {
    #[must_use]
    pub(crate) fn new(config: CaptureConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub(crate) fn config(&self) -> &CaptureConfig {
        &self.config
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionErrorKind {
    Effect,
    StaleGeneration,
    WorkerStopped,
    CommandQueueFull,
    ReplyTimeout,
    Spawn,
    Registry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionError {
    pub(crate) kind: SessionErrorKind,
    pub(crate) operation: &'static str,
    pub(crate) message: String,
}

impl SessionError {
    fn effect(error: EffectError) -> Self {
        Self {
            kind: SessionErrorKind::Effect,
            operation: error.operation,
            message: error.message,
        }
    }

    fn stale() -> Self {
        Self {
            kind: SessionErrorKind::StaleGeneration,
            operation: "generation check",
            message: "GStreamer publisher worker is stale; reconnecting refreshes it".into(),
        }
    }

    fn worker_stopped(operation: &'static str) -> Self {
        Self {
            kind: SessionErrorKind::WorkerStopped,
            operation,
            message: "GStreamer publisher worker stopped".into(),
        }
    }

    fn queue_full(operation: &'static str) -> Self {
        Self {
            kind: SessionErrorKind::CommandQueueFull,
            operation,
            message: "GStreamer publisher command queue is full".into(),
        }
    }

    fn timeout(operation: &'static str, error: mpsc::RecvTimeoutError) -> Self {
        Self {
            kind: SessionErrorKind::ReplyTimeout,
            operation,
            message: error.to_string(),
        }
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectError {
    pub(crate) operation: &'static str,
    pub(crate) message: String,
}

impl EffectError {
    pub(crate) fn new(operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            operation,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionSnapshot {
    pub(crate) phase: SessionPhase,
    pub(crate) generation: u64,
    pub(crate) applied_video: Option<VideoIntent>,
    pub(crate) queued_video: Option<VideoIntent>,
    pub(crate) has_queued_stop: bool,
    pub(crate) current_ceiling_kbps: Option<u32>,
    pub(crate) last_error: Option<SessionError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InPlaceChange {
    Applied(CeilingUpdate),
    RequiresRebuild,
}

pub(crate) trait LifecycleEffects: Send {
    fn is_generation_current(&self) -> bool;
    fn build(&mut self, intent: Option<&VideoIntent>) -> Result<(), EffectError>;
    fn teardown(&mut self);
    fn stop_video(&mut self) -> Result<(), EffectError>;
    fn change_in_place(&mut self, intent: &VideoIntent) -> Result<InPlaceChange, EffectError>;
    fn can_apply_ceiling(&self) -> bool;
    fn apply_ceiling(&mut self, ceiling_kbps: u32) -> Result<CeilingUpdate, EffectError>;
    fn telemetry(&self) -> Option<NativeTelemetry>;
    fn poll_fault(&self) -> Option<EffectError>;
    fn install_live_binding(&mut self, intent: &VideoIntent) -> Result<(), EffectError>;
    fn install_dormant_binding(&mut self);
}

#[derive(Debug)]
enum QueuedVideo {
    Change(VideoIntent),
    Stop,
}

#[derive(Debug)]
struct AppliedVideo {
    intent: VideoIntent,
    rate: RateController,
}

pub(crate) struct SessionMachine<E> {
    effects: E,
    generation: u64,
    phase: SessionPhase,
    applied: Option<AppliedVideo>,
    queued: Option<QueuedVideo>,
    last_error: Option<SessionError>,
    retry_at: Duration,
    retry_delay: Duration,
    next_backpressure_at: Duration,
    next_rate_at: Duration,
}

impl<E: LifecycleEffects> SessionMachine<E> {
    pub(crate) fn new(effects: E, generation: u64) -> Self {
        Self {
            effects,
            generation,
            phase: SessionPhase::Dormant,
            applied: None,
            queued: None,
            last_error: None,
            retry_at: Duration::ZERO,
            retry_delay: RECONNECT_DELAY_BASE,
            next_backpressure_at: Duration::ZERO,
            next_rate_at: Duration::ZERO,
        }
    }

    pub(crate) fn start(&mut self, now: Duration) -> Result<(), SessionError> {
        self.ensure_current()?;
        if self.phase != SessionPhase::Dormant {
            return Ok(());
        }
        if let Err(error) = self.build_and_activate(None, None) {
            let error = SessionError::effect(error);
            self.last_error = Some(error.clone());
            self.effects.install_dormant_binding();
            self.effects.teardown();
            return Err(error);
        }

        self.last_error = None;
        self.enter_connected(now);
        Ok(())
    }

    pub(crate) fn change_video(
        &mut self,
        intent: VideoIntent,
        now: Duration,
    ) -> Result<ConfigOutcome, SessionError> {
        self.ensure_current()?;
        match self.phase {
            SessionPhase::Dormant => self.apply_from_dormant(intent, now),
            SessionPhase::Connected => self.apply_while_connected(intent, now),
            SessionPhase::Recovery => {
                self.queued = Some(QueuedVideo::Change(intent));
                Ok(ConfigOutcome::Queued)
            }
            SessionPhase::Shutdown => Err(SessionError::worker_stopped("change video")),
        }
    }

    pub(crate) fn stop_video(&mut self, now: Duration) -> Result<(), SessionError> {
        self.ensure_current()?;
        match self.phase {
            SessionPhase::Dormant => {
                self.applied = None;
                self.queued = None;
                Ok(())
            }
            SessionPhase::Connected => {
                self.effects.install_dormant_binding();
                if let Err(error) = self.effects.stop_video() {
                    let error = SessionError::effect(error);
                    self.last_error = Some(error.clone());
                    self.effects.teardown();
                    self.applied = None;
                    self.queued = Some(QueuedVideo::Stop);
                    self.phase = SessionPhase::Recovery;
                    self.retry_at = now;
                    return Err(error);
                }
                self.applied = None;
                self.queued = None;
                Ok(())
            }
            SessionPhase::Recovery => {
                self.queued = Some(QueuedVideo::Stop);
                self.retry_at = now;
                Ok(())
            }
            SessionPhase::Shutdown => Err(SessionError::worker_stopped("stop video")),
        }
    }

    pub(crate) fn telemetry(&self) -> Option<NativeTelemetry> {
        if self.phase != SessionPhase::Connected {
            return None;
        }
        self.effects.telemetry()
    }

    #[must_use]
    pub(crate) fn snapshot(&self) -> SessionSnapshot {
        let (queued_video, has_queued_stop) = match self.queued.as_ref() {
            Some(QueuedVideo::Change(intent)) => (Some(intent.clone()), false),
            Some(QueuedVideo::Stop) => (None, true),
            None => (None, false),
        };
        SessionSnapshot {
            phase: self.phase,
            generation: self.generation,
            applied_video: self.applied.as_ref().map(|applied| applied.intent.clone()),
            queued_video,
            has_queued_stop,
            current_ceiling_kbps: self
                .applied
                .as_ref()
                .map(|applied| applied.rate.current_kbps()),
            last_error: self.last_error.clone(),
        }
    }

    pub(crate) fn tick(&mut self, now: Duration) {
        if self.phase == SessionPhase::Shutdown {
            return;
        }
        if !self.effects.is_generation_current() {
            self.shutdown_with_error(SessionError::stale());
            return;
        }

        match self.phase {
            SessionPhase::Connected => self.tick_connected(now),
            SessionPhase::Recovery if now >= self.retry_at => self.tick_recovery(now),
            SessionPhase::Dormant | SessionPhase::Recovery | SessionPhase::Shutdown => {}
        }
    }

    pub(crate) fn shutdown(&mut self) {
        if self.phase == SessionPhase::Shutdown {
            return;
        }
        self.effects.install_dormant_binding();
        self.effects.teardown();
        self.phase = SessionPhase::Shutdown;
        self.queued = None;
    }

    fn ensure_current(&mut self) -> Result<(), SessionError> {
        if self.effects.is_generation_current() {
            return Ok(());
        }

        let error = SessionError::stale();
        self.shutdown_with_error(error.clone());
        Err(error)
    }

    fn apply_from_dormant(
        &mut self,
        intent: VideoIntent,
        now: Duration,
    ) -> Result<ConfigOutcome, SessionError> {
        let rate = RateController::from_config(intent.config());
        if let Err(error) = self.build_and_activate(Some(&intent), None) {
            let error = SessionError::effect(error);
            self.last_error = Some(error.clone());
            self.effects.install_dormant_binding();
            self.effects.teardown();
            return Err(error);
        }

        self.applied = Some(AppliedVideo { intent, rate });
        self.last_error = None;
        self.enter_connected(now);
        Ok(ConfigOutcome::Applied)
    }

    fn apply_while_connected(
        &mut self,
        intent: VideoIntent,
        now: Duration,
    ) -> Result<ConfigOutcome, SessionError> {
        if self
            .applied
            .as_ref()
            .is_some_and(|applied| applied.intent == intent)
        {
            return Ok(ConfigOutcome::Applied);
        }

        match self
            .effects
            .change_in_place(&intent)
            .map_err(SessionError::effect)?
        {
            InPlaceChange::Applied(CeilingUpdate::Applied | CeilingUpdate::Attempted) => {
                if let Err(error) = self.effects.install_live_binding(&intent) {
                    let error = SessionError::effect(error);
                    self.last_error = Some(error.clone());
                    self.effects.install_dormant_binding();
                    self.effects.teardown();
                    self.enter_recovery(now);
                    return Err(error);
                }
                self.applied = Some(AppliedVideo {
                    rate: RateController::from_config(intent.config()),
                    intent,
                });
                self.last_error = None;
                return Ok(ConfigOutcome::Applied);
            }
            InPlaceChange::Applied(CeilingUpdate::Pinned) | InPlaceChange::RequiresRebuild => {}
        }

        let previous = self.applied.take();
        self.effects.install_dormant_binding();
        self.effects.teardown();
        let candidate_rate = RateController::from_config(intent.config());
        match self.build_and_activate(Some(&intent), None) {
            Ok(()) => {
                self.applied = Some(AppliedVideo {
                    intent,
                    rate: candidate_rate,
                });
                self.last_error = None;
                self.enter_connected(now);
                Ok(ConfigOutcome::Applied)
            }
            Err(candidate_error) => {
                self.effects.install_dormant_binding();
                self.effects.teardown();
                let public_error = SessionError::effect(candidate_error);
                self.last_error = Some(public_error.clone());
                match self.restore(previous.as_ref()) {
                    Ok(()) => {
                        self.applied = previous;
                        self.enter_connected(now);
                    }
                    Err(error) => {
                        log::warn!(
                            "Publisher session failed to restore after rejected rebuild: {}",
                            error.message
                        );
                        self.effects.install_dormant_binding();
                        self.effects.teardown();
                        self.applied = previous;
                        self.enter_recovery(now);
                    }
                }
                Err(public_error)
            }
        }
    }

    fn tick_connected(&mut self, now: Duration) {
        if let Some(error) = self.effects.poll_fault() {
            self.last_error = Some(SessionError::effect(error));
            self.effects.install_dormant_binding();
            self.effects.teardown();
            self.enter_recovery(now);
            return;
        }

        if now >= self.next_backpressure_at {
            self.next_backpressure_at =
                advance_deadline(self.next_backpressure_at, now, RATE_BACKPRESSURE_INTERVAL);
            self.observe_rate(RateObservationKind::Backpressure);
        }
        if now >= self.next_rate_at {
            self.next_rate_at = advance_deadline(self.next_rate_at, now, RATE_ADAPT_INTERVAL);
            self.observe_rate(RateObservationKind::Loss);
        }
    }

    fn observe_rate(&mut self, kind: RateObservationKind) {
        let Some(applied) = self.applied.as_mut() else {
            return;
        };
        if !applied.rate.enabled || !self.effects.can_apply_ceiling() {
            return;
        }
        let Some(telemetry) = self.effects.telemetry() else {
            return;
        };
        let observation = match kind {
            RateObservationKind::Backpressure => applied.rate.propose_backpressure(&telemetry),
            RateObservationKind::Loss => applied.rate.propose_loss(&telemetry),
        };
        let Some(ceiling_kbps) = observation.ceiling_kbps else {
            applied.rate = observation.controller;
            return;
        };

        match self.effects.apply_ceiling(ceiling_kbps) {
            Ok(CeilingUpdate::Applied | CeilingUpdate::Attempted) => {
                applied.rate = observation.controller;
            }
            Ok(CeilingUpdate::Pinned) => {}
            Err(error) => {
                self.last_error = Some(SessionError::effect(error));
            }
        }
    }

    fn tick_recovery(&mut self, now: Duration) {
        let queued = self.queued.take();
        match queued {
            Some(QueuedVideo::Change(intent)) => {
                let previous = self.applied.take();
                let candidate_rate = RateController::from_config(intent.config());
                match self.build_and_activate(Some(&intent), None) {
                    Ok(()) => {
                        self.applied = Some(AppliedVideo {
                            intent,
                            rate: candidate_rate,
                        });
                        self.last_error = None;
                        self.enter_connected(now);
                    }
                    Err(error) => {
                        self.last_error = Some(SessionError::effect(error));
                        self.effects.install_dormant_binding();
                        self.effects.teardown();
                        match self.restore(previous.as_ref()) {
                            Ok(()) => {
                                self.applied = previous;
                                self.enter_connected(now);
                            }
                            Err(error) => {
                                log::warn!(
                                    "Publisher session failed to restore after queued change: {}",
                                    error.message
                                );
                                self.effects.install_dormant_binding();
                                self.effects.teardown();
                                self.applied = previous;
                                self.schedule_retry(now);
                            }
                        }
                    }
                }
            }
            Some(QueuedVideo::Stop) => match self.build_and_activate(None, None) {
                Ok(()) => {
                    self.applied = None;
                    self.last_error = None;
                    self.enter_connected(now);
                }
                Err(error) => {
                    self.last_error = Some(SessionError::effect(error));
                    self.effects.install_dormant_binding();
                    self.effects.teardown();
                    self.schedule_retry(now);
                }
            },
            None => {
                let applied = self.applied.take();
                let restore = self.restore(applied.as_ref());
                self.applied = applied;
                match restore {
                    Ok(()) => self.enter_connected(now),
                    Err(error) => {
                        self.last_error = Some(SessionError::effect(error));
                        self.effects.install_dormant_binding();
                        self.effects.teardown();
                        self.schedule_retry(now);
                    }
                }
            }
        }
    }

    fn build_and_activate(
        &mut self,
        intent: Option<&VideoIntent>,
        restored_ceiling_kbps: Option<u32>,
    ) -> Result<(), EffectError> {
        self.effects.build(intent)?;
        if let Some(ceiling_kbps) = restored_ceiling_kbps {
            match self.effects.apply_ceiling(ceiling_kbps)? {
                CeilingUpdate::Applied | CeilingUpdate::Attempted => {}
                CeilingUpdate::Pinned => {
                    return Err(EffectError::new(
                        "restore encoder rate",
                        "encoder pinned a previously applied ceiling",
                    ));
                }
            }
        }
        if let Some(intent) = intent {
            self.effects.install_live_binding(intent)?;
        } else {
            self.effects.install_dormant_binding();
        }
        Ok(())
    }

    fn restore(&mut self, applied: Option<&AppliedVideo>) -> Result<(), EffectError> {
        match applied {
            Some(applied) => {
                self.build_and_activate(Some(&applied.intent), Some(applied.rate.current_kbps()))
            }
            None => self.build_and_activate(None, None),
        }
    }

    fn enter_connected(&mut self, now: Duration) {
        self.phase = SessionPhase::Connected;
        self.retry_delay = RECONNECT_DELAY_BASE;
        self.next_backpressure_at = now + RATE_BACKPRESSURE_INTERVAL;
        self.next_rate_at = now + RATE_ADAPT_INTERVAL;
    }

    fn enter_recovery(&mut self, now: Duration) {
        self.phase = SessionPhase::Recovery;
        self.retry_delay = RECONNECT_DELAY_BASE;
        self.retry_at = now + self.retry_delay;
    }

    fn schedule_retry(&mut self, now: Duration) {
        self.phase = SessionPhase::Recovery;
        self.retry_delay = (self.retry_delay * 2).min(RECONNECT_DELAY_MAX);
        self.retry_at = now + self.retry_delay;
    }

    fn shutdown_with_error(&mut self, error: SessionError) {
        let is_stale = error.kind == SessionErrorKind::StaleGeneration;
        self.last_error = Some(error);
        if is_stale {
            self.effects.teardown();
            self.phase = SessionPhase::Shutdown;
            self.queued = None;
            return;
        }
        self.shutdown();
    }
}

#[derive(Clone, Copy)]
enum RateObservationKind {
    Backpressure,
    Loss,
}

fn advance_deadline(deadline: Duration, now: Duration, interval: Duration) -> Duration {
    let next = deadline + interval;
    if next <= now {
        return now + interval;
    }
    next
}

#[derive(Debug, Clone, Copy)]
struct RateObservation {
    controller: RateController,
    ceiling_kbps: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RateController {
    pub(crate) enabled: bool,
    ceiling_kbps: u32,
    current_kbps: u32,
    pub(crate) clean_ticks: u32,
    last_packets_sent: u64,
    last_packets_lost: u64,
    last_appsrc_dropped: u64,
    pub(crate) queue_full_ticks: u32,
    fullness_cooldown_ticks: u32,
    loss_primed: bool,
    backpressure_primed: bool,
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "telemetry counters are finite and rate proposals are clamped before narrowing"
)]
impl RateController {
    pub(crate) fn from_config(config: &CaptureConfig) -> Self {
        let ceiling_kbps = configured_ceiling_kbps(config);
        Self {
            enabled: config.auto_bitrate,
            ceiling_kbps,
            current_kbps: ceiling_kbps,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn reset(&mut self, config: &CaptureConfig) {
        *self = Self::from_config(config);
    }

    #[must_use]
    pub(crate) fn current_kbps(&self) -> u32 {
        self.current_kbps
    }

    fn propose_loss(&self, telemetry: &NativeTelemetry) -> RateObservation {
        let mut controller = *self;
        let ceiling_kbps = controller.observe(telemetry);
        RateObservation {
            controller,
            ceiling_kbps,
        }
    }

    fn propose_backpressure(&self, telemetry: &NativeTelemetry) -> RateObservation {
        let mut controller = *self;
        let ceiling_kbps = controller.observe_backpressure(telemetry);
        RateObservation {
            controller,
            ceiling_kbps,
        }
    }

    pub(crate) fn observe(&mut self, telemetry: &NativeTelemetry) -> Option<u32> {
        if !self.enabled {
            return None;
        }
        let (Some(sent), Some(lost)) = (telemetry.video_packets_sent, telemetry.video_packets_lost)
        else {
            return None;
        };
        let (sent, lost) = (sent as u64, lost as u64);
        if !self.loss_primed || sent < self.last_packets_sent {
            self.last_packets_sent = sent;
            self.last_packets_lost = lost;
            self.loss_primed = true;
            return None;
        }
        let delta_sent = sent.saturating_sub(self.last_packets_sent);
        let delta_lost = lost.saturating_sub(self.last_packets_lost);
        self.last_packets_sent = sent;
        self.last_packets_lost = lost;
        if delta_sent == 0 {
            return None;
        }
        let loss_ratio = (delta_lost as f64 / delta_sent as f64).min(1.0);
        if loss_ratio >= RATE_LOSS_HIGH {
            self.clean_ticks = 0;
            let next = ((f64::from(self.current_kbps)) * RATE_STEP_DOWN).round() as u32;
            let next = next.max(RATE_FLOOR_KBPS);
            if next < self.current_kbps {
                self.current_kbps = next;
                return Some(next);
            }
            return None;
        }
        if loss_ratio <= RATE_LOSS_LOW {
            self.clean_ticks += 1;
            if self.clean_ticks >= RATE_RECOVER_TICKS && self.current_kbps < self.ceiling_kbps {
                self.clean_ticks = 0;
                let next = ((f64::from(self.current_kbps)) * RATE_STEP_UP)
                    .round()
                    .clamp(f64::from(RATE_FLOOR_KBPS), f64::from(self.ceiling_kbps))
                    as u32;
                if next > self.current_kbps {
                    self.current_kbps = next;
                    return Some(next);
                }
            }
            return None;
        }
        self.clean_ticks = 0;
        None
    }

    pub(crate) fn observe_backpressure(&mut self, telemetry: &NativeTelemetry) -> Option<u32> {
        if !self.enabled {
            return None;
        }
        let dropped = telemetry.video_appsrc_dropped.unwrap_or(0);
        if !self.backpressure_primed || dropped < self.last_appsrc_dropped {
            self.last_appsrc_dropped = dropped;
            self.queue_full_ticks = 0;
            self.fullness_cooldown_ticks = 0;
            self.backpressure_primed = true;
            return None;
        }
        let dropped_delta = dropped - self.last_appsrc_dropped;
        self.last_appsrc_dropped = dropped;
        let queue_full = telemetry
            .video_appsrc_level_buffers
            .is_some_and(|level| u64::from(level) >= APPSRC_MAX_BUFFERS);
        if queue_full && self.fullness_cooldown_ticks == 0 {
            self.queue_full_ticks += 1;
        } else {
            self.queue_full_ticks = 0;
        }
        if self.fullness_cooldown_ticks > 0 {
            self.fullness_cooldown_ticks -= 1;
        }
        let fullness_only = dropped_delta == 0;
        if fullness_only && self.queue_full_ticks < RATE_QUEUE_FULL_TICKS {
            return None;
        }
        if fullness_only {
            self.queue_full_ticks = 0;
            self.fullness_cooldown_ticks = RATE_QUEUE_FULL_COOLDOWN_TICKS;
        }
        self.clean_ticks = 0;
        let next = ((f64::from(self.current_kbps)) * RATE_STEP_BACKPRESSURE)
            .round()
            .max(f64::from(RATE_FLOOR_KBPS)) as u32;
        if next < self.current_kbps {
            self.current_kbps = next;
            return Some(next);
        }
        None
    }
}

#[must_use]
pub(crate) fn configured_ceiling_kbps(config: &CaptureConfig) -> u32 {
    let bps = config
        .max_bitrate
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(DEFAULT_VIDEO_BITRATE_BPS);
    crate::gstreamer_encoder::bitrate_bps_to_kbps(bps)
}

enum SessionCommand {
    ChangeVideo {
        intent: VideoIntent,
        reply: SyncSender<Result<ConfigOutcome, SessionError>>,
    },
    StopVideo(SyncSender<Result<(), SessionError>>),
    Telemetry(SyncSender<Option<NativeTelemetry>>),
    #[allow(
        dead_code,
        reason = "reserved by the typed PublisherSession snapshot interface"
    )]
    Snapshot(SyncSender<SessionSnapshot>),
    Shutdown,
}

pub(crate) struct PublisherSession {
    command_sender: SyncSender<SessionCommand>,
    join: JoinHandle<()>,
}

#[derive(Clone)]
struct SessionClient {
    command_sender: SyncSender<SessionCommand>,
}

impl SessionClient {
    fn submit(&self, command: SessionCommand, operation: &'static str) -> Result<(), SessionError> {
        match self.command_sender.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(SessionError::queue_full(operation)),
            Err(TrySendError::Disconnected(_)) => Err(SessionError::worker_stopped(operation)),
        }
    }

    fn change_video(&self, intent: VideoIntent) -> Result<ConfigOutcome, SessionError> {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.submit(
            SessionCommand::ChangeVideo {
                intent,
                reply: reply_sender,
            },
            "change video",
        )?;
        reply_receiver
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| SessionError::timeout("change video", error))?
    }

    fn stop_video(&self) -> Result<(), SessionError> {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.submit(SessionCommand::StopVideo(reply_sender), "stop video")?;
        reply_receiver
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| SessionError::timeout("stop video", error))?
    }

    fn telemetry(&self) -> Option<NativeTelemetry> {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.submit(SessionCommand::Telemetry(reply_sender), "telemetry")
            .ok()?;
        reply_receiver.recv_timeout(TELEMETRY_TIMEOUT).ok()?
    }

    fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.submit(SessionCommand::Snapshot(reply_sender), "session snapshot")?;
        reply_receiver
            .recv_timeout(TELEMETRY_TIMEOUT)
            .map_err(|error| SessionError::timeout("session snapshot", error))
    }
}

impl PublisherSession {
    pub(crate) fn spawn<E: LifecycleEffects + 'static>(
        effects: E,
        generation: u64,
    ) -> Result<Self, SessionError> {
        let (command_sender, command_receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name("slopcast-gstreamer-livekit".into())
            .spawn(move || run_session(effects, generation, &command_receiver, &startup_sender))
            .map_err(|error| SessionError {
                kind: SessionErrorKind::Spawn,
                operation: "spawn publisher session",
                message: error.to_string(),
            })?;

        let startup = startup_receiver
            .recv_timeout(COMMAND_TIMEOUT)
            .map_err(|error| SessionError::timeout("start publisher session", error));
        match startup {
            Ok(Ok(())) => Ok(Self {
                command_sender,
                join,
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(error) => {
                let _ = command_sender.try_send(SessionCommand::Shutdown);
                crate::reap_detached(join, "slopcast-gstreamer-livekit-startup-reaper");
                Err(error)
            }
        }
    }

    fn client(&self) -> SessionClient {
        SessionClient {
            command_sender: self.command_sender.clone(),
        }
    }

    #[allow(
        dead_code,
        reason = "the owner registry clones a client before blocking so shutdown stays bounded"
    )]
    pub(crate) fn change_video(&self, intent: VideoIntent) -> Result<ConfigOutcome, SessionError> {
        self.client().change_video(intent)
    }

    #[allow(
        dead_code,
        reason = "the owner registry clones a client before blocking so shutdown stays bounded"
    )]
    pub(crate) fn stop_video(&self) -> Result<(), SessionError> {
        self.client().stop_video()
    }

    #[allow(
        dead_code,
        reason = "the owner registry clones a client before blocking so shutdown stays bounded"
    )]
    pub(crate) fn telemetry(&self) -> Option<NativeTelemetry> {
        self.client().telemetry()
    }

    #[allow(
        dead_code,
        reason = "the owner registry clones a client before blocking so shutdown stays bounded"
    )]
    pub(crate) fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        self.client().snapshot()
    }

    pub(crate) fn shutdown(self) {
        if let Err(error) = self.command_sender.try_send(SessionCommand::Shutdown) {
            log::warn!("GStreamer publisher Shutdown send failed: {error}");
        }
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while !self.join.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if self.join.is_finished() {
            let _ = self.join.join();
        } else {
            log::warn!(
                "GStreamer publisher worker did not stop within {SHUTDOWN_GRACE:?}; reaping asynchronously"
            );
            crate::reap_detached(self.join, "slopcast-gstreamer-livekit-reaper");
        }
    }
}

fn run_session<E: LifecycleEffects>(
    effects: E,
    generation: u64,
    command_receiver: &Receiver<SessionCommand>,
    startup_sender: &SyncSender<Result<(), SessionError>>,
) {
    let anchor = Instant::now();
    let mut machine = SessionMachine::new(effects, generation);
    let startup = machine.start(Duration::ZERO);
    let should_run = startup.is_ok();
    let _ = startup_sender.send(startup);
    if !should_run {
        machine.shutdown();
        return;
    }

    loop {
        match command_receiver.recv_timeout(POLL_INTERVAL) {
            Ok(SessionCommand::ChangeVideo { intent, reply }) => {
                let _ = reply.send(machine.change_video(intent, anchor.elapsed()));
            }
            Ok(SessionCommand::StopVideo(reply)) => {
                let _ = reply.send(machine.stop_video(anchor.elapsed()));
            }
            Ok(SessionCommand::Telemetry(reply)) => {
                let _ = reply.send(machine.telemetry());
            }
            Ok(SessionCommand::Snapshot(reply)) => {
                let _ = reply.send(machine.snapshot());
            }
            Ok(SessionCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        machine.tick(anchor.elapsed());
        if machine.phase == SessionPhase::Shutdown {
            break;
        }
    }
    machine.shutdown();
}

pub(crate) fn install_active(session: PublisherSession) -> Result<(), SessionError> {
    let previous = ACTIVE_SESSION
        .lock()
        .map_err(|_| SessionError {
            kind: SessionErrorKind::Registry,
            operation: "install publisher session",
            message: "publisher session registry lock poisoned".into(),
        })?
        .replace(session);
    if let Some(previous) = previous {
        previous.shutdown();
    }
    Ok(())
}

pub(crate) fn shutdown_active() {
    let session = ACTIVE_SESSION
        .lock()
        .ok()
        .and_then(|mut active| active.take());
    if let Some(session) = session {
        session.shutdown();
    }
}

pub(crate) fn has_active() -> bool {
    ACTIVE_SESSION.lock().is_ok_and(|active| {
        active
            .as_ref()
            .is_some_and(|session| !session.join.is_finished())
    })
}

pub(crate) fn change_active(intent: VideoIntent) -> Result<ConfigOutcome, SessionError> {
    active_client("change video")?.change_video(intent)
}

pub(crate) fn stop_active_video() -> Result<(), SessionError> {
    let client = optional_active_client("stop video")?;
    match client {
        Some(client) => client.stop_video(),
        None => Ok(()),
    }
}

pub(crate) fn active_telemetry() -> Option<NativeTelemetry> {
    optional_active_client("telemetry")
        .ok()
        .flatten()
        .and_then(|client| client.telemetry())
}

#[allow(
    dead_code,
    reason = "typed lifecycle diagnostics are retained for crate-internal callers"
)]
pub(crate) fn active_snapshot() -> Result<SessionSnapshot, SessionError> {
    active_client("session snapshot")?.snapshot()
}

fn active_client(operation: &'static str) -> Result<SessionClient, SessionError> {
    optional_active_client(operation)?.ok_or_else(|| SessionError::worker_stopped(operation))
}

fn optional_active_client(operation: &'static str) -> Result<Option<SessionClient>, SessionError> {
    let active = ACTIVE_SESSION.lock().map_err(|_| SessionError {
        kind: SessionErrorKind::Registry,
        operation,
        message: "publisher session registry lock poisoned".into(),
    })?;
    Ok(active.as_ref().map(PublisherSession::client))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Default)]
    struct EffectState {
        generation_current: bool,
        builds: Vec<Option<VideoIntent>>,
        build_results: VecDeque<Result<(), EffectError>>,
        in_place_results: VecDeque<Result<InPlaceChange, EffectError>>,
        ceiling_results: VecDeque<Result<CeilingUpdate, EffectError>>,
        can_apply_ceiling: bool,
        ceiling_attempts: Vec<u32>,
        telemetry: Option<NativeTelemetry>,
        fault: Option<EffectError>,
        live_bindings: Vec<VideoIntent>,
        dormant_bindings: u32,
        teardowns: u32,
        stops: u32,
        operations: Vec<&'static str>,
    }

    #[derive(Clone)]
    struct TestEffects(Arc<Mutex<EffectState>>);

    impl TestEffects {
        fn new() -> (Self, Arc<Mutex<EffectState>>) {
            let state = Arc::new(Mutex::new(EffectState {
                generation_current: true,
                can_apply_ceiling: true,
                ..EffectState::default()
            }));
            (Self(Arc::clone(&state)), state)
        }
    }

    impl LifecycleEffects for TestEffects {
        fn is_generation_current(&self) -> bool {
            self.0.lock().is_ok_and(|state| state.generation_current)
        }

        fn build(&mut self, intent: Option<&VideoIntent>) -> Result<(), EffectError> {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(|_| panic!("effect state lock poisoned"));
            state.builds.push(intent.cloned());
            state.operations.push("build");
            state.build_results.pop_front().unwrap_or(Ok(()))
        }

        fn teardown(&mut self) {
            if let Ok(mut state) = self.0.lock() {
                state.teardowns += 1;
                state.operations.push("teardown");
            }
        }

        fn stop_video(&mut self) -> Result<(), EffectError> {
            if let Ok(mut state) = self.0.lock() {
                state.stops += 1;
            }
            Ok(())
        }

        fn change_in_place(&mut self, _intent: &VideoIntent) -> Result<InPlaceChange, EffectError> {
            self.0
                .lock()
                .unwrap_or_else(|_| panic!("effect state lock poisoned"))
                .in_place_results
                .pop_front()
                .unwrap_or(Ok(InPlaceChange::RequiresRebuild))
        }

        fn can_apply_ceiling(&self) -> bool {
            self.0.lock().is_ok_and(|state| state.can_apply_ceiling)
        }

        fn apply_ceiling(&mut self, ceiling_kbps: u32) -> Result<CeilingUpdate, EffectError> {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(|_| panic!("effect state lock poisoned"));
            state.ceiling_attempts.push(ceiling_kbps);
            state.operations.push("rate");
            state
                .ceiling_results
                .pop_front()
                .unwrap_or(Ok(CeilingUpdate::Applied))
        }

        fn telemetry(&self) -> Option<NativeTelemetry> {
            self.0.lock().ok().and_then(|state| state.telemetry.clone())
        }

        fn poll_fault(&self) -> Option<EffectError> {
            self.0.lock().ok().and_then(|mut state| state.fault.take())
        }

        fn install_live_binding(&mut self, intent: &VideoIntent) -> Result<(), EffectError> {
            if let Ok(mut state) = self.0.lock() {
                state.live_bindings.push(intent.clone());
                state.operations.push("live");
            }
            Ok(())
        }

        fn install_dormant_binding(&mut self) {
            if let Ok(mut state) = self.0.lock() {
                if !state.generation_current {
                    return;
                }
                state.dormant_bindings += 1;
                state.operations.push("dormant");
            }
        }
    }

    fn intent(width: u32, fps: u32, bitrate: f64) -> VideoIntent {
        VideoIntent::new(CaptureConfig {
            width,
            height: 1080,
            fps,
            video_codec: Some("h264".into()),
            max_bitrate: Some(bitrate),
            auto_bitrate: true,
        })
    }

    fn connected_machine() -> (SessionMachine<TestEffects>, Arc<Mutex<EffectState>>) {
        let (effects, state) = TestEffects::new();
        let mut machine = SessionMachine::new(effects, 1);
        machine
            .change_video(intent(1920, 60, 20_000_000.0), Duration::ZERO)
            .unwrap_or_else(|error| panic!("initial apply failed: {error}"));
        (machine, state)
    }

    #[test]
    fn startup_builds_audio_only_and_recovers_audio_only() {
        let (effects, state) = TestEffects::new();
        let mut machine = SessionMachine::new(effects, 1);
        machine
            .start(Duration::ZERO)
            .unwrap_or_else(|error| panic!("audio-only startup failed: {error}"));
        assert_eq!(machine.snapshot().phase, SessionPhase::Connected);
        assert!(machine.snapshot().applied_video.is_none());
        state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .fault = Some(EffectError::new("poll", "connection lost"));

        machine.tick(Duration::from_millis(20));
        assert_eq!(machine.snapshot().phase, SessionPhase::Recovery);
        machine.tick(Duration::from_secs(2));
        assert_eq!(machine.snapshot().phase, SessionPhase::Connected);
        assert_eq!(
            state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"))
                .builds,
            vec![None, None]
        );
    }

    #[test]
    fn startup_build_failure_is_returned_to_connect_caller() {
        let (effects, state) = TestEffects::new();
        state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .build_results
            .push_back(Err(EffectError::new("build", "audio unavailable")));
        let error = PublisherSession::spawn(effects, 1)
            .err()
            .unwrap_or_else(|| panic!("startup failure must reject the session"));

        assert_eq!(error.kind, SessionErrorKind::Effect);
        assert_eq!(error.message, "audio unavailable");
    }

    #[test]
    fn full_command_queue_rejects_without_blocking() {
        let (command_sender, _command_receiver) = mpsc::sync_channel(0);
        let client = SessionClient { command_sender };
        let error = client
            .change_video(intent(1920, 60, 20_000_000.0))
            .err()
            .unwrap_or_else(|| panic!("full command queue must reject"));

        assert_eq!(error.kind, SessionErrorKind::CommandQueueFull);
    }

    #[test]
    fn typed_owner_interface_drives_commands_replies_and_snapshot() {
        let (effects, state) = TestEffects::new();
        state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .telemetry = Some(NativeTelemetry::default());
        let session = PublisherSession::spawn(effects, 1)
            .unwrap_or_else(|error| panic!("session spawn failed: {error}"));
        let outcome = session
            .change_video(intent(1920, 60, 20_000_000.0))
            .unwrap_or_else(|error| panic!("change failed: {error}"));
        assert_eq!(outcome, ConfigOutcome::Applied);
        let snapshot = session
            .snapshot()
            .unwrap_or_else(|error| panic!("snapshot failed: {error}"));
        assert_eq!(snapshot.phase, SessionPhase::Connected);
        session
            .stop_video()
            .unwrap_or_else(|error| panic!("stop failed: {error}"));
        assert!(session.telemetry().is_some());
        session.shutdown();
        assert!(
            state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"))
                .teardowns
                >= 1
        );
    }

    #[test]
    fn active_registry_replaces_a_stale_session_without_clearing_the_new_binding() {
        shutdown_active();
        let (first_effects, first_state) = TestEffects::new();
        let first = PublisherSession::spawn(first_effects, 1)
            .unwrap_or_else(|error| panic!("first session spawn failed: {error}"));
        install_active(first)
            .unwrap_or_else(|error| panic!("first session install failed: {error}"));
        let dormant_before_replacement = first_state
            .lock()
            .unwrap_or_else(|_| panic!("first state lock poisoned"))
            .dormant_bindings;
        first_state
            .lock()
            .unwrap_or_else(|_| panic!("first state lock poisoned"))
            .generation_current = false;

        let (second_effects, second_state) = TestEffects::new();
        let second = PublisherSession::spawn(second_effects, 2)
            .unwrap_or_else(|error| panic!("second session spawn failed: {error}"));
        install_active(second)
            .unwrap_or_else(|error| panic!("second session install failed: {error}"));
        let selected = intent(1280, 30, 10_000_000.0);
        let outcome = change_active(selected.clone())
            .unwrap_or_else(|error| panic!("active change failed: {error}"));
        assert_eq!(outcome, ConfigOutcome::Applied);
        stop_active_video().unwrap_or_else(|error| panic!("active stop failed: {error}"));
        shutdown_active();

        assert_eq!(
            first_state
                .lock()
                .unwrap_or_else(|_| panic!("first state lock poisoned"))
                .dormant_bindings,
            dormant_before_replacement
        );
        assert_eq!(
            second_state
                .lock()
                .unwrap_or_else(|_| panic!("second state lock poisoned"))
                .live_bindings,
            vec![selected]
        );
    }

    #[test]
    fn interface_transitions_dormant_connected_and_shutdown() {
        let (mut machine, state) = connected_machine();
        assert_eq!(machine.snapshot().phase, SessionPhase::Connected);
        machine.shutdown();
        assert_eq!(machine.snapshot().phase, SessionPhase::Shutdown);
        let state = state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"));
        assert!(state.dormant_bindings >= 1);
        assert!(state.teardowns >= 1);
    }

    #[test]
    fn change_video_reports_applied_or_queued() {
        let (mut machine, state) = connected_machine();
        state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .fault = Some(EffectError::new("poll", "connection lost"));
        machine.tick(Duration::from_millis(20));
        assert_eq!(machine.snapshot().phase, SessionPhase::Recovery);
        let state_guard = state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"));
        let dormant = state_guard
            .operations
            .iter()
            .rposition(|operation| *operation == "dormant");
        let teardown = state_guard
            .operations
            .iter()
            .rposition(|operation| *operation == "teardown");
        assert!(dormant.is_some_and(|dormant| teardown.is_some_and(|teardown| dormant < teardown)));
        drop(state_guard);
        let outcome = machine
            .change_video(intent(1280, 30, 10_000_000.0), Duration::from_millis(30))
            .unwrap_or_else(|error| panic!("queue failed: {error}"));
        assert_eq!(outcome, ConfigOutcome::Queued);
    }

    #[test]
    fn stale_generation_rejects_and_shuts_down() {
        let (mut machine, state) = connected_machine();
        state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .generation_current = false;
        let error = machine
            .change_video(intent(1280, 30, 10_000_000.0), Duration::ZERO)
            .err()
            .unwrap_or_else(|| panic!("stale generation must reject the change"));
        assert_eq!(error.kind, SessionErrorKind::StaleGeneration);
        assert_eq!(machine.snapshot().phase, SessionPhase::Shutdown);
        assert_eq!(
            state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"))
                .dormant_bindings,
            0
        );
    }

    #[test]
    fn stop_interrupts_recovery_backoff() {
        let (mut machine, state) = connected_machine();
        state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .fault = Some(EffectError::new("poll", "connection lost"));
        machine.tick(Duration::from_millis(20));
        machine
            .stop_video(Duration::from_millis(30))
            .unwrap_or_else(|error| panic!("stop failed: {error}"));
        assert!(machine.snapshot().has_queued_stop);
        machine.tick(Duration::from_millis(30));
        assert_eq!(machine.snapshot().phase, SessionPhase::Connected);
        assert!(machine.snapshot().applied_video.is_none());
    }

    #[test]
    fn shutdown_interrupts_recovery_backoff() {
        let (mut machine, state) = connected_machine();
        state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .fault = Some(EffectError::new("poll", "connection lost"));
        machine.tick(Duration::from_millis(20));
        let builds_before_shutdown = state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .builds
            .len();
        machine.shutdown();
        machine.tick(Duration::from_secs(30));
        assert_eq!(machine.snapshot().phase, SessionPhase::Shutdown);
        assert_eq!(
            state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"))
                .builds
                .len(),
            builds_before_shutdown
        );
    }

    #[test]
    fn latest_queued_intent_wins() {
        let (mut machine, state) = connected_machine();
        state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .fault = Some(EffectError::new("poll", "connection lost"));
        machine.tick(Duration::from_millis(20));
        let first = intent(1280, 30, 10_000_000.0);
        let latest = intent(960, 24, 8_000_000.0);
        machine
            .change_video(first, Duration::from_millis(30))
            .unwrap_or_else(|error| panic!("first queue failed: {error}"));
        machine
            .change_video(latest.clone(), Duration::from_millis(40))
            .unwrap_or_else(|error| panic!("latest queue failed: {error}"));
        machine.tick(Duration::from_secs(2));
        assert_eq!(machine.snapshot().applied_video, Some(latest));
    }

    #[test]
    fn failed_queued_change_restores_applied_rate_and_records_error() {
        let (mut machine, state) = connected_machine();
        if let Some(applied) = machine.applied.as_mut() {
            applied.rate.current_kbps = 15_000;
        }
        {
            let mut state = state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"));
            state.fault = Some(EffectError::new("poll", "connection lost"));
            state
                .build_results
                .push_back(Err(EffectError::new("build", "queued rejected")));
            state.build_results.push_back(Ok(()));
        }
        machine.tick(Duration::from_millis(20));
        let original = machine.snapshot().applied_video;
        machine
            .change_video(intent(1280, 30, 10_000_000.0), Duration::from_millis(30))
            .unwrap_or_else(|error| panic!("queue failed: {error}"));
        machine.tick(Duration::from_secs(2));
        let snapshot = machine.snapshot();
        assert_eq!(snapshot.phase, SessionPhase::Connected);
        assert_eq!(snapshot.applied_video, original);
        assert_eq!(snapshot.current_ceiling_kbps, Some(15_000));
        assert!(snapshot.queued_video.is_none());
        assert_eq!(
            snapshot.last_error.map(|error| error.message),
            Some("queued rejected".into())
        );
    }

    #[test]
    fn failed_queued_change_and_restore_retries_last_applied_state() {
        let (mut machine, state) = connected_machine();
        {
            let mut state = state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"));
            state.fault = Some(EffectError::new("poll", "connection lost"));
            state
                .build_results
                .push_back(Err(EffectError::new("build", "queued rejected")));
            state
                .build_results
                .push_back(Err(EffectError::new("build", "restore unavailable")));
        }
        machine.tick(Duration::from_millis(20));
        let original = machine.snapshot().applied_video;
        machine
            .change_video(intent(1280, 30, 10_000_000.0), Duration::from_millis(30))
            .unwrap_or_else(|error| panic!("queue failed: {error}"));
        machine.tick(Duration::from_secs(2));
        let recovery = machine.snapshot();
        assert_eq!(recovery.phase, SessionPhase::Recovery);
        assert_eq!(recovery.applied_video, original);
        assert_eq!(
            recovery.last_error.map(|error| error.message),
            Some("queued rejected".into())
        );
        machine.tick(Duration::from_secs(5));
        assert_eq!(machine.snapshot().phase, SessionPhase::Connected);
        assert_eq!(machine.snapshot().applied_video, original);
    }

    #[test]
    fn failed_connected_rebuild_restores_previous_intent_and_adapted_rate() {
        let (mut machine, state) = connected_machine();
        if let Some(applied) = machine.applied.as_mut() {
            applied.rate.current_kbps = 15_000;
        }
        {
            let mut state = state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"));
            state
                .in_place_results
                .push_back(Ok(InPlaceChange::RequiresRebuild));
            state
                .build_results
                .push_back(Err(EffectError::new("build", "new config failed")));
            state.build_results.push_back(Ok(()));
        }
        let original = machine.snapshot().applied_video;
        let result = machine.change_video(intent(1280, 30, 10_000_000.0), Duration::ZERO);
        assert!(result.is_err());
        assert_eq!(machine.snapshot().applied_video, original);
        assert_eq!(machine.snapshot().current_ceiling_kbps, Some(15_000));
        assert_eq!(machine.snapshot().phase, SessionPhase::Connected);
        assert!(
            state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"))
                .ceiling_attempts
                .contains(&15_000)
        );
    }

    #[test]
    fn reconnect_restores_adapted_rate_before_live_binding() {
        let (mut machine, state) = connected_machine();
        if let Some(applied) = machine.applied.as_mut() {
            applied.rate.current_kbps = 15_000;
        }
        state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .fault = Some(EffectError::new("poll", "connection lost"));
        machine.tick(Duration::from_millis(20));
        machine.tick(Duration::from_secs(2));
        let state = state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"));
        assert!(state.ceiling_attempts.contains(&15_000));
        let rate_index = state
            .operations
            .iter()
            .rposition(|operation| *operation == "rate");
        let live_index = state
            .operations
            .iter()
            .rposition(|operation| *operation == "live");
        assert!(rate_index.is_some_and(|rate| live_index.is_some_and(|live| rate < live)));
        assert_eq!(machine.snapshot().current_ceiling_kbps, Some(15_000));
    }

    #[test]
    fn successful_in_place_change_is_authoritative_for_reconnect() {
        let (mut machine, state) = connected_machine();
        state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .in_place_results
            .push_back(Ok(InPlaceChange::Applied(CeilingUpdate::Applied)));
        let changed = intent(1920, 30, 12_000_000.0);
        machine
            .change_video(changed.clone(), Duration::ZERO)
            .unwrap_or_else(|error| panic!("in-place change failed: {error}"));
        state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .fault = Some(EffectError::new("poll", "connection lost"));
        machine.tick(Duration::from_millis(20));
        machine.tick(Duration::from_secs(2));
        let builds = &state
            .lock()
            .unwrap_or_else(|_| panic!("state lock poisoned"))
            .builds;
        assert_eq!(builds.last().and_then(Clone::clone), Some(changed));
    }

    #[test]
    fn failed_rate_application_does_not_commit_proposal() {
        let (mut machine, state) = connected_machine();
        {
            let mut state = state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"));
            state.telemetry = Some(NativeTelemetry {
                video_packets_sent: Some(10_000.0),
                video_packets_lost: Some(0.0),
                ..NativeTelemetry::default()
            });
        }
        machine.tick(Duration::from_secs(1));
        {
            let mut state = state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"));
            state.telemetry = Some(NativeTelemetry {
                video_packets_sent: Some(12_000.0),
                video_packets_lost: Some(600.0),
                ..NativeTelemetry::default()
            });
            state
                .ceiling_results
                .push_back(Err(EffectError::new("rate", "driver rejected")));
        }
        machine.tick(Duration::from_secs(2));
        assert_eq!(machine.snapshot().current_ceiling_kbps, Some(20_000));
        assert_eq!(
            machine.snapshot().last_error.map(|error| error.message),
            Some("driver rejected".into())
        );
    }

    #[test]
    fn pinned_rate_application_does_not_commit_proposal() {
        let (mut machine, state) = connected_machine();
        {
            let mut state = state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"));
            state.telemetry = Some(NativeTelemetry {
                video_packets_sent: Some(10_000.0),
                video_packets_lost: Some(0.0),
                ..NativeTelemetry::default()
            });
        }
        machine.tick(Duration::from_secs(1));
        {
            let mut state = state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"));
            state.telemetry = Some(NativeTelemetry {
                video_packets_sent: Some(12_000.0),
                video_packets_lost: Some(600.0),
                ..NativeTelemetry::default()
            });
            state.ceiling_results.push_back(Ok(CeilingUpdate::Pinned));
        }
        machine.tick(Duration::from_secs(2));
        assert_eq!(machine.snapshot().current_ceiling_kbps, Some(20_000));
    }

    #[test]
    fn attempted_rate_application_commits_under_adr_0001() {
        let (mut machine, state) = connected_machine();
        {
            let mut state = state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"));
            state.telemetry = Some(NativeTelemetry {
                video_packets_sent: Some(10_000.0),
                video_packets_lost: Some(0.0),
                ..NativeTelemetry::default()
            });
        }
        machine.tick(Duration::from_secs(1));
        {
            let mut state = state
                .lock()
                .unwrap_or_else(|_| panic!("state lock poisoned"));
            state.telemetry = Some(NativeTelemetry {
                video_packets_sent: Some(12_000.0),
                video_packets_lost: Some(600.0),
                ..NativeTelemetry::default()
            });
            state
                .ceiling_results
                .push_back(Ok(CeilingUpdate::Attempted));
        }
        machine.tick(Duration::from_secs(2));
        assert_eq!(machine.snapshot().current_ceiling_kbps, Some(15_000));
    }
}
