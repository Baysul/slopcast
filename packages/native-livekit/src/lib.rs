#[cfg(not(target_os = "linux"))]
use std::collections::VecDeque;
#[cfg(not(target_os = "linux"))]
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
#[cfg(not(target_os = "linux"))]
use std::sync::{Arc, Mutex};

mod desktop_capture;

#[cfg(target_os = "linux")]
mod gstreamer_encoder;

#[cfg(target_os = "linux")]
mod gstreamer_publisher;

#[cfg(target_os = "linux")]
mod linux_capture;

#[cfg(target_os = "windows")]
mod wgc_capture;

#[cfg(target_os = "windows")]
pub use wgc_capture::{CaptureSourceInfo, WgcSourceKind};

#[cfg(not(target_os = "linux"))]
use arc_swap::ArcSwapOption;
#[cfg(not(target_os = "linux"))]
use livekit::options::{TrackPublishOptions, VideoCodec, VideoEncoding};
#[cfg(not(target_os = "linux"))]
use livekit::prelude::*;
#[cfg(not(target_os = "linux"))]
use livekit::track::{LocalTrack, LocalVideoTrack, TrackSource};
#[cfg(not(target_os = "linux"))]
use livekit::webrtc::audio_frame::AudioFrame;
#[cfg(not(target_os = "linux"))]
use livekit::webrtc::audio_source::AudioSourceOptions;
#[cfg(not(target_os = "linux"))]
use livekit::webrtc::audio_source::native::NativeAudioSource;
#[cfg(not(target_os = "linux"))]
use livekit::webrtc::prelude::{RtcAudioSource, RtcVideoSource, VideoResolution};
#[cfg(not(target_os = "linux"))]
use livekit::webrtc::stats::RtcStats;
#[cfg(not(target_os = "linux"))]
use livekit::webrtc::video_source::native::NativeVideoSource;
#[cfg(not(target_os = "linux"))]
use std::collections::HashMap;
#[cfg(not(target_os = "linux"))]
use std::time::{Duration, Instant};

/// Reaps a worker `JoinHandle` on a detached thread so a wedged worker can
/// never block its caller indefinitely. Shared by the startup-timeout paths
/// (a worker that ignores its stop flag is detached, not joined). The handle
/// is dropped with the closure — the worker's OS thread is reclaimed whenever
/// it finally unwinds.
#[cfg(target_os = "linux")]
pub(crate) fn reap_detached(join: std::thread::JoinHandle<()>, name: &'static str) {
    if join.is_finished() {
        let _ = join.join();
        return;
    }
    let _ = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = join.join();
        });
}

pub const SAMPLE_RATE: u32 = 48000;
pub const CHANNELS: u32 = 2;

/// Maximum audio backlog in the room worker before the oldest samples are
/// dropped (drop-oldest): bounds how far audio content can fall behind the
/// live video after an upstream stall, at the cost of skipping the stale
/// tail instead of playing it out late.
#[cfg(not(target_os = "linux"))]
const MAX_AUDIO_BACKLOG_MS: usize = 100;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_codec: Option<String>,
    pub max_bitrate: Option<f64>,
}

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeTelemetry {
    pub video_codec: Option<String>,
    /// The actual encoder libwebrtc used for the video track, from the
    /// `encoderImplementation` outbound-rtp stat; `None` until the stack
    /// reports it.
    pub encoder_implementation: Option<String>,
    pub video_bytes_sent: Option<f64>,
    pub video_packets_sent: Option<f64>,
    pub video_packets_lost: Option<f64>,
    /// `framesEncoded` from the outbound-rtp stat (m144's `framesSent`
    /// never increments); the renderer derives fps from this. On the Linux
    /// `GStreamer` branch this is the count of encoded H.264 access units
    /// measured after `h264parse` — the true encoder-throughput counter.
    pub video_frames_encoded: Option<f64>,
    /// On the Linux `GStreamer` branch: frames pushed into the video appsrc
    /// (`push_frame` successes). Any shortfall vs. `video_frames_encoded`
    /// is frames dropped by the leaky-appsrc / queue backpressure path
    /// before they could be encoded; `video_appsrc_dropped` quantifies it.
    pub video_frames_submitted: Option<u64>,
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    /// Live video appsrc statistics — `dropped` counts buffers the appsrc
    /// discarded (leaky downstream on a full queue), the stutter diagnostic;
    /// the levels show how close the appsrc is to its 2-buffer cap.
    pub video_appsrc_input: Option<u64>,
    pub video_appsrc_output: Option<u64>,
    pub video_appsrc_dropped: Option<u64>,
    pub video_appsrc_level_buffers: Option<u32>,
    pub video_appsrc_level_bytes: Option<u32>,
    pub video_appsrc_level_time: Option<u64>,
    pub audio_codec: Option<String>,
    pub audio_bytes_sent: Option<f64>,
    pub audio_packets_sent: Option<f64>,
    pub audio_packets_lost: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub timestamp_ms: Option<f64>,
}

/// A codec the bundled libwebrtc can actually encode with, as exposed to the
/// renderer's codec picker. The picker must NEVER read the webview's
/// `RTCRtpSender.getCapabilities` — that stack is not used for encoding.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeCodecInfo {
    pub codec: String,
    pub label: String,
    /// True when the bundled libwebrtc ships a hardware encoder factory for
    /// this codec on this platform (VA-API/NVENC on Linux, `Media Foundation`
    /// on Windows, `VideoToolbox` on macOS); VP8/VP9/AV1 are software-only.
    pub hardware: bool,
}

/// Encoder support baked into the bundled libwebrtc build
/// (`webrtc-sys` prebuilts: `rtc_use_h264`/`rtc_use_h265` +
/// `rtc_libvpx_build_vp9` + `enable_libaom` on every platform). The native
/// stack hardware-encodes only H264 (H265 on some platforms), so H264 is the
/// only codec that can ever use a hardware encoder.
pub const NATIVE_VIDEO_CODECS: [(&str, &str); 4] = [
    ("h264", "H.264"),
    ("vp8", "VP8"),
    ("vp9", "VP9"),
    ("av1", "AV1"),
];

/// The single codec with a hardware encoder factory in the bundled libwebrtc
/// build (VA-API + NVENC on Linux, `Media Foundation` on Windows,
/// `VideoToolbox` on macOS).
pub const NATIVE_HW_CODEC: &str = "h264";

/// Returns the codecs the native stack can encode with (build-time constant,
/// verified against the bundled libwebrtc in `get_native_supported_codecs`).
#[must_use]
pub fn get_native_supported_codecs() -> Vec<NativeCodecInfo> {
    #[cfg(target_os = "linux")]
    {
        let _ = gstreamer::init();
        gstreamer_publisher::available_video_codecs()
            .into_iter()
            .map(|(codec, label, hardware)| NativeCodecInfo {
                codec: codec.into(),
                label: label.into(),
                hardware,
            })
            .collect()
    }

    #[cfg(not(target_os = "linux"))]
    NATIVE_VIDEO_CODECS
        .iter()
        .map(|(codec, label)| NativeCodecInfo {
            codec: (*codec).to_string(),
            label: (*label).to_string(),
            hardware: *codec == NATIVE_HW_CODEC,
        })
        .collect()
}

/// Per-stage capture counters, reset on every `startDesktopCapture`.
/// `previewFramesSent` counts preview frames scaled to the renderer's
/// preview card (OBS-style "scale to the window") at the stream framerate;
/// `captureErrors` counts capturer failures.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopCaptureStats {
    pub frames_dequeued: i64,
    pub frames_pushed: i64,
    pub frames_dropped: i64,
    pub capture_errors: i64,
    pub preview_frames_sent: i64,
    pub keepalive_attempted: i64,
    pub keepalive_pushed: i64,
    pub keepalive_dropped: i64,
    pub last_width: i64,
    pub last_height: i64,
}

#[cfg(not(target_os = "linux"))]
enum WorkerCmd {
    StartVideo {
        config: CaptureConfig,
    },
    StopVideo,
    GetTelemetry {
        reply: std::sync::mpsc::Sender<NativeTelemetry>,
    },
    Shutdown,
}

#[cfg(not(target_os = "linux"))]
struct NativeLiveKit {
    cmd_tx: tokio::sync::mpsc::UnboundedSender<WorkerCmd>,
    stop: tokio::sync::oneshot::Sender<()>,
    _join: std::thread::JoinHandle<()>,
}

/// ~1.28 s of 10 ms chunks; full means WebRTC encoding is stalled, so the
/// newest chunk is dropped (drop-newest, like `audio_ring`).
#[cfg(not(target_os = "linux"))]
const PCM_CHANNEL_CAPACITY: usize = 128;

#[cfg(not(target_os = "linux"))]
static LIVEKIT: Mutex<Option<NativeLiveKit>> = Mutex::new(None);

/// Worker's PCM sender, kept under its own lock (not `LIVEKIT`): `feed_pcm`
/// runs on the audio-ring worker and must never block on
/// `connect_livekit_room`'s long lock hold — that was the original deadlock.
#[cfg(not(target_os = "linux"))]
static PCM_SENDER: Mutex<Option<tokio::sync::mpsc::Sender<Vec<i16>>>> = Mutex::new(None);

#[cfg(not(target_os = "linux"))]
static ROOM_CONNECTED: AtomicBool = AtomicBool::new(false);
#[cfg(not(target_os = "linux"))]
static SPECTATOR_COUNT: AtomicU32 = AtomicU32::new(0);
#[cfg(not(target_os = "linux"))]
static VIDEO_ACTIVE: AtomicBool = AtomicBool::new(false);
/// The published video track's source; the desktop capturer feeds it frames.
/// `None` while no track is active.
#[cfg(not(target_os = "linux"))]
pub(crate) static VIDEO_SOURCE: ArcSwapOption<NativeVideoSource> = ArcSwapOption::const_empty();

// The bundled libwebrtc statically links hidden-weak `pw_*` dlopen shims
// (`pipewire_stubs.o`, `modules::portal::*`). Our `DesktopCapturer` usage
// (the Linux PipeWire capturer in `linux_capture`) and the peer connection
// factory (which keeps libwebrtc's `PipeWire` *video capture module*,
// `video_capture_pipewire.o`, in the link) both drag the shims in. The
// shims tail-jump through static pointers that stay NULL until
// `InitializePipewire` dlopens `libpipewire` and arms them — any earlier
// `pw_init` call SIGSEGVs, so the app must arm them at startup.
#[cfg(target_os = "linux")]
unsafe extern "C" {
    #[link_name = "_ZN14modules_portal18InitializePipewireEPv"]
    fn webrtc_initialize_pipewire(module: *mut std::ffi::c_void);
}

/// Arms libwebrtc's bundled `PipeWire` dlopen shims so `pipewire-rs` calls
/// reach the real libpipewire. Must run before any native-rust `PipeWire`
/// usage (see the Tauri setup wiring).
#[cfg(target_os = "linux")]
pub fn arm_pipewire_shims() {
    // SAFETY: `InitializePipewire` only dlopens libpipewire and stores
    // dlsym results into static pointers; the module argument is forwarded
    // to the stub machinery and never dereferenced.
    unsafe { webrtc_initialize_pipewire(std::ptr::null_mut()) };
}

#[cfg(not(target_os = "linux"))]
pub fn arm_pipewire_shims() {}

/// Scans the packaged dynamic `GStreamer` plugins before the publisher starts.
///
/// # Errors
///
/// Returns an error when the directory cannot be scanned or a required
/// publication element is unavailable.
#[cfg(target_os = "linux")]
pub fn load_gstreamer_plugins(plugin_dir: &std::path::Path) -> Result<(), String> {
    gstreamer_publisher::load_plugins(plugin_dir)
}

#[cfg(not(target_os = "linux"))]
pub fn load_gstreamer_plugins(_plugin_dir: &std::path::Path) -> Result<(), String> {
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn with_guard<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&NativeLiveKit) -> Result<R, String>,
{
    let guard = LIVEKIT.lock().map_err(|e| format!("Lock poisoned: {e}"))?;
    let Some(state) = guard.as_ref() else {
        return Err("Room not connected".into());
    };
    f(state)
}

/// Connects to a `LiveKit` room and publishes a screenshare audio track. The
/// worker thread runs its own tokio runtime; video tracks are published
/// through `start_video_track` once the room is live.
///
/// # Errors
///
/// Returns an error if a room is already connected or the worker thread cannot
/// be spawned.
pub fn connect_livekit_room(
    url: String,
    token: String,
    room_name: String,
    identity: String,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        gstreamer_publisher::connect(url, token, room_name, identity)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (room_name, identity);
        let mut guard = LIVEKIT.lock().map_err(|e| format!("Lock poisoned: {e}"))?;
        if guard.is_some() {
            return Err("Native LiveKit room is already connected".into());
        }

        ROOM_CONNECTED.store(false, Ordering::Relaxed);
        SPECTATOR_COUNT.store(0, Ordering::Relaxed);
        VIDEO_ACTIVE.store(false, Ordering::Relaxed);
        VIDEO_SOURCE.store(None);

        let (pcm_tx, pcm_rx) = tokio::sync::mpsc::channel::<Vec<i16>>(PCM_CHANNEL_CAPACITY);
        // Publish the sender under its own tiny lock before the long `LIVEKIT`
        // hold below, so the audio path can reach it without ever contending
        // with `connect_livekit_room`.
        {
            let Ok(mut sender_guard) = PCM_SENDER.lock() else {
                return Err("PCM sender lock poisoned".into());
            };
            *sender_guard = Some(pcm_tx);
        }
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<WorkerCmd>();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();

        let handle = std::thread::Builder::new()
            .name("lk-worker".into())
            .spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    eprintln!("[livekit] Failed to build tokio runtime");
                    return;
                };

                rt.block_on(async {
                    if let Err(e) = run_worker(url, token, pcm_rx, cmd_rx, stop_rx).await {
                        log::error!("[livekit] worker error: {e}");
                    }
                });
            })
            .map_err(|e| format!("Failed to spawn: {e}"))?;

        *guard = Some(NativeLiveKit {
            cmd_tx,
            stop: stop_tx,
            _join: handle,
        });
        Ok(())
    }
}

/// Disconnects the room and tears down the worker thread.
///
/// # Errors
///
/// Returns an error if the room state lock is poisoned.
pub fn disconnect_livekit_room() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        gstreamer_publisher::disconnect();
        desktop_capture::stop();
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let mut guard = LIVEKIT.lock().map_err(|e| format!("Lock poisoned: {e}"))?;
        if let Some(state) = guard.take() {
            let _ = state.cmd_tx.send(WorkerCmd::StopVideo);
            let _ = state.cmd_tx.send(WorkerCmd::Shutdown);
            let _ = state.stop.send(());
        }
        *guard = None;
        // Clear the dedicated PCM sender so a stale sender can never outlive the
        // room (a late `feed_pcm` would otherwise try_send into a closed channel
        // and report a spurious error). Never blocks the audio path: this lock is
        // independent of `LIVEKIT`.
        if let Ok(mut sender_guard) = PCM_SENDER.lock() {
            *sender_guard = None;
        }
        // Release the room lock before the capture teardown below: `feed_pcm`
        // (the audio-callback path) and every room command block on `LIVEKIT`,
        // and `desktop_capture::stop()` joins the capture thread (~100-300 ms).
        // Holding the lock across that join stalled the audio callback for the
        // whole teardown on every room recreate.
        drop(guard);
        desktop_capture::stop();
        ROOM_CONNECTED.store(false, Ordering::SeqCst);
        SPECTATOR_COUNT.store(0, Ordering::Relaxed);
        VIDEO_ACTIVE.store(false, Ordering::SeqCst);
        VIDEO_SOURCE.store(None);
        Ok(())
    }
}

#[must_use]
pub fn is_livekit_room_connected() -> bool {
    #[cfg(target_os = "linux")]
    {
        gstreamer_publisher::is_connected()
    }

    #[cfg(not(target_os = "linux"))]
    ROOM_CONNECTED.load(Ordering::Relaxed)
}

/// Resolves a codec string to a `VideoCodec`, defaulting to VP9 for anything
/// unrecognized (including `None`).
#[cfg(not(target_os = "linux"))]
#[cfg(not(target_os = "linux"))]
fn parse_video_codec(codec: Option<&str>) -> VideoCodec {
    match codec {
        Some("vp8") => VideoCodec::VP8,
        Some("h264") => VideoCodec::H264,
        Some("av1") => VideoCodec::AV1,
        _ => VideoCodec::VP9,
    }
}

/// Drains full `samples_per_chunk`-sized chunks from the worker's buffer,
/// leaving a partial tail queued for the next push. Off-by-one safe: a buffer
/// with exactly `n * samples_per_chunk` samples produces exactly `n` chunks.
#[cfg(not(target_os = "linux"))]
#[cfg(not(target_os = "linux"))]
fn drain_pcm_chunks(buffer: &mut VecDeque<i16>, samples_per_chunk: usize, out: &mut Vec<Vec<i16>>) {
    while buffer.len() >= samples_per_chunk {
        out.push(buffer.drain(..samples_per_chunk).collect());
    }
}

/// Feeds one PCM chunk (48 kHz stereo `i16` samples) into the room's audio
/// track. When the channel is full, WebRTC encoding is stalled and the newest
/// chunk is dropped rather than queued.
///
/// # Errors
///
/// Returns an error if no room is connected or the worker's PCM channel is
/// closed.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the non-Linux publisher transfers PCM ownership into its bounded worker channel"
)]
pub fn feed_pcm(pcm: Vec<i16>) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        gstreamer_publisher::feed_pcm(&pcm);
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Read the sender out under the dedicated `PCM_SENDER` lock (never
        // `LIVEKIT`): `connect_livekit_room` holds `LIVEKIT` for the whole
        // worker-start + Room::connect + publish_track sequence, and this runs
        // on the audio-ring worker. Blocking on `LIVEKIT` there stalls the ring
        // and, via its join, the whole app — the original deadlock.
        let sender = {
            let guard = PCM_SENDER
                .lock()
                .map_err(|e| format!("PCM sender lock poisoned: {e}"))?;
            let Some(sender) = guard.as_ref() else {
                return Err("Room not connected".into());
            };
            sender.clone()
        };
        // Channel full: WebRTC encoding is stalled, drop the newest chunk.
        match sender.try_send(pcm) {
            Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err("PCM channel closed".into())
            }
        }
    }
}

/// Publishes (or re-publishes) the screenshare video track with the given
/// encoder settings, restarting the track without restarting the capture.
///
/// # Errors
///
/// Returns an error if no room is connected or the worker channel is closed.
pub fn start_video_track(config: CaptureConfig) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let target = (config.width, config.height, config.fps);
        let result = gstreamer_publisher::start_video(config);
        if result.is_ok() {
            desktop_capture::set_scale_target(target.0, target.1, target.2);
        }
        result
    }

    #[cfg(not(target_os = "linux"))]
    with_guard(|state| {
        state
            .cmd_tx
            .send(WorkerCmd::StartVideo { config })
            .map_err(|_| "Worker channel closed".into())
    })
}

/// Unpublishes the screenshare video track.
///
/// # Errors
///
/// Returns an error if the room state lock is poisoned.
pub fn stop_video_track() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        desktop_capture::clear_scale_target();
        gstreamer_publisher::stop_video()
    }

    #[cfg(not(target_os = "linux"))]
    {
        let guard = LIVEKIT.lock().map_err(|e| format!("Lock poisoned: {e}"))?;
        let Some(state) = guard.as_ref() else {
            return Ok(());
        };
        let _ = state.cmd_tx.send(WorkerCmd::StopVideo);
        Ok(())
    }
}

#[must_use]
pub fn is_video_track_active() -> bool {
    #[cfg(target_os = "linux")]
    {
        gstreamer_publisher::is_video_active()
    }

    #[cfg(not(target_os = "linux"))]
    VIDEO_ACTIVE.load(Ordering::Relaxed)
}

/// Starts the desktop capturer on its own thread. Returns once the capturer is
/// running; on Wayland the native portal picker then appears and frames flow
/// after the user selects a source.
///
/// # Errors
///
/// Returns an error if a capture session is already active, the thread cannot
/// be spawned, or the capturer fails to initialize within five seconds.
pub fn start_desktop_capture() -> Result<bool, String> {
    desktop_capture::start()
}

/// Starts the WGC desktop capturer (Windows-only) for the source the
/// renderer's picker selected.
///
/// # Errors
///
/// Returns an error if a capture session is already active or the capturer
/// fails to initialize within five seconds.
#[cfg(target_os = "windows")]
pub fn start_windows_capture(kind: WgcSourceKind, id: u64) -> Result<bool, String> {
    desktop_capture::start_windows(kind, id)
}

/// Enumerates the screens and windows capturable through WGC (Windows-only),
/// for the renderer's in-app source picker. The chosen `(kind, id)` is fed
/// back into [`start_windows_capture`].
///
/// # Errors
///
/// Returns an error when COM cannot be initialized or no capturer can be
/// created.
#[cfg(target_os = "windows")]
pub fn get_windows_capture_sources() -> Result<Vec<CaptureSourceInfo>, String> {
    wgc_capture::get_windows_capture_sources()
}

/// Stops the active desktop capture session.
#[must_use]
pub fn stop_desktop_capture() -> bool {
    desktop_capture::stop()
}

/// Starts synthetic test-pattern capture (headless e2e / probes): generated
/// BGRA frames feed the exact same conversion and publish path as the portal
/// engine — no picker, no Wayland requirement.
///
/// # Errors
///
/// Returns an error if a capture session is already active or the generator
/// thread cannot be spawned.
pub fn start_synthetic_capture(config: &CaptureConfig) -> Result<bool, String> {
    desktop_capture::start_synthetic_capture(config)
}

/// Returns `true` while the capturer is running (the portal picker may still
/// be awaiting a selection).
#[must_use]
pub fn is_desktop_capture_active() -> bool {
    desktop_capture::is_active()
}

#[must_use]
pub fn get_desktop_capture_stats() -> DesktopCaptureStats {
    desktop_capture::stats()
}

/// Registers the preview callback: the capture thread invokes it with
/// BGRA frames `(data, pts_us)` while a capture session is active. The
/// engine stays Tauri-unaware — the Tauri backend forwards the bytes to the
/// renderer's preview channel. Replaces any previously registered callback.
pub fn set_preview_callback(callback: Box<dyn Fn(Vec<u8>, i64) + Send + Sync>) {
    desktop_capture::set_preview_callback(callback);
}

/// Clears the registered preview callback.
pub fn clear_preview_callback() {
    desktop_capture::clear_preview_callback();
}

/// Registers the capture-ended callback: invoked once per capture session
/// when the portal closes it unexpectedly — the compositor ended the
/// stream (e.g. the presenter closed the captured window/app). The Tauri
/// backend forwards this to the renderer as the `capture-ended` event.
/// Replaces any previously registered callback.
pub fn set_capture_ended_callback(callback: Box<dyn Fn() + Send + Sync>) {
    desktop_capture::set_capture_ended_callback(callback);
}

/// Reports the renderer's preview viewport size in device pixels; the
/// preview emitter scales every frame to fit inside it (OBS-style "scale to
/// the window"), so the IPC channel only carries what the card can show.
pub fn set_preview_viewport(width: u32, height: u32) {
    desktop_capture::set_preview_viewport(width, height);
}

/// Clears the reported preview viewport; the emitter skips frames until the
/// renderer reports a size again (no mounted preview card = nothing to
/// display — full-source-resolution fallback emission was an OOM vector
/// into the channel queue).
pub fn clear_preview_viewport() {
    desktop_capture::clear_preview_viewport();
}

#[must_use]
pub fn get_spectator_count() -> u32 {
    #[cfg(target_os = "linux")]
    {
        0
    }

    #[cfg(not(target_os = "linux"))]
    SPECTATOR_COUNT.load(Ordering::Relaxed)
}

/// Collects the latest native publisher stats for the local tracks and falls
/// back to an empty snapshot if the publisher cannot answer within 500 ms.
#[must_use]
pub fn get_native_telemetry() -> NativeTelemetry {
    #[cfg(target_os = "linux")]
    {
        gstreamer_publisher::telemetry().unwrap_or_default()
    }

    #[cfg(not(target_os = "linux"))]
    {
        let Ok(guard) = LIVEKIT.lock() else {
            return NativeTelemetry::default();
        };
        let Some(state) = guard.as_ref() else {
            return NativeTelemetry::default();
        };
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if state
            .cmd_tx
            .send(WorkerCmd::GetTelemetry { reply: reply_tx })
            .is_err()
        {
            return NativeTelemetry::default();
        }
        drop(guard);
        reply_rx
            .recv_timeout(Duration::from_millis(500))
            .unwrap_or_default()
    }
}

#[cfg(not(target_os = "linux"))]
async fn collect_telemetry(room: &Room) -> NativeTelemetry {
    let mut telemetry = NativeTelemetry::default();
    for (_sid, publication) in room.local_participant().track_publications() {
        let Some(track) = publication.track() else {
            continue;
        };
        let Ok(stats) = track.get_stats().await else {
            continue;
        };
        fold_stats(&mut telemetry, &stats);
    }
    telemetry
}

// Byte counters and epoch-millisecond timestamps stay well below 2^53, so the
// f64 conversion loses no precision in practice (JSON cannot carry u64).
#[allow(
    clippy::cast_precision_loss,
    reason = "Byte counters and epoch-millisecond timestamps stay well below 2^53; f64 is exact in this range"
)]
#[cfg(not(target_os = "linux"))]
fn fold_stats(telemetry: &mut NativeTelemetry, stats: &[RtcStats]) {
    let mut video_ssrc = None;
    let mut audio_ssrc = None;
    let mut codecs = HashMap::<String, String>::new();

    for stat in stats {
        match stat {
            RtcStats::Codec(codec) => {
                codecs.insert(codec.rtc.id.clone(), codec.codec.mime_type.clone());
            }
            RtcStats::OutboundRtp(outbound) => {
                let mime = codecs.get(&outbound.stream.codec_id);
                match outbound.stream.kind.as_str() {
                    "video" => {
                        video_ssrc = Some(outbound.stream.ssrc);
                        telemetry.video_codec = mime.cloned();
                        if !outbound.outbound.encoder_implementation.is_empty() {
                            telemetry.encoder_implementation =
                                Some(outbound.outbound.encoder_implementation.clone());
                        }
                        telemetry.video_bytes_sent = Some(outbound.sent.bytes_sent as f64);
                        telemetry.video_packets_sent = Some(outbound.sent.packets_sent as f64);
                        telemetry.video_frames_encoded =
                            Some(f64::from(outbound.outbound.frames_encoded));
                        telemetry.video_width = Some(outbound.outbound.frame_width);
                        telemetry.video_height = Some(outbound.outbound.frame_height);
                        // libwebrtc timestamps are µs since epoch; the renderer computes
                        // deltas from this ms field, and a µs delta read as ms made every
                        // rate 1000x too small (28 Mbps read as 28 kbps).
                        telemetry.timestamp_ms = Some(outbound.rtc.timestamp as f64 / 1000.0);
                    }
                    "audio" => {
                        audio_ssrc = Some(outbound.stream.ssrc);
                        telemetry.audio_codec = mime.cloned();
                        telemetry.audio_bytes_sent = Some(outbound.sent.bytes_sent as f64);
                        telemetry.audio_packets_sent = Some(outbound.sent.packets_sent as f64);
                    }
                    _ => {}
                }
            }
            RtcStats::RemoteInboundRtp(inbound) => {
                if inbound.remote_inbound.round_trip_time > 0.0 {
                    telemetry.rtt_ms = Some(inbound.remote_inbound.round_trip_time * 1000.0);
                }
                if let Ok(packets_lost) = u64::try_from(inbound.received.packets_lost.max(0)) {
                    if Some(inbound.stream.ssrc) == video_ssrc {
                        telemetry.video_packets_lost = Some(packets_lost as f64);
                    } else if Some(inbound.stream.ssrc) == audio_ssrc {
                        telemetry.audio_packets_lost = Some(packets_lost as f64);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(not(target_os = "linux"))]
async fn run_worker(
    url: String,
    token: String,
    mut pcm_rx: tokio::sync::mpsc::Receiver<Vec<i16>>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerCmd>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let audio = NativeAudioSource::new(AudioSourceOptions::default(), SAMPLE_RATE, CHANNELS, 0);

    let (room, mut room_events) = Room::connect(&url, &token, RoomOptions::default()).await?;

    let audio_track = livekit::track::LocalAudioTrack::create_audio_track(
        "screen_audio",
        RtcAudioSource::Native(audio.clone()),
    );

    room.local_participant()
        .publish_track(
            livekit::track::LocalTrack::Audio(audio_track),
            TrackPublishOptions {
                source: TrackSource::ScreenshareAudio,
                ..Default::default()
            },
        )
        .await?;

    log::info!("[livekit] audio track published");
    ROOM_CONNECTED.store(true, Ordering::SeqCst);

    let samples_per_10ms = (SAMPLE_RATE / 100 * CHANNELS) as usize;

    // PCM delivery runs as its own task on this current-thread runtime: the
    // main loop must keep polling `pcm_rx` while a command handler awaits
    // (publish/unpublish SDP+ICE negotiation, `get_stats`), or the 128-chunk
    // channel fills in ~1.28 s and `feed_pcm` drops-newest — audible audio
    // gaps during every go-live and encoder-settings change.
    let audio_pump = {
        let audio = audio.clone();
        tokio::spawn(async move {
            let mut buffer = VecDeque::new();
            let max_backlog_samples = MAX_AUDIO_BACKLOG_MS * samples_per_10ms;
            while let Some(pcm_chunk) = pcm_rx.recv().await {
                buffer.extend(pcm_chunk);
                // Drop-oldest backlog bound: a stalled upstream (ring,
                // channel or worker) must never push audio content seconds
                // behind the live video. Skipping the stale tail after a
                // hiccup keeps audio near-live instead of lagging forever
                // (the C++ audio source plays its buffer at real-time rate,
                // so an unbound backlog would never drain faster than it
                // grows).
                while buffer.len() > max_backlog_samples {
                    buffer.pop_front();
                }
                let mut chunks = Vec::new();
                drain_pcm_chunks(&mut buffer, samples_per_10ms, &mut chunks);
                for chunk in chunks {
                    let frame = AudioFrame {
                        data: chunk.into(),
                        sample_rate: SAMPLE_RATE,
                        num_channels: CHANNELS,
                        samples_per_channel: SAMPLE_RATE / 100,
                    };
                    if let Err(e) = audio.capture_frame(&frame).await {
                        log::error!("[livekit] audio capture_frame error: {e}");
                    }
                }
            }
        })
    };

    loop {
        tokio::select! {
            biased;
            _ = &mut stop_rx => {
                break;
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    break;
                };
                match cmd {
                    WorkerCmd::StartVideo { config } => {
                        handle_start_video(&room, &config).await;
                    }
                    WorkerCmd::StopVideo => {
                        handle_stop_video(&room).await;
                    }
                    WorkerCmd::GetTelemetry { reply } => {
                        let telemetry = collect_telemetry(&room).await;
                        let _ = reply.send(telemetry);
                    }
                    WorkerCmd::Shutdown => {
                        break;
                    }
                }
            }
            event = room_events.recv() => {
                if let Some(
                    RoomEvent::ParticipantConnected(_)
                    | RoomEvent::ParticipantDisconnected(_)
                    | RoomEvent::ParticipantsUpdated { .. },
                ) = event
                {
                    SPECTATOR_COUNT.store(
                        u32::try_from(room.remote_participants().len()).unwrap_or(u32::MAX),
                        Ordering::Relaxed,
                    );
                }
            }
        }
    }

    // Drop the pump; the channel senders are gone, so nothing further is delivered.
    drop(audio_pump);

    handle_stop_video(&room).await;

    ROOM_CONNECTED.store(false, Ordering::SeqCst);
    SPECTATOR_COUNT.store(0, Ordering::Relaxed);
    room.close().await?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
async fn handle_start_video(room: &Room, config: &CaptureConfig) {
    handle_stop_video(room).await;

    let resolution = VideoResolution {
        width: config.width,
        height: config.height,
    };
    let source = NativeVideoSource::new(resolution, true);

    let track =
        LocalVideoTrack::create_video_track("screen_share", RtcVideoSource::Native(source.clone()));

    let codec = parse_video_codec(config.video_codec.as_deref());

    let encoding = config.max_bitrate.map(|bitrate| VideoEncoding {
        // The renderer sends the bitrate limit from its settings UI as a whole
        // number; rounding the f64 to u64 is exact for all sane values.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "Bitrate limits from the settings UI are whole numbers; rounding is exact"
        )]
        max_bitrate: bitrate.round() as u64,
        max_framerate: f64::from(config.fps),
    });

    let publish_start = Instant::now();
    match room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                video_codec: codec,
                video_encoding: encoding,
                // livekit-rs 0.8 defaults to simulcast; its screenshare LOW/MID
                // presets cap the lower layers at 3 FPS and a few hundred kbps
                // (half resolution), which is what spectators then receive.
                // One layer at the user's configured fps/bitrate instead.
                simulcast: false,
                ..Default::default()
            },
        )
        .await
    {
        Ok(_) => log::info!(
            "[livekit] video track published (codec={config:?}) in {:.0} ms",
            publish_start.elapsed().as_secs_f64() * 1000.0
        ),
        Err(e) => {
            log::error!(
                "[livekit] video track publish failed (codec={:?}, width={}, height={}, fps={}, bitrate={:?}): {e}",
                config.video_codec,
                config.width,
                config.height,
                config.fps,
                config.max_bitrate,
            );
            return;
        }
    }

    desktop_capture::set_scale_target(config.width, config.height, config.fps);
    VIDEO_SOURCE.store(Some(Arc::new(source)));
    VIDEO_ACTIVE.store(true, Ordering::SeqCst);
}

#[cfg(not(target_os = "linux"))]
async fn handle_stop_video(room: &Room) {
    desktop_capture::clear_scale_target();
    VIDEO_SOURCE.store(None);
    VIDEO_ACTIVE.store(false, Ordering::SeqCst);

    let publications: Vec<_> = room
        .local_participant()
        .track_publications()
        .into_iter()
        .filter(|(_sid, p)| p.kind() == livekit::track::TrackKind::Video)
        .collect();
    for (_sid, pub_info) in publications {
        let _ = room
            .local_participant()
            .unpublish_track(&pub_info.sid())
            .await;
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;
    use livekit::webrtc::stats::dictionaries as d;
    use livekit::webrtc::stats::{CodecStats, OutboundRtpStats, RemoteInboundRtpStats};

    fn codec_stats(id: &str, mime: &str) -> RtcStats {
        RtcStats::Codec(CodecStats {
            rtc: d::RtcStats {
                id: id.into(),
                timestamp: 0,
            },
            codec: d::CodecStats {
                mime_type: mime.into(),
                ..Default::default()
            },
        })
    }

    fn outbound_stats(
        kind: &str,
        ssrc: u32,
        codec_id: &str,
        bytes: u64,
        frames: u32,
        encoder_impl: &str,
    ) -> RtcStats {
        RtcStats::OutboundRtp(OutboundRtpStats {
            rtc: d::RtcStats {
                id: format!("out-{kind}"),
                timestamp: 1_750_000_000_000,
            },
            stream: d::RtpStreamStats {
                ssrc,
                kind: kind.into(),
                codec_id: codec_id.into(),
                ..Default::default()
            },
            sent: d::SentRtpStreamStats {
                packets_sent: 100,
                bytes_sent: bytes,
            },
            outbound: d::OutboundRtpStreamStats {
                frames_encoded: frames,
                frame_width: 1920,
                frame_height: 1080,
                encoder_implementation: encoder_impl.into(),
                ..Default::default()
            },
        })
    }

    fn remote_inbound_stats(ssrc: u32, packets_lost: i64, rtt_s: f64) -> RtcStats {
        RtcStats::RemoteInboundRtp(RemoteInboundRtpStats {
            rtc: d::RtcStats {
                id: format!("inb-{ssrc}"),
                timestamp: 0,
            },
            stream: d::RtpStreamStats {
                ssrc,
                ..Default::default()
            },
            received: d::ReceivedRtpStreamStats {
                packets_lost,
                ..Default::default()
            },
            remote_inbound: d::RemoteInboundRtpStreamStats {
                round_trip_time: rtt_s,
                ..Default::default()
            },
        })
    }

    #[test]
    fn fold_stats_extracts_outbound_video_and_audio() {
        let stats = [
            codec_stats("codec-v", "video/VP8"),
            codec_stats("codec-a", "audio/OPUS"),
            outbound_stats("video", 11, "codec-v", 2_000_000, 1200, "libvpx"),
            outbound_stats("audio", 22, "codec-a", 400_000, 0, ""),
        ];
        let mut t = NativeTelemetry::default();
        fold_stats(&mut t, &stats);
        assert_eq!(t.video_codec.as_deref(), Some("video/VP8"));
        assert_eq!(t.encoder_implementation.as_deref(), Some("libvpx"));
        assert_eq!(t.video_bytes_sent, Some(2_000_000.0));
        assert_eq!(t.video_packets_sent, Some(100.0));
        assert_eq!(t.video_frames_encoded, Some(1200.0));
        assert_eq!(t.video_width, Some(1920));
        assert_eq!(t.video_height, Some(1080));
        assert_eq!(t.timestamp_ms, Some(1_750_000_000.0));
        assert_eq!(t.audio_codec.as_deref(), Some("audio/OPUS"));
        assert_eq!(t.audio_bytes_sent, Some(400_000.0));
        assert_eq!(t.audio_packets_sent, Some(100.0));
    }

    #[test]
    fn fold_stats_reports_the_actual_hardware_encoder_implementation() {
        // "VAAPI H264 Encoder" proves the VA-API hardware path was taken
        // instead of OpenH264; empty strings stay None (stack never reported).
        let stats = [outbound_stats(
            "video",
            11,
            "codec-v",
            2_000_000,
            1200,
            "VAAPI H264 Encoder",
        )];
        let mut t = NativeTelemetry::default();
        fold_stats(&mut t, &stats);
        assert_eq!(
            t.encoder_implementation.as_deref(),
            Some("VAAPI H264 Encoder")
        );

        let stats = [outbound_stats("video", 11, "codec-v", 0, 0, "")];
        let mut t = NativeTelemetry::default();
        fold_stats(&mut t, &stats);
        assert_eq!(t.encoder_implementation, None);
    }

    #[test]
    fn native_codec_list_marks_only_h264_as_hardware() {
        let codecs = get_native_supported_codecs();
        assert_eq!(codecs.len(), 4);
        let h264 = codecs
            .iter()
            .find(|c| c.codec == "h264")
            .unwrap_or_else(|| {
                panic!("h264 must be in the native codec list");
            });
        assert!(h264.hardware);
        for codec in codecs.iter().filter(|c| c.codec != "h264") {
            assert!(!codec.hardware, "{} must be software", codec.codec);
        }
        // The picker contract: every codec has a non-empty display label.
        for codec in &codecs {
            assert!(!codec.label.is_empty());
            assert!(!codec.codec.is_empty());
        }
    }

    #[test]
    fn fold_stats_attributes_packets_lost_by_ssrc_and_reports_rtt() {
        let stats = [
            outbound_stats("video", 11, "codec-v", 0, 0, ""),
            outbound_stats("audio", 22, "codec-a", 0, 0, ""),
            remote_inbound_stats(11, -3, 0.05),
            remote_inbound_stats(22, 7, 0.0),
        ];
        let mut t = NativeTelemetry::default();
        fold_stats(&mut t, &stats);
        // Negative lost values clamp to 0; RTT of 0.0 (unmeasured) is ignored.
        assert_eq!(t.video_packets_lost, Some(0.0));
        assert_eq!(t.audio_packets_lost, Some(7.0));
        assert_eq!(t.rtt_ms, Some(50.0));
    }

    #[test]
    fn fold_stats_ignores_unrelated_stat_types() {
        let mut t = NativeTelemetry::default();
        fold_stats(&mut t, &[]);
        assert!(t.video_bytes_sent.is_none());
        assert!(t.audio_codec.is_none());
        assert!(t.rtt_ms.is_none());
    }

    #[test]
    #[allow(
        clippy::cast_possible_wrap,
        reason = "test fixture: 0xABCD_EF01 deliberately reinterpreted as a negative i32"
    )]
    fn parse_video_codec_maps_known_strings() {
        assert_eq!(parse_video_codec(Some("vp8")), VideoCodec::VP8);
        assert_eq!(parse_video_codec(Some("vp9")), VideoCodec::VP9);
        assert_eq!(parse_video_codec(Some("h264")), VideoCodec::H264);
        assert_eq!(parse_video_codec(Some("av1")), VideoCodec::AV1);
    }

    #[test]
    fn parse_video_codec_falls_back_to_vp9() {
        assert_eq!(parse_video_codec(Some("theora")), VideoCodec::VP9);
        assert_eq!(parse_video_codec(Some("VP9")), VideoCodec::VP9);
        assert_eq!(parse_video_codec(Some("")), VideoCodec::VP9);
        assert_eq!(parse_video_codec(None), VideoCodec::VP9);
    }

    #[test]
    fn drain_chunks_emits_nothing_below_chunk_size() {
        let mut buffer = VecDeque::from(vec![1i16, 2, 3]);
        let mut chunks = Vec::new();
        drain_pcm_chunks(&mut buffer, 960, &mut chunks);
        assert!(chunks.is_empty());
        assert_eq!(buffer.len(), 3);
    }

    #[test]
    fn drain_chunks_splits_exact_multiples_without_remainder() {
        let mut buffer: VecDeque<i16> = (0..1920).collect();
        let mut chunks = Vec::new();
        drain_pcm_chunks(&mut buffer, 960, &mut chunks);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], (0..960).collect::<Vec<i16>>());
        assert_eq!(chunks[1], (960..1920).collect::<Vec<i16>>());
        assert!(buffer.is_empty());
    }

    #[test]
    fn drain_chunks_carries_partial_tail_between_pushes() {
        let mut buffer: VecDeque<i16> = (0..1000).collect();
        let mut chunks = Vec::new();
        drain_pcm_chunks(&mut buffer, 960, &mut chunks);
        assert_eq!(chunks.len(), 1);
        assert_eq!(buffer.len(), 40);
        // The next push fills the tail to a full chunk.
        buffer.extend(1000..1920);
        drain_pcm_chunks(&mut buffer, 960, &mut chunks);
        assert_eq!(chunks.len(), 2);
        assert!(buffer.is_empty());
    }

    #[test]
    fn with_guard_errors_when_no_room_is_connected() {
        // The singleton is empty in a fresh test binary; the guard must fail
        // with the "Room not connected" reason, not poison or hang.
        let Err(err) = with_guard(|_| Ok(())) else {
            panic!("expected an error without a connected room");
        };
        assert!(err.contains("Room not connected"));
    }
}
