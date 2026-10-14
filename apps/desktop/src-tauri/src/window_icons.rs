//! Window-button glyphs taken from the desktop's icon theme.
//!
//! Every freedesktop icon theme names these four icons, and each desktop's own titlebar uses them
//! (Breeze's chevrons and diamond, Adwaita's dash and squares), so reading them keeps the custom
//! titlebar visually in step with whatever theme the user runs.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(serde::Serialize)]
pub(crate) struct WindowButtonIcons {
    minimize: Option<String>,
    maximize: Option<String>,
    restore: Option<String>,
    close: Option<String>,
}

/// Directories of the icon themes that hold these actions, in preference order.
const ICON_DIRECTORIES: [&str; 8] = [
    "actions/16",
    "actions/symbolic",
    "symbolic/ui",
    "symbolic/actions",
    "scalable/actions",
    "actions/scalable",
    "actions/22",
    "actions/24",
];

fn ini_value(path: &Path, section: &str, key: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let header = format!("[{section}]");
    let mut in_section = false;

    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_section = line == header;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((name, value)) = line.split_once('=')
            && name.trim() == key
        {
            return Some(value.trim().to_owned());
        }
    }

    None
}

fn home_relative(variable: &str, fallback: &str) -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(directory));
    }

    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback))
}

fn icon_theme_name() -> Option<String> {
    let kdeglobals = home_relative("XDG_CONFIG_HOME", ".config")?.join("kdeglobals");
    if let Some(theme) = ini_value(&kdeglobals, "Icons", "Theme") {
        return Some(theme);
    }

    let output = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()?;
    let theme = String::from_utf8(output.stdout).ok()?;

    Some(theme.trim().trim_matches('\'').to_owned()).filter(|theme| !theme.is_empty())
}

fn icon_search_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(data_home) = home_relative("XDG_DATA_HOME", ".local/share") {
        directories.push(data_home.join("icons"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        directories.push(PathBuf::from(home).join(".icons"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    directories.extend(
        data_dirs
            .split(':')
            .map(|directory| Path::new(directory).join("icons")),
    );

    directories
}

/// The theme followed by everything it inherits, ending in the spec's `hicolor` fallback.
fn theme_chain(theme: String, search: &[PathBuf]) -> Vec<String> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([theme, "hicolor".to_owned()]);

    while let Some(name) = queue.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let inherits = search.iter().find_map(|root| {
            ini_value(
                &root.join(&name).join("index.theme"),
                "Icon Theme",
                "Inherits",
            )
        });
        queue.extend(
            inherits
                .iter()
                .flat_map(|list| list.split(','))
                .map(|parent| parent.trim().to_owned()),
        );
        chain.push(name);
    }

    chain
}

fn find_icon(name: &str, chain: &[String], search: &[PathBuf]) -> Option<String> {
    let file_names = [format!("{name}-symbolic.svg"), format!("{name}.svg")];

    for theme in chain {
        for root in search {
            for directory in ICON_DIRECTORIES {
                for file_name in &file_names {
                    let path = root.join(theme).join(directory).join(file_name);
                    if let Ok(svg) = fs::read_to_string(path) {
                        return Some(svg);
                    }
                }
            }
        }
    }

    None
}

#[tauri::command]
pub(crate) fn get_window_button_icons() -> WindowButtonIcons {
    let search = icon_search_directories();
    let chain = icon_theme_name().map(|theme| theme_chain(theme, &search));
    let lookup = |name: &str| {
        chain
            .as_deref()
            .and_then(|chain| find_icon(name, chain, &search))
    };

    WindowButtonIcons {
        minimize: lookup("window-minimize"),
        maximize: lookup("window-maximize"),
        restore: lookup("window-restore"),
        close: lookup("window-close"),
    }
}
