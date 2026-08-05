//! Capture commands — ports of the Electron `video.ts` handlers, driving
//! `native-livekit`'s desktop capturer and video track.

use std::sync::Mutex;
use std::time::Duration;

use native_livekit::{CaptureConfig, DesktopCaptureStats};
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::platform::is_wayland;

/// When set, the capture commands route to the synthetic test-pattern source
/// instead of the portal: the headless e2e drives the real UI flow (preview →
/// go live) without a portal picker or a Wayland session. Production runs
/// never set this.
fn e2e_capture_mode() -> bool {
    std::env::var("SLOPCAST_E2E_CAPTURE").as_deref() == Ok("synthetic")
}

/// Starts a capture through the active route: the synthetic source in e2e
/// mode, the portal source otherwise. Returns the `CaptureStartResult`.
fn start_capture(config: &CaptureConfig) -> CaptureStartResult {
    let mut result = CaptureStartResult {
        ok: true,
        ..CaptureStartResult::default()
    };
    let started = if e2e_capture_mode() {
        native_livekit::start_synthetic_capture(config)
    } else {
        native_livekit::start_desktop_capture()
    };
    match started {
        Ok(video_enabled) => result.video_enabled = video_enabled,
        Err(e) => return CaptureStartResult::failed(e),
    }
    result
}

/// The renderer's preview channel: the preview callback forwards JPEG bytes
/// here (prefixed with an 8-byte little-endian `pts_us` header) whenever the
/// renderer has registered one. Raw payloads travel over Tauri's binary
/// channel path — no base64, no JSON per frame — and the header lets the
/// renderer measure end-to-end preview latency.
static PREVIEW_CHANNEL: Mutex<Option<Channel<InvokeResponseBody>>> = Mutex::new(None);

/// Registers the preview callback (MIGRATION §9.1): the capture thread
/// invokes it with JPEG bytes `(data, pts_us)` at up to 60 fps, and they are
/// forwarded to the renderer's preview channel. The capture engine only
/// knows this callback — Tauri stays out of native-livekit.
pub fn register_preview_channel_callback() {
    native_livekit::set_preview_callback(Box::new(move |bytes, pts_us| {
        let Ok(guard) = PREVIEW_CHANNEL.lock() else {
            return;
        };
        let Some(channel) = guard.as_ref() else {
            return;
        };
        let mut payload = Vec::with_capacity(8 + bytes.len());
        payload.extend_from_slice(&pts_us.to_le_bytes());
        payload.extend_from_slice(&bytes);
        let _ = channel.send(InvokeResponseBody::Raw(payload));
    }));
}

/// Registers the renderer's preview channel; the renderer calls this once on
/// mount so preview frames can flow. Replaces any previously registered
/// channel (e.g. after a webview reload).
///
/// # Errors
///
/// Returns an error when the channel state lock is poisoned.
#[tauri::command(rename_all = "camelCase")]
pub async fn register_preview_channel(channel: Channel<InvokeResponseBody>) -> Result<(), String> {
    let Ok(mut guard) = PREVIEW_CHANNEL.lock() else {
        return Err("preview channel lock poisoned".into());
    };
    *guard = Some(channel);
    Ok(())
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
/// handler (synthetic e2e mode bypasses the Wayland gate).
#[tauri::command(rename_all = "camelCase")]
pub async fn start_native_capture(config: CaptureConfig) -> CaptureStartResult {
    if !is_wayland() && !e2e_capture_mode() {
        return CaptureStartResult {
            ok: true,
            ..CaptureStartResult::default()
        };
    }
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(e) = native_livekit::start_video_track(config.clone()) {
            return CaptureStartResult::failed(e);
        }
        start_capture(&config)
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
    if !is_wayland() && !e2e_capture_mode() {
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
/// appears, frames flow to the preview channel, but no track is
/// published yet — `go_live` publishes it. Audio stays renderer-owned: the
/// audio target is only known to the renderer, which calls
/// `start_audio_capture` separately after going live. In synthetic e2e mode
/// the test-pattern source runs instead of the portal.
///
/// # Errors
///
/// Returns an error on non-Wayland sessions (D2 hard gate) or when the
/// capturer fails to start.
#[tauri::command(rename_all = "camelCase")]
pub async fn start_capture_preview() -> Result<(), String> {
    if !is_wayland() && !e2e_capture_mode() {
        return Err("Wayland is required for screen capture".into());
    }
    tauri::async_runtime::spawn_blocking(|| {
        if e2e_capture_mode() {
            let config = native_livekit::CaptureConfig {
                width: 1280,
                height: 720,
                fps: 30,
                video_codec: None,
                max_bitrate: None,
            };
            native_livekit::start_synthetic_capture(&config)?;
        } else {
            native_livekit::start_desktop_capture()?;
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
    if !is_wayland() && !e2e_capture_mode() {
        return Err("Wayland is required for screen capture".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        if native_livekit::is_desktop_capture_active() {
            return native_livekit::start_video_track(config);
        }
        native_livekit::start_video_track(config.clone())?;
        if e2e_capture_mode() {
            native_livekit::start_synthetic_capture(&config)?;
        } else {
            native_livekit::start_desktop_capture()?;
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("go live task failed: {e}"))?
}

/// Benchmark-only (Phase 2 of the preview transport comparison): a channel
/// the `bench_push_frames` command pushes raw payloads into, so the renderer
/// can measure Tauri's raw channel throughput and latency in isolation —
/// option 1's ~100 KB JPEG frames vs option 2's 921 KB RGBA frames.
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
