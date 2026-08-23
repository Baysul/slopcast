use std::sync::Mutex;
use std::time::Duration;

use native_livekit::{CaptureConfig, DesktopCaptureStats};
use tauri::Emitter;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::AppHandle;
use crate::platform::video_capture_available;

fn e2e_capture_mode() -> bool {
    std::env::var("SLOPCAST_E2E_CAPTURE").as_deref() == Ok("synthetic")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureSourceKind {
    Screen,
    Window,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSourceInfo {
    pub id: u64,
    pub title: String,
    pub display_id: i64,
    pub kind: CaptureSourceKind,
}

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

pub static LATEST_FRAME: Mutex<Option<Vec<u8>>> = Mutex::new(None);

fn clear_latest_frame() -> Result<(), String> {
    let mut frame = LATEST_FRAME
        .lock()
        .map_err(|error| format!("latest preview frame lock poisoned: {error}"))?;

    *frame = None;

    Ok(())
}

pub fn register_preview_frame_callback() {
    native_livekit::set_preview_callback(Box::new(move |bytes, _pts_us| {
        if let Ok(mut slot) = LATEST_FRAME.lock() {
            *slot = Some(bytes);
        }
    }));
}

static CAPTURE_ENDED_EMITTER: Mutex<Option<AppHandle>> = Mutex::new(None);

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

#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn set_preview_viewport(width: u32, height: u32) -> bool {
    native_livekit::set_preview_viewport(width, height);
    true
}

#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn clear_preview_viewport() -> bool {
    native_livekit::clear_preview_viewport();
    true
}

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
        if let Err(error) = clear_latest_frame() {
            return CaptureStartResult::failed(error);
        }
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

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if the video update task fails.
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

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if an active capture component cannot stop.
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

    clear_latest_frame()?;

    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if the video capture cannot stop.
pub async fn stop_video_capture() -> Result<(), String> {
    let track_result = native_livekit::stop_video_track();
    let capture_stopped = native_livekit::stop_desktop_capture();
    track_result?;
    if !capture_stopped {
        return Err("Failed to stop desktop capture".into());
    }

    clear_latest_frame()?;

    Ok(())
}

#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn is_native_capture_active() -> bool {
    native_livekit::is_video_track_active()
}

#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn get_video_capture_stats() -> DesktopCaptureStats {
    native_livekit::get_desktop_capture_stats()
}

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

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if preview capture cannot start.
pub async fn start_capture_preview(
    app: AppHandle,
    source: Option<CaptureSourceSelection>,
) -> Result<(), String> {
    if !video_capture_available() && !e2e_capture_mode() {
        return Err("Screen capture is not supported on this platform".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        clear_latest_frame()?;
        if e2e_capture_mode() {
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
                auto_bitrate: false,
            };
            native_livekit::start_synthetic_capture(&config)?;
        } else {
            let config = native_livekit::CaptureConfig {
                width: 0,
                height: 0,
                fps: 30,
                video_codec: None,
                max_bitrate: None,
                auto_bitrate: false,
            };
            start_real_capture(&config, source)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("start capture preview task failed: {e}"))?
}

#[tauri::command(rename_all = "camelCase")]
pub async fn start_synthetic_capture(config: CaptureConfig) -> CaptureStartResult {
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = clear_latest_frame() {
            return CaptureStartResult::failed(error);
        }

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

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if the video track or capture cannot start.
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

        clear_latest_frame()?;
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

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if capture source enumeration fails.
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

static BENCH_CHANNEL: Mutex<Option<Channel<InvokeResponseBody>>> = Mutex::new(None);

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if the benchmark channel lock is unavailable.
pub async fn bench_register_channel(channel: Channel<InvokeResponseBody>) -> Result<(), String> {
    let Ok(mut guard) = BENCH_CHANNEL.lock() else {
        return Err("bench channel lock poisoned".into());
    };
    *guard = Some(channel);
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if the benchmark channel is unavailable.
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
