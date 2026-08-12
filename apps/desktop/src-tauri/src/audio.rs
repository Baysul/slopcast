//! Audio commands: application enumeration, exclusive capture, metering and
//! the Wayland audio-source resolution cascade.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use tauri::{Emitter, Manager};

use crate::AppHandle;
use crate::context::CaptureContextCache;
use crate::dto::{AudioAppDto, AudioAppWaveDto, CaptureContextDto};

/// Waveform columns below this amplitude delta are not worth re-rendering;
/// shared with the renderer meter store (same value as `WAVE_EPSILON` in
/// `@slopcast/shared-types`).
const WAVE_EPSILON: f64 = 0.002;

/// Registers the PCM and waveform callbacks once at startup.
pub fn register_audio_callbacks(app: &AppHandle) {
    native_rust::set_audio_data_callback(Box::new(|pcm| {
        let _ = native_livekit::feed_pcm(pcm);
    }));

    let dedup = Mutex::new(HashMap::<i32, Vec<f64>>::new());
    let wave_handle = app.clone();
    native_rust::set_wave_callback(Box::new(move |waves| {
        {
            let Ok(mut last) = dedup.lock() else {
                return;
            };
            let any_changed = waves.iter().any(|wave| {
                last.get(&wave.id).is_none_or(|prev| {
                    prev.len() != wave.columns.len()
                        || prev
                            .iter()
                            .zip(&wave.columns)
                            .any(|(a, b)| (a - b).abs() > WAVE_EPSILON)
                })
            });
            if !any_changed {
                return;
            }
            for wave in &waves {
                last.insert(wave.id, wave.columns.clone());
            }
        }
        let payload: Vec<AudioAppWaveDto> = waves.into_iter().map(AudioAppWaveDto::from).collect();
        let _ = wave_handle.emit("audio-wave-update", payload);
    }));
}

/// Audio capture target as sent by the renderer: a numeric node id / PID, or a textual label.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
pub enum AudioTargetArg {
    Number(i32),
    Text(String),
}

impl From<AudioTargetArg> for native_rust::AudioTarget {
    fn from(arg: AudioTargetArg) -> Self {
        match arg {
            AudioTargetArg::Number(id) => Self::Id(id),
            AudioTargetArg::Text(label) => Self::Label(label),
        }
    }
}

/// Lists active audio applications visible to the native layer.
///
/// # Errors
///
/// Returns an error if the platform-specific audio enumeration fails.
#[tauri::command(rename_all = "camelCase")]
pub async fn get_audio_apps() -> Result<Vec<AudioAppDto>, String> {
    tauri::async_runtime::spawn_blocking(native_rust::list_audio_applications)
        .await
        .map_err(|e| format!("audio apps task failed: {e}"))?
        .map(|apps| apps.into_iter().map(AudioAppDto::from).collect())
}

/// Dumps the full property dictionaries of every live audio stream node.
///
/// # Errors
///
/// Returns an error if `PipeWire` node enumeration fails.
#[tauri::command(rename_all = "camelCase")]
pub async fn dump_audio_sources() -> Result<Vec<HashMap<String, String>>, String> {
    tauri::async_runtime::spawn_blocking(native_rust::dump_audio_sources)
        .await
        .map_err(|e| format!("dump audio sources task failed: {e}"))?
}

/// Starts exclusive audio capture for the given application.
///
/// # Errors
///
/// Returns an error if `PipeWire` node creation / WASAPI activation or
/// linking fails.
#[tauri::command(rename_all = "camelCase")]
pub async fn start_audio_capture(target_id: AudioTargetArg) -> Result<bool, String> {
    let target = native_rust::AudioTarget::from(target_id);
    tauri::async_runtime::spawn_blocking(move || native_rust::start_audio_capture(&target))
        .await
        .map_err(|e| format!("start audio capture task failed: {e}"))?
}

/// Stops the active audio capture session.
///
/// # Errors
///
/// Returns an error if the capture state lock is poisoned.
#[tauri::command(rename_all = "camelCase")]
pub async fn stop_audio_capture() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(native_rust::stop_audio_capture)
        .await
        .map_err(|e| format!("stop audio capture task failed: {e}"))?
}

/// Switches the active capture to a new target application.
///
/// # Errors
///
/// Returns an error if no capture session is active or the switch fails.
#[tauri::command(rename_all = "camelCase")]
pub async fn switch_audio_capture(target_id: AudioTargetArg) -> Result<bool, String> {
    let target = native_rust::AudioTarget::from(target_id);
    tauri::async_runtime::spawn_blocking(move || native_rust::switch_audio_capture(&target))
        .await
        .map_err(|e| format!("switch audio capture task failed: {e}"))?
}

/// Starts per-app audio waveform metering.
///
/// # Errors
///
/// Returns an error if the `PipeWire` meter thread fails to start.
#[tauri::command(rename_all = "camelCase")]
pub async fn start_audio_metering() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(native_rust::start_audio_metering)
        .await
        .map_err(|e| format!("start audio metering task failed: {e}"))?
}

/// Stops the active audio meter session.
///
/// # Errors
///
/// Returns an error if the meter state lock is poisoned.
#[tauri::command(rename_all = "camelCase")]
pub async fn stop_audio_metering() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(native_rust::stop_audio_metering)
        .await
        .map_err(|e| format!("stop audio metering task failed: {e}"))?
}

/// Resolves the audio application for the captured source (Wayland-only):
/// `PipeWire` introspection first (retried as xdg-desktop-portal may lag),
/// then a name match, then the capture context.
///
/// # Errors
///
/// Returns an error if the native introspection fails outright.
#[tauri::command(rename_all = "camelCase")]
pub async fn resolve_audio_source(
    app: AppHandle,
    name_hint: Option<String>,
) -> Result<Option<AudioAppDto>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // Layer 1: PipeWire introspection — retry as xdg-desktop-portal may lag.
        for attempt in 0..3 {
            if let Some(app_match) = native_rust::resolve_audio_app_for_captured_window() {
                return Ok(Some(AudioAppDto::from(app_match)));
            }
            if attempt < 2 {
                std::thread::sleep(Duration::from_millis(200));
            }
        }

        // Layer 2: name match.
        if let Some(hint) = name_hint.filter(|h| !h.trim().is_empty())
            && let Ok(Some(app_match)) = native_rust::resolve_audio_app_by_name(&hint)
        {
            return Ok(Some(AudioAppDto::from(app_match)));
        }

        // Layer 3: capture context.
        if let Ok(context) = native_rust::get_capture_context() {
            if let Some(cache) = app.try_state::<CaptureContextCache>() {
                cache.update(CaptureContextDto::from(&context));
            }
            if let Some(app_match) = context.app {
                return Ok(Some(AudioAppDto::from(app_match)));
            }
        }

        Ok(None)
    })
    .await
    .map_err(|e| format!("resolve audio source task failed: {e}"))?
}
