// Native Rust Screenshare & Audio Filter Module
#![deny(clippy::all)]

use napi::Either;
use napi_derive::napi;

pub mod linux;
pub mod macos;
pub mod windows;

/// An audio-producing application, exposed to TypeScript.
///
/// The meaning of `id` is platform specific:
/// - Linux: PipeWire node ID
/// - Windows: process ID (PID)
/// - macOS: process ID (PID); `bundle_id` carries the bundle identifier used
///   to resolve the ScreenCaptureKit capture target.
#[napi(object)]
#[derive(Debug, Clone)]
pub struct AudioApp {
    pub id: i32,
    pub name: String,
    pub process_id: i32,
    pub bundle_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Platform routing
//
// Each platform module exposes the same internal interface:
//   list_audio_applications() -> NapiResult<Vec<AudioApp>>
//   start_audio_capture(&Either<String, i32>) -> NapiResult<bool>
//   stop_audio_capture() -> NapiResult<bool>
//   is_audio_capture_active() -> NapiResult<bool>
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
use crate::linux as platform;

#[cfg(target_os = "windows")]
use crate::windows as platform;

#[cfg(target_os = "macos")]
use crate::macos as platform;

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod platform {
    use super::AudioApp;
    use napi::{Either, Error, Result as NapiResult};

    fn unsupported() -> Error {
        Error::from_reason("Native audio capture is not supported on this platform")
    }

    pub fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
        Err(unsupported())
    }

    pub fn start_audio_capture(_target_app_id: &Either<String, i32>) -> NapiResult<bool> {
        Err(unsupported())
    }

    pub fn stop_audio_capture() -> NapiResult<bool> {
        Err(unsupported())
    }

    pub fn is_audio_capture_active() -> NapiResult<bool> {
        Ok(false)
    }
}

#[napi]
pub fn init_engine() -> String {
    "Native engine initialized".to_string()
}

// ---------------------------------------------------------------------------
// Unified cross-platform NAPI interface
// ---------------------------------------------------------------------------

/// Lists applications that currently produce audio on this platform.
#[napi]
pub fn list_audio_applications() -> napi::Result<Vec<AudioApp>> {
    platform::list_audio_applications()
}

/// Starts audio capture of a single target application. ONLY the target
/// application's audio is captured; everything else is excluded from the
/// stream (the stream carries the shared window's audio and nothing else).
///
/// The target identifier is platform specific:
/// - Linux: PipeWire node ID (number; numeric strings are also parsed)
/// - Windows: process ID (number; numeric strings are also parsed)
/// - macOS: bundle identifier (string; a number is resolved via PID lookup)
#[napi]
pub fn start_audio_capture(target_app_id: Either<String, i32>) -> napi::Result<bool> {
    platform::start_audio_capture(&target_app_id)
}

/// Stops the currently running audio capture, if any.
#[napi]
pub fn stop_audio_capture() -> napi::Result<bool> {
    platform::stop_audio_capture()
}

/// Returns whether audio capture is currently running.
#[napi]
pub fn is_audio_capture_active() -> napi::Result<bool> {
    platform::is_audio_capture_active()
}

// ---------------------------------------------------------------------------
// Audio-source resolution (auto-detect from window selection)
// ---------------------------------------------------------------------------

/// Resolves an X11 window ID to its owning application's audio capture
/// target. Uses `_NET_WM_PID` to find the process ID, then looks up the
/// matching `AudioApp` in the PipeWire audio application list.
///
/// Only meaningful on Linux/X11; returns `None` on other platforms or
/// when the window has no accessible PID property.
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

/// Finds the audio application whose name best matches the given label
/// (e.g. a `MediaStreamTrack.label` from `getDisplayMedia` on Wayland).
/// Works on all platforms by matching against the current audio app list.
#[napi]
pub fn resolve_audio_app_by_name(label: String) -> napi::Result<Option<AudioApp>> {
    #[cfg(target_os = "linux")]
    {
        Ok(crate::linux::resolve_audio_by_name(&label))
    }
    #[cfg(target_os = "windows")]
    {
        Ok(crate::windows::resolve_audio_by_name(&label))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(crate::macos::resolve_audio_by_name(&label))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = label;
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Legacy bindings (kept for the existing Electron main process call sites)
// ---------------------------------------------------------------------------

#[napi]
pub fn get_audio_applications() -> napi::Result<Vec<AudioApp>> {
    platform::list_audio_applications()
}
