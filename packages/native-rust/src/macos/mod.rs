// ScreenCaptureKit audio capture of a single target application for macOS.
//
// Uses `SCShareableContent` to enumerate running applications, resolves the
// capture target to an `SCRunningApplication`, and builds an
// `SCContentFilter` (`initWithDisplay:includingApplications:exceptingWindows:`)
// whose inclusion set limits the captured stream to ONLY the target
// application's audio — the shared window's audio and nothing else. Audio is
// captured through `SCStream` configured with audio capture enabled
// (48 kHz stereo).

use crate::AudioApp;
use napi::{Either, Result as NapiResult};

// ---------------------------------------------------------------------------
// macOS implementation
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod sck {
    use super::AudioApp;
    use napi::Result as NapiResult;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use screencapturekit::prelude::{
        CMSampleBuffer, SCContentFilter, SCRunningApplication, SCShareableContent, SCStream,
        SCStreamConfiguration, SCStreamOutputTrait, SCStreamOutputType,
    };

    fn napi_err(context: &str, e: impl std::fmt::Display) -> napi::Error {
        napi::Error::from_reason(format!("{}: {}", context, e))
    }

    /// Receives captured audio sample buffers. This is the handoff point for
    /// the WebRTC/Opus encoding pipeline; buffers are currently counted and
    /// released (mirroring the topology-only Linux filter).
    struct AudioOutputHandler {
        buffers_received: Arc<AtomicU64>,
    }

    impl SCStreamOutputTrait for AudioOutputHandler {
        fn did_output_sample_buffer(&self, _sample: CMSampleBuffer, of_type: SCStreamOutputType) {
            if matches!(of_type, SCStreamOutputType::Audio) {
                self.buffers_received.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    struct MacCaptureState {
        is_active: bool,
        stream: Option<SCStream>,
        target_bundle_id: Option<String>,
        buffers_received: Arc<AtomicU64>,
    }

    impl MacCaptureState {
        fn new() -> Self {
            Self {
                is_active: false,
                stream: None,
                target_bundle_id: None,
                buffers_received: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    static MAC_STATE: Mutex<Option<MacCaptureState>> = Mutex::new(None);

    fn shareable_content() -> NapiResult<SCShareableContent> {
        SCShareableContent::get().map_err(|e| {
            napi_err(
                "SCShareableContent::get failed (is screen recording permission granted?)",
                e,
            )
        })
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

    /// Resolves the unified capture target to a bundle identifier. A string
    /// is treated as a bundle identifier directly; a number is resolved to
    /// the bundle identifier of the running app with that PID.
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
                .ok_or_else(|| {
                    napi::Error::from_reason(format!(
                        "No running application found for target PID {}",
                        pid
                    ))
                }),
            _ => Err(napi::Error::from_reason(
                "A bundle identifier or process ID is required as the audio capture target",
            )),
        }
    }

    pub fn start_audio_capture(target_app_id: &napi::Either<String, i32>) -> NapiResult<bool> {
        let mut guard = MAC_STATE
            .lock()
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        let state = guard.get_or_insert_with(MacCaptureState::new);

        // Restart semantics: stop any running capture first.
        if let Some(old_stream) = state.stream.take() {
            let _ = old_stream.stop_capture();
        }
        state.is_active = false;

        let content = shareable_content()?;
        let applications = content.applications();
        let target_bundle_id = resolve_target_bundle_id(target_app_id, &applications)?;

        // Build the inclusion set for the content filter so that ONLY the
        // target application's audio is captured.
        let target_app = applications
            .iter()
            .find(|a| a.bundle_identifier() == target_bundle_id)
            .ok_or_else(|| {
                napi::Error::from_reason(format!(
                    "No running application found for target bundle ID {}",
                    target_bundle_id
                ))
            })?;
        let included_apps: Vec<&SCRunningApplication> = vec![target_app];

        let display = content.displays().into_iter().next().ok_or_else(|| {
            napi::Error::from_reason(
                "No display available for ScreenCaptureKit audio capture".to_string(),
            )
        })?;

        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_including_applications(&included_apps, &[])
            .try_build()
            .map_err(|e| napi_err("Failed to build SCContentFilter", e))?;

        let config = SCStreamConfiguration::new()
            .with_captures_audio(true)
            .with_sample_rate(48_000)
            .with_channel_count(2)
            .with_excludes_current_process_audio(true)
            // Keep the (unused) video pipeline as cheap as possible.
            .with_width(64)
            .with_height(64)
            .with_shows_cursor(false);

        let mut stream = SCStream::new(&filter, &config);
        stream.add_output_handler(
            AudioOutputHandler {
                buffers_received: state.buffers_received.clone(),
            },
            SCStreamOutputType::Audio,
        );
        stream
            .start_capture()
            .map_err(|e| napi_err("SCStream::start_capture failed", e))?;

        state.stream = Some(stream);
        state.target_bundle_id = Some(target_bundle_id);
        state.is_active = true;
        Ok(true)
    }

    pub fn stop_audio_capture() -> NapiResult<bool> {
        let mut guard = MAC_STATE
            .lock()
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        if let Some(state) = guard.as_mut() {
            if let Some(stream) = state.stream.take() {
                stream
                    .stop_capture()
                    .map_err(|e| napi_err("SCStream::stop_capture failed", e))?;
            }
            state.is_active = false;
            state.target_bundle_id = None;
        }
        Ok(true)
    }

    pub fn is_audio_capture_active() -> NapiResult<bool> {
        let guard = MAC_STATE
            .lock()
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        if let Some(state) = guard.as_ref() {
            Ok(state.is_active)
        } else {
            Ok(false)
        }
    }
}

// ---------------------------------------------------------------------------
// Module-level interface (uniform with linux/windows modules)
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
    sck::list_audio_applications()
}

#[cfg(target_os = "macos")]
pub fn start_audio_capture(target_app_id: &Either<String, i32>) -> NapiResult<bool> {
    sck::start_audio_capture(target_app_id)
}

#[cfg(target_os = "macos")]
pub fn stop_audio_capture() -> NapiResult<bool> {
    sck::stop_audio_capture()
}

#[cfg(target_os = "macos")]
pub fn is_audio_capture_active() -> NapiResult<bool> {
    sck::is_audio_capture_active()
}

#[cfg(target_os = "macos")]
pub fn resolve_audio_by_name(label: &str) -> Option<AudioApp> {
    let apps = list_audio_applications().ok()?;
    let query_lower = label.to_lowercase();

    if let Some(app) = apps
        .iter()
        .find(|a| a.name.to_lowercase() == query_lower)
    {
        return Some(app.clone());
    }
    if let Some(app) = apps.iter().find(|a| {
        let name_lower = a.name.to_lowercase();
        query_lower.contains(&name_lower)
    }) {
        return Some(app.clone());
    }
    if let Some(app) = apps.iter().find(|a| {
        let name_lower = a.name.to_lowercase();
        name_lower.contains(&query_lower)
    }) {
        return Some(app.clone());
    }

    let first_word = query_lower.split_whitespace().next()?;
    apps.iter()
        .find(|a| {
            let name_lower = a.name.to_lowercase();
            name_lower.contains(first_word) || first_word.contains(&name_lower)
        })
        .cloned()
}

// ---------------------------------------------------------------------------
// Non-macOS stubs
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "macos"))]
fn unsupported() -> napi::Error {
    napi::Error::from_reason("ScreenCaptureKit audio capture is only supported on macOS")
}

#[cfg(not(target_os = "macos"))]
pub fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
    Err(unsupported())
}

#[cfg(not(target_os = "macos"))]
pub fn start_audio_capture(_target_app_id: &Either<String, i32>) -> NapiResult<bool> {
    Err(unsupported())
}

#[cfg(not(target_os = "macos"))]
pub fn stop_audio_capture() -> NapiResult<bool> {
    Err(unsupported())
}

#[cfg(not(target_os = "macos"))]
pub fn is_audio_capture_active() -> NapiResult<bool> {
    Ok(false)
}

#[cfg(not(target_os = "macos"))]
pub fn resolve_audio_by_name(_label: &str) -> Option<AudioApp> {
    None
}
