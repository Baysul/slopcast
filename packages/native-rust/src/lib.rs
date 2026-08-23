use std::collections::HashMap;

mod audio_ring;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use crate::linux as platform;
#[cfg(target_os = "windows")]
use crate::windows as platform;

#[cfg(target_os = "linux")]
pub(crate) fn reap_detached(join: std::thread::JoinHandle<()>, name: &'static str) {
    if join.is_finished() {
        let _ = join.join();
        return;
    }
    let _ = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = join.join();
        });
}

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

#[derive(Debug, Clone)]
pub struct AudioAppWave {
    pub id: i32,
    pub columns: Vec<f64>,
}

pub enum AudioTarget {
    Id(i32),
    Label(String),
}

#[derive(Debug, Clone)]
pub struct CaptureContext {
    pub de: String,
    pub source_type: String,
    pub media_name: Option<String>,
    pub video_node_count: i32,
    pub app: Option<AudioApp>,
    pub screencast_node_id: Option<u32>,
    pub highest_serial: Option<f64>,
    pub portal_props: Option<HashMap<String, String>>,
    pub window_pid: Option<i32>,
    pub window_caption: Option<String>,
}

#[allow(
    clippy::too_many_lines,
    reason = "nine ordered match strategies are clearer as one cascade than as separate helpers"
)]
pub fn find_best_audio_match(apps: &[AudioApp], label: &str) -> Option<AudioApp> {
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

    if let Some((a, _, _, _, _)) = norm_apps
        .iter()
        .find(|(_, name_lower, _, _, _)| *name_lower == lower)
    {
        return Some((*a).clone());
    }

    if !lower_clean.is_empty()
        && let Some((a, _, _, _, _)) = norm_apps.iter().find(|(_, _, name_clean, _, _)| {
            let stem = name_clean.trim_end_matches("exe");
            !stem.is_empty()
                && (stem == lower_clean
                    || lower_clean.contains(stem)
                    || stem.contains(&lower_clean))
        })
    {
        return Some((*a).clone());
    }

    if let Some((a, _, _, _, _)) = norm_apps
        .iter()
        .find(|(_, name_lower, _, _, _)| !name_lower.is_empty() && lower.contains(name_lower))
    {
        return Some((*a).clone());
    }

    if let Some((a, _, _, _, _)) = norm_apps
        .iter()
        .find(|(_, name_lower, _, _, _)| !name_lower.is_empty() && name_lower.contains(&lower))
    {
        return Some((*a).clone());
    }

    if acronym.len() >= 3
        && let Some((a, _, _, _, _)) = norm_apps.iter().find(|(_, name_lower, _, _, _)| {
            name_lower.starts_with(&acronym) || name_lower.contains(&acronym)
        })
    {
        return Some((*a).clone());
    }

    if lower.len() >= 3
        && let Some((a, _, _, _, _)) = norm_apps
            .iter()
            .find(|(_, _, _, _, cmdline)| !cmdline.is_empty() && cmdline.contains(&lower))
    {
        return Some((*a).clone());
    }

    for word in &words {
        if word.len() >= 4
            && let Some((a, _, _, _, _)) = norm_apps
                .iter()
                .find(|(_, name_lower, _, _, _)| name_lower.contains(word))
        {
            return Some((*a).clone());
        }
    }

    if !first_word.is_empty()
        && let Some((a, _, _, _, _)) = norm_apps.iter().find(|(_, name_lower, _, _, _)| {
            !name_lower.is_empty()
                && (name_lower.contains(first_word) || first_word.contains(name_lower))
        })
    {
        return Some((*a).clone());
    }

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
pub fn init_engine() -> String {
    "Native engine initialized".into()
}

#[cfg(target_os = "linux")]
pub fn ensure_pipewire_init() {
    platform::ensure_pipewire_init();
}

#[cfg(not(target_os = "linux"))]
pub fn ensure_pipewire_init() {}

/// Lists active audio applications.
///
/// # Errors
/// Returns an error if platform enumeration fails.
pub fn list_audio_applications() -> Result<Vec<AudioApp>, String> {
    platform::list_audio_applications()
}

/// Returns the property maps for active audio sources.
///
/// # Errors
/// Returns an error if source enumeration fails.
pub fn dump_audio_sources() -> Result<Vec<HashMap<String, String>>, String> {
    platform::dump_audio_sources()
}

/// Starts exclusive audio capture for `target`.
///
/// # Errors
/// Returns an error if capture setup fails.
pub fn start_audio_capture(target: &AudioTarget) -> Result<bool, String> {
    platform::start_audio_capture(target)
}

/// Stops exclusive audio capture.
///
/// # Errors
/// Returns an error if capture state cannot be accessed.
pub fn stop_audio_capture() -> Result<bool, String> {
    Ok(platform::stop_audio_capture())
}

/// Switches exclusive audio capture to `target`.
///
/// # Errors
/// Returns an error if the active capture cannot switch targets.
pub fn switch_audio_capture(target: &AudioTarget) -> Result<bool, String> {
    platform::switch_audio_capture(target)
}

/// Reports whether exclusive audio capture is active.
///
/// # Errors
/// Returns an error if capture state cannot be accessed.
pub fn is_audio_capture_active() -> Result<bool, String> {
    Ok(platform::is_audio_capture_active())
}

/// Resolves the best matching audio application for `label`.
///
/// # Errors
/// Returns an error if audio enumeration fails.
pub fn resolve_audio_app_by_name(label: &str) -> Result<Option<AudioApp>, String> {
    let apps = platform::list_audio_applications()?;
    Ok(find_best_audio_match(&apps, label))
}

#[must_use]
pub fn resolve_audio_app_for_captured_window() -> Option<AudioApp> {
    platform::resolve_audio_app_for_captured_window()
}

/// Returns the current capture context.
///
/// # Errors
/// Returns an error if capture introspection fails.
pub fn get_capture_context() -> Result<CaptureContext, String> {
    platform::get_capture_context()
}

/// Starts per-application audio metering.
///
/// # Errors
/// Returns an error if the meter cannot start.
pub fn start_audio_metering() -> Result<bool, String> {
    platform::start_audio_metering()
}

/// Stops per-application audio metering.
///
/// # Errors
/// Returns an error if meter state cannot be accessed.
pub fn stop_audio_metering() -> Result<bool, String> {
    Ok(platform::stop_audio_metering())
}

pub fn set_wave_callback(callback: Box<dyn Fn(Vec<AudioAppWave>) + Send + Sync>) {
    platform::set_wave_callback(callback);
}

pub fn clear_wave_callback() {
    platform::clear_wave_callback();
}

#[derive(Debug, Clone, Copy)]
pub struct AudioRingStats {
    pub captured_chunks: i64,
    pub captured_bytes: i64,
    pub ring_drops: i64,
    pub truncated_bytes: i64,
}

#[must_use]
#[allow(
    clippy::cast_possible_wrap,
    reason = "telemetry counters are far below i64::MAX; the public stats keep the historical i64 field types"
)]
pub fn get_audio_ring_stats() -> AudioRingStats {
    let s = audio_ring::get_audio_ring_stats();
    AudioRingStats {
        captured_chunks: s.captured_chunks as i64,
        captured_bytes: s.captured_bytes as i64,
        ring_drops: s.ring_drops as i64,
        truncated_bytes: s.truncated_bytes as i64,
    }
}

pub fn reset_audio_ring_stats() {
    audio_ring::reset_audio_ring_stats();
}

pub fn set_audio_data_callback(callback: Box<dyn Fn(Vec<i16>) + Send + Sync>) {
    audio_ring::set_audio_data_callback(callback);
}

pub fn clear_audio_data_callback() {
    audio_ring::clear_audio_data_callback();
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
        #[allow(clippy::redundant_clone, reason = "the clone under test")]
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
            highest_serial: Some(999.0),
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
