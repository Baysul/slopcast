#![allow(
    clippy::needless_pass_by_value,
    reason = "napi-rs #[napi] function signatures must take ownership of Either/String params for JS type conversion"
)]

use napi_derive::napi;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod unsupported_platform {
    use crate::AudioApp;

    pub fn list_audio_applications() -> napi::Result<Vec<AudioApp>> {
        Err(napi::Error::from_reason(
            "Native audio capture is not supported on this platform",
        ))
    }

    pub fn start_audio_capture(_: &napi::Either<String, i32>) -> napi::Result<bool> {
        Err(napi::Error::from_reason(
            "Native audio capture is not supported on this platform",
        ))
    }

    pub fn stop_audio_capture() -> bool {
        true
    }

    pub fn is_audio_capture_active() -> bool {
        false
    }

    pub fn switch_audio_capture(_: &napi::Either<String, i32>) -> napi::Result<bool> {
        Err(napi::Error::from_reason(
            "Native audio capture is not supported on this platform",
        ))
    }

    pub fn resolve_audio_app_for_x11_window(_: u32) -> Option<AudioApp> {
        None
    }

    pub fn resolve_audio_app_for_captured_window() -> Option<AudioApp> {
        None
    }

    pub fn get_capture_context() -> napi::Result<crate::CaptureContext> {
        Err(napi::Error::from_reason(
            "Capture context is only available on Linux",
        ))
    }

    pub fn start_audio_metering() -> napi::Result<bool> {
        Ok(false)
    }

    pub fn stop_audio_metering() -> bool {
        true
    }

    pub fn get_audio_levels() -> Vec<crate::AudioAppLevel> {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
use crate::linux as platform;
#[cfg(target_os = "macos")]
use crate::macos as platform;
#[cfg(target_os = "windows")]
use crate::windows as platform;
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
use unsupported_platform as platform;

#[napi(object)]
#[derive(Debug, Clone)]
pub struct AudioApp {
    pub id: i32,
    pub name: String,
    pub process_id: i32,
    pub bundle_id: Option<String>,
    pub window_title: Option<String>,
    pub client_id: Option<i32>,
    pub media_title: Option<String>,
}

#[napi(object)]
#[derive(Debug, Clone, Copy)]
pub struct AudioAppLevel {
    pub id: i32,
    pub level: f64,
}

/// Wayland video-capture introspection for the desktop main process: which
/// desktop environment is streaming, whether the source is a monitor or a
/// window, and the best-matched audio application for the captured source.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct CaptureContext {
    pub de: String,
    pub source_type: String,
    pub media_name: Option<String>,
    pub video_node_count: i32,
    pub app: Option<AudioApp>,
}

pub(crate) fn find_best_audio_match(apps: &[AudioApp], label: &str) -> Option<AudioApp> {
    let lower = label.to_lowercase();
    let first_word = lower.split_whitespace().next()?;
    apps.iter()
        .find(|a| a.name.to_lowercase() == lower)
        .or_else(|| apps.iter().find(|a| lower.contains(&a.name.to_lowercase())))
        .or_else(|| apps.iter().find(|a| a.name.to_lowercase().contains(&lower)))
        .or_else(|| {
            apps.iter().find(|a| {
                let n = a.name.to_lowercase();
                n.contains(first_word) || first_word.contains(&n)
            })
        })
        .cloned()
}

#[must_use]
#[napi]
pub fn init_engine() -> String {
    "Native engine initialized".into()
}

/// Lists active audio applications visible to the native layer.
///
/// # Errors
///
/// Returns an error if the platform-specific audio enumeration fails.
#[napi]
pub fn list_audio_applications() -> napi::Result<Vec<AudioApp>> {
    platform::list_audio_applications()
}

/// Starts exclusive audio capture for the given application.
///
/// `target_app_id` can be a `PipeWire` node ID (as a number) or an application
/// name (as a string). Pass `-1` to capture system audio (every application).
///
/// # Errors
///
/// Returns an error if `PipeWire` node creation or linking fails.
#[napi]
pub fn start_audio_capture(target_app_id: napi::Either<String, i32>) -> napi::Result<bool> {
    platform::start_audio_capture(&target_app_id)
}

/// Stops the active audio capture session.
///
/// # Errors
///
/// Returns an error if the capture state lock is poisoned.
#[napi]
pub fn stop_audio_capture() -> napi::Result<bool> {
    Ok(platform::stop_audio_capture())
}

/// Switches the active capture to a new target application.
///
/// `target_app_id` can be a `PipeWire` node ID (as a number) or an application
/// name (as a string). Pass `-1` to capture system audio (every application).
///
/// # Errors
///
/// Returns an error if sending the switch command to the capture thread fails.
#[napi]
pub fn switch_audio_capture(target_app_id: napi::Either<String, i32>) -> napi::Result<bool> {
    platform::switch_audio_capture(&target_app_id)
}

/// Returns `true` if an audio capture session is currently active.
///
/// # Errors
///
/// Returns an error if the capture state lock is poisoned.
#[napi]
pub fn is_audio_capture_active() -> napi::Result<bool> {
    Ok(platform::is_audio_capture_active())
}

/// Resolves the audio application for the given X11 window ID.
///
/// # Errors
///
/// Always returns `Ok`.
#[napi]
pub fn resolve_audio_app_for_x11_window(window_id: i32) -> napi::Result<Option<AudioApp>> {
    Ok(platform::resolve_audio_app_for_x11_window(
        window_id.cast_unsigned(),
    ))
}

/// Resolves the audio application for the currently portal-captured window.
///
/// # Errors
///
/// Always returns `Ok`.
#[napi]
pub fn resolve_audio_app_for_captured_window() -> napi::Result<Option<AudioApp>> {
    Ok(platform::resolve_audio_app_for_captured_window())
}

/// Resolves the best-matching audio application by name.
///
/// # Errors
///
/// Delegates to `list_audio_applications` and propagates its errors.
#[napi]
pub fn resolve_audio_app_by_name(label: String) -> napi::Result<Option<AudioApp>> {
    let apps = platform::list_audio_applications()?;
    Ok(find_best_audio_match(&apps, &label))
}

/// Returns a snapshot of the currently active `PipeWire` video capture context.
///
/// # Errors
///
/// Returns an error if `PipeWire` video node introspection fails.
#[napi]
pub fn get_capture_context() -> napi::Result<CaptureContext> {
    platform::get_capture_context()
}

/// Starts per-app audio level metering.
///
/// # Errors
///
/// Returns an error if the `PipeWire` meter thread fails to start.
#[napi]
pub fn start_audio_metering() -> napi::Result<bool> {
    platform::start_audio_metering()
}

/// Stops the active audio meter session.
///
/// # Errors
///
/// Returns an error if the meter state lock is poisoned.
#[napi]
pub fn stop_audio_metering() -> napi::Result<bool> {
    Ok(platform::stop_audio_metering())
}

/// Returns current audio level readings for all metered applications.
///
/// # Errors
///
/// Always returns `Ok`.
#[napi]
pub fn get_audio_levels() -> napi::Result<Vec<AudioAppLevel>> {
    Ok(platform::get_audio_levels())
}
