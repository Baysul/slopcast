//! Capture commands — ports of the Electron `video.ts` handlers, driving
//! `native-livekit`'s desktop capturer and video track.

use tauri::Emitter;

use native_livekit::{CaptureConfig, DesktopCaptureStats};

use crate::dto::PreviewFrameDto;
use crate::platform::is_wayland;

/// Registers the preview-frame callback (MIGRATION §9.1): the capture thread
/// invokes it with base64 JPEG previews (640×360 @ ~15 fps), and they are
/// forwarded to the renderer as `preview-frame` events. The capture engine
/// only knows this callback — Tauri stays out of native-livekit.
pub fn register_preview_callback(app: &tauri::AppHandle) {
    let app = app.clone();
    native_livekit::set_preview_callback(Box::new(move |width, height, data, pts_us| {
        let _ = app.emit(
            "preview-frame",
            PreviewFrameDto {
                data,
                width,
                height,
                pts_us,
            },
        );
    }));
}

/// Result of `start_native_capture`, matching the Electron handler's
/// `{ ok, nodeId, videoEnabled }` shape (X11/Windows degrades to audio-only).
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
/// desktop capturer (the portal picker appears on Wayland). On non-Wayland
/// sessions the share degrades to audio-only, exactly like the Electron
/// handler.
#[tauri::command(rename_all = "camelCase")]
pub async fn start_native_capture(config: CaptureConfig) -> CaptureStartResult {
    if !is_wayland() {
        return CaptureStartResult {
            ok: true,
            ..CaptureStartResult::default()
        };
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut result = CaptureStartResult {
            ok: true,
            ..CaptureStartResult::default()
        };
        if let Err(e) = native_livekit::start_video_track(config) {
            return CaptureStartResult::failed(e);
        }
        match native_livekit::start_desktop_capture() {
            Ok(video_enabled) => result.video_enabled = video_enabled,
            Err(e) => return CaptureStartResult::failed(e),
        }
        result
    })
    .await
    .unwrap_or_else(|e| CaptureStartResult::failed(format!("start capture task failed: {e}")))
}

/// Re-publishes the video track with new encoder settings without restarting
/// the capture. Returns `false` on non-Wayland sessions or when the publish
/// fails.
///
/// # Errors
///
/// Returns an error if the update task fails to run.
#[tauri::command(rename_all = "camelCase")]
pub async fn update_native_video(config: CaptureConfig) -> Result<bool, String> {
    if !is_wayland() {
        return Ok(false);
    }
    let updated =
        tauri::async_runtime::spawn_blocking(move || native_livekit::start_video_track(config))
            .await
            .map_err(|e| format!("update video task failed: {e}"))?
            .is_ok();
    Ok(updated)
}

/// Stops everything: the video track, the desktop capture and the audio
/// capture (MIGRATION §5: "track + capture + audio stop").
#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub async fn stop_native_capture() -> bool {
    let _ = native_livekit::stop_video_track();
    let _ = native_livekit::stop_desktop_capture();
    native_rust::stop_audio_capture().unwrap_or(false)
}

/// Stops only the desktop capture session (closing its portal stream).
#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub async fn stop_video_capture() -> bool {
    native_livekit::stop_desktop_capture()
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

/// Starts a capture-only pre-roll (MIGRATION §9.1): the portal picker
/// appears, frames flow and `preview-frame` events fire, but no track is
/// published yet — `go_live` publishes it. Audio stays renderer-owned: the
/// audio target is only known to the renderer, which calls
/// `start_audio_capture` separately after going live.
///
/// # Errors
///
/// Returns an error on non-Wayland sessions (D2 hard gate) or when the
/// capturer fails to start.
#[tauri::command(rename_all = "camelCase")]
pub async fn start_capture_preview() -> Result<(), String> {
    if !is_wayland() {
        return Err("Wayland is required for screen capture".into());
    }
    tauri::async_runtime::spawn_blocking(|| {
        native_livekit::start_desktop_capture()?;
        Ok(())
    })
    .await
    .map_err(|e| format!("start capture preview task failed: {e}"))?
}

/// Publishes the previewed capture (MIGRATION §9.1): when a pre-roll capture
/// is already active the track is published against it (frames keep flowing
/// to both the preview and the track); otherwise the combined start runs
/// (publish + portal capture), preserving the pre-preview behavior.
///
/// # Errors
///
/// Returns an error on non-Wayland sessions or when the track publish or the
/// capturer start fails.
#[tauri::command(rename_all = "camelCase")]
pub async fn go_live(config: CaptureConfig) -> Result<(), String> {
    if !is_wayland() {
        return Err("Wayland is required for screen capture".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        if native_livekit::is_desktop_capture_active() {
            return native_livekit::start_video_track(config);
        }
        native_livekit::start_video_track(config)?;
        native_livekit::start_desktop_capture()?;
        Ok(())
    })
    .await
    .map_err(|e| format!("go live task failed: {e}"))?
}
