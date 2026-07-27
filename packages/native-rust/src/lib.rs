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

    pub fn list_audio_applications() -> napi::Result<Vec<AudioApp>> {
        Err(napi::Error::from_reason("Native audio capture is not supported on this platform"))
    }

    pub fn start_audio_capture(_: &napi::Either<String, i32>) -> napi::Result<bool> {
        Err(napi::Error::from_reason("Native audio capture is not supported on this platform"))
    }

    pub fn stop_audio_capture() -> napi::Result<bool> {
        Err(napi::Error::from_reason("Native audio capture is not supported on this platform"))
    }

    pub fn is_audio_capture_active() -> napi::Result<bool> {
        Ok(false)
    }

    pub fn switch_audio_capture(_: &napi::Either<String, i32>) -> napi::Result<bool> {
        Err(napi::Error::from_reason("Native audio capture is not supported on this platform"))
    }

    pub fn resolve_audio_app_for_x11_window(_: u32) -> Option<AudioApp> {
        None
    }

    pub fn resolve_audio_app_for_captured_window() -> Option<AudioApp> {
        None
    }

    pub fn start_audio_metering() -> napi::Result<bool> {
        Ok(false)
    }

    pub fn stop_audio_metering() -> napi::Result<bool> {
        Ok(true)
    }

    pub fn get_audio_levels() -> napi::Result<Vec<crate::AudioAppLevel>> {
        Ok(Vec::new())
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

#[napi]
pub fn init_engine() -> String {
    "Native engine initialized".into()
}

#[napi]
pub fn list_audio_applications() -> napi::Result<Vec<AudioApp>> {
    platform::list_audio_applications()
}

#[napi]
pub fn start_audio_capture(target_app_id: napi::Either<String, i32>) -> napi::Result<bool> {
    platform::start_audio_capture(&target_app_id)
}

#[napi]
pub fn stop_audio_capture() -> napi::Result<bool> {
    platform::stop_audio_capture()
}

#[napi]
pub fn switch_audio_capture(target_app_id: napi::Either<String, i32>) -> napi::Result<bool> {
    platform::switch_audio_capture(&target_app_id)
}

#[napi]
pub fn is_audio_capture_active() -> napi::Result<bool> {
    platform::is_audio_capture_active()
}

#[napi]
pub fn resolve_audio_app_for_x11_window(window_id: i32) -> napi::Result<Option<AudioApp>> {
    Ok(platform::resolve_audio_app_for_x11_window(window_id as u32))
}

#[napi]
pub fn resolve_audio_app_for_captured_window() -> napi::Result<Option<AudioApp>> {
    Ok(platform::resolve_audio_app_for_captured_window())
}

#[napi]
pub fn resolve_audio_app_by_name(label: String) -> napi::Result<Option<AudioApp>> {
    let apps = platform::list_audio_applications()?;
    Ok(find_best_audio_match(&apps, &label))
}

#[napi]
pub fn start_audio_metering() -> napi::Result<bool> {
    platform::start_audio_metering()
}

#[napi]
pub fn stop_audio_metering() -> napi::Result<bool> {
    platform::stop_audio_metering()
}

#[napi]
pub fn get_audio_levels() -> napi::Result<Vec<AudioAppLevel>> {
    platform::get_audio_levels()
}
