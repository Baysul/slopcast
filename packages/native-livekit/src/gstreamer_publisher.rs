//! Linux `LiveKit` publication through the stock `livekitwebrtcsink` plugin.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{LazyLock, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::gstreamer_encoder::{
    APPSRC_MAX_BUFFERS, GstreamerEncoder, VideoInput, bitrate_bps_to_kbps, encoded_frames,
    reset_encoded_frames, select_encoder,
};
use crate::{CHANNELS, CaptureConfig, NativeTelemetry, SAMPLE_RATE};
use gst::glib::translate::{ToGlibPtr, from_glib_full};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

/// Audio appsrc queue depth in buffers (~20 ms of PCM per chunk): ~160 ms.
/// Was 50 (~1 s of PCM) under a non-leaky queue — during congestion the
/// audio chain buffered a full second of stale speech ahead of the video
/// stream (AV-sync drift), then dropped nothing until the backlog overflow.
/// Leaky-downstream bound at 160 ms (see `attach_audio`).
const AUDIO_APPSRC_MAX_BUFFERS: u64 = 8;
/// PCM samples pushed at pipeline attach so the sink's codec discovery can
/// see the audio stream. Must span at least one full opusenc frame (20 ms =
/// 960 frames at 48 kHz; with stereo that is 1920 samples) or opusenc never
/// emits, the sink never gets the audio caps, and its signaller stays
/// gated on `codec_discovery_done` — the presenter would never join the
/// room. Two frames (40 ms) cover frame-boundary rounding.
const AUDIO_DISCOVERY_SAMPLES: usize = 3840;
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

// Congestion-controller tuning. One tick is one POLL_INTERVAL loop
// iteration (~20 ms), so the observation cadences below are ~200 ms for
// the local backpressure path and ~1 s for the receiver-loss path.
/// Observations between fast backpressure observations (~200 ms): local
/// signals (appsrc drops, a persistently full queue) show the encoder is
/// falling behind *now*, so they must not wait for the ~1 s receiver-loss
/// report to confirm the overload.
const RATE_BACKPRESSURE_TICKS: u32 = 10;
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
/// Ceiling multiplier on a local backpressure signal. Gentler than the
/// receiver-loss step (`RATE_STEP_DOWN`): it fires as often as every 200 ms
/// while the encoder is behind, so a deep overload compounds quickly
/// without a single transient stall cratering the rate.
const RATE_STEP_BACKPRESSURE: f64 = 0.85;
/// Consecutive fast ticks the appsrc queue must sit at full depth before
/// the queue level alone counts as backpressure. Keyframe and pipeline-
/// warmup bursts fill the 6-buffer queue for a window or two without a
/// drop; three windows (~600 ms) of sustained fullness means the encoder
/// cannot keep up.
const RATE_QUEUE_FULL_TICKS: u32 = 3;
/// Fast ticks after a fullness-only rate step before the queue level alone
/// may drive another one (~2 s at the 200 ms backpressure cadence). A
/// still-full queue after a step cannot keep compounding the weakest
/// congestion signal; drops (a stronger signal) still fire immediately
/// during the cooldown.
const RATE_QUEUE_FULL_COOLDOWN_TICKS: u32 = 10;
/// Hard floor for the adapted ceiling: below this the encoder quality
/// degrades faster than the congestion it is trying to escape.
const RATE_FLOOR_KBPS: u32 = 500;
/// Ceiling used when the stream settings carry no usable `max_bitrate`
/// (missing, zero, or non-finite): the "automatic" start point the
/// controller adapts around.
const DEFAULT_VIDEO_BITRATE_BPS: f64 = 20_000_000.0;

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
/// Real-path diagnostic switch. It removes the audio track from the WebRTC
/// publication, unlike muting the spectator element, so Chromium cannot use
/// it for A/V synchronization during an isolation run.
static AUDIO_PUBLICATION_DISABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("SLOPCAST_DISABLE_AUDIO").is_some());

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

/// The video encoder target the publisher can change without a pipeline
/// rebuild. `codec` is missing only from `CaptureConfig` (VP8 default), but
/// after the first `attach_video` the codec is always known and fixed for the
/// session — an in-place update with a different codec never happens here.
#[derive(Debug, Clone, PartialEq)]
struct VideoTarget {
    codec: String,
    fps: u32,
    ceiling_kbps: u32,
    auto_bitrate: bool,
}

impl VideoTarget {
    fn from_config(config: &CaptureConfig) -> Self {
        Self {
            codec: config.video_codec.as_deref().unwrap_or("vp8").to_string(),
            fps: config.fps,
            ceiling_kbps: configured_ceiling_kbps(config),
            auto_bitrate: config.auto_bitrate,
        }
    }
}

enum ConnectedOutcome {
    Reconnect,
    /// A video settings change arrived that requires a full pipeline rebuild
    /// (the sink's `video-caps` is only changeable in NULL/READY state). The
    /// in-place path in `run_connected` handles codec-unchanged fps/bitrate
    /// changes without one.
    Rebuild {
        config: CaptureConfig,
        reply: SyncSender<Result<(), String>>,
    },
    Shutdown,
}

/// Congestion controller for the video encoder. Two observation paths share
/// one adapted ceiling:
///
/// - **Local backpressure** (`observe_backpressure`, ~200 ms): appsrc drops
///   and a persistently full appsrc queue mean the encoder is falling behind
///   the capture cadence at the current bitrate. These react immediately —
///   long before the receiver's loss report.
/// - **Receiver loss** (`observe`, ~1 s): the remote-inbound loss ratio
///   confirms the overload on the wire. The 3% threshold and 25% step stay;
///   by the time this report lands, backpressure has already cut the rate.
///
/// The ceiling steps down fast and recovers slowly: a step up needs
/// `RATE_RECOVER_TICKS` clean (~1 s) intervals, so an overload cannot
/// oscillate. The adapted rate is re-applied to a freshly rebuilt pipeline
/// after an auto-reconnect; `reset` (explicit stream-settings change) starts
/// from the configured ceiling again.
#[derive(Debug, Clone, Copy, Default)]
struct RateController {
    /// Whether the controller may step the encoder ceiling at all. `false`
    /// (manual bitrate) pins the encoder at the configured ceiling; the
    /// caller must still gate `observe` on this so a manual session never
    /// adapts.
    enabled: bool,
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
    /// Cumulative appsrc dropped buffers of the previous fast observation.
    last_appsrc_dropped: u64,
    /// Consecutive fast ticks the appsrc queue sat at full depth (persistent
    /// encoder underrun; distinct from the drop counter).
    queue_full_ticks: u32,
    /// Fast ticks after a fullness-only step before the queue level may
    /// drive another one; suppresses the weakest signal while the encoder
    /// digests the new rate (drops still fire).
    fullness_cooldown_ticks: u32,
    /// Whether the receiver-loss packet counters have been primed with a
    /// first observation.
    loss_primed: bool,
    /// Whether the backpressure counters (appsrc drops) have been primed
    /// with a first observation.
    backpressure_primed: bool,
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
        self.enabled = config.auto_bitrate;
        let ceiling = configured_ceiling_kbps(config);
        self.ceiling_kbps = ceiling;
        self.current_kbps = ceiling;
        self.clean_ticks = 0;
        self.loss_primed = false;
        self.backpressure_primed = false;
        self.last_appsrc_dropped = 0;
        self.queue_full_ticks = 0;
        self.fullness_cooldown_ticks = 0;
    }

    /// The currently applied ceiling (re-applied after an auto-reconnect
    /// rebuilds the pipeline, which starts at the configured ceiling).
    fn current_kbps(&self) -> u32 {
        self.current_kbps
    }

    /// One ~1 s observation. Returns the new ceiling to apply, or `None` to
    /// hold the current rate. Always holds when disabled (manual bitrate):
    /// a fixed ceiling must never be adapted, regardless of which caller
    /// invokes this.
    fn observe(&mut self, telemetry: &NativeTelemetry) -> Option<u32> {
        if !self.enabled {
            return None;
        }
        let (Some(sent), Some(lost)) = (telemetry.video_packets_sent, telemetry.video_packets_lost)
        else {
            // No outbound or remote-inbound report yet (early session,
            // mid-reconnect, or the SFU hasn't sent a receiver report):
            // hold the current rate — a missing report says nothing.
            return None;
        };
        let (sent, lost) = (sent as u64, lost as u64);
        // Re-prime when the cumulative sent counter regresses: an
        // auto-reconnect builds a fresh pipeline whose GStreamer stats start
        // back near zero, so the previous baseline would read as zero deltas
        // until the new cumulative count caught up (the controller frozen for
        // that window). The lost counter resets together with sent on a
        // rebuild, so the sent regression is the unambiguous signal.
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

    /// One ~200 ms backpressure observation. Steps the ceiling down as soon
    /// as the encoder shows it cannot absorb the capture cadence at the
    /// current bitrate: any appsrc drop (`video_appsrc_dropped`), or an
    /// appsrc queue that stays full across consecutive windows. Receiver
    /// loss (`observe`) remains the slower confirmation — by the time its
    /// ~1 s report lands, the local signal has already cut the rate.
    fn observe_backpressure(&mut self, telemetry: &NativeTelemetry) -> Option<u32> {
        if !self.enabled {
            return None;
        }
        let dropped = telemetry.video_appsrc_dropped.unwrap_or(0);
        // A settings rebuild / reconnect installs a fresh appsrc and resets
        // the drop counter: re-prime on regression instead of reading a
        // bogus (or negative) delta.
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
        // The queue level is the weakest signal: a full queue from a
        // keyframe/warmup burst must persist for several windows to count,
        // and a fullness-only step resets the accumulation and starts a
        // cooldown, so a still-full queue cannot keep stepping every window.
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
            // No drop and the queue either drained or only filled
            // transiently (keyframe/warmup burst): not backpressure yet.
            return None;
        }
        if fullness_only {
            self.queue_full_ticks = 0;
            self.fullness_cooldown_ticks = RATE_QUEUE_FULL_COOLDOWN_TICKS;
        }
        // Backpressure is congestion: recovery must wait for clean intervals
        // again, like the loss path.
        self.clean_ticks = 0;
        let next = ((f64::from(self.current_kbps)) * RATE_STEP_BACKPRESSURE)
            .round()
            .max(f64::from(RATE_FLOOR_KBPS)) as u32;
        if next < self.current_kbps {
            self.current_kbps = next;
            return Some(next);
        }
        // Already at the floor.
        None
    }
}

/// The configured ceiling in kbps from the stream settings. The low-level
/// `bitrate_bps_to_kbps` conversion stays strict; this helper owns the
/// *config* semantics: a missing, zero, or non-finite `max_bitrate` is not a
/// 1 kbps stream, it is "no usable ceiling" — fall back to the automatic
/// default so an automatic/`None` session can never become a 1 kbps encode.
/// Shared with `gstreamer_encoder::attach` so the encoder's initial ceiling
/// always agrees with the controller's.
pub(crate) fn configured_ceiling_kbps(config: &CaptureConfig) -> u32 {
    let bps = config
        .max_bitrate
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(DEFAULT_VIDEO_BITRATE_BPS);
    bitrate_bps_to_kbps(bps)
}

/// Whether the active codec supports in-place ceiling changes. `VPx` and
/// VA-API rate knobs change mid-stream — including `vah264enc` CBR, whose
/// `bitrate` re-derives the driver target in PLAYING state. NVENC
/// mid-stream re-targeting is attempted and bookkept as applied but is
/// unverified on hardware (docs/adr/0001-nvenc-bitrate-retarget.md;
/// `apply_encoder_ceiling` warns once per process). libaom `av1enc`'s CBR
/// `target-bitrate` reconfiguration mid-stream is unverified, so AV1 is
/// pinned to its configured ceiling for now — regardless of which AV1
/// encoder won the selection chain. The default codec (no `video_codec`)
/// is VP8, which can adapt.
fn can_adapt(codec: Option<&str>) -> bool {
    !matches!(codec.unwrap_or("vp8"), "av1")
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
            crate::reap_detached(join, "slopcast-gstreamer-livekit-reaper");
            return Err(error);
        }
        Err(error) => {
            let _ = command_sender.send(PublisherCommand::Shutdown);
            crate::reap_detached(join, "slopcast-gstreamer-livekit-reaper");
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

pub(crate) fn has_active_session() -> bool {
    PUBLISHER.lock().is_ok_and(|publisher| {
        publisher
            .as_ref()
            .is_some_and(|handle| !handle.join.is_finished())
    })
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
    let command_sender = PUBLISHER
        .lock()
        .map_err(|_| "GStreamer publisher lock poisoned")?
        .as_ref()
        .map(|publisher| publisher.command_sender.clone());
    let Some(command_sender) = command_sender else {
        return Ok(());
    };
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

pub(crate) fn push_video_frame(sample: crate::desktop_capture::VideoSample) -> Result<(), String> {
    let input = VIDEO_INPUT
        .lock()
        .map_err(|_| "GStreamer video input lock poisoned")?
        .clone()
        .ok_or_else(|| "GStreamer video publication is not active".to_string())?;

    input.push_frame(sample)?;
    VIDEO_FRAMES_SUBMITTED.fetch_add(1, Ordering::Relaxed);

    Ok(())
}

pub(crate) fn feed_pcm(samples: &[i16]) {
    if *AUDIO_PUBLICATION_DISABLED || samples.is_empty() {
        return;
    }
    // A silent no-input return is legitimate before any room is connected
    // (the audio ring runs as soon as capture starts, before Go Live), so
    // only count it as a drop once a room session exists: the input is then
    // missing because a rebuild/reconnect tore the audio branch down, and
    // the chunks really are lost.
    if !ROOM_CONNECTED.load(Ordering::Relaxed) {
        return;
    }
    let input = AUDIO_INPUT.lock().ok().and_then(|input| input.clone());
    let Some(input) = input else {
        AUDIO_PCM_DROPS.fetch_add(1, Ordering::Relaxed);
        // Rate-limit the warning: at ~50 Hz push cadence a sustained rebuild
        // would otherwise flood the log with one line per dropped chunk.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now.saturating_sub(LAST_AUDIO_DROP_WARN_AT.load(Ordering::Relaxed)) >= 5 {
            LAST_AUDIO_DROP_WARN_AT.store(now, Ordering::Relaxed);
            log::warn!(
                "GStreamer audio input absent during video rebuild/reconnect: {} PCM chunks dropped since last report",
                AUDIO_PCM_DROPS.load(Ordering::Relaxed),
            );
        }
        return;
    };

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
    // Keep the worker dormant until Go Live so the initial offer contains both
    // tracks. After that, the bundled 1.28-era sink renegotiates request-pad
    // additions and removals, allowing video to restart without leaving the
    // room or rebuilding its audio branch.
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
                            rate_controller.reset(&config);
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
                    video_config = None;
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
            command_receiver,
            &mut video_config,
            &mut rate_controller,
        ) {
            ConnectedOutcome::Shutdown => break,
            ConnectedOutcome::Rebuild { config, reply } => {
                finish_if_current(generation);
                // Snapshot the controller before tearing down the pipeline: a
                // failed settings-change rebuild must resume the old settings
                // with the *adapted* ceiling it had reached, not a reset one
                // (an 11.25 Mbps adaptation must not snap back to 20 Mbps).
                let previous_controller = rate_controller;
                drop(pipeline.take());
                match rebuild(connection, &config, generation) {
                    Ok(fresh) => {
                        // Settings changed: the fresh encoder starts at the
                        // NEW configured ceiling and the controller resets to
                        // match — never the old adapted rate.
                        rate_controller.reset(&config);
                        video_config = Some(config);
                        pipeline = Some(fresh);
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        rate_controller = previous_controller;
                        let _ = reply.send(Err(error));
                        match reconnect(
                            connection,
                            command_receiver,
                            &mut video_config,
                            &mut rate_controller,
                            generation,
                        ) {
                            Some(mut reconnected) => {
                                reconnected.apply_rate(rate_controller.current_kbps());
                                pipeline = Some(reconnected);
                            }
                            None => break,
                        }
                    }
                }
            }
            ConnectedOutcome::Reconnect => {
                finish_if_current(generation);
                drop(pipeline.take());
                match reconnect(
                    connection,
                    command_receiver,
                    &mut video_config,
                    &mut rate_controller,
                    generation,
                ) {
                    Some(mut reconnected) => {
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
    command_receiver: &Receiver<PublisherCommand>,
    video_config: &mut Option<CaptureConfig>,
    rate_controller: &mut RateController,
) -> ConnectedOutcome {
    let mut rate_ticks: u32 = 0;
    let mut backpressure_ticks: u32 = 0;
    loop {
        match command_receiver.recv_timeout(POLL_INTERVAL) {
            Ok(PublisherCommand::StartVideo { config, reply }) => {
                if Some(&config) == pipeline.video_config.as_ref() {
                    let _ = reply.send(Ok(()));
                    continue;
                }
                // An fps/bitrate change under the same codec is applied to
                // the running encoder in place: the capture cadence and the
                // PTS clock adapt without touching the WebRTC session, so
                // the spectator's decoder keeps receiving frames. Width,
                // height, or a codec change is a pipeline rebuild.
                let target = VideoTarget::from_config(&config);
                let current = pipeline.video_config.as_ref().map(VideoTarget::from_config);
                let same_frame = pipeline.video_config.as_ref().is_some_and(|current| {
                    current.width == config.width && current.height == config.height
                });
                if current.as_ref().is_some_and(|c| c.codec == target.codec)
                    && same_frame
                    && pipeline.apply_target(&target)
                {
                    pipeline.video_config = Some(config.clone());
                    rate_controller.reset(&config);
                    let _ = reply.send(Ok(()));
                } else {
                    return ConnectedOutcome::Rebuild { config, reply };
                }
            }
            Ok(PublisherCommand::StopVideo { reply }) => {
                *video_config = None;
                let _ = reply.send(pipeline.detach_video());
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

        // ~200 ms backpressure cadence: local signals (appsrc drops, a
        // persistently full queue) mean the encoder is falling behind *now* —
        // step the ceiling immediately instead of waiting a full second for
        // the receiver-loss report to confirm the overload.
        // Only in automatic mode (manual bitrate is pinned) and only for
        // codecs that accept in-place ceiling changes (libaom av1enc's CBR
        // rate path is not verified to).
        backpressure_ticks += 1;
        if backpressure_ticks >= RATE_BACKPRESSURE_TICKS {
            backpressure_ticks = 0;
            if rate_controller.enabled
                && can_adapt(
                    pipeline
                        .video_config
                        .as_ref()
                        .and_then(|config| config.video_codec.as_deref()),
                )
            {
                pipeline.adapt_rate_backpressure(rate_controller);
            }
        }

        // ~1 s congestion-control cadence: step the encoder ceiling down on
        // sustained remote-inbound loss, back up toward the configured
        // ceiling after clean intervals. Only in automatic mode (manual
        // bitrate is pinned) and only for codecs that accept in-place
        // ceiling changes (libaom av1enc's CBR rate path is not verified to).
        rate_ticks += 1;
        if rate_ticks >= RATE_ADAPT_TICKS {
            rate_ticks = 0;
            if rate_controller.enabled
                && can_adapt(
                    pipeline
                        .video_config
                        .as_ref()
                        .and_then(|config| config.video_codec.as_deref()),
                )
            {
                pipeline.adapt_rate(rate_controller);
            }
        }
    }
}

fn reconnect(
    connection: &ConnectionConfig,
    command_receiver: &Receiver<PublisherCommand>,
    video_config: &mut Option<CaptureConfig>,
    rate_controller: &mut RateController,
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
                    if WORKER_GENERATION.load(Ordering::Relaxed) != generation {
                        // A late process from a reaped worker: answering Ok
                        // here would leave the caller holding a dead stream.
                        let _ = reply.send(Err(
                            "GStreamer publisher worker is stale; reconnecting refreshes it".into(),
                        ));
                        continue;
                    }
                    rate_controller.reset(&config);
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

/// Builds a fresh pipeline for a changed video configuration. The new
/// encoder starts at the *new* configured ceiling — unlike `reconnect`,
/// which re-applies the controller's currently adapted rate because the
/// configuration is unchanged.
fn rebuild(
    connection: &ConnectionConfig,
    config: &CaptureConfig,
    generation: u64,
) -> Result<PublisherPipeline, String> {
    let pipeline = PublisherPipeline::new(connection, Some(config), generation)?;
    log::info!("GStreamer LiveKit publisher rebuilt for video settings change");
    Ok(pipeline)
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
        let sink = gst::ElementFactory::make("livekitwebrtcsink")
            .property("audio-caps", &audio_caps)
            .property(
                "video-caps",
                video_config
                    .map(|c| sink_video_caps(c.video_codec.as_deref().unwrap_or("vp8")))
                    .transpose()?
                    .unwrap_or_else(supported_video_caps),
            )
            .build()
            .map_err(|error| format!("Failed to create livekitwebrtcsink: {error}"))?;
        // We hand the sink an already-encoded stream and drive its bitrate
        // ourselves through the encoder + `RateController`. The sink's own
        // GCC congestion controller would fight that (it holds the encoder
        // bitrate hostage on TWCC feedback), and its FEC bloat/burst
        // bandwidth on top of the presenter's configured ceiling — disable
        // both for diagnostic isolation. NACK/RTX retransmission stays on so
        // a lost packet is repaired rather than always surfacing as stutter.
        sink.set_property_from_str("congestion-control", "disabled");
        sink.set_property("do-fec", false);
        sink.set_property("do-retransmission", true);
        connect_payloader_setup(&sink);
        let pipeline = gst::Pipeline::new();
        pipeline
            .add(&sink)
            .map_err(|error| format!("Failed to add livekitwebrtcsink: {error}"))?;
        configure_signaller(&sink, &pipeline, connection, generation);
        let audio_input = if *AUDIO_PUBLICATION_DISABLED {
            log::info!("GStreamer audio publication disabled for diagnostic isolation");
            None
        } else {
            Some(attach_audio(&pipeline, &sink)?)
        };
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
        if WORKER_GENERATION.load(Ordering::Relaxed) == generation
            && let Some(audio_input) = audio_input
        {
            set_audio_input(Some(audio_input.clone()))?;
            push_pcm(&audio_input, &[0; AUDIO_DISCOVERY_SAMPLES])?;
        }

        Ok(publisher)
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

    fn detach_video(&mut self) -> Result<(), String> {
        if WORKER_GENERATION.load(Ordering::Relaxed) == self.generation {
            set_video_input(None)?;
            VIDEO_ACTIVE.store(false, Ordering::Relaxed);
        }
        self.video_config = None;
        let Some(encoder) = self.encoder.as_ref() else {
            return Ok(());
        };

        encoder.detach(&self.pipeline, &self.sink)?;
        self.encoder = None;

        Ok(())
    }

    /// Re-applies an adapted encoder ceiling after a rebuild/reconnect (the
    /// fresh encoder starts at the configured ceiling). Returns whether the
    /// encoder actually changed rate (false for libaom av1enc, whose CBR
    /// rate path is not verified to move mid-stream).
    fn apply_rate(&mut self, ceiling_kbps: u32) -> bool {
        let Some(encoder) = self.encoder.as_mut() else {
            return true;
        };
        encoder.set_ceiling_kbps(ceiling_kbps)
    }

    /// Applies an fps/bitrate change to the running encoder in place (codec
    /// and frame size unchanged). The shared fps atomic updates the PTS
    /// clock and buffer durations on the very next pushed frame, and the
    /// ceiling moves through the same knob the congestion controller uses.
    /// Returns `false` when the encoder cannot move its rate live (libaom
    /// av1enc), in which case the caller must rebuild the pipeline. The
    /// caller's `RateController::reset` re-arms the controller at the new
    /// configured ceiling.
    fn apply_target(&mut self, target: &VideoTarget) -> bool {
        let Some(encoder) = self.encoder.as_mut() else {
            return false;
        };
        if encoder.input().fps() != target.fps {
            encoder.input().set_fps(target.fps);
            log::info!("GStreamer encoder: live fps change to {}", target.fps);
        }
        encoder.set_ceiling_kbps(target.ceiling_kbps)
    }

    /// One ~1 s congestion-control observation: fold the sink stats, step
    /// the `RateController`, and re-target the encoder when it decides to
    /// move.
    fn adapt_rate(&mut self, rate_controller: &mut RateController) {
        let Some(config) = self.video_config.as_ref() else {
            return;
        };
        if !can_adapt(config.video_codec.as_deref()) {
            return;
        }
        let telemetry = self.telemetry();
        if let Some(ceiling_kbps) = rate_controller.observe(&telemetry) {
            log::info!(
                "GStreamer congestion controller: applying encoder ceiling {ceiling_kbps} kbps"
            );
            let _ = self.apply_rate(ceiling_kbps);
        }
    }

    /// One ~200 ms local-backpressure observation: fold the sink stats, step
    /// the `RateController` on encoder-overload signals, and re-target the
    /// encoder when it decides to move.
    fn adapt_rate_backpressure(&mut self, rate_controller: &mut RateController) {
        let Some(config) = self.video_config.as_ref() else {
            return;
        };
        if !can_adapt(config.video_codec.as_deref()) {
            return;
        }
        let telemetry = self.telemetry();
        if let Some(ceiling_kbps) = rate_controller.observe_backpressure(&telemetry) {
            log::info!(
                "GStreamer congestion controller: backpressure, applying encoder ceiling {ceiling_kbps} kbps"
            );
            let _ = self.apply_rate(ceiling_kbps);
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
        // Leaky-downstream with a small bound (~160 ms, see
        // AUDIO_APPSRC_MAX_BUFFERS): during congestion the *oldest* PCM is
        // dropped (freshest wins) instead of letting a second of stale
        // speech queue up ahead of the video stream — a half-second
        // AV-sync drift where the presenter's voice lags the screen. The
        // `block(false)` push path never waits; drops are counted and
        // rate-limited-logged in `feed_pcm`.
        .leaky_type(gst_app::AppLeakyType::Downstream)
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

fn configure_signaller(
    sink: &gst::Element,
    pipeline: &gst::Pipeline,
    config: &ConnectionConfig,
    generation: u64,
) {
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
    // The signaller reports websocket failures (server disconnect, ping
    // timeout, join rejection) through its `error` signal, NOT a GStreamer
    // bus error — the worker's poll_error would never see them and the
    // pipeline would keep encoding into a dead connection while
    // `ROOM_CONNECTED` stays true. Surface the failure as a bus error on
    // the pipeline so the existing reconnect machinery tears down and
    // rejoins. This is the watch on the silent signaller death observed
    // in the field: the websocket vanishes, the app still claims live, and
    // spectators see nothing.
    let pipeline_for_error = pipeline.clone();
    signaller.connect("error", false, move |values| {
        let message = values[1]
            .get::<String>()
            .unwrap_or_else(|_| "LiveKit signaller error".into());
        if WORKER_GENERATION.load(Ordering::Relaxed) == generation {
            log::warn!("GStreamer LiveKit signaller error: {message}");
            let _ = pipeline_for_error.post_message(gst::message::Error::new(
                gst::LibraryError::Failed,
                &format!("LiveKit signaller: {message}"),
            ));
        }
        None
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
    // `videoconvert` stays required: the VA-API and software encoder chains
    // need it (base plugin, always installed) even though NVENC branches
    // build cudaupload/cudaconvertscale instead.
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

/// Whether `GStreamer` can create and initialize an element without the Rust
/// builder's panic path. Transitioning to `READY` also opens hardware device
/// handles, catching unavailable VA displays and drivers before selection.
pub(crate) fn can_initialize_element(name: &str) -> bool {
    let Some(factory) = gst::ElementFactory::find(name) else {
        return false;
    };
    let factory_ptr = factory.to_glib_none().0;
    // SAFETY: `factory` owns a valid factory reference for the duration of the
    // call, and a null name asks GStreamer to use the factory's default name.
    // `from_glib_full` takes ownership of the returned element, if any.
    let element: Option<gst::Element> = unsafe {
        from_glib_full(gst::ffi::gst_element_factory_create(
            factory_ptr,
            std::ptr::null(),
        ))
    };
    let Some(element) = element else {
        return false;
    };
    let initialized = element.set_state(gst::State::Ready).is_ok()
        && element
            .state(Some(gst::ClockTime::from_seconds(1)))
            .0
            .is_ok();
    let _ = element.set_state(gst::State::Null);
    initialized
}

pub(crate) fn verify_codec_elements(codec: &str) -> Result<(), String> {
    // Only the encoder itself must be present and initializable here. On the
    // bundled livekitwebrtcsink we observed an internal parser being inserted
    // after the encoder (see `gstreamer_encoder::attach`), so no external
    // parser factories are required in our branch — but that observation is
    // specific to the bundled 0.15.3 build, not asserted for every runtime.
    // The whole preference chain is probed (NVENC → VA-API → software), so
    // the error names every tried encoder.
    if !matches!(codec, "h264" | "h265" | "vp8" | "vp9" | "av1") {
        return Err(format!("Unsupported GStreamer video codec: {codec}"));
    }
    let encoder = selected_encoder_name(codec);
    if can_initialize_element(encoder) {
        return Ok(());
    }
    let chain = match crate::gstreamer_encoder::codec_chains(codec) {
        Ok(chains) => chains
            .iter()
            .map(|chain| chain.encoder)
            .collect::<Vec<_>>()
            .join(" -> "),
        Err(error) => error,
    };
    Err(format!(
        "GStreamer encoder unavailable for {codec}: tried {chain}"
    ))
}

fn connect_payloader_setup(sink: &gst::Element) {
    sink.connect("payloader-setup", false, |values| {
        let Ok(payloader) = values[3].get::<gst::Element>() else {
            return Some(false.to_value());
        };
        let factory_name = payloader.factory().map(|factory| factory.name());
        if factory_name.is_some_and(|name| name.starts_with("rtpvp9pay"))
            && payloader.find_property("picture-id-mode").is_some()
        {
            // Stock webrtcsink configures the classic rtpvp9pay factory but
            // not rtpvp9pay2, which has equal rank in the bundled runtime.
            payloader.set_property_from_str("picture-id-mode", "15-bit");
        }

        // Preserve webrtcsink's default MTU and header-extension setup.
        Some(false.to_value())
    });
}

pub(crate) fn selected_encoder_name(codec: &str) -> &'static str {
    // Factory presence does not guarantee instantiation (no CUDA device, or
    // a missing VA display / driver encode support), so the whole chain is
    // probed in preference order before a hardware encoder is selected.
    select_encoder(codec, can_initialize_element)
}

fn supported_video_caps() -> gst::Caps {
    gst::Caps::builder_full()
        // Offer the full H.264 profile ladder (see sink_video_caps): any
        // `High` decoder can decode `constrained-baseline`; pinning only
        // the baseline would filter Chrome's `High` offer for no gain.
        .structure(
            gst::Structure::builder("video/x-h264")
                .field("stream-format", "avc")
                .field("alignment", "au")
                .field("profile", "constrained-baseline")
                .build(),
        )
        .structure(
            gst::Structure::builder("video/x-h264")
                .field("stream-format", "avc")
                .field("alignment", "au")
                .field("profile", "main")
                .build(),
        )
        .structure(
            gst::Structure::builder("video/x-h264")
                .field("stream-format", "avc")
                .field("alignment", "au")
                .field("profile", "high")
                .build(),
        )
        .structure(
            gst::Structure::builder("video/x-h265")
                .field("stream-format", "byte-stream")
                .field("alignment", "au")
                .build(),
        )
        .structure(gst::Structure::builder("video/x-vp8").build())
        .structure(gst::Structure::builder("video/x-vp9").build())
        .structure(gst::Structure::builder("video/x-av1").build())
        .build()
}

fn sink_video_caps(codec: &str) -> Result<gst::Caps, String> {
    let caps = match codec {
        "vp8" => gst::Caps::builder("video/x-vp8").build(),
        "vp9" => gst::Caps::builder("video/x-vp9").build(),
        "h264" => gst::Caps::builder_full()
            .structure(
                gst::Structure::builder("video/x-h264")
                    .field("stream-format", "avc")
                    .field("alignment", "au")
                    .field("profile", "constrained-baseline")
                    .build(),
            )
            .structure(
                gst::Structure::builder("video/x-h264")
                    .field("stream-format", "avc")
                    .field("alignment", "au")
                    .field("profile", "main")
                    .build(),
            )
            .structure(
                gst::Structure::builder("video/x-h264")
                    .field("stream-format", "avc")
                    .field("alignment", "au")
                    .field("profile", "high")
                    .build(),
            )
            .build(),
        "h265" => gst::Caps::builder("video/x-h265")
            .field("stream-format", "byte-stream")
            .field("alignment", "au")
            .build(),
        "av1" => gst::Caps::builder("video/x-av1").build(),
        other => return Err(format!("Unsupported codec: {other}")),
    };
    Ok(caps)
}

pub(crate) fn available_video_codecs() -> Vec<(&'static str, String, bool)> {
    codec_chains_list()
        .into_iter()
        .filter(|(codec, _, _)| verify_codec_elements(codec).is_ok())
        .map(|(codec, label, _)| {
            let selected_encoder = selected_encoder_name(codec);
            (
                codec,
                codec_label(label, selected_encoder),
                is_hardware_encoder(selected_encoder),
            )
        })
        .collect()
}

/// The codec table in picker order. Each entry lists the chain's encoders
/// so a codec is offered when any of its encoders exist; the actual
/// selection (and the hardware flag) comes from the probed chain in
/// `gstreamer_encoder::codec_chains`, which is the single source of truth.
fn codec_chains_list() -> [(&'static str, &'static str, [&'static str; 3]); 5] {
    [
        ("vp8", "VP8", ["vp8enc", "", ""]),
        ("h264", "H.264", ["nvh264enc", "vah264enc", "x264enc"]),
        ("h265", "H.265", ["nvh265enc", "vah265enc", "x265enc"]),
        ("vp9", "VP9", ["vp9enc", "", ""]),
        ("av1", "AV1", ["nvav1enc", "vaav1enc", "av1enc"]),
    ]
}

/// `NativeCodecInfo.hardware` flag: the encoder that won the selection
/// chain is a hardware encoder factory.
fn is_hardware_encoder(encoder: &str) -> bool {
    encoder.starts_with("nv") || encoder.starts_with("va")
}

/// Suffix the display label with the winning encoder's hardware vendor so
/// the picker can show e.g. "H.264 (NVENC)"; software labels stay
/// unchanged.
fn codec_label(label: &str, encoder: &str) -> String {
    if let Some(suffix) = encoder_suffix(encoder) {
        format!("{label} ({suffix})")
    } else {
        label.to_string()
    }
}

fn encoder_suffix(encoder: &str) -> Option<&'static str> {
    if encoder.starts_with("nv") {
        Some("NVENC")
    } else if encoder.starts_with("va") {
        Some("VA-API")
    } else {
        None
    }
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
    // pipeline's only audio codec is opusenc, so the clock rate is
    // unambiguous regardless of which video encoder won the selection
    // chain. Structure *names* also differ by version — 1.28.6
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
        // Encoded count is measured after the codec parser (the real encoder
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
        // Clone the input out and read stats after dropping the lock: the
        // capture thread's per-frame push takes the same lock, and a stats
        // read stalled behind a busy GLib context must not hold it up.
        let video_input = VIDEO_INPUT.lock().ok().and_then(|input| input.clone());
        if let Some(input) = video_input {
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
/// again on master). The pipeline's only audio codec — opusenc at 48 kHz —
/// fixes the audio clock rate; video runs at 90 kHz regardless of which
/// video encoder won the selection chain, so the split is unambiguous.
/// `None` when the codec cannot be resolved; such streams are ignored
/// rather than guessed.
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
        telemetry.video_bytes_sent = sum_optional_stat(telemetry.video_bytes_sent, bytes);
        telemetry.video_packets_sent = sum_optional_stat(telemetry.video_packets_sent, packets);
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

fn sum_optional_stat(current: Option<f64>, next: Option<f64>) -> Option<f64> {
    match (current, next) {
        (Some(current), Some(next)) => Some(current + next),
        (Some(current), None) => Some(current),
        (None, next) => next,
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

    #[test]
    fn element_initialization_probe_handles_present_and_missing_factories() {
        init_gst();
        assert!(can_initialize_element("identity"));
        assert!(!can_initialize_element("slopcast-missing-element"));
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
    fn telemetry_sums_primary_and_retransmitted_video_streams() {
        init_gst();
        let stats = gst::Structure::builder("application/x-webrtcsink-stats")
            .field("video-codec", codec("codec-stats-src_0", 90_000))
            .field("rtx-codec", codec("codec-stats-src_1", 90_000))
            .field("video", outbound("codec-stats-src_0", 1000, 42, 7))
            .field("rtx", outbound("codec-stats-src_1", 1001, 13, 3))
            .build();

        let telemetry = fold_telemetry(&stats, None);

        assert_eq!(telemetry.video_bytes_sent, Some(55.0));
        assert_eq!(telemetry.video_packets_sent, Some(10.0));
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

    fn telemetry_with_backpressure(
        appsrc_dropped: u64,
        level_buffers: Option<u32>,
    ) -> NativeTelemetry {
        NativeTelemetry {
            video_appsrc_dropped: Some(appsrc_dropped),
            video_appsrc_level_buffers: level_buffers,
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
            auto_bitrate: true,
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
            auto_bitrate: true,
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
            auto_bitrate: true,
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
            auto_bitrate: true,
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
            auto_bitrate: true,
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
            auto_bitrate: true,
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
            auto_bitrate: true,
            ..Default::default()
        };
        controller.reset(&changed);
        assert_eq!(controller.current_kbps(), 8_000);
    }

    #[test]
    fn rate_controller_reprimes_after_counter_regression() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            auto_bitrate: true,
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert!(
            controller
                .observe(&telemetry_with_loss(10_000.0, 0.0))
                .is_none()
        );

        // An auto-reconnect rebuilds the pipeline: the fresh GStreamer stats
        // start near zero, so the cumulative counters regress. The controller
        // must re-prime instead of computing a bogus zero (or huge) delta.
        assert!(
            controller
                .observe(&telemetry_with_loss(50.0, 0.0))
                .is_none()
        );
        // The re-primed baseline is now 50; the next observation measures
        // against it normally.
        assert_eq!(
            controller.observe(&telemetry_with_loss(2_050.0, 60.0)),
            Some(15_000) // 60/2000 = 3% → step down 20_000 × 0.75
        );
    }

    #[test]
    fn rate_controller_steps_down_immediately_on_appsrc_drops() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            auto_bitrate: true,
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert_eq!(controller.current_kbps(), 20_000);

        // First fast observation primes the backpressure baseline.
        assert!(
            controller
                .observe_backpressure(&telemetry_with_backpressure(0, Some(0)))
                .is_none()
        );
        // A dropped frame in the next window steps down at the fast cadence
        // — no ~1 s wait for the receiver-loss report.
        let next = controller.observe_backpressure(&telemetry_with_backpressure(1, Some(6)));
        assert_eq!(next, Some(17_000)); // 20_000 × 0.85
        assert_eq!(controller.current_kbps(), 17_000);
    }

    #[test]
    fn rate_controller_queue_level_requires_sustained_fullness() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            auto_bitrate: true,
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert!(
            controller
                .observe_backpressure(&telemetry_with_backpressure(0, Some(0)))
                .is_none()
        );

        // Keyframe/warmup bursts fill the queue for a window or two without
        // a drop: the level alone must hold full for several windows.
        for _ in 0..(RATE_QUEUE_FULL_TICKS - 1) {
            assert!(
                controller
                    .observe_backpressure(&telemetry_with_backpressure(0, Some(6)))
                    .is_none()
            );
            assert_eq!(controller.current_kbps(), 20_000);
        }
        let next = controller.observe_backpressure(&telemetry_with_backpressure(0, Some(6)));
        assert_eq!(next, Some(17_000));

        // A fullness-only step resets the accumulation and starts a cooldown:
        // a still-full queue cannot keep stepping every ~200 ms window.
        for _ in 0..RATE_QUEUE_FULL_COOLDOWN_TICKS {
            assert!(
                controller
                    .observe_backpressure(&telemetry_with_backpressure(0, Some(6)))
                    .is_none()
            );
            assert_eq!(controller.current_kbps(), 17_000);
        }
        // After the cooldown the queue must re-accumulate sustained fullness
        // before the weak signal may step again.
        for _ in 0..(RATE_QUEUE_FULL_TICKS - 1) {
            assert!(
                controller
                    .observe_backpressure(&telemetry_with_backpressure(0, Some(6)))
                    .is_none()
            );
        }
        let next = controller.observe_backpressure(&telemetry_with_backpressure(0, Some(6)));
        assert_eq!(next, Some(14_450)); // 17_000 × 0.85

        // Queue drains → no further steps.
        assert!(
            controller
                .observe_backpressure(&telemetry_with_backpressure(0, Some(0)))
                .is_none()
        );
        assert_eq!(controller.current_kbps(), 14_450);
    }

    #[test]
    fn rate_controller_backpressure_reprimes_after_counter_regression() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            auto_bitrate: true,
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert!(
            controller
                .observe_backpressure(&telemetry_with_backpressure(0, Some(0)))
                .is_none()
        );
        assert_eq!(
            controller.observe_backpressure(&telemetry_with_backpressure(1, Some(6))),
            Some(17_000)
        );

        // An auto-reconnect rebuilds the pipeline: the fresh appsrc starts
        // its counters back at zero. The controller must re-prime instead of
        // reading the regression as a (negative) delta.
        assert!(
            controller
                .observe_backpressure(&telemetry_with_backpressure(0, Some(0)))
                .is_none()
        );
        // The re-primed baseline is now 0; the next observation measures
        // against it normally.
        assert_eq!(
            controller.observe_backpressure(&telemetry_with_backpressure(1, Some(0))),
            Some(14_450) // 17_000 × 0.85
        );
    }

    #[test]
    fn rate_controller_observation_paths_prime_independently() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            auto_bitrate: true,
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);

        // The loss path primes its own baseline...
        assert!(
            controller
                .observe(&telemetry_with_loss(10_000.0, 0.0))
                .is_none()
        );
        // ...so the backpressure path's first observation must not read the
        // loss baseline as a drop delta: it primes its own counters instead.
        assert!(
            controller
                .observe_backpressure(&telemetry_with_backpressure(5, Some(6)))
                .is_none()
        );
        assert_eq!(controller.current_kbps(), 20_000);
        // Primed now, the next observation measures against the new baseline.
        assert_eq!(
            controller.observe_backpressure(&telemetry_with_backpressure(6, Some(6))),
            Some(17_000) // 20_000 × 0.85
        );

        // The other direction: backpressure priming must not let the loss
        // path's first observation read a stale zero baseline as 3% loss.
        controller.reset(&config);
        assert!(
            controller
                .observe_backpressure(&telemetry_with_backpressure(0, Some(0)))
                .is_none()
        );
        assert!(
            controller
                .observe(&telemetry_with_loss(10_000.0, 300.0))
                .is_none()
        );
        assert_eq!(controller.current_kbps(), 20_000);
    }

    #[test]
    fn rate_controller_backpressure_never_goes_below_the_floor() {
        let config = CaptureConfig {
            max_bitrate: Some(500_000.0), // ceiling = 500 kbps = the floor
            auto_bitrate: true,
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert!(
            controller
                .observe_backpressure(&telemetry_with_backpressure(0, Some(0)))
                .is_none()
        );

        // Sustained drops cannot step below the floor.
        for _ in 0..10 {
            assert!(
                controller
                    .observe_backpressure(&telemetry_with_backpressure(1, Some(6)))
                    .is_none()
            );
        }
        assert_eq!(controller.current_kbps(), 500);
    }

    #[test]
    fn rate_controller_backpressure_respects_manual_mode() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            auto_bitrate: false,
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert!(!controller.enabled);

        // Even sustained backpressure must not move a manual ceiling.
        assert!(
            controller
                .observe_backpressure(&telemetry_with_backpressure(1, Some(6)))
                .is_none()
        );
        assert_eq!(controller.current_kbps(), 20_000);
    }

    #[test]
    fn rate_controller_backpressure_restarts_the_clean_streak() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            auto_bitrate: true,
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert!(
            controller
                .observe(&telemetry_with_loss(10_000.0, 0.0))
                .is_none()
        );
        assert!(
            controller
                .observe_backpressure(&telemetry_with_backpressure(0, Some(0)))
                .is_none()
        );
        // The loss path accrued clean intervals...
        assert!(
            controller
                .observe(&telemetry_with_loss(12_000.0, 0.0))
                .is_none()
        );
        assert!(
            controller
                .observe(&telemetry_with_loss(14_000.0, 0.0))
                .is_none()
        );
        assert_eq!(controller.clean_ticks, 2);

        // ...then backpressure steps down and must reset the streak so
        // recovery does not jump back up right after an overload.
        assert_eq!(
            controller.observe_backpressure(&telemetry_with_backpressure(1, Some(6))),
            Some(17_000)
        );
        assert_eq!(controller.clean_ticks, 0);
    }

    #[test]
    fn rate_controller_manual_never_adapts() {
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            auto_bitrate: false,
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert!(!controller.enabled);
        assert_eq!(controller.current_kbps(), 20_000);

        // Even 100% interval loss must not move a manual ceiling.
        assert!(
            controller
                .observe(&telemetry_with_loss(12_000.0, 2_000.0))
                .is_none()
        );
        assert_eq!(controller.current_kbps(), 20_000);
    }

    #[test]
    fn rate_controller_automatic_steps_down_on_loss() {
        let config = CaptureConfig {
            max_bitrate: Some(10_000_000.0),
            auto_bitrate: true,
            ..Default::default()
        };
        let mut controller = RateController::default();
        controller.reset(&config);
        assert!(controller.enabled);
        assert_eq!(controller.current_kbps(), 10_000);

        assert!(
            controller
                .observe(&telemetry_with_loss(10_000.0, 0.0))
                .is_none()
        );
        assert_eq!(
            controller.observe(&telemetry_with_loss(12_000.0, 600.0)),
            Some(7_500) // 30% loss → 10_000 × 0.75
        );
        assert_eq!(controller.current_kbps(), 7_500);
    }

    #[test]
    fn rate_controller_snapshot_restore_preserves_adapted_rate() {
        // The worker's failed settings-rebuild path snapshots the controller
        // before the pipeline teardown and restores it on failure, so the old
        // configuration resumes at its *adapted* ceiling, not a reset one.
        let config = CaptureConfig {
            max_bitrate: Some(20_000_000.0),
            auto_bitrate: true,
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

        let snapshot = controller;
        let changed = CaptureConfig {
            max_bitrate: Some(6_000_000.0),
            auto_bitrate: true,
            ..Default::default()
        };
        controller.reset(&changed);
        assert_eq!(controller.current_kbps(), 6_000);

        controller = snapshot;
        assert_eq!(controller.current_kbps(), 15_000);
    }

    #[test]
    fn configured_ceiling_kbps_never_yields_one_kbps_from_invalid_input() {
        // The "automatic gives one frame" failure class: a missing/zero/
        // non-finite ceiling must normalize to the automatic default, never
        // to a 1 kbps encode.
        assert_eq!(
            configured_ceiling_kbps(&CaptureConfig {
                max_bitrate: Some(0.0),
                ..Default::default()
            }),
            20_000
        );
        assert_eq!(
            configured_ceiling_kbps(&CaptureConfig {
                max_bitrate: Some(f64::NAN),
                ..Default::default()
            }),
            20_000
        );
        assert_eq!(
            configured_ceiling_kbps(&CaptureConfig {
                max_bitrate: None,
                ..Default::default()
            }),
            20_000
        );
        assert_eq!(
            configured_ceiling_kbps(&CaptureConfig {
                max_bitrate: Some(20_000_000.0),
                ..Default::default()
            }),
            20_000
        );
    }

    #[test]
    fn can_adapt_allows_every_codec_except_av1() {
        assert!(!can_adapt(Some("av1")));
        assert!(can_adapt(Some("h264")));
        assert!(can_adapt(Some("h265")));
        assert!(can_adapt(Some("vp8")));
        assert!(can_adapt(Some("vp9")));
        assert!(can_adapt(None));
    }

    #[test]
    fn available_video_codecs_derives_labels_and_hardware_from_the_selection_chain() {
        init_gst();
        let codecs = available_video_codecs();
        assert!(!codecs.is_empty());
        for (codec, label, hardware) in &codecs {
            let selected = selected_encoder_name(codec);
            let is_hardware = selected.starts_with("nv") || selected.starts_with("va");
            assert_eq!(*hardware, is_hardware, "{codec} via {selected}");
            if is_hardware {
                assert!(
                    label.contains("NVENC") || label.contains("VA-API"),
                    "{codec} label must carry the encoder suffix: {label}"
                );
            } else {
                assert!(
                    !label.contains('('),
                    "{codec} label must stay bare: {label}"
                );
            }
        }
    }
}
