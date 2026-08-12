//! Linux `LiveKit` publication through the stock `livekitwebrtcsink` plugin.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{LazyLock, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use livekit::webrtc::prelude::{I420Buffer, VideoFrame};

use crate::gstreamer_encoder::{
    GstreamerEncoder, VideoInput, bitrate_bps_to_kbps, encoded_frames, reset_encoded_frames,
};
use crate::{CHANNELS, CaptureConfig, NativeTelemetry, SAMPLE_RATE};

/// Audio appsrc queue depth in buffers (~20 ms of PCM per chunk): ~160 ms.
/// Was 50 (~1 s of PCM) under a non-leaky queue — during congestion the
/// audio chain buffered a full second of stale speech ahead of the video
/// stream (AV-sync drift), then dropped nothing until the backlog overflow.
/// Leaky-upstream bound at 160 ms (see `attach_audio`).
const AUDIO_APPSRC_MAX_BUFFERS: u64 = 8;
const AUDIO_DISCOVERY_SAMPLES: usize = 960;
/// Command reply timeout. 30 s (was 10 s): a legitimate `StartVideo`
/// rebuild can be slow when the SFU connection path stalls, and the worker
/// processes commands serially — a timed-out caller must not race state
/// that the worker will still settle (the worker's eventual `StopVideo`/
/// `Shutdown` processing makes the settled state consistent either way).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const INITIAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Reconnect retry backoff (exponential, capped): the first attempt after a
/// drop waits one second, then 2, 4, 8… up to `RECONNECT_DELAY_MAX`. The
/// SFU outage is usually transient, so retries never give up — but a
/// sustained outage no longer rebuilds the pipeline every second forever.
const RECONNECT_DELAY_BASE: Duration = Duration::from_secs(1);
const RECONNECT_DELAY_MAX: Duration = Duration::from_secs(15);
/// Grace period for `disconnect()`'s bounded worker join: a healthy worker
/// answers `Shutdown` within ~20 ms (its recv timeout); anything still
/// running after the grace is reaped on a detached thread.
const DISCONNECT_GRACE: Duration = Duration::from_secs(2);
const OPUS_BITRATE: i32 = 128_000;
const SINK_MAX_BITRATE: u32 = 200_000_000;

// Congestion-controller tuning. One tick is one POLL_INTERVAL loop
// iteration (~20 ms), so the observation cadence below is ~1 s.
/// Observations between rate steps (~1 s).
const RATE_ADAPT_TICKS: u32 = 50;
/// Interval loss ratio ≥ this triggers a step down (3% of packets lost in
/// one second is a decisive congestion signal; transient wifi single-packet
/// loss stays below it).
const RATE_LOSS_HIGH: f64 = 0.03;
/// Interval loss ratio ≤ this counts as a clean interval (0.5%).
const RATE_LOSS_LOW: f64 = 0.005;
/// Clean intervals before stepping back up toward the configured ceiling
/// (10 s of steady, low-loss sending).
const RATE_RECOVER_TICKS: u32 = 10;
const RATE_STEP_DOWN: f64 = 0.75;
const RATE_STEP_UP: f64 = 1.15;
/// Hard floor for the adapted ceiling: below this the encoder quality
/// degrades faster than the congestion it is trying to escape.
const RATE_FLOOR_KBPS: u32 = 500;

static PUBLISHER: LazyLock<Mutex<Option<PublisherHandle>>> = LazyLock::new(|| Mutex::new(None));
static AUDIO_INPUT: LazyLock<Mutex<Option<gst_app::AppSrc>>> = LazyLock::new(|| Mutex::new(None));
static VIDEO_INPUT: LazyLock<Mutex<Option<VideoInput>>> = LazyLock::new(|| Mutex::new(None));
static ROOM_CONNECTED: AtomicBool = AtomicBool::new(false);
static VIDEO_ACTIVE: AtomicBool = AtomicBool::new(false);
static VIDEO_FRAMES_SUBMITTED: AtomicU64 = AtomicU64::new(0);
// Sample clock for the audio appsrc: PTS is derived from a monotonically
// increasing PCM frame count (see push_pcm), never from the pipeline clock.
static NEXT_AUDIO_FRAME: AtomicU64 = AtomicU64::new(0);
/// Incremented on every `connect`. Workers snapshot it at startup and gate
/// every write to the shared publish state (inputs, connection/video flags)
/// on it: a stale worker that finishes late — reaped after `disconnect()`'s
/// grace period — can never clear or overwrite the *next* worker's state.
static WORKER_GENERATION: AtomicU64 = AtomicU64::new(0);
/// Total PCM chunks dropped by the audio appsrc (for the rate-limited
/// drop report in `feed_pcm`).
static AUDIO_PCM_DROPS: AtomicU64 = AtomicU64::new(0);
/// Wall-clock seconds of the last audio-drop warning (rate limiting).
static LAST_AUDIO_DROP_WARN_AT: AtomicU64 = AtomicU64::new(0);

struct PublisherHandle {
    command_sender: SyncSender<PublisherCommand>,
    join: JoinHandle<()>,
}

#[derive(Clone)]
struct ConnectionConfig {
    url: String,
    token: String,
    room_name: String,
    identity: String,
}

enum PublisherCommand {
    StartVideo {
        config: CaptureConfig,
        reply: SyncSender<Result<(), String>>,
    },
    StopVideo {
        reply: SyncSender<Result<(), String>>,
    },
    GetTelemetry(SyncSender<Option<NativeTelemetry>>),
    Shutdown,
}

enum ConnectedOutcome {
    Idle,
    Reconnect,
    Shutdown,
}

/// Congestion controller for the video encoder: observes the remote-inbound
/// loss ratio (packet deltas between ~1 s telemetry folds) and steps the
/// `vah264enc` VBR ceiling down on sustained loss, back up toward the
/// configured ceiling after clean intervals. Nothing else in the pipeline
/// reacts to congestion — without this the sender keeps pumping at the
/// configured ceiling and the bottleneck simply drops packets (loss +
/// stutter on the spectator side). The adapted rate is re-applied to a
/// freshly rebuilt pipeline after an auto-reconnect; `reset` (explicit
/// stream-settings change) starts from the configured ceiling again.
#[derive(Debug, Clone, Copy, Default)]
struct RateController {
    /// Configured ceiling from the stream settings (the cap `current_kbps`
    /// never exceeds).
    ceiling_kbps: u32,
    /// The currently applied ceiling; starts at `ceiling_kbps` and is
    /// stepped by `observe`.
    current_kbps: u32,
    /// Consecutive clean (~1 s) intervals without high loss.
    clean_ticks: u32,
    /// Packet counters of the previous observation (for interval deltas).
    last_packets_sent: u64,
    last_packets_lost: u64,
    /// Whether the counters have been primed with a first observation.
    primed: bool,
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "packet counters came from the stats fold as finite f64 (exact below 2^53); the loss ratio is clamped, and the stepped ceiling is bounded by [RATE_FLOOR_KBPS, ceiling] before the narrowing"
)]
impl RateController {
    /// (Re)start from a fresh stream-settings ceiling. Called whenever a
    /// `StartVideo` rebuild succeeds with a (possibly new) configuration.
    fn reset(&mut self, config: &CaptureConfig) {
        let ceiling = bitrate_bps_to_kbps(config.max_bitrate.unwrap_or(20_000_000.0));
        self.ceiling_kbps = ceiling;
        self.current_kbps = ceiling;
        self.clean_ticks = 0;
        self.primed = false;
    }

    /// The currently applied ceiling (re-applied after an auto-reconnect
    /// rebuilds the pipeline, which starts at the configured ceiling).
    fn current_kbps(&self) -> u32 {
        self.current_kbps
    }

    /// One ~1 s observation. Returns the new ceiling to apply, or `None` to
    /// hold the current rate.
    fn observe(&mut self, telemetry: &NativeTelemetry) -> Option<u32> {
        let (Some(sent), Some(lost)) = (telemetry.video_packets_sent, telemetry.video_packets_lost)
        else {
            // No outbound or remote-inbound report yet (early session,
            // mid-reconnect, or the SFU hasn't sent a receiver report):
            // hold the current rate — a missing report says nothing.
            return None;
        };
        let (sent, lost) = (sent as u64, lost as u64);
        if !self.primed {
            self.last_packets_sent = sent;
            self.last_packets_lost = lost;
            self.primed = true;
            return None;
        }
        let delta_sent = sent.saturating_sub(self.last_packets_sent);
        let delta_lost = lost.saturating_sub(self.last_packets_lost);
        self.last_packets_sent = sent;
        self.last_packets_lost = lost;
        if delta_sent == 0 {
            // Nothing was sent this interval (encoder idle); no signal.
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
            // Already at the floor.
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
        // Between thresholds: hold, and reset the clean streak — some loss
        // happened, so it was not a clean interval.
        self.clean_ticks = 0;
        None
    }
}

/// Clears the shared publish state, but only if this worker is still the
/// current one. A stale worker that finishes late (reaped after
/// `disconnect()`'s grace period) must never clear the *next* worker's
/// inputs or connection/video flags.
fn finish_if_current(generation: u64) {
    if WORKER_GENERATION.load(Ordering::Relaxed) != generation {
        return;
    }
    clear_inputs();
    ROOM_CONNECTED.store(false, Ordering::Relaxed);
    VIDEO_ACTIVE.store(false, Ordering::Relaxed);
}

struct PublisherPipeline {
    pipeline: gst::Pipeline,
    sink: gst::Element,
    video_config: Option<CaptureConfig>,
    /// The active video encoder: the congestion controller re-targets its
    /// VBR ceiling in place (`adapt_rate`) without rebuilding.
    encoder: Option<GstreamerEncoder>,
    /// Worker generation this pipeline belongs to; every write to the
    /// shared publish state checks it so a stale pipeline (leftover of a
    /// reaped worker) can never clobber the current worker's state.
    generation: u64,
    is_shutdown: bool,
}

pub(crate) fn load_plugins(plugin_dir: &Path) -> Result<(), String> {
    gst::init().map_err(|error| format!("Failed to initialize GStreamer: {error}"))?;
    if !plugin_dir.is_dir() {
        return Err(format!(
            "Bundled GStreamer plugin directory is missing: {}",
            plugin_dir.display()
        ));
    }

    // `gst_registry_scan_path` returns TRUE only when the registry *changed*
    // (i.e. new plugins were discovered). A cache hit — plugins already
    // registered from a previous run — returns FALSE, which is not an error:
    // the required elements are still present. Treat only a genuinely missing
    // directory as fatal.
    gst::Registry::get().scan_path(plugin_dir);

    verify_required_elements()
}

pub(crate) fn connect(
    url: String,
    token: String,
    room_name: String,
    identity: String,
) -> Result<(), String> {
    if url.is_empty() || token.is_empty() || room_name.is_empty() || identity.is_empty() {
        return Err("LiveKit URL, token, room name, and identity are required".into());
    }

    disconnect();
    gst::init().map_err(|error| format!("Failed to initialize GStreamer: {error}"))?;
    verify_required_elements()?;
    let connection = ConnectionConfig {
        url,
        token,
        room_name,
        identity,
    };
    let (command_sender, command_receiver) = mpsc::sync_channel(32);
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    // Bump the generation *after* `disconnect()` so any worker reaped
    // beyond `disconnect()`'s grace period counts as stale: it will gate
    // its teardown and cannot clear the state this new worker installs.
    let generation = WORKER_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let join = thread::Builder::new()
        .name("slopcast-gstreamer-livekit".into())
        .spawn(move || run_worker(&connection, &command_receiver, &ready_sender, generation))
        .map_err(|error| format!("Failed to spawn GStreamer publisher worker: {error}"))?;
    match ready_receiver.recv_timeout(INITIAL_CONNECT_TIMEOUT) {
        Ok(Ok(())) => {
            let mut publisher = PUBLISHER
                .lock()
                .map_err(|_| "GStreamer publisher lock poisoned")?;
            *publisher = Some(PublisherHandle {
                command_sender,
                join,
            });
        }
        Ok(Err(error)) => {
            let _ = join.join();
            return Err(error);
        }
        Err(error) => {
            let _ = command_sender.send(PublisherCommand::Shutdown);
            let _ = join.join();
            return Err(format!("GStreamer publisher startup timed out: {error}"));
        }
    }

    Ok(())
}

pub(crate) fn disconnect() {
    let handle = PUBLISHER
        .lock()
        .ok()
        .and_then(|mut publisher| publisher.take());
    if let Some(handle) = handle {
        // Best-effort Shutdown: if the command queue is saturated (worker
        // wedged in a GStreamer call for 32+ commands), the bounded wait
        // below and the reaper still guarantee eventual reaping.
        if let Err(error) = handle.command_sender.try_send(PublisherCommand::Shutdown) {
            log::warn!("GStreamer publisher Shutdown send failed (queue full): {error}");
        }
        // Bounded wait: a healthy worker answers Shutdown within ~20 ms
        // (its recv timeout), but a worker stuck in a GStreamer call
        // (pathological plugin hang) must not block the Tauri command
        // thread forever. Anything still running after the grace period is
        // reaped on a detached thread (same pattern as the audio ring's
        // worker reaper); the generation gate keeps the late-finishing
        // worker from clearing the *next* worker's state.
        let deadline = std::time::Instant::now() + DISCONNECT_GRACE;
        while !handle.join.is_finished() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if handle.join.is_finished() {
            let _ = handle.join.join();
        } else {
            log::warn!(
                "GStreamer publisher worker did not stop within {DISCONNECT_GRACE:?}; reaping asynchronously"
            );
            let _ = thread::Builder::new()
                .name("slopcast-gstreamer-livekit-reaper".into())
                .spawn(move || {
                    let _ = handle.join.join();
                });
        }
    }
    clear_inputs();
    ROOM_CONNECTED.store(false, Ordering::Relaxed);
    VIDEO_ACTIVE.store(false, Ordering::Relaxed);
}

pub(crate) fn is_connected() -> bool {
    ROOM_CONNECTED.load(Ordering::Relaxed)
}

pub(crate) fn start_video(config: CaptureConfig) -> Result<(), String> {
    let command_sender = command_sender()?;
    let (reply_sender, reply_receiver) = mpsc::sync_channel(1);

    command_sender
        .send(PublisherCommand::StartVideo {
            config,
            reply: reply_sender,
        })
        .map_err(|_| "GStreamer publisher worker stopped")?;

    reply_receiver
        .recv_timeout(COMMAND_TIMEOUT)
        .map_err(|error| format!("GStreamer video start timed out: {error}"))?
}

pub(crate) fn stop_video() -> Result<(), String> {
    let command_sender = command_sender()?;
    let (reply_sender, reply_receiver) = mpsc::sync_channel(1);

    command_sender
        .send(PublisherCommand::StopVideo {
            reply: reply_sender,
        })
        .map_err(|_| "GStreamer publisher worker stopped")?;

    reply_receiver
        .recv_timeout(COMMAND_TIMEOUT)
        .map_err(|error| format!("GStreamer video stop timed out: {error}"))?
}

pub(crate) fn is_video_active() -> bool {
    VIDEO_ACTIVE.load(Ordering::Relaxed)
}

pub(crate) fn push_video_frame(frame: &VideoFrame<I420Buffer>) -> Result<(), String> {
    let input = VIDEO_INPUT
        .lock()
        .map_err(|_| "GStreamer video input lock poisoned")?
        .clone()
        .ok_or_else(|| "GStreamer video publication is not active".to_string())?;

    input.push_frame(frame)?;
    VIDEO_FRAMES_SUBMITTED.fetch_add(1, Ordering::Relaxed);

    Ok(())
}

pub(crate) fn feed_pcm(samples: &[i16]) {
    let input = AUDIO_INPUT.lock().ok().and_then(|input| input.clone());
    let Some(input) = input else {
        return;
    };
    if samples.is_empty() {
        return;
    }

    if let Err(error) = push_pcm(&input, samples) {
        AUDIO_PCM_DROPS.fetch_add(1, Ordering::Relaxed);
        // Rate-limit the warning: at ~50 Hz push cadence a sustained stall
        // would otherwise flood the log with one line per dropped chunk.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now.saturating_sub(LAST_AUDIO_DROP_WARN_AT.load(Ordering::Relaxed)) >= 5 {
            LAST_AUDIO_DROP_WARN_AT.store(now, Ordering::Relaxed);
            log::warn!(
                "GStreamer audio input dropping PCM: {error} ({} chunks dropped since last report)",
                AUDIO_PCM_DROPS.load(Ordering::Relaxed),
            );
        }
    }
}

pub(crate) fn telemetry() -> Option<NativeTelemetry> {
    let command_sender = command_sender().ok()?;
    let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
    command_sender
        .send(PublisherCommand::GetTelemetry(reply_sender))
        .ok()?;

    reply_receiver
        .recv_timeout(Duration::from_millis(500))
        .ok()?
}

fn command_sender() -> Result<SyncSender<PublisherCommand>, String> {
    PUBLISHER
        .lock()
        .map_err(|_| "GStreamer publisher lock poisoned")?
        .as_ref()
        .map(|publisher| publisher.command_sender.clone())
        .ok_or_else(|| "GStreamer publisher is not connected".to_string())
}

fn run_worker(
    connection: &ConnectionConfig,
    command_receiver: &Receiver<PublisherCommand>,
    ready_sender: &SyncSender<Result<(), String>>,
    generation: u64,
) {
    let mut pipeline = None;
    let mut video_config = None;
    let mut rate_controller = RateController::default();
    // The room worker is deliberately dormant until Go Live. Constructing an
    // audio-only sink and putting it in PLAYING publishes audio before the
    // video pad exists; livekitwebrtcsink then keeps the negotiated topology
    // audio-only. The first pipeline is therefore created with both pads.
    let _ = ready_sender.send(Ok(()));

    loop {
        let Some(active_pipeline) = pipeline.as_mut() else {
            match command_receiver.recv_timeout(POLL_INTERVAL) {
                Ok(PublisherCommand::StartVideo { config, reply }) => {
                    if WORKER_GENERATION.load(Ordering::Relaxed) != generation {
                        // A late process from a reaped worker: do not build a
                        // pipeline that would fight the current worker.
                        let _ = reply.send(Err(
                            "GStreamer publisher worker is stale; reconnecting refreshes it".into(),
                        ));
                        continue;
                    }
                    match PublisherPipeline::new(connection, Some(&config), generation) {
                        Ok(new_pipeline) => {
                            video_config = Some(config);
                            pipeline = Some(new_pipeline);
                            let _ = reply.send(Ok(()));
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                Ok(PublisherCommand::StopVideo { reply }) => {
                    let _ = reply.send(Ok(()));
                }
                Ok(PublisherCommand::GetTelemetry(reply)) => {
                    let _ = reply.send(None);
                }
                Ok(PublisherCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            continue;
        };

        match run_connected(
            active_pipeline,
            connection,
            command_receiver,
            &mut video_config,
            &mut rate_controller,
            generation,
        ) {
            ConnectedOutcome::Idle => {
                pipeline = None;
                finish_if_current(generation);
            }
            ConnectedOutcome::Shutdown => break,
            ConnectedOutcome::Reconnect => {
                finish_if_current(generation);
                drop(pipeline.take());
                match reconnect(connection, command_receiver, &mut video_config, generation) {
                    Some(mut reconnected) => {
                        // The new pipeline's encoder starts at the configured
                        // ceiling; re-apply the rate the controller settled
                        // on before the drop so congestion relief survives
                        // the reconnect.
                        reconnected.apply_rate(rate_controller.current_kbps());
                        pipeline = Some(reconnected);
                    }
                    None => break,
                }
            }
        }
    }

    finish_if_current(generation);
}

fn run_connected(
    pipeline: &mut PublisherPipeline,
    connection: &ConnectionConfig,
    command_receiver: &Receiver<PublisherCommand>,
    video_config: &mut Option<CaptureConfig>,
    rate_controller: &mut RateController,
    generation: u64,
) -> ConnectedOutcome {
    let mut rate_ticks: u32 = 0;
    loop {
        match command_receiver.recv_timeout(POLL_INTERVAL) {
            Ok(PublisherCommand::StartVideo { config, reply }) => {
                let result = pipeline.rebuild(connection, Some(&config), generation);
                let should_reconnect = result.is_err();
                if result.is_ok() {
                    *video_config = Some(config);
                }
                let _ = reply.send(result);
                if should_reconnect {
                    return ConnectedOutcome::Reconnect;
                }
                // Rebuild succeeded with (possibly new) settings: start the
                // congestion controller from the configured ceiling again.
                if let Some(config) = video_config.as_ref() {
                    rate_controller.reset(config);
                }
            }
            Ok(PublisherCommand::StopVideo { reply }) => {
                *video_config = None;
                pipeline.shutdown();
                VIDEO_ACTIVE.store(false, Ordering::Relaxed);
                let _ = reply.send(Ok(()));
                return ConnectedOutcome::Idle;
            }
            Ok(PublisherCommand::GetTelemetry(reply)) => {
                let _ = reply.send(Some(pipeline.telemetry()));
            }
            Ok(PublisherCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                return ConnectedOutcome::Shutdown;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        if let Some(error) = pipeline.poll_error() {
            log::warn!("GStreamer LiveKit publisher will reconnect after error: {error}");
            return ConnectedOutcome::Reconnect;
        }

        // ~1 s congestion-control cadence: step the encoder ceiling down on
        // sustained remote-inbound loss, back up toward the configured
        // ceiling after clean intervals.
        rate_ticks += 1;
        if rate_ticks >= RATE_ADAPT_TICKS {
            rate_ticks = 0;
            if pipeline.video_config.is_some() {
                pipeline.adapt_rate(rate_controller);
            }
        }
    }
}

fn reconnect(
    connection: &ConnectionConfig,
    command_receiver: &Receiver<PublisherCommand>,
    video_config: &mut Option<CaptureConfig>,
    generation: u64,
) -> Option<PublisherPipeline> {
    let mut delay = RECONNECT_DELAY_BASE;
    loop {
        // Wait out the backoff, answering commands as they arrive (a
        // StopVideo or Shutdown interrupts the wait promptly even at the
        // deepest backoff).
        let deadline = std::time::Instant::now() + delay;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match command_receiver.recv_timeout(remaining) {
                Ok(PublisherCommand::StartVideo { config, reply }) => {
                    *video_config = Some(config);
                    let _ = reply.send(Ok(()));
                }
                Ok(PublisherCommand::StopVideo { reply }) => {
                    *video_config = None;
                    let _ = reply.send(Ok(()));
                }
                Ok(PublisherCommand::GetTelemetry(reply)) => {
                    let _ = reply.send(None);
                }
                Ok(PublisherCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                    return None;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }

        match PublisherPipeline::new(connection, video_config.as_ref(), generation) {
            Ok(pipeline) => {
                log::info!("GStreamer LiveKit publisher reconnected");
                return Some(pipeline);
            }
            Err(error) => {
                log::warn!("GStreamer LiveKit reconnect failed: {error}");
                delay = (delay * 2).min(RECONNECT_DELAY_MAX);
            }
        }
    }
}

impl PublisherPipeline {
    fn new(
        connection: &ConnectionConfig,
        video_config: Option<&CaptureConfig>,
        generation: u64,
    ) -> Result<Self, String> {
        let audio_caps = gst::Caps::builder("audio/x-opus")
            .field(
                "channels",
                i32::try_from(CHANNELS).map_err(|_| "Audio channels exceed i32")?,
            )
            .field(
                "rate",
                i32::try_from(SAMPLE_RATE).map_err(|_| "Audio rate exceeds i32")?,
            )
            .build();
        let video_caps = video_caps(video_config.and_then(|config| config.video_codec.as_deref()));
        let sink = gst::ElementFactory::make("livekitwebrtcsink")
            .property("audio-caps", &audio_caps)
            .property("video-caps", &video_caps)
            .property("max-bitrate", SINK_MAX_BITRATE)
            .build()
            .map_err(|error| format!("Failed to create livekitwebrtcsink: {error}"))?;
        configure_signaller(&sink, connection, generation);
        let pipeline = gst::Pipeline::new();
        pipeline
            .add(&sink)
            .map_err(|error| format!("Failed to add livekitwebrtcsink: {error}"))?;
        let audio_input = attach_audio(&pipeline, &sink)?;
        let mut publisher = Self {
            pipeline,
            sink,
            video_config: None,
            encoder: None,
            generation,
            is_shutdown: false,
        };
        if let Some(config) = video_config {
            publisher.attach_video(config.clone())?;
        }
        // Only the current worker may take its pipeline live: a stale
        // pipeline (reaped worker still winding down) would otherwise
        // publish a silent zombie track to the room.
        if WORKER_GENERATION.load(Ordering::Relaxed) == generation {
            publisher
                .pipeline
                .set_state(gst::State::Playing)
                .map_err(|error| format!("Failed to start GStreamer LiveKit pipeline: {error}"))?;
        }
        // Gate the shared audio-input install (and the discovery PCM push)
        // on the generation: a stale pipeline must not steal the input a
        // newer worker already installed.
        if WORKER_GENERATION.load(Ordering::Relaxed) == generation {
            set_audio_input(Some(audio_input.clone()))?;
            push_pcm(&audio_input, &[0; AUDIO_DISCOVERY_SAMPLES])?;
        }

        Ok(publisher)
    }

    fn rebuild(
        &mut self,
        connection: &ConnectionConfig,
        video_config: Option<&CaptureConfig>,
        generation: u64,
    ) -> Result<(), String> {
        self.shutdown();
        let replacement = Self::new(connection, video_config, generation)?;
        *self = replacement;

        Ok(())
    }

    fn attach_video(&mut self, config: CaptureConfig) -> Result<(), String> {
        let encoder = GstreamerEncoder::attach(&self.pipeline, &self.sink, &config)?;
        // Gate the shared state install on the generation (see `new`): a
        // stale worker's rebuild must never overwrite the current worker's
        // `VIDEO_INPUT` — two live pipelines fighting over one input would
        // interleave frames.
        if WORKER_GENERATION.load(Ordering::Relaxed) == self.generation {
            set_video_input(Some(encoder.input()))?;
            VIDEO_FRAMES_SUBMITTED.store(0, Ordering::Relaxed);
            reset_encoded_frames();
            VIDEO_ACTIVE.store(true, Ordering::Relaxed);
        }
        self.encoder = Some(encoder);
        self.video_config = Some(config);

        Ok(())
    }

    /// Re-applies an adapted encoder ceiling after a rebuild/reconnect (the
    /// fresh encoder starts at the configured ceiling).
    fn apply_rate(&mut self, ceiling_kbps: u32) {
        if ceiling_kbps == 0 {
            return;
        }
        if let Some(encoder) = self.encoder.as_mut() {
            encoder.set_ceiling_kbps(ceiling_kbps);
        }
    }

    /// One ~1 s congestion-control observation: fold the sink stats, step
    /// the `RateController`, and re-target the encoder when it decides to
    /// move.
    fn adapt_rate(&mut self, rate_controller: &mut RateController) {
        if self.video_config.is_none() {
            return;
        }
        let telemetry = self.telemetry();
        if let Some(ceiling_kbps) = rate_controller.observe(&telemetry) {
            log::info!(
                "GStreamer congestion controller: applying encoder ceiling {ceiling_kbps} kbps"
            );
            self.apply_rate(ceiling_kbps);
        }
    }

    fn poll_error(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        for message in bus.iter_timed(gst::ClockTime::ZERO) {
            match message.view() {
                gst::MessageView::Error(error) => {
                    let source = error.src().map_or_else(
                        || "unknown".to_string(),
                        |source| source.path_string().to_string(),
                    );
                    return Some(format!("{source}: {} ({:?})", error.error(), error.debug()));
                }
                gst::MessageView::Eos(_) => return Some("pipeline reached EOS".into()),
                _ => {}
            }
        }

        None
    }

    fn telemetry(&self) -> NativeTelemetry {
        let stats = self.sink.property::<gst::Structure>("stats");
        fold_telemetry(&stats, self.video_config.as_ref())
    }
}

impl Drop for PublisherPipeline {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl PublisherPipeline {
    fn shutdown(&mut self) {
        if self.is_shutdown {
            return;
        }

        // A stale pipeline (leftover of a reaped worker) still tears down
        // its own pipeline, but must not clear the inputs the *current*
        // worker installed.
        if WORKER_GENERATION.load(Ordering::Relaxed) == self.generation {
            let _ = set_audio_input(None);
            let _ = set_video_input(None);
            VIDEO_ACTIVE.store(false, Ordering::Relaxed);
        }
        let _ = self.pipeline.set_state(gst::State::Null);
        self.is_shutdown = true;
    }
}

fn attach_audio(pipeline: &gst::Pipeline, sink: &gst::Element) -> Result<gst_app::AppSrc, String> {
    let raw_caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("layout", "interleaved")
        .field(
            "rate",
            i32::try_from(SAMPLE_RATE).map_err(|_| "Audio rate exceeds i32")?,
        )
        .field(
            "channels",
            i32::try_from(CHANNELS).map_err(|_| "Audio channels exceed i32")?,
        )
        .build();
    let appsrc = gst_app::AppSrc::builder()
        .caps(&raw_caps)
        .format(gst::Format::Time)
        .is_live(true)
        .block(false)
        .max_buffers(AUDIO_APPSRC_MAX_BUFFERS)
        // Leaky-upstream with a small bound (~160 ms, see
        // AUDIO_APPSRC_MAX_BUFFERS): during congestion the *oldest* PCM is
        // dropped (freshest wins) instead of letting a second of stale
        // speech queue up ahead of the video stream — a half-second
        // AV-sync drift where the presenter's voice lags the screen. The
        // `block(false)` push path never waits; drops are counted and
        // rate-limited-logged in `feed_pcm`.
        .leaky_type(gst_app::AppLeakyType::Upstream)
        .build();
    // PTS is sample-clocked in push_pcm, so the pipeline clock must not
    // stamp buffers (do-timestamp would drift against the sample count and
    // the resampler would hear it as rate wobble). Reset the sample clock
    // per pipeline instance — rebuild() constructs a fresh audio chain.
    NEXT_AUDIO_FRAME.store(0, Ordering::Relaxed);
    let queue = gst::ElementFactory::make("queue")
        .property("max-size-buffers", 10_u32)
        .property("max-size-bytes", 0_u32)
        .property("max-size-time", 100_000_000_u64)
        .build()
        .map_err(|error| format!("Failed to create GStreamer audio queue: {error}"))?;
    let convert = make_element("audioconvert")?;
    let resample = make_element("audioresample")?;
    let encoder = gst::ElementFactory::make("opusenc")
        .property("bitrate", OPUS_BITRATE)
        .property("inband-fec", true)
        .build()
        .map_err(|error| format!("Failed to create GStreamer Opus encoder: {error}"))?;
    let parser = make_element("opusparse")?;
    let output_queue = gst::ElementFactory::make("queue")
        .property("max-size-buffers", 4_u32)
        .property("max-size-bytes", 0_u32)
        .property("max-size-time", 0_u64)
        .build()
        .map_err(|error| format!("Failed to create GStreamer encoded-audio queue: {error}"))?;
    let elements = [
        appsrc.clone().upcast::<gst::Element>(),
        queue,
        convert,
        resample,
        encoder,
        parser,
        output_queue.clone(),
    ];

    pipeline
        .add_many(elements.iter())
        .map_err(|error| format!("Failed to add GStreamer audio elements: {error}"))?;
    gst::Element::link_many(elements.iter())
        .map_err(|error| format!("Failed to link GStreamer audio pipeline: {error}"))?;
    let sink_pad = sink
        .request_pad_simple("audio_%u")
        .ok_or_else(|| "livekitwebrtcsink refused an audio pad".to_string())?;
    output_queue
        .static_pad("src")
        .ok_or_else(|| "GStreamer encoded-audio queue has no src pad".to_string())?
        .link(&sink_pad)
        .map_err(|error| format!("Failed to link Opus into livekitwebrtcsink: {error}"))?;

    Ok(appsrc)
}

fn configure_signaller(sink: &gst::Element, config: &ConnectionConfig, generation: u64) {
    let signaller = sink.property::<gst::glib::Object>("signaller");
    signaller.set_property("ws-url", &config.url);
    signaller.set_property("auth-token", &config.token);
    signaller.set_property("room-name", &config.room_name);
    signaller.set_property("identity", &config.identity);
    // LiveKit join state lives on the signaller's `connection-state`
    // property (`server-connected` and beyond once the join completes);
    // mirror it so is_native_room_connected reflects a real connection,
    // not merely a pipeline that was built. The store is gated on the
    // worker generation: a stale pipeline's signaller must not flick the
    // flag while the current worker is connected.
    signaller.connect_notify(Some("connection-state"), move |obj, _| {
        let connected = obj
            .property_value("connection-state")
            .transform::<String>()
            .ok()
            .and_then(|value| value.get::<String>().ok())
            .is_some_and(|state| {
                matches!(
                    state.as_str(),
                    "server-connected" | "publishing" | "published" | "subscribed"
                )
            });
        if WORKER_GENERATION.load(Ordering::Relaxed) == generation {
            ROOM_CONNECTED.store(connected, Ordering::Relaxed);
        }
    });
}

fn push_pcm(appsrc: &gst_app::AppSrc, samples: &[i16]) -> Result<(), String> {
    let channel_count = usize::try_from(CHANNELS).map_err(|_| "Audio channels exceed usize")?;
    if !samples.len().is_multiple_of(channel_count) {
        return Err(format!(
            "PCM buffer has {} samples, not divisible by {} channels",
            samples.len(),
            channel_count
        ));
    }
    let frames = samples.len() / channel_count;

    let mut buffer = gst::Buffer::with_size(samples.len().saturating_mul(size_of::<i16>()))
        .map_err(|error| format!("Failed to allocate GStreamer audio buffer: {error}"))?;
    {
        let buffer_ref = buffer
            .get_mut()
            .ok_or_else(|| "GStreamer audio buffer is unexpectedly shared".to_string())?;
        let mut mapped = buffer_ref
            .map_writable()
            .map_err(|_| "Failed to map GStreamer audio buffer writable".to_string())?;
        for (sample, destination) in samples.iter().zip(mapped.chunks_exact_mut(2)) {
            destination.copy_from_slice(&sample.to_le_bytes());
        }
        drop(mapped);

        // Sample-clocked PTS: timestamps must advance exactly with the
        // sample count, or the resampler → opusenc → RTP chain hears rate
        // drift as pops and out-of-tune audio. The counter is reset per
        // pipeline instance in attach_audio.
        let start_frame = NEXT_AUDIO_FRAME.fetch_add(frames as u64, Ordering::Relaxed);
        let pts_ns = start_frame * gst::ClockTime::SECOND.nseconds() / u64::from(SAMPLE_RATE);
        let end_ns = (start_frame + frames as u64) * gst::ClockTime::SECOND.nseconds()
            / u64::from(SAMPLE_RATE);
        buffer_ref.set_pts(gst::ClockTime::from_nseconds(pts_ns));
        buffer_ref.set_duration(gst::ClockTime::from_nseconds(end_ns - pts_ns));
    }
    appsrc
        .push_buffer(buffer)
        .map_err(|error| format!("GStreamer appsrc rejected PCM: {error}"))?;

    Ok(())
}

fn set_audio_input(input: Option<gst_app::AppSrc>) -> Result<(), String> {
    *AUDIO_INPUT
        .lock()
        .map_err(|_| "GStreamer audio input lock poisoned")? = input;

    Ok(())
}

fn set_video_input(input: Option<VideoInput>) -> Result<(), String> {
    *VIDEO_INPUT
        .lock()
        .map_err(|_| "GStreamer video input lock poisoned")? = input;

    Ok(())
}

fn clear_inputs() {
    let _ = set_audio_input(None);
    let _ = set_video_input(None);
}

fn make_element(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|error| format!("Failed to create GStreamer element {name}: {error}"))
}

fn verify_required_elements() -> Result<(), String> {
    const REQUIRED: &[&str] = &[
        "appsrc",
        "queue",
        "videoconvert",
        "audioconvert",
        "audioresample",
        "opusenc",
        "opusparse",
        "webrtcbin",
        "nicesrc",
        "nicesink",
        "rtpbin",
        "rtpgccbwe",
        "livekitwebrtcsink",
    ];

    let missing = REQUIRED
        .iter()
        .copied()
        .filter(|name| gst::ElementFactory::find(name).is_none())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    Err(format!(
        "Required GStreamer elements are unavailable: {}",
        missing.join(", ")
    ))
}

pub(crate) fn verify_codec_elements(codec: &str) -> Result<(), String> {
    let parser = match codec {
        "h264" => "h264parse",
        "vp8" | "vp9" => "identity",
        "av1" => "av1parse",
        other => return Err(format!("Unsupported GStreamer video codec: {other}")),
    };
    let required = [parser, selected_encoder_name(codec)];
    let missing = required
        .iter()
        .copied()
        .filter(|name| gst::ElementFactory::make(name).build().is_err())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "GStreamer elements unavailable for {codec}: {}",
        missing.join(", ")
    ))
}

fn selected_encoder_name(codec: &str) -> &'static str {
    let hardware = match codec {
        "h264" => "vah264enc",
        "vp9" => "vavp9enc",
        "av1" => "vaav1enc",
        _ => "",
    };
    let software = match codec {
        "h264" => "x264enc",
        "vp9" => "vp9enc",
        "av1" => "av1enc",
        _ => "vp8enc",
    };
    if hardware.is_empty() {
        return software;
    }
    if gst::ElementFactory::find(hardware).is_none() {
        return software;
    }
    // Factory presence does not guarantee instantiation (missing VA display,
    // driver without encode support). Probe so AV1 does not stick to a
    // non-functional `vaav1enc` when no AV1 hardware exists.
    match gst::ElementFactory::make(hardware).build() {
        Ok(_) => hardware,
        Err(_) => software,
    }
}

fn video_caps(codec: Option<&str>) -> gst::Caps {
    match codec.unwrap_or("h264") {
        "vp8" => gst::Caps::builder("video/x-vp8").build(),
        "vp9" => gst::Caps::builder("video/x-vp9").build(),
        "av1" => gst::Caps::builder("video/x-av1").build(),
        _ => gst::Caps::builder("video/x-h264").build(),
    }
}

pub(crate) fn available_video_codecs() -> Vec<(&'static str, &'static str, bool)> {
    [
        ("vp8", "VP8", "vp8enc", ""),
        ("h264", "H.264", "x264enc", "vah264enc"),
        ("vp9", "VP9", "vp9enc", "vavp9enc"),
        ("av1", "AV1", "av1enc", "vaav1enc"),
    ]
    .into_iter()
    .filter(|(codec, _, software, hardware)| {
        let encoder_available = gst::ElementFactory::find(software).is_some()
            || (!hardware.is_empty() && gst::ElementFactory::find(hardware).is_some());
        encoder_available && verify_codec_elements(codec).is_ok()
    })
    .map(|(codec, label, _software, hardware)| {
        let selected_encoder = selected_encoder_name(codec);
        (
            codec,
            label,
            !hardware.is_empty() && selected_encoder == hardware,
        )
    })
    .collect()
}

fn fold_telemetry(stats: &gst::Structure, config: Option<&CaptureConfig>) -> NativeTelemetry {
    let structures = flatten_structures(stats);
    let mut telemetry = NativeTelemetry {
        timestamp_ms: Some(epoch_timestamp_ms()),
        ..NativeTelemetry::default()
    };
    // gst-webrtc-bin stats carry no reliable `kind` split across
    // versions: the bundled 1.28.6 emits `kind` on the RTP stream
    // structures but master removed it again ("To be added: kind" in
    // gstwebrtcstats.c), so the video/audio split is recovered from the
    // codec each RTP stream references: codec structures expose their
    // `clock-rate` (video is always 90 kHz, Opus 48 kHz) under their
    // `id`, which the RTP stream structures reference via `codec-id`. The
    // pipeline fixes the codecs (vah264enc + opusenc), so the clock rate
    // is unambiguous. Structure *names* also differ by version — 1.28.6
    // still uses `outbound-rtp` / `remote-inbound-rtp` / `codec`, while
    // master renamed them to `rtp-outbound-stream-stats_<ssrc>` /
    // `rtp-remote-inbound-stream-stats_<ssrc>` / `codec-stats-<pad>` —
    // so both variants are matched.
    let codec_clock_rates = structures
        .iter()
        .filter(|structure| is_codec_stats(structure.name().as_str()))
        .filter_map(|structure| {
            Some((
                structure.get::<String>("id").ok()?,
                structure.get::<u32>("clock-rate").ok()?,
            ))
        })
        .collect::<HashMap<_, _>>();
    for structure in structures {
        let name = structure.name().as_str();
        if is_outbound_stats(name) {
            fold_outbound(&structure, &codec_clock_rates, &mut telemetry);
        } else if is_remote_inbound_stats(name) {
            fold_remote_inbound(&structure, &codec_clock_rates, &mut telemetry);
        }
    }
    if let Some(config) = config {
        // Encoded count is measured after h264parse (the real encoder
        // throughput); the submitted count is frames pushed into the appsrc.
        // The gap between them is backpressure drops.
        telemetry.video_frames_encoded = Some(stat_as_f64(encoded_frames()));
        telemetry.video_frames_submitted = Some(VIDEO_FRAMES_SUBMITTED.load(Ordering::Relaxed));
        telemetry.video_width = Some(config.width);
        telemetry.video_height = Some(config.height);
        telemetry.encoder_implementation = Some(format!(
            "GStreamer {}",
            selected_encoder_name(config.video_codec.as_deref().unwrap_or("vp8"))
        ));
        telemetry.video_codec.get_or_insert_with(|| {
            format!(
                "video/{}",
                config
                    .video_codec
                    .as_deref()
                    .unwrap_or("vp8")
                    .to_uppercase()
            )
        });
        // Live appsrc statistics — `dropped` is the stutter diagnostic
        // (buffers the leaky appsrc discarded when the queue was full).
        if let Ok(input) = VIDEO_INPUT.lock()
            && let Some(input) = input.as_ref()
        {
            let stats = input.appsrc_stats();
            telemetry.video_appsrc_input = stats.input;
            telemetry.video_appsrc_output = stats.output;
            telemetry.video_appsrc_dropped = stats.dropped;
            telemetry.video_appsrc_level_buffers = stats.level_buffers;
            telemetry.video_appsrc_level_bytes = stats.level_bytes;
            telemetry.video_appsrc_level_time = stats.level_time;
        }
    }

    telemetry
}

fn flatten_structures(root: &gst::Structure) -> Vec<gst::Structure> {
    let mut flattened = Vec::new();
    flatten_into(root, &mut flattened);
    flattened
}

fn flatten_into(structure: &gst::Structure, flattened: &mut Vec<gst::Structure>) {
    flattened.push(structure.to_owned());
    for (_, value) in structure.iter() {
        if let Ok(nested) = value.get::<gst::Structure>() {
            flatten_into(&nested, flattened);
        }
    }
}

/// Stats structure-name variants seen across gst-webrtc-bin versions
/// (classic names in the bundled 1.28.6, W3C-style names on master).
fn is_outbound_stats(name: &str) -> bool {
    name == "outbound-rtp" || name.starts_with("rtp-outbound-stream-stats_")
}

fn is_remote_inbound_stats(name: &str) -> bool {
    name == "remote-inbound-rtp" || name.starts_with("rtp-remote-inbound-stream-stats_")
}

fn is_codec_stats(name: &str) -> bool {
    name == "codec" || name.starts_with("codec-stats-")
}

/// The RTP stream's media kind, recovered through the codec it references
/// (gst-webrtc-bin's `kind` field is present in 1.28.6 but was removed
/// again on master). The pipeline fixes the codecs — vah264enc at 90 kHz,
/// opusenc at 48 kHz — so the clock rate is unambiguous. `None` when the
/// codec cannot be resolved; such streams are ignored rather than
/// guessed.
fn stream_is_video(
    structure: &gst::Structure,
    codec_clock_rates: &HashMap<String, u32>,
) -> Option<bool> {
    let clock_rate = structure
        .get::<String>("codec-id")
        .ok()
        .and_then(|codec_id| codec_clock_rates.get(&codec_id).copied())?;
    match clock_rate {
        90_000 => Some(true),
        48_000 => Some(false),
        _ => None,
    }
}

fn fold_outbound(
    structure: &gst::Structure,
    codec_clock_rates: &HashMap<String, u32>,
    telemetry: &mut NativeTelemetry,
) {
    let Some(is_video) = stream_is_video(structure, codec_clock_rates) else {
        return;
    };
    let bytes = structure.get::<u64>("bytes-sent").ok().map(stat_as_f64);
    let packets = structure.get::<u64>("packets-sent").ok().map(stat_as_f64);
    if is_video {
        telemetry.video_bytes_sent = bytes;
        telemetry.video_packets_sent = packets;
    } else {
        telemetry.audio_bytes_sent = bytes;
        telemetry.audio_packets_sent = packets;
        // The codec structure carries no usable format marker across
        // versions; the pipeline's only audio codec is Opus at 48 kHz.
        telemetry
            .audio_codec
            .get_or_insert_with(|| "audio/OPUS".into());
    }
}

fn fold_remote_inbound(
    structure: &gst::Structure,
    codec_clock_rates: &HashMap<String, u32>,
    telemetry: &mut NativeTelemetry,
) {
    if stream_is_video(structure, codec_clock_rates) != Some(true) {
        return;
    }

    telemetry.video_packets_lost = structure
        .get::<i32>("packets-lost")
        .ok()
        .map(|packets| stat_as_f64(u64::try_from(packets.max(0)).unwrap_or(0)));
    telemetry.rtt_ms = structure
        .get::<f64>("round-trip-time")
        .ok()
        .filter(|seconds| *seconds > 0.0)
        .map(|seconds| seconds * 1000.0);
}

#[allow(
    clippy::cast_precision_loss,
    reason = "WebRTC byte/frame counters remain well below f64's exact integer range during a session"
)]
fn stat_as_f64(value: u64) -> f64 {
    value as f64
}

fn epoch_timestamp_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;

    /// `Structure` creation asserts a `gst::init`; `gst_init` is not
    /// thread-safe to race, so every test initializes through this once.
    fn init_gst() {
        static INIT: OnceLock<()> = OnceLock::new();
        INIT.get_or_init(|| {
            if let Err(error) = gst::init() {
                panic!("GStreamer must initialize in tests: {error}");
            }
        });
    }

    fn codec(id: &str, clock_rate: u32) -> gst::Structure {
        // Bundled 1.28.6 names the codec structure `codec` (master:
        // `codec-stats-<pad>`) and exposes the W3C-style id, which the
        // RTP stream structures reference via `codec-id`.
        gst::Structure::builder("codec")
            .field("id", id)
            .field("clock-rate", clock_rate)
            .build()
    }

    fn outbound(codec_id: &str, ssrc: u32, bytes: u64, packets: u64) -> gst::Structure {
        // Bundled 1.28.6 names it `outbound-rtp` (master:
        // `rtp-outbound-stream-stats_<ssrc>`).
        gst::Structure::builder("outbound-rtp")
            .field("id", format!("rtp-outbound-stream-stats_{ssrc}"))
            .field("codec-id", codec_id)
            .field("bytes-sent", bytes)
            .field("packets-sent", packets)
            .build()
    }

    // The fixtures mirror what gst-webrtc-bin stats actually emit: classic
    // structure names with W3C-style `id`/`codec-id` fields. `packets-lost`
    // stays an i32.
    #[test]
    fn telemetry_folds_gstreamer_outbound_stats() {
        init_gst();
        let stats = gst::Structure::builder("application/x-webrtcsink-stats")
            .field("video-codec", codec("codec-stats-src_0", 90_000))
            .field("audio-codec", codec("codec-stats-src_1", 48_000))
            .field("video", outbound("codec-stats-src_0", 1000, 42, 7))
            .field("audio", outbound("codec-stats-src_1", 2000, 13, 3))
            .build();

        let telemetry = fold_telemetry(&stats, None);

        assert_eq!(telemetry.video_bytes_sent, Some(42.0));
        assert_eq!(telemetry.video_packets_sent, Some(7.0));
        assert_eq!(telemetry.audio_bytes_sent, Some(13.0));
        assert_eq!(telemetry.audio_packets_sent, Some(3.0));
        assert_eq!(telemetry.audio_codec.as_deref(), Some("audio/OPUS"));
    }

    #[test]
    fn telemetry_folds_video_remote_inbound_loss_and_rtt() {
        init_gst();
        let stats = gst::Structure::builder("application/x-webrtcsink-stats")
            .field("video-codec", codec("codec-stats-src_0", 90_000))
            .field(
                "remote-video",
                gst::Structure::builder("remote-inbound-rtp")
                    .field("id", "rtp-remote-inbound-stream-stats_1000")
                    .field("codec-id", "codec-stats-src_0")
                    .field("packets-lost", -3_i32)
                    .field("round-trip-time", 0.05_f64)
                    .build(),
            )
            .build();

        let telemetry = fold_telemetry(&stats, None);

        assert_eq!(telemetry.video_packets_lost, Some(0.0));
        assert_eq!(telemetry.rtt_ms, Some(50.0));
    }

    #[test]
    fn telemetry_ignores_audio_remote_inbound() {
        init_gst();
        let stats = gst::Structure::builder("application/x-webrtcsink-stats")
            .field("audio-codec", codec("codec-stats-src_1", 48_000))
            .field(
                "remote-audio",
                gst::Structure::builder("remote-inbound-rtp")
                    .field("id", "rtp-remote-inbound-stream-stats_2000")
                    .field("codec-id", "codec-stats-src_1")
                    .field("packets-lost", 7_i32)
                    .build(),
            )
            .build();

        let telemetry = fold_telemetry(&stats, None);

        assert!(telemetry.video_packets_lost.is_none());
        assert!(telemetry.rtt_ms.is_none());
    }

    #[test]
    fn telemetry_ignores_streams_without_a_resolvable_codec() {
        init_gst();
        let stats = gst::Structure::builder("application/x-webrtcsink-stats")
            .field(
                "unknown",
                gst::Structure::builder("rtp-outbound-stream-stats_999")
                    .field("codec-id", "no-such-codec")
                    .field("bytes-sent", 99_u64)
                    .build(),
            )
            .build();

        let telemetry = fold_telemetry(&stats, None);

        assert!(telemetry.video_bytes_sent.is_none());
        assert!(telemetry.audio_bytes_sent.is_none());
    }

    fn telemetry_with_loss(sent: f64, lost: f64) -> NativeTelemetry {
        NativeTelemetry {
            video_packets_sent: Some(sent),
            video_packets_lost: Some(lost),
            ..Default::default()
        }
    }

    fn telemetry_without_remote_inbound() -> NativeTelemetry {
        NativeTelemetry {
            video_packets_sent: Some(1_000.0),
            ..Default::default()
        }
    }

    #[test]
    fn rate_controller_steps_down_on_sustained_loss() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert_eq!(controller.current_kbps(), 20_000);

        // First observation primes the counters; the loss shows up in the
        // second one.
        assert!(
            controller
                .observe(&telemetry_with_loss(10_000.0, 0.0))
                .is_none()
        );
        let next = controller.observe(&telemetry_with_loss(12_000.0, 600.0));
        // 600/2000 = 30% interval loss ≥ 3%: step down 20_000 × 0.75.
        assert_eq!(next, Some(15_000));
        assert_eq!(controller.current_kbps(), 15_000);
    }

    #[test]
    fn rate_controller_holds_between_thresholds() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert!(
            controller
                .observe(&telemetry_with_loss(10_000.0, 0.0))
                .is_none()
        );
        // 2% interval loss: above the clean threshold, below the step-down
        // threshold — hold, and it is not a clean interval.
        assert!(
            controller
                .observe(&telemetry_with_loss(12_000.0, 40.0))
                .is_none()
        );
        assert_eq!(controller.current_kbps(), 20_000);
        assert_eq!(controller.clean_ticks, 0);
    }

    #[test]
    fn rate_controller_holds_without_remote_inbound_stats() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);

        assert!(
            controller
                .observe(&telemetry_without_remote_inbound())
                .is_none()
        );
        assert_eq!(controller.current_kbps(), 20_000);
    }

    #[test]
    fn rate_controller_recovers_after_clean_intervals() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert!(
            controller
                .observe(&telemetry_with_loss(10_000.0, 0.0))
                .is_none()
        );
        assert_eq!(
            controller.observe(&telemetry_with_loss(12_000.0, 600.0)),
            Some(15_000)
        );

        // 9 clean (~1 s) intervals hold; the 10th clean interval steps the
        // rate back up toward the configured ceiling.
        let mut sent = 12_000.0;
        for _ in 0..(RATE_RECOVER_TICKS - 1) {
            sent += 2_000.0;
            assert!(
                controller
                    .observe(&telemetry_with_loss(sent, 0.0))
                    .is_none()
            );
        }
        let next = controller.observe(&telemetry_with_loss(sent + 2_000.0, 0.0));
        assert_eq!(next, Some(17_250)); // 15_000 × 1.15
        assert_eq!(controller.current_kbps(), 17_250);
    }

    #[test]
    fn rate_controller_never_goes_below_the_floor_or_above_the_ceiling() {
        let config = CaptureConfig {
            max_bitrate: Some(500_000.0), // ceiling = 500 kbps = the floor
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert!(
            controller
                .observe(&telemetry_with_loss(10_000.0, 0.0))
                .is_none()
        );

        // 100% interval loss: cannot step below the floor.
        let next = controller.observe(&telemetry_with_loss(12_000.0, 2_000.0));
        assert_eq!(next, None);
        assert_eq!(controller.current_kbps(), 500);

        // Clean intervals cannot push past the ceiling either.
        let mut sent = 12_000.0;
        for _ in 0..(RATE_RECOVER_TICKS * 2) {
            sent += 2_000.0;
            assert!(
                controller
                    .observe(&telemetry_with_loss(sent, 0.0))
                    .is_none()
            );
        }
        assert_eq!(controller.current_kbps(), 500);
    }

    #[test]
    fn rate_controller_reset_starts_from_the_configured_ceiling_again() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert!(
            controller
                .observe(&telemetry_with_loss(10_000.0, 0.0))
                .is_none()
        );
        assert_eq!(
            controller.observe(&telemetry_with_loss(12_000.0, 600.0)),
            Some(15_000)
        );

        // A stream-settings change resets the controller to the new ceiling.
        let changed = CaptureConfig {
            max_bitrate: Some(8_000_000.0),
            ..Default::default()
        };
        controller.reset(&changed);
        assert_eq!(controller.current_kbps(), 8_000);
    }
}
