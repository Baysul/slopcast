use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::AppHandle;
use tauri::{Emitter, Manager};

const PREFERENCES_FILE: &str = "desktop-preferences.json";
const LOCATION_FILE: &str = "settings-location.json";
const SETTINGS_FILES: [&str; 3] = ["stream-settings.json", "onboarding.json", PREFERENCES_FILE];
pub(crate) const ACTIONS: [(&str, &str, &str); 5] = [
    ("start-share", "Start screenshare", "Control+Shift+F7"),
    ("stop-share", "Stop screenshare", "Control+Shift+F8"),
    ("close-room", "Close room", "Control+Shift+F9"),
    (
        "switch-source",
        "Switch screenshare source",
        "Control+Shift+F10",
    ),
    ("toggle-mute", "Toggle audio mute", "Control+Shift+F11"),
];

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct DesktopPreferences {
    pub tray_enabled: bool,
    pub auto_select_audio: bool,
    pub shortcuts: BTreeMap<String, String>,
    pub telemetry_shortcut: String,
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            tray_enabled: true,
            auto_select_audio: true,
            telemetry_shortcut: if cfg!(target_os = "macos") {
                "Command+KeyT"
            } else {
                "Control+KeyT"
            }
            .into(),
            shortcuts: ACTIONS
                .iter()
                .map(|(action, _, chord)| ((*action).into(), (*chord).into()))
                .collect(),
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreferencesSnapshot {
    pub preferences: DesktopPreferences,
    pub settings_directory: String,
}

pub(crate) struct SettingsStorage {
    directory: Mutex<PathBuf>,
    default_directory: PathBuf,
}

impl SettingsStorage {
    pub(crate) fn load(app: &AppHandle) -> Result<Self, String> {
        let default_directory = app
            .path()
            .app_config_dir()
            .map_err(|error| error.to_string())?;
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

    pub(crate) fn directory(&self) -> Result<MutexGuard<'_, PathBuf>, String> {
        self.directory
            .lock()
            .map_err(|error| format!("Settings storage lock poisoned: {error}"))
    }
}

pub(crate) fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
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

pub(crate) fn read_preferences(app: &AppHandle) -> Result<DesktopPreferences, String> {
    let storage = app.state::<SettingsStorage>();
    let directory = storage.directory()?;
    let bytes = match std::fs::read(directory.join(PREFERENCES_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DesktopPreferences::default());
        }
        Err(error) => return Err(format!("Cannot read desktop preferences: {error}")),
    };

    serde_json::from_slice(&bytes).map_err(|error| format!("Invalid desktop preferences: {error}"))
}

pub(crate) fn persist_preferences(
    app: &AppHandle,
    preferences: &DesktopPreferences,
) -> Result<(), String> {
    let storage = app.state::<SettingsStorage>();
    let directory = storage.directory()?;

    write_json(&directory.join(PREFERENCES_FILE), preferences)
}

#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command injection requires an owned AppHandle"
)]
pub(crate) fn get_desktop_preferences(app: AppHandle) -> Result<PreferencesSnapshot, String> {
    let preferences = read_preferences(&app)?;
    let storage = app.state::<SettingsStorage>();
    let directory = storage.directory()?;

    Ok(PreferencesSnapshot {
        preferences,
        settings_directory: directory.to_string_lossy().into_owned(),
    })
}

#[tauri::command(rename_all = "camelCase")]
pub(crate) fn save_general_preferences(
    app: AppHandle,
    tray_enabled: bool,
    auto_select_audio: bool,
) -> Result<PreferencesSnapshot, String> {
    let controls = app.state::<crate::controls::DesktopControls>();
    let operation = controls
        .preferences_lock
        .lock()
        .map_err(|error| error.to_string())?;
    let mut preferences = read_preferences(&app)?;
    let old_tray_enabled = preferences.tray_enabled;
    preferences.tray_enabled = tray_enabled;
    preferences.auto_select_audio = auto_select_audio;

    crate::controls::set_tray_enabled(&app, tray_enabled)?;
    if let Err(error) = persist_preferences(&app, &preferences) {
        if let Err(rollback) = crate::controls::set_tray_enabled(&app, old_tray_enabled) {
            log::error!("Tray rollback failed: {rollback}");
        }
        return Err(error);
    }
    app.emit("desktop-preferences-changed", &preferences)
        .map_err(|error| error.to_string())?;
    drop(operation);

    get_desktop_preferences(app)
}

#[tauri::command]
pub(crate) fn save_telemetry_shortcut(
    app: AppHandle,
    chord: Option<String>,
) -> Result<PreferencesSnapshot, String> {
    let controls = app.state::<crate::controls::DesktopControls>();
    let operation = controls
        .preferences_lock
        .lock()
        .map_err(|error| error.to_string())?;
    let mut preferences = read_preferences(&app)?;
    let chord = chord.unwrap_or_else(|| DesktopPreferences::default().telemetry_shortcut);

    crate::shortcuts::validate_telemetry_shortcut(&app, &chord, &preferences.shortcuts)?;
    preferences.telemetry_shortcut = chord;
    persist_preferences(&app, &preferences)?;
    app.emit("desktop-preferences-changed", &preferences)
        .map_err(|error| error.to_string())?;
    drop(operation);

    get_desktop_preferences(app)
}

#[tauri::command]
pub(crate) async fn choose_settings_directory(
    app: AppHandle,
) -> Result<PreferencesSnapshot, String> {
    let selected = rfd::AsyncFileDialog::new()
        .set_title("Choose an empty folder for Slopcast settings")
        .pick_folder()
        .await;
    let Some(selected) = selected else {
        return get_desktop_preferences(app);
    };
    let target = selected
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;

    migrate_settings(&app, target)?;
    app.emit("settings-directory-changed", ())
        .map_err(|error| error.to_string())?;

    get_desktop_preferences(app)
}

fn migrate_settings(app: &AppHandle, target: PathBuf) -> Result<(), String> {
    let storage = app.state::<SettingsStorage>();
    let mut directory = storage.directory()?;
    if directory
        .canonicalize()
        .map_err(|error| error.to_string())?
        == target
    {
        return Ok(());
    }
    if SETTINGS_FILES.iter().any(|name| target.join(name).exists()) {
        return Err("This folder already contains Slopcast settings. Choose an empty folder to avoid overwriting them.".into());
    }

    let mut copied = Vec::new();
    let transfer = || -> Result<(), String> {
        for name in SETTINGS_FILES {
            let source = directory.join(name);
            let bytes = match std::fs::read(source) {
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
        std::fs::create_dir_all(&storage.default_directory).map_err(|error| error.to_string())?;
        write_json(&storage.default_directory.join(LOCATION_FILE), &target)
    };
    let mut transfer = transfer;
    if let Err(error) = transfer() {
        for file in copied {
            if let Err(cleanup) = std::fs::remove_file(file) {
                log::error!("Cannot remove incomplete settings copy: {cleanup}");
            }
        }
        return Err(format!("Settings were not moved: {error}"));
    }
    *directory = target;

    Ok(())
}
