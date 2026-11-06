//! Settings files in Slopcast's existing configuration directory.
//!
//! Default location: `<config dir>/slopcast` (the `slopcast` application identity): `~/.config/slopcast` on Linux and `%APPDATA%\slopcast` on Windows. A
//! `settings-location.json` marker there can point to a relocated directory. Set
//! `SLOPCAST_SETTINGS_DIR` to use an isolated directory during development and side-by-side tests.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::{Map, Value};

use crate::preferences::DesktopPreferences;
use crate::settings::{StreamSettings, sanitize_stream_settings};

pub const STREAM_SETTINGS_FILE: &str = "stream-settings.json";
pub const ONBOARDING_FILE: &str = "onboarding.json";
pub const PREFERENCES_FILE: &str = "desktop-preferences.json";
pub const LOCATION_FILE: &str = "settings-location.json";
const SETTINGS_FILES: [&str; 3] = [STREAM_SETTINGS_FILE, ONBOARDING_FILE, PREFERENCES_FILE];

pub struct SettingsStorage {
    directory: Mutex<PathBuf>,
    default_directory: PathBuf,
}

/// The directory that holds `settings-location.json` and, unless relocated, the settings.
///
/// # Errors
/// Returns an error when the platform has no user configuration directory.
pub fn default_directory() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("SLOPCAST_SETTINGS_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    dirs::config_dir()
        .map(|dir| dir.join("slopcast"))
        .ok_or_else(|| "No user configuration directory is available".into())
}

/// Writes JSON atomically: a synced temporary file in the same directory, then a rename.
///
/// # Errors
/// Returns an error if the file cannot be written or replaced.
pub fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("Settings file has no parent directory")?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let mut pending = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    pending
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    pending
        .write_all(b"\n")
        .map_err(|error| error.to_string())?;
    pending
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    pending
        .persist(path)
        .map_err(|error| format!("Cannot save settings: {error}"))?;
    Ok(())
}

fn read_object(path: &Path) -> Result<Map<String, Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) => Ok(Map::new()),
            Err(error) => {
                log::warn!(
                    "Failed to parse {}, using defaults: {error}",
                    path.display()
                );
                Ok(Map::new())
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(format!("Cannot read {}: {error}", path.display())),
    }
}

impl SettingsStorage {
    /// Opens the settings directory recorded under `default_directory`.
    ///
    /// # Errors
    /// Returns an error if the location marker is invalid or the directory cannot be created.
    pub fn open(default_directory: PathBuf) -> Result<Self, String> {
        let marker = default_directory.join(LOCATION_FILE);
        let directory = match std::fs::read(&marker) {
            Ok(bytes) => serde_json::from_slice::<PathBuf>(&bytes)
                .map_err(|error| format!("Invalid settings location: {error}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => default_directory.clone(),
            Err(error) => return Err(format!("Cannot read settings location: {error}")),
        };
        if !directory.is_absolute() {
            return Err("Settings location must be an absolute directory".into());
        }
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("Cannot open settings directory: {error}"))?;
        Ok(Self {
            directory: Mutex::new(directory),
            default_directory,
        })
    }

    /// # Errors
    /// See [`SettingsStorage::open`] and [`default_directory`].
    pub fn open_default() -> Result<Self, String> {
        Self::open(default_directory()?)
    }

    fn directory(&self) -> Result<MutexGuard<'_, PathBuf>, String> {
        self.directory
            .lock()
            .map_err(|error| format!("Settings storage lock poisoned: {error}"))
    }

    /// # Errors
    /// Returns an error if the storage lock is poisoned.
    pub fn current_directory(&self) -> Result<PathBuf, String> {
        Ok(self.directory()?.clone())
    }

    /// Reads stream settings; unreadable values fall back to defaults field by field.
    ///
    /// # Errors
    /// Returns an error if the storage lock is poisoned.
    pub fn read_stream_settings(&self) -> Result<StreamSettings, String> {
        let directory = self.directory()?;
        let raw = read_object(&directory.join(STREAM_SETTINGS_FILE)).unwrap_or_else(|error| {
            log::warn!("{error}; using default stream settings");
            Map::new()
        });
        Ok(sanitize_stream_settings(&Value::Object(raw)))
    }

    /// Merges changed fields into the saved stream settings, under the storage lock.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or written.
    pub fn save_stream_settings(
        &self,
        changes: Map<String, Value>,
    ) -> Result<StreamSettings, String> {
        let directory = self.directory()?;
        let path = directory.join(STREAM_SETTINGS_FILE);
        let mut merged = read_object(&path)?;
        merged.extend(changes);
        let sanitized = sanitize_stream_settings(&Value::Object(merged));
        std::fs::create_dir_all(directory.as_path())
            .map_err(|error| format!("Cannot create stream settings directory: {error}"))?;
        write_json(&path, &sanitized)?;
        Ok(sanitized)
    }

    #[must_use]
    pub fn onboarding_completed(&self) -> bool {
        let Ok(directory) = self.directory() else {
            return false;
        };
        std::fs::read_to_string(directory.join(ONBOARDING_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|v| v.get("completed").and_then(Value::as_bool))
            .unwrap_or(false)
    }

    /// # Errors
    /// Returns an error if the onboarding file cannot be written.
    pub fn set_onboarding_completed(&self) -> Result<(), String> {
        let directory = self.directory()?;
        write_json(
            &directory.join(ONBOARDING_FILE),
            &serde_json::json!({ "completed": true }),
        )
    }

    /// # Errors
    /// Returns an error if the preferences file exists but is unreadable or invalid.
    pub fn read_preferences(&self) -> Result<DesktopPreferences, String> {
        let directory = self.directory()?;
        Self::read_preferences_in(&directory)
    }

    fn read_preferences_in(directory: &Path) -> Result<DesktopPreferences, String> {
        let bytes = match std::fs::read(directory.join(PREFERENCES_FILE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DesktopPreferences::default());
            }
            Err(error) => return Err(format!("Cannot read desktop preferences: {error}")),
        };
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid desktop preferences: {error}"))
    }

    /// Reads, edits and writes the preferences while holding the storage lock.
    ///
    /// # Errors
    /// Returns an error from reading, the edit itself, or writing.
    pub fn update_preferences(
        &self,
        edit: impl FnOnce(&mut DesktopPreferences) -> Result<(), String>,
    ) -> Result<DesktopPreferences, String> {
        let directory = self.directory()?;
        let mut preferences = Self::read_preferences_in(&directory)?;
        edit(&mut preferences)?;
        write_json(&directory.join(PREFERENCES_FILE), &preferences)?;
        Ok(preferences)
    }

    /// Copies the settings into an empty `target` directory and records it as the new location.
    ///
    /// # Errors
    /// Returns an error, after removing partial copies, if the move cannot complete.
    pub fn move_to(&self, target: &Path) -> Result<PathBuf, String> {
        if !target.is_absolute() {
            return Err("Enter an absolute folder path.".into());
        }
        std::fs::create_dir_all(target).map_err(|error| error.to_string())?;
        let target = target.canonicalize().map_err(|error| error.to_string())?;
        let mut directory = self.directory()?;
        if directory
            .canonicalize()
            .map_err(|error| error.to_string())?
            == target
        {
            return Ok(target);
        }
        if SETTINGS_FILES.iter().any(|name| target.join(name).exists()) {
            return Err("This folder already contains Slopcast settings. Choose an empty folder to avoid overwriting them.".into());
        }

        let mut copied = Vec::new();
        let mut transfer = || -> Result<(), String> {
            for name in SETTINGS_FILES {
                let bytes = match std::fs::read(directory.join(name)) {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.to_string()),
                };
                let destination = target.join(name);
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                    .map_err(|error| error.to_string())?;
                copied.push(destination);
                file.write_all(&bytes).map_err(|error| error.to_string())?;
                file.sync_all().map_err(|error| error.to_string())?;
            }
            std::fs::create_dir_all(&self.default_directory).map_err(|error| error.to_string())?;
            write_json(&self.default_directory.join(LOCATION_FILE), &target)
        };
        if let Err(error) = transfer() {
            for file in copied {
                if let Err(cleanup) = std::fs::remove_file(file) {
                    log::error!("Cannot remove incomplete settings copy: {cleanup}");
                }
            }
            return Err(format!("Settings were not moved: {error}"));
        }
        directory.clone_from(&target);
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"))
    }

    #[test]
    fn reads_existing_files_in_place_and_tolerates_bad_ones() {
        let dir = temp();
        std::fs::write(
            dir.path().join(STREAM_SETTINGS_FILE),
            r#"{"fps":30,"videoCodec":"theora"}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(dir.path().join(ONBOARDING_FILE), r#"{"completed":true}"#)
            .unwrap_or_else(|e| panic!("{e}"));
        let storage = SettingsStorage::open(dir.path().into()).unwrap_or_else(|e| panic!("{e}"));
        let settings = storage.read_stream_settings().unwrap_or_default();
        assert!((settings.fps - 30.0).abs() < f64::EPSILON);
        assert_eq!(settings.video_codec, "vp8");
        assert!(storage.onboarding_completed());

        std::fs::write(dir.path().join(STREAM_SETTINGS_FILE), "{not json")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            storage.read_stream_settings().unwrap_or_default(),
            StreamSettings::default()
        );
        std::fs::write(dir.path().join(PREFERENCES_FILE), r#"{"unknown":true}"#)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(storage.read_preferences().is_err());
    }

    #[test]
    fn saves_only_changed_fields_over_the_file() {
        let dir = temp();
        let storage = SettingsStorage::open(dir.path().into()).unwrap_or_else(|e| panic!("{e}"));
        let mut first = Map::new();
        first.insert("fps".into(), 30.into());
        storage
            .save_stream_settings(first)
            .unwrap_or_else(|e| panic!("{e}"));
        let mut second = Map::new();
        second.insert("resolution".into(), "720p".into());
        let saved = storage
            .save_stream_settings(second)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!((saved.fps - 30.0).abs() < f64::EPSILON);
        assert_eq!(saved.resolution, "720p");
    }

    #[test]
    fn relocation_refuses_occupied_folders_and_follows_the_marker() {
        let defaults = temp();
        let target = temp();
        let occupied = temp();
        std::fs::write(occupied.path().join(PREFERENCES_FILE), "{}")
            .unwrap_or_else(|e| panic!("{e}"));
        let storage =
            SettingsStorage::open(defaults.path().into()).unwrap_or_else(|e| panic!("{e}"));
        storage
            .update_preferences(|p| {
                p.tray_enabled = false;
                Ok(())
            })
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(storage.move_to(occupied.path()).is_err());
        assert!(storage.move_to(Path::new("relative/dir")).is_err());
        let moved = storage
            .move_to(target.path())
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(storage.current_directory().ok(), Some(moved.clone()));
        assert!(!storage.read_preferences().unwrap_or_default().tray_enabled);

        let reopened =
            SettingsStorage::open(defaults.path().into()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(reopened.current_directory().ok(), Some(moved));
        assert!(!reopened.read_preferences().unwrap_or_default().tray_enabled);
    }

    #[test]
    fn unavailable_relative_marker_is_an_error() {
        let defaults = temp();
        std::fs::write(defaults.path().join(LOCATION_FILE), r#""relative/path""#)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(SettingsStorage::open(defaults.path().into()).is_err());
    }
}
