use crate::{AudioApp, AudioAppLevel};
use napi::Result as NapiResult;

use screencapturekit::prelude::SCShareableContent;

fn napi_err(context: &str, e: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(format!("{context}: {e}"))
}

fn shareable_content() -> NapiResult<SCShareableContent> {
    SCShareableContent::get().map_err(|e| {
        napi_err(
            "SCShareableContent::get (grant screen recording permission?)",
            e,
        )
    })
}

pub(crate) fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
    let content = shareable_content()?;
    let mut apps = Vec::new();
    for app in content.applications() {
        let name = app.application_name();
        if name.is_empty() {
            continue;
        }
        let pid = app.process_id();
        apps.push(AudioApp {
            id: pid,
            name,
            process_id: pid,
            bundle_id: Some(app.bundle_identifier()),
            window_title: None,
            client_id: None,
            media_title: None,
        });
    }
    Ok(apps)
}

// Audio capture on macOS is not wired to any consumer: ScreenCaptureKit sample
// buffers would need a virtual device or IPC path to reach the renderer, and
// neither exists yet (AGENTS.md Task 4). Start reports an explicit error
// instead of pretending to capture.
pub(crate) fn start_audio_capture(_: &napi::Either<String, i32>) -> NapiResult<bool> {
    Err(napi::Error::from_reason(
        "Per-application audio capture is not yet implemented on macOS",
    ))
}

pub(crate) fn stop_audio_capture() -> bool {
    true
}

pub(crate) fn is_audio_capture_active() -> bool {
    false
}

pub(crate) fn switch_audio_capture(_: &napi::Either<String, i32>) -> NapiResult<bool> {
    Err(napi::Error::from_reason(
        "Audio target switching is not yet supported on macOS",
    ))
}

pub(crate) fn get_capture_context() -> NapiResult<crate::CaptureContext> {
    Err(napi::Error::from_reason(
        "Capture context is only available on Linux",
    ))
}

pub(crate) fn resolve_audio_app_for_x11_window(_: u32) -> Option<AudioApp> {
    None
}

pub(crate) fn resolve_audio_app_for_captured_window() -> Option<AudioApp> {
    None
}

pub(crate) fn start_audio_metering() -> NapiResult<bool> {
    Ok(false)
}

pub(crate) fn stop_audio_metering() -> bool {
    true
}

pub(crate) fn get_audio_levels() -> Vec<AudioAppLevel> {
    Vec::new()
}
