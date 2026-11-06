//! Desktop session detection.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformInfo {
    pub platform: &'static str,
    pub is_wayland: bool,
    pub desktop_environment: String,
    pub video_capture_available: bool,
}

#[must_use]
pub fn is_wayland() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland")
            || std::env::var("WAYLAND_DISPLAY").is_ok_and(|v| !v.is_empty()))
}

/// Whether the presenter runs the synthetic capture source used by end-to-end tests.
#[must_use]
pub fn synthetic_capture() -> bool {
    std::env::var("SLOPCAST_E2E_CAPTURE").as_deref() == Ok("synthetic")
}

#[must_use]
pub fn video_capture_available() -> bool {
    cfg!(target_os = "windows") || is_wayland()
}

#[must_use]
pub fn platform_info() -> PlatformInfo {
    PlatformInfo {
        platform: std::env::consts::OS,
        is_wayland: is_wayland(),
        desktop_environment: std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_lowercase(),
        video_capture_available: video_capture_available() || synthetic_capture(),
    }
}
