//! Application icons for the audio picker, resolved from the desktop's own metadata.
//!
//! On Linux an audio stream only names its process, so the icon is found the way a launcher
//! would: match the process (executable, name, Steam app id) to a `.desktop` entry, take its
//! `Icon=`, and look that name up in the current icon theme. Results are cached per process.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::audio::AudioApp;

/// The icon file for an audio application, if one can be found.
#[must_use]
pub fn resolve(app: &AudioApp) -> Option<Arc<Path>> {
    #[cfg(target_os = "linux")]
    {
        linux::resolve(app)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{Arc, AudioApp, Path, PathBuf};
    use std::collections::HashMap;
    use std::fs;
    use std::sync::{Mutex, OnceLock, PoisonError};
    use std::time::{Duration, Instant};

    /// The size the picker shows icons at, doubled for HiDPI displays.
    const TARGET_SIZE: u32 = 48;
    /// How long a miss may keep a stale desktop-entry index before it is rebuilt.
    const INDEX_REFRESH: Duration = Duration::from_secs(30);
    /// Executables that host other programs, so their names say nothing about the app.
    const HOST_BINARIES: [&str; 12] = [
        "wine-preloader",
        "wine64-preloader",
        "wine",
        "wine64",
        "python",
        "python3",
        "java",
        "node",
        "electron",
        "bwrap",
        "sh",
        "bash",
    ];

    #[derive(Default)]
    struct Cache {
        by_process: HashMap<(i32, String), Option<Arc<Path>>>,
        by_icon: HashMap<String, Option<Arc<Path>>>,
        index: Option<(Instant, HashMap<String, String>)>,
    }

    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    static THEME: OnceLock<Vec<ThemeDir>> = OnceLock::new();

    pub(super) fn resolve(app: &AudioApp) -> Option<Arc<Path>> {
        let key = (app.process_id, app.name.clone());
        let cache = CACHE.get_or_init(Mutex::default);
        if let Some(hit) = cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .by_process
            .get(&key)
        {
            return hit.clone();
        }
        let candidates = candidates(app);
        let mut found = lookup(&candidates, false);
        if found.is_none() {
            // A newly installed application may be missing from an old index.
            found = lookup(&candidates, true);
        }
        cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .by_process
            .insert(key, found.clone());
        found
    }

    fn lookup(candidates: &[String], refresh: bool) -> Option<Arc<Path>> {
        let icon_names: Vec<String> = {
            let mut cache = CACHE
                .get_or_init(Mutex::default)
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let stale = cache
                .index
                .as_ref()
                .is_none_or(|(built, _)| refresh && built.elapsed() >= INDEX_REFRESH);
            if stale {
                cache.index = Some((Instant::now(), desktop_index()));
            } else if refresh {
                return None;
            }
            let index = cache.index.as_ref().map(|(_, index)| index)?;
            candidates
                .iter()
                .filter_map(|candidate| index.get(candidate).cloned())
                .chain(candidates.iter().cloned())
                .collect()
        };
        icon_names.iter().find_map(|name| icon_file(name))
    }

    /// Lowercase identifiers that may name the application's desktop entry or icon.
    fn candidates(app: &AudioApp) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |value: &str| {
            let value = value.trim().to_lowercase();
            if !value.is_empty() && !out.contains(&value) {
                out.push(value);
            }
        };
        let pid = app.process_id;
        if pid > 0 {
            let exe = fs::read_link(format!("/proc/{pid}/exe")).ok();
            let exe_name = exe.as_deref().and_then(Path::file_name).map(|name| {
                name.to_string_lossy()
                    .trim_end_matches(" (deleted)")
                    .to_string()
            });
            if let Some(name) = exe_name.filter(|name| !is_host(name)) {
                push(&name);
            }
            if let Ok(comm) = fs::read_to_string(format!("/proc/{pid}/comm"))
                && !is_host(comm.trim())
                && !comm.trim().ends_with(".exe")
            {
                push(&comm);
            }
        }
        push(&app.name);
        push(&app.name.replace(' ', "-"));
        if let Some(bundle) = &app.bundle_id {
            push(bundle);
        }
        if pid > 0 {
            for id in cgroup_app_ids(pid) {
                push(&id);
            }
        }
        out
    }

    fn is_host(name: &str) -> bool {
        let name = name.to_lowercase();
        HOST_BINARIES.contains(&name.as_str())
    }

    /// App ids from the process's systemd scope: `app-<launcher>-<id>-<pid>.scope`,
    /// `app-flatpak-<id>-<n>.scope` or `app-<id>@<n>.service`. Steam games map to the
    /// `steam_icon_<appid>` icons Steam installs.
    fn cgroup_app_ids(pid: i32) -> Vec<String> {
        let Ok(text) = fs::read_to_string(format!("/proc/{pid}/cgroup")) else {
            return Vec::new();
        };
        let Some(unit) = text
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .and_then(|path| path.rsplit('/').next())
        else {
            return Vec::new();
        };
        let unit = unescape_unit(unit);
        let Some(body) = unit
            .strip_suffix(".scope")
            .or_else(|| unit.strip_suffix(".service"))
            .and_then(|body| body.strip_prefix("app-"))
        else {
            return Vec::new();
        };
        let body = body.split('@').next().unwrap_or(body);
        let body = body
            .rsplit_once('-')
            .filter(|(_, tail)| tail.chars().all(|c| c.is_ascii_digit()))
            .map_or(body, |(head, _)| head);
        if let Some(app_id) = body.strip_prefix("steam-app") {
            return vec![format!("steam_icon_{app_id}")];
        }
        let mut ids = vec![body.to_string()];
        if let Some((_, id)) = body.split_once('-') {
            ids.push(id.to_string());
        }
        ids
    }

    fn unescape_unit(unit: &str) -> String {
        let mut out = String::with_capacity(unit.len());
        let mut rest = unit;
        while let Some(index) = rest.find("\\x") {
            out.push_str(&rest[..index]);
            let hex = rest.get(index + 2..index + 4);
            match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                Some(byte) => {
                    out.push(char::from(byte));
                    rest = &rest[index + 4..];
                }
                None => {
                    out.push_str("\\x");
                    rest = &rest[index + 2..];
                }
            }
        }
        out.push_str(rest);
        out
    }

    fn data_home() -> Option<PathBuf> {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
    }

    fn data_dirs() -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = data_home().into_iter().collect();
        let system = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        dirs.extend(
            system
                .split(':')
                .filter(|d| !d.is_empty())
                .map(PathBuf::from),
        );
        for extra in ["/var/lib/flatpak/exports/share", "/usr/share"] {
            let extra = PathBuf::from(extra);
            if !dirs.contains(&extra) {
                dirs.push(extra);
            }
        }
        if let Some(home) = dirs::home_dir() {
            let flatpak = home.join(".local/share/flatpak/exports/share");
            if !dirs.contains(&flatpak) {
                dirs.push(flatpak);
            }
        }
        dirs
    }

    /// Maps lowercase desktop ids, executables, window classes and names to `Icon=` values.
    /// Earlier data directories take precedence, as in the desktop entry specification.
    fn desktop_index() -> HashMap<String, String> {
        let mut index = HashMap::new();
        for dir in data_dirs() {
            let Ok(entries) = fs::read_dir(dir.join("applications")) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "desktop") {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let Some(fields) = desktop_fields(&text) else {
                    continue;
                };
                let Some(icon) = fields.icon else { continue };
                // Self-updating apps leave entries pointing at icons of removed versions.
                if icon.starts_with('/') && !Path::new(&icon).is_file() {
                    continue;
                }
                let id = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                let mut keys = vec![id.clone()];
                if let Some((_, last)) = id.rsplit_once('.') {
                    keys.push(last.to_string());
                }
                keys.extend(fields.exec);
                keys.extend(fields.wm_class);
                keys.extend(fields.name);
                for key in keys.into_iter().filter(|key| !key.is_empty()) {
                    index.entry(key).or_insert_with(|| icon.clone());
                }
            }
        }
        index
    }

    struct DesktopFields {
        icon: Option<String>,
        exec: Option<String>,
        wm_class: Option<String>,
        name: Option<String>,
    }

    fn desktop_fields(text: &str) -> Option<DesktopFields> {
        let mut in_entry = false;
        let mut fields = DesktopFields {
            icon: None,
            exec: None,
            wm_class: None,
            name: None,
        };
        let mut seen_entry = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                seen_entry |= in_entry;
                continue;
            }
            if !in_entry {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "Icon" => fields.icon = Some(value.to_string()),
                "Exec" => fields.exec = exec_name(value),
                "StartupWMClass" => fields.wm_class = Some(value.to_lowercase()),
                "Name" => fields.name = Some(value.to_lowercase()),
                _ => {}
            }
        }
        seen_entry.then_some(fields)
    }

    /// The program an `Exec=` line runs, skipping `env` and flatpak wrappers.
    fn exec_name(exec: &str) -> Option<String> {
        let mut words = exec.split_whitespace().map(|word| word.trim_matches('"'));
        let mut program = words.next()?;
        if Path::new(program)
            .file_name()
            .is_some_and(|name| name == "env")
        {
            program = words.find(|word| !word.contains('=') && !word.starts_with('-'))?;
        }
        let name = Path::new(program)
            .file_name()?
            .to_string_lossy()
            .to_lowercase();
        (name != "flatpak").then_some(name)
    }

    fn icon_file(name: &str) -> Option<Arc<Path>> {
        if name.starts_with('/') {
            let path = Path::new(name);
            return supported(path).then(|| Arc::from(path));
        }
        let cache = CACHE.get_or_init(Mutex::default);
        if let Some(hit) = cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .by_icon
            .get(name)
        {
            return hit.clone();
        }
        let found = find_in_theme(name).map(Arc::from);
        cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .by_icon
            .insert(name.to_string(), found.clone());
        found
    }

    fn supported(path: &Path) -> bool {
        path.is_file()
            && path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("png") || ext.eq_ignore_ascii_case("svg")
            })
    }

    fn find_in_theme(name: &str) -> Option<PathBuf> {
        let dirs = THEME.get_or_init(theme_dirs);
        for dir in dirs {
            for ext in ["svg", "png"] {
                let path = dir.path.join(format!("{name}.{ext}"));
                if path.is_file() {
                    return Some(path);
                }
            }
        }
        data_dirs().into_iter().find_map(|dir| {
            ["png", "svg"]
                .iter()
                .map(|ext| dir.join("pixmaps").join(format!("{name}.{ext}")))
                .find(|path| path.is_file())
        })
    }

    struct ThemeDir {
        path: PathBuf,
    }

    /// Every application-icon directory of the current theme and its parents, best size first
    /// within each theme.
    fn theme_dirs() -> Vec<ThemeDir> {
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(home) = data_home() {
            roots.push(home.join("icons"));
        }
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join(".icons"));
        }
        for root in data_dirs().into_iter().map(|dir| dir.join("icons")) {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }

        let mut chain: Vec<String> = Vec::new();
        let mut queue: std::collections::VecDeque<String> = theme_name()
            .into_iter()
            .chain(["hicolor".to_string()])
            .collect();
        let mut out = Vec::new();
        while let Some(theme) = queue.pop_front() {
            if chain.contains(&theme) {
                continue;
            }
            let Some(index) = roots
                .iter()
                .find_map(|root| fs::read_to_string(root.join(&theme).join("index.theme")).ok())
            else {
                chain.push(theme);
                continue;
            };
            let (mut subdirs, inherits) = parse_index(&index);
            subdirs.sort_by_key(|dir| dir.1);
            for (subdir, _) in subdirs {
                for root in &roots {
                    let path = root.join(&theme).join(&subdir);
                    if path.is_dir() {
                        out.push(ThemeDir { path });
                    }
                }
            }
            queue.extend(inherits);
            chain.push(theme);
        }
        out
    }

    /// Application directories with their distance from the target size, and parent themes.
    fn parse_index(text: &str) -> (Vec<(String, u32)>, Vec<String>) {
        let mut inherits = Vec::new();
        let mut sections: Vec<(String, HashMap<String, String>)> = Vec::new();
        for line in text.lines().map(str::trim) {
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                sections.push((name.to_string(), HashMap::new()));
                continue;
            }
            if let (Some((key, value)), Some((_, section))) =
                (line.split_once('='), sections.last_mut())
            {
                section.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
        let mut dirs = Vec::new();
        for (name, keys) in &sections {
            if name == "Icon Theme" {
                inherits = keys
                    .get("Inherits")
                    .map(|list| list.split(',').map(|t| t.trim().to_string()).collect())
                    .unwrap_or_default();
                continue;
            }
            let context = keys.get("Context").map(String::as_str);
            if !matches!(context, Some("Applications" | "Apps")) && !name.contains("apps") {
                continue;
            }
            let number = |key: &str| keys.get(key).and_then(|v| v.parse::<u32>().ok());
            let size = number("Size").unwrap_or(TARGET_SIZE);
            let scale = number("Scale").unwrap_or(1);
            let kind = keys.get("Type").map_or("Threshold", String::as_str);
            let (min, max) = match kind {
                "Scalable" => (
                    number("MinSize").unwrap_or(size),
                    number("MaxSize").unwrap_or(size),
                ),
                "Fixed" => (size, size),
                _ => {
                    let threshold = number("Threshold").unwrap_or(2);
                    (size.saturating_sub(threshold), size + threshold)
                }
            };
            let distance = if (min..=max).contains(&TARGET_SIZE) {
                0
            } else {
                TARGET_SIZE
                    .abs_diff(size)
                    .min(TARGET_SIZE.abs_diff(min))
                    .min(TARGET_SIZE.abs_diff(max))
            };
            // Symbolic icons are monochrome glyphs; prefer them only when nothing else exists.
            let symbolic = if name.contains("symbolic") { 1000 } else { 0 };
            dirs.push((
                name.clone(),
                distance * 4 + scale.saturating_sub(1) + symbolic,
            ));
        }
        (dirs, inherits)
    }

    fn theme_name() -> Option<String> {
        let kde = std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|desktop| desktop.split(':').any(|d| d.eq_ignore_ascii_case("KDE")));
        let from_kde = || {
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| dirs::home_dir().map(|home| home.join(".config")))?;
            let text = fs::read_to_string(config.join("kdeglobals")).ok()?;
            let mut in_icons = false;
            text.lines().map(str::trim).find_map(|line| {
                if line.starts_with('[') {
                    in_icons = line == "[Icons]";
                    return None;
                }
                let (key, value) = line.split_once('=')?;
                (in_icons && key.trim() == "Theme").then(|| value.trim().to_string())
            })
        };
        let from_gnome = || {
            let output = std::process::Command::new("gsettings")
                .args(["get", "org.gnome.desktop.interface", "icon-theme"])
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            let theme = String::from_utf8(output.stdout).ok()?;
            Some(theme.trim().trim_matches('\'').to_string()).filter(|t| !t.is_empty())
        };
        if kde {
            from_kde().or_else(from_gnome)
        } else {
            from_gnome().or_else(from_kde)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{desktop_fields, exec_name, parse_index, unescape_unit};

        #[test]
        fn exec_skips_env_wrappers_and_paths() {
            assert_eq!(
                exec_name("/usr/lib/signal-desktop/signal-desktop %U").as_deref(),
                Some("signal-desktop")
            );
            assert_eq!(
                exec_name("env GDK_BACKEND=x11 \"/opt/Foo/foo\" --bar").as_deref(),
                Some("foo")
            );
            assert_eq!(exec_name("flatpak run org.example.App"), None);
        }

        #[test]
        fn desktop_fields_read_only_the_main_entry() {
            let fields = desktop_fields(
                "[Desktop Entry]\nName=Signal\nExec=signal-desktop\nIcon=signal\nStartupWMClass=Signal\n[Desktop Action new]\nIcon=other\n",
            )
            .unwrap_or_else(|| panic!("entry"));
            assert_eq!(fields.icon.as_deref(), Some("signal"));
            assert_eq!(fields.wm_class.as_deref(), Some("signal"));
            assert_eq!(fields.name.as_deref(), Some("signal"));
        }

        #[test]
        fn units_unescape_systemd_hex() {
            assert_eq!(
                unescape_unit("app-flatpak-org.foo\\x2dbar-12.scope"),
                "app-flatpak-org.foo-bar-12.scope"
            );
        }

        #[test]
        fn index_prefers_target_sized_app_dirs() {
            let (mut dirs, inherits) = parse_index(
                "[Icon Theme]\nInherits=hicolor\nDirectories=16x16/apps,48x48/apps,scalable/apps,48x48/actions\n\
                 [16x16/apps]\nSize=16\nContext=Applications\nType=Threshold\n\
                 [48x48/apps]\nSize=48\nContext=Applications\nType=Threshold\n\
                 [scalable/apps]\nSize=128\nMinSize=8\nMaxSize=512\nContext=Applications\nType=Scalable\n\
                 [48x48/actions]\nSize=48\nContext=Actions\n",
            );
            dirs.sort_by_key(|dir| dir.1);
            assert_eq!(inherits, vec!["hicolor".to_string()]);
            let names: Vec<_> = dirs.iter().map(|dir| dir.0.as_str()).collect();
            assert_eq!(names, vec!["48x48/apps", "scalable/apps", "16x16/apps"]);
        }
    }
}
