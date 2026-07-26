use crate::AudioApp;
use napi::Result as NapiResult;
use std::sync::{Arc, Mutex};

use screencapturekit::prelude::{
    CMSampleBuffer, SCContentFilter, SCRunningApplication, SCShareableContent, SCStream,
    SCStreamConfiguration, SCStreamOutputTrait, SCStreamOutputType,
};

fn napi_err(context: &str, e: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(format!("{context}: {e}"))
}

struct AudioOutputHandler;

impl SCStreamOutputTrait for AudioOutputHandler {
    fn did_output_sample_buffer(&self, _sample: CMSampleBuffer, _of_type: SCStreamOutputType) {}
}

struct MacCaptureState {
    is_active: bool,
    stream: Option<SCStream>,
    target_bundle_id: Option<String>,
}

impl MacCaptureState {
    const fn new() -> Self {
        Self { is_active: false, stream: None, target_bundle_id: None }
    }
}

static CAPTURE_STATE: Mutex<Option<MacCaptureState>> = Mutex::new(None);

fn shareable_content() -> NapiResult<SCShareableContent> {
    SCShareableContent::get()
        .map_err(|e| napi_err("SCShareableContent::get (grant screen recording permission?)", e))
}

fn resolve_target_bundle_id(
    target_app_id: &napi::Either<String, i32>,
    applications: &[SCRunningApplication],
) -> NapiResult<String> {
    match target_app_id {
        napi::Either::A(bundle) if !bundle.is_empty() => Ok(bundle.clone()),
        napi::Either::B(pid) => applications
            .iter()
            .find(|a| a.process_id() == *pid)
            .map(|a| a.bundle_identifier())
            .ok_or_else(|| napi::Error::from_reason(format!("No running app for PID {pid}"))),
        _ => Err(napi::Error::from_reason("A bundle identifier or process ID is required")),
    }
}

pub fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
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
        });
    }
    Ok(apps)
}

pub fn start_audio_capture(target_app_id: &napi::Either<String, i32>) -> NapiResult<bool> {
    let mut guard = CAPTURE_STATE.lock().map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let state = guard.get_or_insert_with(MacCaptureState::new);

    if let Some(old_stream) = state.stream.take() {
        let _ = old_stream.stop_capture();
    }
    state.is_active = false;

    let content = shareable_content()?;
    let applications = content.applications();
    let target_bundle_id = resolve_target_bundle_id(target_app_id, &applications)?;

    let target_app =
        applications.iter().find(|a| a.bundle_identifier() == target_bundle_id).ok_or_else(
            || napi::Error::from_reason(format!("No running app for bundle {target_bundle_id}")),
        )?;

    let display = content.displays().into_iter().next().ok_or_else(|| {
        napi::Error::from_reason("No display available for ScreenCaptureKit audio capture".into())
    })?;

    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_including_applications(&[target_app], &[])
        .try_build()
        .map_err(|e| napi_err("SCContentFilter::try_build", e))?;

    let config = SCStreamConfiguration::new()
        .with_captures_audio(true)
        .with_sample_rate(48_000)
        .with_channel_count(2)
        .with_excludes_current_process_audio(true)
        .with_width(64)
        .with_height(64)
        .with_shows_cursor(false);

    let mut stream = SCStream::new(&filter, &config);
    stream.add_output_handler(AudioOutputHandler, SCStreamOutputType::Audio);
    stream.start_capture().map_err(|e| napi_err("SCStream::start_capture", e))?;

    state.stream = Some(stream);
    state.target_bundle_id = Some(target_bundle_id);
    state.is_active = true;
    Ok(true)
}

pub fn stop_audio_capture() -> NapiResult<bool> {
    let Ok(mut guard) = CAPTURE_STATE.lock() else { return Ok(true) };
    if let Some(state) = guard.as_mut() {
        if let Some(stream) = state.stream.take() {
            stream.stop_capture().map_err(|e| napi_err("SCStream::stop_capture", e))?;
        }
        state.is_active = false;
        state.target_bundle_id = None;
    }
    Ok(true)
}

pub fn is_audio_capture_active() -> NapiResult<bool> {
    let Ok(guard) = CAPTURE_STATE.lock() else { return Ok(false) };
    Ok(guard.as_ref().map(|s| s.is_active).unwrap_or(false))
}

pub fn switch_audio_capture(_: &napi::Either<String, i32>) -> NapiResult<bool> {
    Err(napi::Error::from_reason("Audio target switching is not yet supported on macOS"))
}

pub fn resolve_audio_app_for_x11_window(_: u32) -> Option<AudioApp> {
    None
}

pub fn resolve_audio_app_for_captured_window() -> Option<AudioApp> {
    None
}
