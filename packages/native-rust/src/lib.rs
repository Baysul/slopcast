#![allow(
    clippy::needless_pass_by_value,
    reason = "napi-rs #[napi] function signatures must take ownership of Either/String params for JS type conversion"
)]

use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;
use std::collections::HashMap;

mod audio_ring;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod unsupported_platform {
    use crate::AudioApp;

    pub fn list_audio_applications() -> napi::Result<Vec<AudioApp>> {
        Err(napi::Error::from_reason(
            "Native audio capture is not supported on this platform",
        ))
    }

    pub fn dump_audio_sources() -> napi::Result<Vec<std::collections::HashMap<String, String>>> {
        Ok(Vec::new())
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

    pub fn set_audio_wave_callback(
        _: std::sync::Arc<crate::WaveThreadsafeFunction>,
    ) -> napi::Result<()> {
        Ok(())
    }

    pub fn clear_audio_wave_callback() -> napi::Result<()> {
        Ok(())
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
}

#[cfg(target_os = "linux")]
use crate::linux as platform;
#[cfg(target_os = "windows")]
use crate::windows as platform;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
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
#[derive(Debug, Clone)]
pub struct AudioAppWave {
    pub id: i32,
    /// 96 interleaved (min, max) amplitude pairs of the last ~85 ms of mono
    /// audio, each value in [-1, 1].
    pub columns: Vec<f64>,
}

/// Cross-thread callback for the per-app waveform snapshots pushed by the
/// meter worker at ~33 ms cadence.
pub type WaveThreadsafeFunction = ThreadsafeFunction<Vec<AudioAppWave>, ()>;

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
    /// xdg-desktop-portal screencast metadata (`portal.screencast.*`) for the
    /// captured window — the portal's own record of what was picked.
    pub portal_props: Option<HashMap<String, String>>,
    /// KWin-resolved owning window PID (KDE window captures only).
    pub window_pid: Option<i32>,
    /// KWin-resolved window caption (KDE window captures only).
    pub window_caption: Option<String>,
}

pub(crate) fn find_best_audio_match(apps: &[AudioApp], label: &str) -> Option<AudioApp> {
    let label_trim = label.trim();
    if label_trim.is_empty() {
        return None;
    }
    let lower = label_trim.to_lowercase();
    let lower_clean = lower
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>();
    let first_word = lower.split_whitespace().next().unwrap_or("");

    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();

    let acronym: String = if words.len() >= 2 {
        words
            .iter()
            .map(|w| w.chars().next().unwrap_or(' '))
            .collect()
    } else {
        String::new()
    };

    let norm_apps: Vec<(&AudioApp, String, String, Option<String>, String)> = apps
        .iter()
        .filter(|a| !a.name.trim().is_empty())
        .map(|a| {
            let name_lower = a.name.trim().to_lowercase();
            let name_clean = name_lower
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>();
            let win_lower = a
                .window_title
                .as_deref()
                .map(|t| t.trim().to_lowercase())
                .filter(|t| !t.is_empty());
            let cmdline_lower = if a.process_id > 0 {
                std::fs::read_to_string(format!("/proc/{}/cmdline", a.process_id))
                    .unwrap_or_default()
                    .replace('\\', "/")
                    .to_lowercase()
            } else {
                String::new()
            };
            (a, name_lower, name_clean, win_lower, cmdline_lower)
        })
        .collect();

    // 1. Exact name match
    if let Some((a, _, _, _, _)) = norm_apps
        .iter()
        .find(|(_, name_lower, _, _, _)| *name_lower == lower)
    {
        return Some((*a).clone());
    }

    // 2. Cleaned name equality (handling spaces / .exe, e.g. "zenless zone zero" vs "zenlesszonezero.exe")
    if !lower_clean.is_empty() {
        if let Some((a, _, _, _, _)) = norm_apps.iter().find(|(_, _, name_clean, _, _)| {
            let stem = name_clean.trim_end_matches("exe");
            !stem.is_empty()
                && (stem == lower_clean
                    || lower_clean.contains(stem)
                    || stem.contains(&lower_clean))
        }) {
            return Some((*a).clone());
        }
    }

    // 3. Name contained in label
    if let Some((a, _, _, _, _)) = norm_apps
        .iter()
        .find(|(_, name_lower, _, _, _)| !name_lower.is_empty() && lower.contains(name_lower))
    {
        return Some((*a).clone());
    }

    // 4. Label contained in name
    if let Some((a, _, _, _, _)) = norm_apps
        .iter()
        .find(|(_, name_lower, _, _, _)| !name_lower.is_empty() && name_lower.contains(&lower))
    {
        return Some((*a).clone());
    }

    // 5. Acronym match (e.g. "Final Fantasy XIV" -> "ffxiv" matching "ffxiv_dx11.exe")
    if acronym.len() >= 3 {
        if let Some((a, _, _, _, _)) = norm_apps.iter().find(|(_, name_lower, _, _, _)| {
            name_lower.starts_with(&acronym) || name_lower.contains(&acronym)
        }) {
            return Some((*a).clone());
        }
    }

    // 6. Cmdline match (e.g. /proc/pid/cmdline containing "final fantasy xiv")
    if lower.len() >= 3 {
        if let Some((a, _, _, _, _)) = norm_apps
            .iter()
            .find(|(_, _, _, _, cmdline)| !cmdline.is_empty() && cmdline.contains(&lower))
        {
            return Some((*a).clone());
        }
    }

    // 7. Significant word match (word length >= 4)
    for word in &words {
        if word.len() >= 4 {
            if let Some((a, _, _, _, _)) = norm_apps
                .iter()
                .find(|(_, name_lower, _, _, _)| name_lower.contains(word))
            {
                return Some((*a).clone());
            }
        }
    }

    // 8. First word match
    if !first_word.is_empty() {
        if let Some((a, _, _, _, _)) = norm_apps.iter().find(|(_, name_lower, _, _, _)| {
            !name_lower.is_empty()
                && (name_lower.contains(first_word) || first_word.contains(name_lower))
        }) {
            return Some((*a).clone());
        }
    }

    // 9. Window title match
    if let Some((a, _, _, _, _)) = norm_apps.iter().find(|(_, _, _, win_lower, _)| {
        win_lower
            .as_deref()
            .is_some_and(|wt| title_matches(wt, &lower, first_word))
    }) {
        return Some((*a).clone());
    }

    None
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
pub struct ListAudioAppsTask;

impl napi::Task for ListAudioAppsTask {
    type Output = Vec<AudioApp>;
    type JsValue = Vec<AudioApp>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        platform::list_audio_applications()
    }

    fn resolve(&mut self, _env: napi::Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

/// Lists active audio applications visible to the native layer asynchronously.
///
/// # Errors
///
/// Returns an error if the platform-specific audio enumeration fails.
#[napi(ts_return_type = "Promise<Array<AudioApp>>")]
pub fn list_audio_applications() -> napi::Result<napi::bindgen_prelude::AsyncTask<ListAudioAppsTask>>
{
    Ok(napi::bindgen_prelude::AsyncTask::new(ListAudioAppsTask))
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

pub struct ResolveAudioAppForX11WindowTask {
    window_id: u32,
}

impl napi::Task for ResolveAudioAppForX11WindowTask {
    type Output = Option<AudioApp>;
    type JsValue = Option<AudioApp>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        Ok(platform::resolve_audio_app_for_x11_window(self.window_id))
    }

    fn resolve(&mut self, _env: napi::Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

/// Resolves the audio application for the given X11 window ID asynchronously.
///
/// # Errors
///
/// Always returns `Ok`.
#[napi(ts_return_type = "Promise<AudioApp | null>")]
pub fn resolve_audio_app_for_x11_window(
    window_id: i32,
) -> napi::Result<napi::bindgen_prelude::AsyncTask<ResolveAudioAppForX11WindowTask>> {
    let wid = if window_id > 0 { window_id as u32 } else { 0 };
    Ok(napi::bindgen_prelude::AsyncTask::new(
        ResolveAudioAppForX11WindowTask { window_id: wid },
    ))
}

pub struct ResolveAudioAppForCapturedWindowTask;

impl napi::Task for ResolveAudioAppForCapturedWindowTask {
    type Output = Option<AudioApp>;
    type JsValue = Option<AudioApp>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        Ok(platform::resolve_audio_app_for_captured_window())
    }

    fn resolve(&mut self, _env: napi::Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

/// Resolves the audio application for the currently portal-captured window asynchronously.
///
/// # Errors
///
/// Always returns `Ok`.
#[napi(ts_return_type = "Promise<AudioApp | null>")]
pub fn resolve_audio_app_for_captured_window()
-> napi::Result<napi::bindgen_prelude::AsyncTask<ResolveAudioAppForCapturedWindowTask>> {
    Ok(napi::bindgen_prelude::AsyncTask::new(
        ResolveAudioAppForCapturedWindowTask,
    ))
}

pub struct DumpAudioSourcesTask;

impl napi::Task for DumpAudioSourcesTask {
    type Output = Vec<std::collections::HashMap<String, String>>;
    type JsValue = Vec<std::collections::HashMap<String, String>>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        platform::dump_audio_sources()
    }

    fn resolve(&mut self, _env: napi::Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

/// Dumps the full property dictionaries of every live audio stream node asynchronously.
///
/// # Errors
///
/// Returns an error if `PipeWire` node enumeration fails.
#[napi(ts_return_type = "Promise<Array<Record<string, string>>>")]
pub fn dump_audio_sources() -> napi::Result<napi::bindgen_prelude::AsyncTask<DumpAudioSourcesTask>>
{
    Ok(napi::bindgen_prelude::AsyncTask::new(DumpAudioSourcesTask))
}

pub struct ResolveAudioAppByNameTask {
    label: String,
}

impl napi::Task for ResolveAudioAppByNameTask {
    type Output = Option<AudioApp>;
    type JsValue = Option<AudioApp>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let apps = platform::list_audio_applications()?;
        Ok(find_best_audio_match(&apps, &self.label))
    }

    fn resolve(&mut self, _env: napi::Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

/// Resolves the best-matching audio application by name asynchronously.
///
/// # Errors
///
/// Delegates to `list_audio_applications` and propagates its errors.
#[napi(ts_return_type = "Promise<AudioApp | null>")]
pub fn resolve_audio_app_by_name(
    label: String,
) -> napi::Result<napi::bindgen_prelude::AsyncTask<ResolveAudioAppByNameTask>> {
    Ok(napi::bindgen_prelude::AsyncTask::new(
        ResolveAudioAppByNameTask { label },
    ))
}

pub struct GetCaptureContextTask;

impl napi::Task for GetCaptureContextTask {
    type Output = CaptureContext;
    type JsValue = CaptureContext;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        platform::get_capture_context()
    }

    fn resolve(&mut self, _env: napi::Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

/// Returns a snapshot of the currently active `PipeWire` video capture context asynchronously.
///
/// # Errors
///
/// Returns an error if `PipeWire` video node introspection fails.
#[napi(ts_return_type = "Promise<CaptureContext>")]
pub fn get_capture_context() -> napi::Result<napi::bindgen_prelude::AsyncTask<GetCaptureContextTask>>
{
    Ok(napi::bindgen_prelude::AsyncTask::new(GetCaptureContextTask))
}

/// Starts per-app audio waveform metering.
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

/// Registers a callback that receives the current waveform readings for all
/// metered applications. Each entry carries 96 interleaved (min, max)
/// amplitude pairs. The meter worker pushes at ~33 ms cadence; the callback is
/// invoked non-blocking, so ticks are dropped (never queued) when the main
/// process is busy.
///
/// # Errors
///
/// Returns an error if the platform module rejects the callback.
#[napi]
pub fn set_audio_wave_callback(
    callback: std::sync::Arc<crate::WaveThreadsafeFunction>,
) -> napi::Result<()> {
    platform::set_audio_wave_callback(callback)
}

/// Clears the registered waveform callback.
#[napi]
pub fn clear_audio_wave_callback() {
    platform::clear_audio_wave_callback();
}

/// Telemetry counters for the audio ring buffer.
#[napi(object)]
#[derive(Debug, Clone, Copy)]
pub struct AudioRingStats {
    pub captured_chunks: i64,
    pub captured_bytes: i64,
    pub ring_drops: i64,
    pub tsfn_drops: i64,
    pub truncated_bytes: i64,
}

/// Returns current telemetry counters for the audio ring buffer.
#[napi]
pub fn get_audio_ring_stats() -> AudioRingStats {
    let s = audio_ring::get_audio_ring_stats();
    AudioRingStats {
        captured_chunks: s.captured_chunks as i64,
        captured_bytes: s.captured_bytes as i64,
        ring_drops: s.ring_drops as i64,
        tsfn_drops: s.tsfn_drops as i64,
        truncated_bytes: s.truncated_bytes as i64,
    }
}

/// Resets telemetry counters for the audio ring buffer.
#[napi]
pub fn reset_audio_ring_stats() {
    audio_ring::reset_audio_ring_stats();
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
    callback: std::sync::Arc<audio_ring::AudioThreadsafeFunction>,
) -> napi::Result<()> {
    audio_ring::set_audio_data_callback(callback)
}

/// Clears the registered PCM audio data callback.
#[napi]
pub fn clear_audio_data_callback() {
    audio_ring::clear_audio_data_callback();
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
    fn test_init_engine() {
        assert_eq!(init_engine(), "Native engine initialized");
    }

    #[test]
    fn test_struct_properties() {
        let audio_app = AudioApp {
            id: 42,
            name: "TestApp".to_string(),
            process_id: 1234,
            bundle_id: Some("com.example.test".to_string()),
            window_title: Some("Test Window".to_string()),
            client_id: Some(100),
            media_title: Some("Song Title".to_string()),
        };
        let cloned_app = audio_app.clone();
        assert_eq!(cloned_app.id, 42);
        assert_eq!(cloned_app.name, "TestApp");
        assert_eq!(cloned_app.process_id, 1234);
        assert_eq!(cloned_app.bundle_id.as_deref(), Some("com.example.test"));
        assert_eq!(cloned_app.window_title.as_deref(), Some("Test Window"));
        assert_eq!(cloned_app.client_id, Some(100));
        assert_eq!(cloned_app.media_title.as_deref(), Some("Song Title"));
        assert!(format!("{audio_app:?}").contains("TestApp"));

        let wave = AudioAppWave {
            id: 1,
            columns: vec![0.75],
        };
        let cloned_wave = wave.clone();
        assert_eq!(cloned_wave.id, 1);
        assert!((cloned_wave.columns[0] - 0.75).abs() < f64::EPSILON);

        let context = CaptureContext {
            de: "kde".to_string(),
            source_type: "window".to_string(),
            media_name: Some("kwin-screencast-test".to_string()),
            video_node_count: 1,
            app: Some(audio_app),
            screencast_node_id: Some(123),
            portal_props: Some(HashMap::from([(
                "portal.screencast.title".to_string(),
                "Test Window".to_string(),
            )])),
            window_pid: Some(4321),
            window_caption: Some("Test Window".to_string()),
        };
        let cloned_context = context.clone();
        assert_eq!(cloned_context.de, "kde");
        assert_eq!(cloned_context.source_type, "window");
        assert_eq!(cloned_context.video_node_count, 1);
        assert_eq!(cloned_context.screencast_node_id, Some(123));
        assert_eq!(cloned_context.window_pid, Some(4321));
        assert_eq!(
            cloned_context.window_caption.as_deref(),
            Some("Test Window")
        );
        assert_eq!(
            cloned_context
                .portal_props
                .as_ref()
                .and_then(|m| m.get("portal.screencast.title"))
                .map(String::as_str),
            Some("Test Window"),
        );
        assert!(format!("{context:?}").contains("kwin-screencast-test"));
    }

    #[test]
    fn test_title_matches_exact_and_case() {
        assert!(title_matches("Discord", "discord", "discord"));
        assert!(title_matches("DISCORD", "discord", "discord"));
    }

    #[test]
    fn test_title_matches_containment() {
        assert!(title_matches(
            "Discord | General",
            "discord | general - brave",
            "discord"
        ));
        assert!(title_matches(
            "Visual Studio Code - main.rs",
            "visual studio code",
            "visual"
        ));
    }

    #[test]
    fn test_title_matches_first_word() {
        assert!(title_matches(
            "Firefox Web Browser",
            "firefox - youtube",
            "firefox"
        ));
    }

    #[test]
    fn test_title_matches_unrelated() {
        assert!(!title_matches("Calculator", "spotify", "spotify"));
    }

    #[test]
    fn matches_exact_app_name() {
        let apps = [app(1, "Spotify", None), app(2, "Firefox", None)];
        assert_eq!(matched_id(&apps, "Spotify"), Some(1));
    }

    #[test]
    fn matches_case_insensitive() {
        let apps = [app(1, "Spotify", None), app(2, "Firefox", None)];
        assert_eq!(matched_id(&apps, "sPoTiFy"), Some(1));
        assert_eq!(matched_id(&apps, "SPOTIFY"), Some(1));
    }

    #[test]
    fn matches_chromium_with_tab_title() {
        let apps = [app(1, "Brave", None), app(2, "Spotify", None)];
        assert_eq!(matched_id(&apps, "Brave - YouTube - Music Video"), Some(1));
    }

    #[test]
    fn matches_wine_proton_acronym_and_clean_name() {
        let apps = [
            app(1, "ffxiv_dx11.exe", None),
            app(2, "ZenlessZoneZero.exe", None),
        ];
        assert_eq!(matched_id(&apps, "Final Fantasy XIV"), Some(1));
        assert_eq!(matched_id(&apps, "FINAL FANTASY XIV"), Some(1));
        assert_eq!(matched_id(&apps, "Zenless Zone Zero"), Some(2));
    }

    #[test]
    fn matches_wine_proton_by_window_title() {
        let apps = [
            app(1, "wine64-preloader", Some("Blue Archive")),
            app(2, "Spotify", None),
        ];
        assert_eq!(matched_id(&apps, "Blue Archive"), Some(1));
    }

    #[test]
    fn matches_chromium_by_first_word() {
        let apps = [app(1, "Brave", None), app(2, "Spotify", None)];
        assert_eq!(matched_id(&apps, "Brave - YouTube"), Some(1));
    }

    #[test]
    fn returns_none_for_unrelated_label() {
        let apps = [app(1, "Spotify", None), app(2, "Firefox", None)];
        assert!(find_best_audio_match(&apps, "Blue Archive").is_none());
    }

    #[test]
    fn returns_none_for_empty_or_whitespace_label() {
        let apps = [app(1, "Spotify", None)];
        assert!(find_best_audio_match(&apps, "").is_none());
        assert!(find_best_audio_match(&apps, "   ").is_none());
    }

    #[test]
    fn returns_none_for_empty_apps_list() {
        assert!(find_best_audio_match(&[], "Spotify").is_none());
    }

    #[test]
    fn prefers_exact_name_over_substring() {
        let apps = [app(1, "Spotify-cli", None), app(2, "Spotify", None)];
        assert_eq!(matched_id(&apps, "Spotify"), Some(2));
    }

    #[test]
    fn handles_apps_with_full_metadata() {
        let apps = [AudioApp {
            id: 10,
            name: "Spotify".to_string(),
            process_id: 1234,
            bundle_id: Some("com.spotify.client".to_string()),
            window_title: Some("Spotify Free".to_string()),
            client_id: Some(50),
            media_title: Some("Artist - Track".to_string()),
        }];
        assert_eq!(matched_id(&apps, "Spotify"), Some(10));
        assert_eq!(matched_id(&apps, "Spotify Free"), Some(10));
    }
}
