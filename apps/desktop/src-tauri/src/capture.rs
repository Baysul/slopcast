//! Capture commands — driving `native-livekit`'s desktop capturer and
//! video track.

use std::sync::Mutex;
use std::time::Duration;

use native_livekit::{CaptureConfig, DesktopCaptureStats};
use tauri::Emitter;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::AppHandle;
use crate::platform::video_capture_available;

/// When set, the capture commands route to the synthetic test-pattern source
/// instead of the portal: the headless e2e drives the real UI flow (preview →
/// go live) without a portal picker or a Wayland session. Production runs
/// never set this.
fn e2e_capture_mode() -> bool {
    std::env::var("SLOPCAST_E2E_CAPTURE").as_deref() == Ok("synthetic")
}

/// A capturable screen or window in the Windows WGC source picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureSourceKind {
    Screen,
    Window,
}

/// One capturable source as reported by `get_capture_sources`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSourceInfo {
    pub id: u64,
    pub title: String,
    pub display_id: i64,
    pub kind: CaptureSourceKind,
}

/// The renderer's picker selection, passed into the capture commands on
/// Windows (ignored on Linux, where the portal picker decides).
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSourceSelection {
    pub kind: CaptureSourceKind,
    pub id: u64,
}

#[cfg(target_os = "windows")]
fn map_kind(kind: CaptureSourceKind) -> native_livekit::WgcSourceKind {
    match kind {
        CaptureSourceKind::Screen => native_livekit::WgcSourceKind::Screen,
        CaptureSourceKind::Window => native_livekit::WgcSourceKind::Window,
    }
}

#[cfg(target_os = "windows")]
fn map_source_info(info: native_livekit::CaptureSourceInfo) -> CaptureSourceInfo {
    CaptureSourceInfo {
        id: info.id,
        title: info.title,
        display_id: info.display_id,
        kind: match info.kind {
            native_livekit::WgcSourceKind::Screen => CaptureSourceKind::Screen,
            native_livekit::WgcSourceKind::Window => CaptureSourceKind::Window,
        },
    }
}

/// Starts a capture through the active route: the synthetic source in e2e
/// mode, the WGC source on Windows (the picker's selection), the portal
/// source otherwise. Returns the `CaptureStartResult`.
fn start_capture(
    config: &CaptureConfig,
    source: Option<CaptureSourceSelection>,
) -> CaptureStartResult {
    let mut result = CaptureStartResult {
        ok: true,
        ..CaptureStartResult::default()
    };
    let start_result = if e2e_capture_mode() {
        native_livekit::start_synthetic_capture(config)
    } else {
        start_real_capture(config, source)
    };
    match start_result {
        Ok(video_enabled) => result.video_enabled = video_enabled,
        Err(e) => return CaptureStartResult::failed(e),
    }
    result
}

/// The real (non-e2e) capture route. On Windows the renderer's picker
/// selection is required — there is no system picker; elsewhere the
/// platform's default source runs (the portal picker on Wayland, a
/// video-less no-op elsewhere).
#[cfg(target_os = "windows")]
fn start_real_capture(
    _config: &CaptureConfig,
    source: Option<CaptureSourceSelection>,
) -> Result<bool, String> {
    let Some(selection) = source else {
        return Err("A capture source is required on Windows".into());
    };
    native_livekit::start_windows_capture(map_kind(selection.kind), selection.id)
}

#[cfg(not(target_os = "windows"))]
fn start_real_capture(
    _config: &CaptureConfig,
    _source: Option<CaptureSourceSelection>,
) -> Result<bool, String> {
    native_livekit::start_desktop_capture()
}

/// The most recent preview payload, kept for the `frame` custom-protocol
/// handler. One slot, replaced per emission — bounded by construction.
///
/// Why not `tauri::ipc::Channel` or a per-invoke `Response`? Both deliver
/// raw bodies (>1 KB) through the same slow `__TAURI_CHANNEL__|fetch`
/// machinery (~4 s per 2 MB response). A custom URI scheme
/// serves bytes directly from the protocol handler — no IPC, no ordering,
/// no queue — the renderer fetches at its own pace.
pub static LATEST_FRAME: Mutex<Option<Vec<u8>>> = Mutex::new(None);

/// Registers the preview callback: stashes each raw BGRA payload in `LATEST_FRAME`.
pub fn register_preview_frame_callback() {
    native_livekit::set_preview_callback(Box::new(move |bytes, _pts_us| {
        if let Ok(mut slot) = LATEST_FRAME.lock() {
            *slot = Some(bytes);
        }
    }));
}

/// The app handle the capture-ended callback emits the `capture-ended`
/// event through; registered once at startup, like the preview callback.
static CAPTURE_ENDED_EMITTER: Mutex<Option<AppHandle>> = Mutex::new(None);

/// Registers the capture-ended callback: the portal closes the `ScreenCast`
/// session when the compositor ends the stream (e.g. the presenter closed
/// the captured window), and the renderer tears the share down on the
/// `capture-ended` event. The callback runs on the capture thread; the emit
/// is non-blocking, so the join in `stop_desktop_capture` never waits on it.
pub fn register_capture_ended_callback(app: &AppHandle) {
    if let Ok(mut guard) = CAPTURE_ENDED_EMITTER.lock() {
        *guard = Some(app.clone());
    }
    native_livekit::set_capture_ended_callback(Box::new(|| {
        let Some(emitter) = CAPTURE_ENDED_EMITTER.lock().ok().and_then(|g| g.clone()) else {
            return;
        };
        let _ = emitter.emit("capture-ended", ());
    }));
}

/// Reports the renderer's preview card size (device pixels) so the preview
/// emitter scales frames to fit it — OBS-style "scale to the window" —
/// instead of shipping full-resolution frames through the channel.
#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn set_preview_viewport(width: u32, height: u32) -> bool {
    native_livekit::set_preview_viewport(width, height);
    true
}

/// Clears the reported preview viewport; previews fall back to the source
/// resolution until the renderer reports again.
#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn clear_preview_viewport() -> bool {
    native_livekit::clear_preview_viewport();
    true
}

/// Result of `start_native_capture` (X11/macOS degrade to audio-only).
#[derive(Debug, Default, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStartResult {
    pub ok: bool,
    pub node_id: Option<u32>,
    pub video_enabled: bool,
    pub error: Option<String>,
}

impl CaptureStartResult {
    fn failed(error: String) -> Self {
        Self {
            ok: false,
            error: Some(error),
            ..Self::default()
        }
    }
}

/// Starts the native capture: publishes the video track and starts the
/// desktop capturer. On Wayland the portal picker appears; on Windows the
/// renderer's picker selection (`source`) is required — there is no system
/// picker. On platforms with no capture route (X11, macOS) the share
/// degrades to audio-only (synthetic e2e mode bypasses the platform gate).
#[tauri::command(rename_all = "camelCase")]
pub async fn start_native_capture(
    config: CaptureConfig,
    source: Option<CaptureSourceSelection>,
) -> CaptureStartResult {
    if !video_capture_available() && !e2e_capture_mode() {
        return CaptureStartResult {
            ok: true,
            ..CaptureStartResult::default()
        };
    }
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(e) = native_livekit::start_video_track(config.clone()) {
            return CaptureStartResult::failed(e);
        }
        let result = start_capture(&config, source);
        if !result.ok {
            let _ = native_livekit::stop_video_track();
        }
        result
    })
    .await
    .unwrap_or_else(|e| CaptureStartResult::failed(format!("start capture task failed: {e}")))
}

/// Re-publishes the video track with new encoder settings without restarting
/// the capture. Returns `false` on platforms without a capture route or when
/// the publish fails.
///
/// # Errors
///
/// Returns an error if the update task fails to run.
#[tauri::command(rename_all = "camelCase")]
pub async fn update_native_video(config: CaptureConfig) -> Result<bool, String> {
    if !video_capture_available() && !e2e_capture_mode() {
        return Ok(false);
    }
    let updated =
        tauri::async_runtime::spawn_blocking(move || native_livekit::start_video_track(config))
            .await
            .map_err(|e| format!("update video task failed: {e}"))?
            .is_ok();
    Ok(updated)
}

/// Stops the video track, desktop capture and audio capture.
///
/// # Errors
///
/// Returns an error when any active capture component cannot be stopped.
#[tauri::command(rename_all = "camelCase")]
pub async fn stop_native_capture() -> Result<(), String> {
    let video_result = native_livekit::stop_video_track();
    let capture_stopped = native_livekit::stop_desktop_capture();
    let audio_result = native_rust::stop_audio_capture();
    video_result?;
    if !capture_stopped {
        return Err("Failed to stop desktop capture".into());
    }
    if !audio_result? {
        return Err("Failed to stop audio capture".into());
    }

    Ok(())
}

/// Stops and unpublishes only the video share, preserving room audio and the
/// `LiveKit` connection.
///
/// # Errors
///
/// Returns an error when the video publication or desktop capturer cannot be
/// stopped.
#[tauri::command(rename_all = "camelCase")]
pub async fn stop_video_capture() -> Result<(), String> {
    let track_result = native_livekit::stop_video_track();
    let capture_stopped = native_livekit::stop_desktop_capture();
    track_result?;
    if !capture_stopped {
        return Err("Failed to stop desktop capture".into());
    }

    Ok(())
}

/// Returns `true` while the published video track is active.
#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn is_native_capture_active() -> bool {
    native_livekit::is_video_track_active()
}

/// Returns the current desktop capture stage counters.
#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn get_video_capture_stats() -> DesktopCaptureStats {
    native_livekit::get_desktop_capture_stats()
}

/// Resolution preset → capture dimensions (mirrors `RESOLUTION_DIMENSIONS`
/// in `@slopcast/shared-types`).
#[must_use]
fn resolution_dims(preset: &str) -> (u32, u32) {
    match preset {
        "480p" => (854, 480),
        "1080p" => (1920, 1080),
        "1440p" => (2560, 1440),
        "2160p" => (3840, 2160),
        _ => (1280, 720),
    }
}

/// Starts the capture in pre-roll mode: frames flow to the preview, no track
/// is published (Linux portal picker appears; Windows starts the WGC
/// capturer for the renderer's source selection). In synthetic e2e mode the
/// test-pattern source runs instead of the real route.
///
/// # Errors
///
/// Returns an error on platforms without a capture route, when a Windows
/// source selection is missing, or when the capturer fails to start.
#[tauri::command(rename_all = "camelCase")]
pub async fn start_capture_preview(
    app: AppHandle,
    source: Option<CaptureSourceSelection>,
) -> Result<(), String> {
    if !video_capture_available() && !e2e_capture_mode() {
        return Err("Screen capture is not supported on this platform".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        if e2e_capture_mode() {
            // Drive the synthetic source from the persisted stream settings
            // (resolution + fps) so e2e passes exercise the configured
            // cadence — a hardcoded 720p@30 source silently caps every pass.
            let saved = crate::settings::get_stream_settings(app)
                .unwrap_or_else(|_| crate::settings::default_stream_settings());
            let (width, height) = resolution_dims(&saved.resolution);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "sanitized fps is bounded to [1, 240], so the f64 → u32 round-trip cannot truncate or lose the sign"
            )]
            let fps = saved.fps.round() as u32;
            let config = native_livekit::CaptureConfig {
                width,
                height,
                fps,
                video_codec: None,
                max_bitrate: None,
            };
            native_livekit::start_synthetic_capture(&config)?;
        } else {
            // The real route ignores the encoder config — capture runs at
            // the source's native resolution; the encoder target is applied
            // when `go_live` publishes the track.
            let config = native_livekit::CaptureConfig {
                width: 0,
                height: 0,
                fps: 30,
                video_codec: None,
                max_bitrate: None,
            };
            start_real_capture(&config, source)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("start capture preview task failed: {e}"))?
}

/// Starts the synthetic test-pattern capture (headless e2e, manual probes):
/// generated frames feed the exact same conversion and publish path as the
/// portal capture, so the full encode → SFU → spectator chain is testable
/// without a picker or a Wayland session.
#[tauri::command(rename_all = "camelCase")]
pub async fn start_synthetic_capture(config: CaptureConfig) -> CaptureStartResult {
    tauri::async_runtime::spawn_blocking(move || {
        let mut result = CaptureStartResult {
            ok: true,
            ..CaptureStartResult::default()
        };
        match native_livekit::start_synthetic_capture(&config) {
            Ok(video_enabled) => result.video_enabled = video_enabled,
            Err(e) => return CaptureStartResult::failed(e),
        }
        result
    })
    .await
    .unwrap_or_else(|e| CaptureStartResult::failed(format!("synthetic capture task failed: {e}")))
}

/// Publishes the previewed capture: when a pre-roll capture
/// is already active the track is published against it (frames keep flowing
/// to both the preview and the track); otherwise the combined start runs
/// (publish + capture start, using the renderer's Windows source selection
/// when one is supplied).
///
/// # Errors
///
/// Returns an error on platforms without a capture route or when the track
/// publish or the capturer start fails.
#[tauri::command(rename_all = "camelCase")]
pub async fn go_live(
    config: CaptureConfig,
    source: Option<CaptureSourceSelection>,
) -> Result<(), String> {
    if !video_capture_available() && !e2e_capture_mode() {
        return Err("Screen capture is not supported on this platform".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        if native_livekit::is_desktop_capture_active() {
            return native_livekit::start_video_track(config);
        }
        native_livekit::start_video_track(config.clone())?;
        let capture_result = if e2e_capture_mode() {
            native_livekit::start_synthetic_capture(&config).map(|_| ())
        } else {
            start_real_capture(&config, source).map(|_| ())
        };
        if let Err(error) = capture_result {
            let _ = native_livekit::stop_video_track();
            return Err(error);
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("go live task failed: {e}"))?
}

/// Enumerates the screens and windows capturable through WGC (Windows-only),
/// for the renderer's in-app source picker.
///
/// # Errors
///
/// Returns an error on non-Windows platforms or when the WGC enumeration
/// fails.
#[tauri::command(rename_all = "camelCase")]
pub async fn get_capture_sources() -> Result<Vec<CaptureSourceInfo>, String> {
    #[cfg(target_os = "windows")]
    {
        tauri::async_runtime::spawn_blocking(native_livekit::get_windows_capture_sources)
            .await
            .map_err(|e| format!("get capture sources task failed: {e}"))?
            .map(|sources| sources.into_iter().map(map_source_info).collect())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("get_capture_sources is only supported on Windows".into())
    }
}

/// Benchmark-only: raw-payload channel for `bench_push_frames` throughput/latency
/// measurements.
static BENCH_CHANNEL: Mutex<Option<Channel<InvokeResponseBody>>> = Mutex::new(None);

/// Registers the benchmark channel. Replaces any previously registered one.
///
/// # Errors
///
/// Returns an error when the channel state lock is poisoned.
#[tauri::command(rename_all = "camelCase")]
pub async fn bench_register_channel(channel: Channel<InvokeResponseBody>) -> Result<(), String> {
    let Ok(mut guard) = BENCH_CHANNEL.lock() else {
        return Err("bench channel lock poisoned".into());
    };
    *guard = Some(channel);
    Ok(())
}

/// Benchmark-only: pushes `count` raw payloads of `size` bytes at
/// `interval_ms` cadence through the registered bench channel. The renderer
/// records arrival timestamps and computes cadence, jitter and bytes/s.
///
/// # Errors
///
/// Returns an error when the channel state lock is poisoned or no channel
/// has been registered yet.
#[tauri::command(rename_all = "camelCase")]
pub async fn bench_push_frames(count: u32, size: usize, interval_ms: u64) -> Result<(), String> {
    let channel = {
        let Ok(guard) = BENCH_CHANNEL.lock() else {
            return Err("bench channel lock poisoned".into());
        };
        let Some(channel) = guard.as_ref() else {
            return Err("no bench channel registered".into());
        };
        channel.clone()
    };
    tauri::async_runtime::spawn_blocking(move || {
        let payload = vec![0xA5u8; size];
        for _ in 0..count {
            let _ = channel.send(InvokeResponseBody::Raw(payload.clone()));
            if interval_ms > 0 {
                std::thread::sleep(Duration::from_millis(interval_ms));
            }
        }
    });
    Ok(())
}
