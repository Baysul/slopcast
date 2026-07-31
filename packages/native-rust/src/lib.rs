#![allow(
    clippy::needless_pass_by_value,
    reason = "napi-rs #[napi] function signatures must take ownership of Either/String params for JS type conversion"
)]

use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;

mod video_file;

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

    pub fn stop_audio_capture() -> napi::Result<bool> {
        Ok(true)
    }

    pub fn is_audio_capture_active() -> napi::Result<bool> {
        Ok(false)
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

    pub fn stop_audio_metering() -> napi::Result<bool> {
        Ok(true)
    }

    pub fn get_audio_levels() -> napi::Result<Vec<crate::AudioAppLevel>> {
        Ok(Vec::new())
    }

    pub fn set_audio_data_callback(
        _: std::sync::Arc<ThreadsafeFunction<napi::bindgen_prelude::Buffer, ()>>,
    ) -> napi::Result<()> {
        Ok(())
    }

    pub fn set_dmabuf_callback(
        _: std::sync::Arc<ThreadsafeFunction<(i32, i32, i32, i32, i32, i32), ()>>,
    ) -> napi::Result<()> {
        Ok(())
    }

    pub fn clear_dmabuf_callback() -> napi::Result<()> {
        Ok(())
    }

    pub fn start_video_capture(_: u32, _: u32, _: u32, _: u32) -> napi::Result<bool> {
        Err(napi::Error::from_reason(
            "Video capture is not supported on this platform",
        ))
    }

    pub fn stop_video_capture() -> napi::Result<bool> {
        Ok(true)
    }

    pub fn is_video_capture_active() -> napi::Result<bool> {
        Ok(false)
    }

    pub fn list_screen_sources() -> napi::Result<Vec<napi::Unknown<'static>>> {
        Ok(Vec::new())
    }

    pub fn set_video_frame_callback(
        _: std::sync::Arc<ThreadsafeFunction<Vec<u8>, ()>>,
    ) -> napi::Result<()> {
        Err(napi::Error::from_reason(
            "Native video file decode is not supported on this platform",
        ))
    }

    pub fn set_audio_frame_callback(
        _: std::sync::Arc<ThreadsafeFunction<Vec<u8>, ()>>,
    ) -> napi::Result<()> {
        Err(napi::Error::from_reason(
            "Native video file decode is not supported on this platform",
        ))
    }

    pub fn probe_video_file(_: String) -> napi::Result<crate::VideoFileInfo> {
        Err(napi::Error::from_reason(
            "Native video file decode is not supported on this platform",
        ))
    }

    pub fn start_video_file_playback(_: String) -> napi::Result<()> {
        Err(napi::Error::from_reason(
            "Native video file decode is not supported on this platform",
        ))
    }

    pub fn stop_video_file_playback() -> napi::Result<()> {
        Ok(())
    }

    pub fn seek_video_file_playback(_: i64) -> napi::Result<()> {
        Ok(())
    }

    pub fn set_video_file_paused(_: bool) -> napi::Result<()> {
        Ok(())
    }

    pub fn is_video_file_playback_active() -> napi::Result<bool> {
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
    pub screencast_node_id: Option<u32>,
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
        // Wine/Proton games report `application.name=wine64-preloader` while
        // the actual window title is the game name. Chromium-family browsers
        // leave `media.name="Playback"` and rely on the track label carrying
        // the window title. Match against `window_title` as a fallback for
        // both.
        .or_else(|| {
            apps.iter().find(|a| {
                a.window_title
                    .as_deref()
                    .is_some_and(|t| title_matches(t, &lower, first_word))
            })
        })
        .cloned()
}

fn title_matches(title: &str, lower: &str, first_word: &str) -> bool {
    let tl = title.to_lowercase();
    tl == lower
        || lower.contains(&tl)
        || tl.contains(lower)
        || tl.contains(first_word)
        || first_word.contains(&tl)
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
    platform::stop_audio_capture()
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
    platform::is_audio_capture_active()
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
    platform::stop_audio_metering()
}

/// Returns current audio level readings for all metered applications.
///
/// # Errors
///
/// Always returns `Ok`.
#[napi]
pub fn get_audio_levels() -> napi::Result<Vec<AudioAppLevel>> {
    platform::get_audio_levels()
}

/// Registers a callback that receives converted PCM audio data as 16-bit
/// signed integer samples (48 kHz, 2 channel). The conversion from F32LE
/// happens in Rust before the callback fires.
///
/// # Errors
///
/// Returns an error if the platform module rejects the callback.
#[napi]
pub fn set_audio_data_callback(
    callback: std::sync::Arc<ThreadsafeFunction<napi::bindgen_prelude::Buffer, ()>>,
) -> napi::Result<()> {
    platform::set_audio_data_callback(callback)
}

/// Registers a callback for receiving DMA-BUF video frames from the PipeWire
/// capture thread. Each frame is packed as `[fd, width, height, format, pts_lo, pts_hi]`.
/// The callback owns the fd and must close it.
#[napi]
pub fn set_dmabuf_callback(
    callback: std::sync::Arc<ThreadsafeFunction<(i32, i32, i32, i32, i32, i32), ()>>,
) -> napi::Result<()> {
    platform::set_dmabuf_callback(callback)
}

/// Clears the DMA-BUF callback. Frames produced after this call are dropped.
#[napi]
pub fn clear_dmabuf_callback() -> napi::Result<()> {
    platform::clear_dmabuf_callback();
    Ok(())
}

/// Starts PipeWire video capture from a screencast node.
#[napi]
pub fn start_video_capture(node_id: u32, width: u32, height: u32, fps: u32) -> napi::Result<bool> {
    platform::start_video_capture(node_id, width, height, fps)
}

/// Stops the active PipeWire video capture session.
#[napi]
pub fn stop_video_capture() -> napi::Result<bool> {
    platform::stop_video_capture()
}

#[napi(object)]
#[derive(Debug, Clone)]
pub struct VideoFileInfo {
    pub width: u32,
    pub height: u32,
    pub duration_ms: i64,
    pub has_audio: bool,
}

/// Registers a callback that receives decoded RGBA video frames from the
/// FFmpeg video file decoder. Each `Vec<u8>` contains `width * height * 4`
/// bytes in RGBA order. An empty `Vec<u8>` signals end-of-file.
#[napi]
pub fn set_video_frame_callback(
    callback: std::sync::Arc<ThreadsafeFunction<napi::bindgen_prelude::Buffer, ()>>,
) -> napi::Result<()> {
    video_file::ffmpeg::set_video_frame_callback(callback)
}

/// Registers a callback that receives decoded PCM audio from the FFmpeg
/// video file decoder. Each `Vec<u8>` contains signed 16-bit little-endian
/// stereo interleaved samples at 48 kHz. An empty `Vec<u8>` signals EOF.
#[napi]
pub fn set_audio_frame_callback(
    callback: std::sync::Arc<ThreadsafeFunction<napi::bindgen_prelude::Buffer, ()>>,
) -> napi::Result<()> {
    video_file::ffmpeg::set_audio_frame_callback(callback)
}

/// Probes a video file path and returns its dimensions, duration, and audio
/// track availability without starting playback.
#[napi]
pub fn probe_video_file(path: String) -> napi::Result<VideoFileInfo> {
    let info = video_file::ffmpeg::probe_file(&path).map_err(|e| napi::Error::from_reason(e))?;
    Ok(VideoFileInfo {
        width: info.width,
        height: info.height,
        duration_ms: info.duration_ms,
        has_audio: info.has_audio,
    })
}

/// Starts FFmpeg-based video file playback, delivering decoded video and
/// audio frames through the registered callbacks.
#[napi]
pub fn start_video_file_playback(path: String) -> napi::Result<()> {
    video_file::ffmpeg::start_playback(&path).map_err(|e| napi::Error::from_reason(e))
}

/// Stops active FFmpeg video file playback and joins the decode thread.
#[napi]
pub fn stop_video_file_playback() -> napi::Result<()> {
    video_file::ffmpeg::stop_playback();
    Ok(())
}

/// Seeks the active playback to a given timestamp in milliseconds.
#[napi]
pub fn seek_video_file_playback(ts_ms: i64) -> napi::Result<()> {
    video_file::ffmpeg::seek_playback(ts_ms);
    Ok(())
}

/// Pauses or resumes active video file playback.
#[napi]
pub fn set_video_file_paused(paused: bool) -> napi::Result<()> {
    video_file::ffmpeg::set_playback_paused(paused);
    Ok(())
}

/// Returns `true` if a video file playback session is currently active.
#[napi]
pub fn is_video_file_playback_active() -> napi::Result<bool> {
    Ok(video_file::ffmpeg::is_playback_active())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: i32, name: &str, window_title: Option<&str>) -> AudioApp {
        AudioApp {
            id,
            name: name.to_string(),
            process_id: id.cast_unsigned().cast_signed(),
            bundle_id: None,
            window_title: window_title.map(str::to_string),
            client_id: None,
            media_title: None,
        }
    }

    fn matched_id(apps: &[AudioApp], label: &str) -> Option<i32> {
        find_best_audio_match(apps, label).map(|a| a.id)
    }

    #[test]
    fn matches_exact_app_name() {
        let apps = [app(1, "Spotify", None), app(2, "Firefox", None)];
        assert_eq!(matched_id(&apps, "Spotify"), Some(1));
    }

    #[test]
    fn matches_chromium_with_tab_title() {
        // Brave tab title is "Brave - YouTube - Music Video" in the portal
        // picker; the audio node's `application.name` is the binary name.
        let apps = [app(1, "Brave", None), app(2, "Spotify", None)];
        assert_eq!(matched_id(&apps, "Brave - YouTube - Music Video"), Some(1));
    }

    #[test]
    fn matches_wine_proton_by_window_title() {
        // Wine/Proton games expose audio under `wine64-preloader` while the
        // window title carries the real game name. Track label coming from
        // the portal is "Blue Archive".
        let apps = [
            app(1, "wine64-preloader", Some("Blue Archive")),
            app(2, "Spotify", None),
        ];
        assert_eq!(matched_id(&apps, "Blue Archive"), Some(1));
    }

    #[test]
    fn matches_chromium_by_first_word() {
        // Brave's audio process is reported as `application.name="Brave"`.
        // A portal track label of "Brave - YouTube - Music Video" picks the
        // audio app because the label contains the app name.
        let apps = [app(1, "Brave", None), app(2, "Spotify", None)];
        assert_eq!(matched_id(&apps, "Brave - YouTube"), Some(1));
    }

    #[test]
    fn returns_none_for_unrelated_label() {
        let apps = [app(1, "Spotify", None), app(2, "Firefox", None)];
        assert!(find_best_audio_match(&apps, "Blue Archive").is_none());
    }

    #[test]
    fn prefers_exact_name_over_substring() {
        // "Spotify" must win over "Spotify-cli" when the label is exactly
        // "Spotify".
        let apps = [app(1, "Spotify-cli", None), app(2, "Spotify", None)];
        assert_eq!(matched_id(&apps, "Spotify"), Some(2));
    }
}
