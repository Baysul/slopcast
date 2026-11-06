//! Audio application listing, grouping and capture targets.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioApp {
    pub id: i32,
    pub name: String,
    pub process_id: i32,
    pub bundle_id: Option<String>,
    pub window_title: Option<String>,
    pub client_id: Option<i32>,
    pub media_title: Option<String>,
}

impl From<native_rust::AudioApp> for AudioApp {
    fn from(app: native_rust::AudioApp) -> Self {
        Self {
            id: app.id,
            name: app.name,
            process_id: app.process_id,
            bundle_id: app.bundle_id,
            window_title: app.window_title,
            client_id: app.client_id,
            media_title: app.media_title,
        }
    }
}

/// The capture target for all system audio.
pub const SYSTEM_AUDIO: i32 = -1;
/// The target id for "no audio": selecting it stops capture.
pub const NO_AUDIO: i32 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioAppGroup {
    pub representative: AudioApp,
    pub members: Vec<AudioApp>,
}

fn identity_key(app: &AudioApp) -> String {
    if let Some(bundle) = app
        .bundle_id
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        return format!("b:{}", bundle.to_lowercase());
    }
    if !app.name.trim().is_empty() {
        return format!("n:{}", app.name.trim().to_lowercase());
    }
    if app.process_id > 0 {
        return format!("p:{}", app.process_id);
    }
    if let Some(client) = app.client_id.filter(|c| *c > 0) {
        return format!("c:{client}");
    }
    format!("i:{}", app.id)
}

fn has_title(title: Option<&String>) -> bool {
    title.is_some_and(|t| !t.is_empty())
}

/// Groups streams by application identity, preferring a representative with a media title.
#[must_use]
pub fn group_audio_apps(apps: &[AudioApp]) -> Vec<AudioAppGroup> {
    let mut groups: Vec<AudioAppGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for app in apps {
        let key = identity_key(app);
        if let Some(&position) = index.get(&key) {
            let group = &mut groups[position];
            group.members.push(app.clone());
            let rep = &group.representative;
            if (!has_title(rep.media_title.as_ref()) && has_title(app.media_title.as_ref()))
                || (!has_title(rep.media_title.as_ref())
                    && !has_title(rep.window_title.as_ref())
                    && has_title(app.window_title.as_ref()))
            {
                group.representative = app.clone();
            }
        } else {
            index.insert(key, groups.len());
            groups.push(AudioAppGroup {
                representative: app.clone(),
                members: vec![app.clone()],
            });
        }
    }
    groups
}

/// Encodes an app as a capture target. On Linux, stream node ids die with the stream,
/// so active streams are targeted by `-processId`, which survives node churn.
#[must_use]
pub fn to_target_id(app: &AudioApp, pid_based_targets: bool) -> i32 {
    if pid_based_targets && app.process_id > 0 {
        -app.process_id
    } else {
        app.id
    }
}

#[must_use]
pub fn selected_target_process_id(selected: Option<i32>) -> Option<i32> {
    selected.filter(|id| *id < -1).map(|id| -id)
}

#[must_use]
pub fn is_group_selected(members: &[AudioApp], selected: Option<i32>) -> bool {
    if let Some(pid) = selected_target_process_id(selected) {
        return members.iter().any(|m| m.process_id == pid);
    }
    selected.is_some_and(|id| members.iter().any(|m| m.id == id))
}

fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    cleaned
        .strip_suffix("exe")
        .map_or(cleaned.clone(), str::to_string)
}

/// The single app whose name matches `query`, if exactly one does.
#[must_use]
pub fn find_best_audio_match(apps: &[AudioApp], query: &str) -> Option<AudioApp> {
    let name = clean_name(query);
    if name.len() < 3 {
        return None;
    }
    let mut candidates = apps.iter().filter(|app| {
        let candidate = clean_name(&app.name);
        app.id != SYSTEM_AUDIO
            && candidate.len() >= 3
            && (name.contains(&candidate) || candidate.contains(&name))
    });
    let first = candidates.next()?;
    candidates.next().is_none().then(|| first.clone())
}

#[must_use]
pub fn app_display_name(app: &AudioApp) -> String {
    if app.id == SYSTEM_AUDIO {
        return "Desktop Audio (All System Sound)".into();
    }
    if app.id == NO_AUDIO {
        return "No Audio".into();
    }
    app.name.clone()
}

#[must_use]
pub fn app_subtitle(app: &AudioApp) -> Option<String> {
    if app.id == SYSTEM_AUDIO {
        return Some("All system audio".into());
    }
    app.media_title
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| app.window_title.clone().filter(|t| !t.is_empty()))
}

/// Suppresses meter updates that do not visibly change any stream's waveform.
#[derive(Default)]
pub struct WaveDeduplicator {
    last: HashMap<i32, CachedWave>,
}

struct CachedWave {
    columns: Vec<f64>,
    seen: bool,
}

const WAVE_EPSILON: f64 = 0.002;

impl WaveDeduplicator {
    /// Returns whether `waves` differs from the last accepted update.
    pub fn update(&mut self, waves: &[native_rust::AudioAppWave]) -> bool {
        let mut changed = false;
        for wave in waves {
            if let Some(previous) = self.last.get_mut(&wave.id) {
                previous.seen = true;
                if previous.columns.len() != wave.columns.len()
                    || previous
                        .columns
                        .iter()
                        .zip(&wave.columns)
                        .any(|(a, b)| (a - b).abs() > WAVE_EPSILON)
                {
                    changed = true;
                }
            } else {
                self.last.insert(
                    wave.id,
                    CachedWave {
                        columns: Vec::new(),
                        seen: true,
                    },
                );
                changed = true;
            }
        }
        let previous_len = self.last.len();
        self.last
            .retain(|_, wave| std::mem::replace(&mut wave.seen, false));
        changed |= self.last.len() != previous_len;
        if changed {
            for wave in waves {
                if let Some(previous) = self.last.get_mut(&wave.id) {
                    previous.columns.clone_from(&wave.columns);
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: i32, name: &str, process_id: i32) -> AudioApp {
        AudioApp {
            id,
            name: name.into(),
            process_id,
            bundle_id: None,
            window_title: None,
            client_id: None,
            media_title: None,
        }
    }

    #[test]
    fn groups_by_bundle_then_name_case_insensitively() {
        let mut spotify = app(1, "Spotify", 100);
        spotify.bundle_id = Some("com.spotify.client".into());
        let mut spotify2 = app(2, "Spotify", 200);
        spotify2.bundle_id = Some("com.spotify.client".into());
        let groups = group_audio_apps(&[spotify, spotify2, app(3, "VLC", 0)]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].members.len(), 2);
        assert_eq!(
            group_audio_apps(&[app(1, "  Spotify ", 0), app(2, "spotify", 0)]).len(),
            1
        );
    }

    #[test]
    fn prefers_a_titled_representative() {
        let mut titled = app(2, "Firefox", 100);
        titled.media_title = Some("Song".into());
        let groups = group_audio_apps(&[app(1, "Firefox", 100), titled]);
        assert_eq!(groups[0].representative.id, 2);
    }

    #[test]
    fn targets() {
        assert_eq!(to_target_id(&app(7, "x", 42), true), -42);
        assert_eq!(to_target_id(&app(7, "x", 42), false), 7);
        assert_eq!(to_target_id(&app(SYSTEM_AUDIO, "x", 0), true), SYSTEM_AUDIO);
        assert_eq!(selected_target_process_id(Some(-42)), Some(42));
        assert_eq!(selected_target_process_id(Some(-1)), None);
        assert!(is_group_selected(&[app(7, "x", 42)], Some(-42)));
        assert!(is_group_selected(&[app(-1, "x", 0)], Some(-1)));
        assert!(!is_group_selected(&[app(7, "x", 42)], None));
    }

    #[test]
    fn best_match_requires_a_unique_candidate() {
        let apps = [
            app(1, "firefox.exe", 1),
            app(2, "Spotify", 2),
            app(-1, "System", 0),
        ];
        assert_eq!(
            find_best_audio_match(&apps, "Firefox").map(|a| a.id),
            Some(1)
        );
        assert_eq!(find_best_audio_match(&apps, "fi"), None);
        let twins = [app(1, "Spotify", 1), app(2, "Spotify Helper", 2)];
        assert_eq!(find_best_audio_match(&twins, "Spotify"), None);
    }

    fn wave(id: i32, level: f64) -> native_rust::AudioAppWave {
        native_rust::AudioAppWave {
            id,
            columns: vec![level; 192],
        }
    }

    #[test]
    fn wave_dedup_tracks_removal_and_small_changes() {
        let mut dedup = WaveDeduplicator::default();
        assert!(dedup.update(&[wave(1, 0.5), wave(2, 0.25)]));
        assert!(!dedup.update(&[wave(1, 0.5), wave(2, 0.25)]));
        assert!(dedup.update(&[wave(2, 0.25)]));
        assert!(!dedup.update(&[wave(2, 0.25)]));
        assert!(dedup.update(&[]));
        assert!(!dedup.update(&[]));
        assert!(dedup.update(&[wave(1, 0.5)]));
        assert!(!dedup.update(&[wave(1, 0.501)]));
        assert!(dedup.update(&[wave(1, 0.503)]));
        for id in 0..100 {
            dedup.update(&[wave(id, 0.5)]);
        }
        assert_eq!(dedup.last.len(), 1);
    }
}
