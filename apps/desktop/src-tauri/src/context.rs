//! Cached capture context — replaces `lastCaptureContext` in the Electron
//! `video.ts` main-process module.
#![allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command arguments (State and owned payloads) must be taken by value for the #[tauri::command] macro"
)]

use std::sync::Mutex;

use tauri::Manager;

use crate::dto::CaptureContextDto;

/// Managed state: the last successful `PipeWire` video-graph introspection,
/// refreshed by `inspect_capture_context` and the `resolve_audio_source`
/// cascade (mirroring how `video.ts` updated `lastCaptureContext`).
#[derive(Default)]
pub struct CaptureContextCache(Mutex<Option<CaptureContextDto>>);

impl CaptureContextCache {
    pub fn update(&self, context: CaptureContextDto) {
        if let Ok(mut guard) = self.0.lock() {
            *guard = Some(context);
        }
    }

    pub fn snapshot(&self) -> Option<CaptureContextDto> {
        self.0.lock().ok().and_then(|guard| guard.clone())
    }
}

/// Returns the cached capture context (or `null` before the first
/// introspection), matching the Electron `get-capture-context` handler.
#[must_use]
#[tauri::command]
pub fn get_capture_context(
    state: tauri::State<'_, CaptureContextCache>,
) -> Option<CaptureContextDto> {
    state.snapshot()
}

/// Runs a fresh `PipeWire` video-graph introspection and caches the result.
///
/// # Errors
///
/// Returns an error if `PipeWire` video node introspection fails.
#[tauri::command]
pub async fn inspect_capture_context(
    app: tauri::AppHandle,
) -> Result<Option<CaptureContextDto>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let context = native_rust::get_capture_context()?;
        let dto = CaptureContextDto::from(&context);
        if let Some(cache) = app.try_state::<CaptureContextCache>() {
            cache.update(dto.clone());
        }
        Ok(Some(dto))
    })
    .await
    .map_err(|e| format!("capture context task failed: {e}"))?
}
