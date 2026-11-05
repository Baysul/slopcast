use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{Emitter, Manager};

use crate::AppHandle;
use crate::context::CaptureContextCache;
use crate::dto::{AudioAppDto, AudioAppWaveDto, CaptureContextDto};

const WAVE_EPSILON: f64 = 0.002;
static AUDIO_MUTED: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct WaveDeduplicator {
    last: HashMap<i32, CachedWave>,
}

struct CachedWave {
    columns: Vec<f64>,
    seen: bool,
}

impl WaveDeduplicator {
    fn update(&mut self, waves: &[native_rust::AudioAppWave]) -> bool {
        let mut changed = false;
        for wave in waves {
            if let Some(previous) = self.last.get_mut(&wave.id) {
                previous.seen = true;
                if previous.columns.len() != wave.columns.len()
                    || previous
                        .columns
                        .iter()
                        .zip(&wave.columns)
                        .any(|(a, b)| (a - b).abs() > WAVE_EPSILON)
                {
                    changed = true;
                }
            } else {
                self.last.insert(
                    wave.id,
                    CachedWave {
                        columns: Vec::new(),
                        seen: true,
                    },
                );
                changed = true;
            }
        }

        let previous_len = self.last.len();
        self.last.retain(|_, wave| {
            let seen = wave.seen;
            wave.seen = false;
            seen
        });
        changed |= self.last.len() != previous_len;
        if changed {
            // The emitted payload contains every current stream. Keep the deduplication
            // baseline in sync with that payload while reusing the cached allocations.
            for wave in waves {
                if let Some(previous) = self.last.get_mut(&wave.id) {
                    previous.columns.clone_from(&wave.columns);
                }
            }
        }
        changed
    }
}

#[tauri::command]
pub(crate) fn get_audio_muted() -> bool {
    AUDIO_MUTED.load(Ordering::Relaxed)
}

#[tauri::command]
pub(crate) fn toggle_audio_mute() -> bool {
    !AUDIO_MUTED.fetch_xor(true, Ordering::Relaxed)
}

pub fn register_audio_callbacks(app: &AppHandle) {
    native_rust::set_audio_data_callback(Box::new(|mut pcm| {
        if AUDIO_MUTED.load(Ordering::Relaxed) {
            pcm.fill(0);
        }
        let _ = native_livekit::feed_pcm(pcm);
    }));

    let dedup = Mutex::new(WaveDeduplicator::default());
    let wave_handle = app.clone();
    native_rust::set_wave_callback(Box::new(move |waves| {
        {
            let Ok(mut last) = dedup.lock() else {
                return;
            };
            if !last.update(&waves) {
                return;
            }
        }
        let payload: Vec<AudioAppWaveDto> = waves.into_iter().map(AudioAppWaveDto::from).collect();
        let _ = wave_handle.emit("audio-wave-update", payload);
    }));
}

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

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if audio application enumeration fails.
pub async fn get_audio_apps() -> Result<Vec<AudioAppDto>, String> {
    tauri::async_runtime::spawn_blocking(native_rust::list_audio_applications)
        .await
        .map_err(|e| format!("audio apps task failed: {e}"))?
        .map(|apps| apps.into_iter().map(AudioAppDto::from).collect())
}

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if audio source enumeration fails.
pub async fn dump_audio_sources() -> Result<Vec<HashMap<String, String>>, String> {
    tauri::async_runtime::spawn_blocking(native_rust::dump_audio_sources)
        .await
        .map_err(|e| format!("dump audio sources task failed: {e}"))?
}

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if audio capture cannot start.
pub async fn start_audio_capture(target_id: AudioTargetArg) -> Result<bool, String> {
    let target = native_rust::AudioTarget::from(target_id);
    tauri::async_runtime::spawn_blocking(move || native_rust::start_audio_capture(&target))
        .await
        .map_err(|e| format!("start audio capture task failed: {e}"))?
}

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if audio capture cannot stop.
pub async fn stop_audio_capture() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(native_rust::stop_audio_capture)
        .await
        .map_err(|e| format!("stop audio capture task failed: {e}"))?
}

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if the active capture cannot switch targets.
pub async fn switch_audio_capture(target_id: AudioTargetArg) -> Result<bool, String> {
    let target = native_rust::AudioTarget::from(target_id);
    tauri::async_runtime::spawn_blocking(move || native_rust::switch_audio_capture(&target))
        .await
        .map_err(|e| format!("switch audio capture task failed: {e}"))?
}

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if audio metering cannot start.
pub async fn start_audio_metering() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(native_rust::start_audio_metering)
        .await
        .map_err(|e| format!("start audio metering task failed: {e}"))?
}

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if audio metering cannot stop.
pub async fn stop_audio_metering() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(native_rust::stop_audio_metering)
        .await
        .map_err(|e| format!("stop audio metering task failed: {e}"))?
}

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if capture context resolution fails.
pub async fn resolve_audio_source(
    app: AppHandle,
    name_hint: Option<String>,
) -> Result<Option<AudioAppDto>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        for attempt in 0..3 {
            if let Some(app_match) = native_rust::resolve_audio_app_for_captured_window() {
                return Ok(Some(AudioAppDto::from(app_match)));
            }
            if attempt < 2 {
                std::thread::sleep(Duration::from_millis(200));
            }
        }

        if let Some(hint) = name_hint.filter(|h| !h.trim().is_empty())
            && let Ok(Some(app_match)) = native_rust::resolve_audio_app_by_name(&hint)
        {
            return Ok(Some(AudioAppDto::from(app_match)));
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn wave(id: i32, level: f64) -> native_rust::AudioAppWave {
        native_rust::AudioAppWave {
            id,
            columns: vec![level; 192],
        }
    }

    #[test]
    fn stream_removal_emits_an_update_even_when_remaining_waves_are_unchanged() {
        let mut dedup = WaveDeduplicator::default();
        assert!(dedup.update(&[wave(1, 0.5), wave(2, 0.25)]));
        assert!(!dedup.update(&[wave(1, 0.5), wave(2, 0.25)]));
        assert!(dedup.update(&[wave(2, 0.25)]));
        assert!(!dedup.last.contains_key(&1));
        assert!(!dedup.update(&[wave(2, 0.25)]));
        assert!(dedup.update(&[]));
        assert!(dedup.last.is_empty());
        assert!(!dedup.update(&[]));
    }

    #[test]
    fn stream_churn_does_not_accumulate_cached_waves() {
        let mut dedup = WaveDeduplicator::default();
        for id in 0..1000 {
            assert!(dedup.update(&[wave(id, 0.5)]));
            assert_eq!(dedup.last.len(), 1);
        }
        assert!(dedup.update(&[]));
        assert!(dedup.last.is_empty());
    }

    #[test]
    fn small_changes_accumulate_against_the_last_emitted_wave() {
        let mut dedup = WaveDeduplicator::default();
        assert!(dedup.update(&[wave(1, 0.5)]));
        assert!(!dedup.update(&[wave(1, 0.501)]));
        assert!(dedup.update(&[wave(1, 0.503)]));
    }

    #[test]
    fn another_streams_update_also_refreshes_the_emitted_baseline() {
        let mut dedup = WaveDeduplicator::default();
        assert!(dedup.update(&[wave(1, 0.5), wave(2, 0.25)]));
        assert!(dedup.update(&[wave(1, 0.501), wave(2, 0.5)]));
        assert!(!dedup.update(&[wave(1, 0.5025), wave(2, 0.5)]));
    }
}
