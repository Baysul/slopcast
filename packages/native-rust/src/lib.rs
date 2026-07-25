#![deny(clippy::all)]

use napi::Either;
use napi_derive::napi;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod unsupported_platform {
    use crate::AudioApp;
    use napi::{Either, Result as NapiResult};

    pub fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
        Err(napi::Error::from_reason("Native audio capture is not supported on this platform"))
    }

    pub fn start_audio_capture(_: &Either<String, i32>) -> NapiResult<bool> {
        Err(napi::Error::from_reason("Native audio capture is not supported on this platform"))
    }

    pub fn stop_audio_capture() -> NapiResult<bool> {
        Err(napi::Error::from_reason("Native audio capture is not supported on this platform"))
    }

    pub fn is_audio_capture_active() -> NapiResult<bool> {
        Ok(false)
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
}

pub(crate) fn find_best_audio_match(apps: &[AudioApp], label: &str) -> Option<AudioApp> {
    let query_lower = label.to_lowercase();

    let first_word = query_lower.split_whitespace().next()?;

    apps.iter()
        .find(|a| a.name.to_lowercase() == query_lower)
        .or_else(|| apps.iter().find(|a| query_lower.contains(&a.name.to_lowercase())))
        .or_else(|| apps.iter().find(|a| a.name.to_lowercase().contains(&query_lower)))
        .or_else(|| {
            apps.iter().find(|a| {
                let name_lower = a.name.to_lowercase();
                name_lower.contains(first_word) || first_word.contains(&name_lower)
            })
        })
        .cloned()
}

#[napi]
pub fn init_engine() -> String {
    "Native engine initialized".to_string()
}

#[napi]
pub fn list_audio_applications() -> napi::Result<Vec<AudioApp>> {
    platform::list_audio_applications()
}

#[napi]
pub fn start_audio_capture(target_app_id: Either<String, i32>) -> napi::Result<bool> {
    platform::start_audio_capture(&target_app_id)
}

#[napi]
pub fn stop_audio_capture() -> napi::Result<bool> {
    platform::stop_audio_capture()
}

#[napi]
pub fn is_audio_capture_active() -> napi::Result<bool> {
    platform::is_audio_capture_active()
}

#[napi]
pub fn resolve_audio_app_for_x11_window(window_id: i32) -> napi::Result<Option<AudioApp>> {
    #[cfg(target_os = "linux")]
    {
        Ok(crate::linux::resolve_audio_by_x11_window(window_id as u32))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = window_id;
        Ok(None)
    }
}

#[napi]
pub fn resolve_audio_app_for_captured_window() -> napi::Result<Option<AudioApp>> {
    #[cfg(target_os = "linux")]
    {
        Ok(crate::linux::resolve_audio_by_captured_window())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(None)
    }
}

#[napi]
pub fn resolve_audio_app_by_name(label: String) -> napi::Result<Option<AudioApp>> {
    let apps = platform::list_audio_applications()?;
    Ok(find_best_audio_match(&apps, &label))
}
