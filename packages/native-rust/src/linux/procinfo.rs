use pipewire::spa::utils::dict::DictRef;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(super) struct ProcEntry {
    pid: i32,
    comm: String,
    cmdline: String,
}

// iter_proc scans /proc for every process (2 file reads each) and is hit on
// every 3 s renderer poll plus each capture session start. Cache the scan for
// a second; a freshly launched app being invisible for <1 s is irrelevant to
// both callers.
const PROC_CACHE_TTL: Duration = Duration::from_millis(1000);
static PROC_CACHE: Mutex<Option<(Instant, Vec<ProcEntry>)>> = Mutex::new(None);

pub(super) fn iter_proc() -> Vec<ProcEntry> {
    let now = Instant::now();
    let Ok(mut cache) = PROC_CACHE.lock() else {
        return Vec::new();
    };
    if let Some((at, entries)) = cache.as_ref() {
        if now.duration_since(*at) < PROC_CACHE_TTL {
            return entries.clone();
        }
    }
    let entries = scan_proc();
    *cache = Some((now, entries.clone()));
    entries
}

fn scan_proc() -> Vec<ProcEntry> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| {
            let pid: i32 = e.file_name().to_str()?.parse().ok()?;
            if pid <= 0 {
                return None;
            }
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            let cmdline =
                std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            Some(ProcEntry {
                pid,
                comm: comm.trim().into(),
                cmdline,
            })
        })
        .collect()
}

fn is_system_or_session_daemon(pid: u32) -> bool {
    if pid <= 1 {
        return true;
    }
    let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else {
        return true;
    };
    let name = comm.trim().to_lowercase();
    matches!(
        name.as_str(),
        "systemd"
            | "systemd-executor"
            | "init"
            | "dbus-daemon"
            | "dbus-broker"
            | "pipewire"
            | "pipewire-pulse"
            | "wireplumber"
            | "gnome-session"
            | "gnome-session-b"
            | "gnome-shell"
            | "plasmashell"
            | "kwin_wayland"
            | "kwin_x11"
            | "xdg-desktop-por"
            | "bash"
            | "zsh"
            | "sh"
            | "fish"
            | "tmux"
            | "screen"
    )
}

fn get_parent_pid(pid: u32) -> Option<u32> {
    if pid == 0 || is_system_or_session_daemon(pid) {
        return None;
    }
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("PPid:") {
            return rest.trim().parse::<u32>().ok();
        }
    }
    None
}

fn get_ancestor_pids(pid: u32) -> Vec<u32> {
    let mut ancestors = Vec::with_capacity(8);
    let mut current = pid;
    for _ in 0..16 {
        if current <= 1 || is_system_or_session_daemon(current) {
            break;
        }
        ancestors.push(current);
        let Some(ppid) = get_parent_pid(current) else {
            break;
        };
        if ppid == current || ppid <= 1 || is_system_or_session_daemon(ppid) {
            break;
        }
        current = ppid;
    }
    ancestors
}

pub(super) fn are_processes_related(pid_a: u32, pid_b: u32) -> bool {
    if pid_a <= 1
        || pid_b <= 1
        || is_system_or_session_daemon(pid_a)
        || is_system_or_session_daemon(pid_b)
    {
        return false;
    }
    if pid_a == pid_b {
        return true;
    }
    let ancestors_a = get_ancestor_pids(pid_a);
    let ancestors_b = get_ancestor_pids(pid_b);
    ancestors_a.iter().any(|a| ancestors_b.contains(a))
}

pub(super) fn is_generic_launcher(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    let norm = lower.replace('\\', "/");
    let stem = std::path::Path::new(&norm)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&norm)
        .trim_end_matches(".exe");
    matches!(
        stem,
        "steam"
            | "steamwebhelper"
            | "wine"
            | "wine64"
            | "wine64-preloader"
            | "wineserver"
            | "pv-bwrap"
            | "pressure-vessel"
            | "reaper"
            | "gamemoded"
            | "explorer"
            | "services"
            | "plugplay"
            | "winedevice"
            | "svchost"
            | "kwin_wayland"
            | "gnome-shell"
            | "xdg-desktop-portal"
    )
}

pub(super) fn is_valid_pid(pid: i32) -> bool {
    pid > 0 && std::path::Path::new(&format!("/proc/{pid}")).exists()
}

fn is_pipewire_daemon(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .is_ok_and(|c| matches!(c.trim(), "pipewire" | "pipewire-pulse" | "wireplumber"))
}

pub(super) fn client_sec_pid(props: &DictRef) -> Option<i32> {
    let pid = props
        .get("pipewire.sec.pid")
        .and_then(|v| v.parse::<i32>().ok())
        .filter(|pid| is_valid_pid(*pid))?;
    if is_pipewire_daemon(pid) {
        None
    } else {
        Some(pid)
    }
}

pub(super) fn resolve_pid_by_binary(procs: &[ProcEntry], binary: &str) -> Option<i32> {
    if binary.is_empty() {
        return None;
    }
    let norm_bin = binary.replace('\\', "/");
    let lower = std::path::Path::new(&norm_bin)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&norm_bin)
        .to_lowercase();
    let lower_stem = lower.trim_end_matches(".exe");

    let candidates: Vec<i32> = procs
        .iter()
        .filter(|e| {
            let comm_lower = e.comm.to_lowercase();
            let comm_stem = comm_lower.trim_end_matches(".exe");
            if comm_lower == lower || comm_stem == lower_stem {
                return true;
            }
            if comm_lower.len() == 15
                && (lower.starts_with(&comm_lower) || lower_stem.starts_with(&comm_lower))
            {
                return true;
            }
            let norm_cmd = e.cmdline.replace('\\', "/");
            let cmd_bin = std::path::Path::new(norm_cmd.split('\0').next().unwrap_or(""))
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            let cmd_stem = cmd_bin.trim_end_matches(".exe");
            cmd_bin == lower || cmd_stem == lower_stem
        })
        .map(|e| e.pid)
        .collect();
    (candidates.len() == 1).then_some(candidates[0])
}

pub(super) fn resolve_pid_by_name(procs: &[ProcEntry], name: &str) -> Option<i32> {
    if name.is_empty() {
        return None;
    }
    let search_key = name.split_whitespace().next().filter(|s| s.len() >= 2)?;
    let search_lower = search_key.to_lowercase();
    let search_stem = search_lower.trim_end_matches(".exe");

    procs
        .iter()
        .find(|e| {
            let comm_lower = e.comm.to_lowercase();
            let comm_stem = comm_lower.trim_end_matches(".exe");
            if comm_lower == search_lower
                || comm_stem == search_stem
                || comm_lower.starts_with(search_stem)
                || search_stem.starts_with(&comm_lower)
            {
                return true;
            }
            let norm_cmd = e.cmdline.replace('\\', "/");
            let base = std::path::Path::new(norm_cmd.split('\0').next().unwrap_or(""))
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            let b_stem = base.trim_end_matches(".exe");
            b_stem == search_stem
                || b_stem.starts_with(search_stem)
                || search_stem.starts_with(b_stem)
        })
        .map(|e| e.pid)
}

#[cfg(test)]
mod tests {
    use super::{
        ProcEntry, are_processes_related, is_generic_launcher, is_valid_pid, resolve_pid_by_binary,
        resolve_pid_by_name,
    };

    #[test]
    fn identifies_generic_launchers() {
        assert!(is_generic_launcher("steam"));
        assert!(is_generic_launcher("wine64-preloader"));
        assert!(is_generic_launcher("wineserver"));
        assert!(is_generic_launcher("pv-bwrap"));
        assert!(is_generic_launcher(
            "C:\\windows\\system32\\wine64-preloader.exe"
        ));

        assert!(!is_generic_launcher("ZenlessZoneZero.exe"));
        assert!(!is_generic_launcher("Z:\\games\\ZenlessZoneZero.exe"));
        assert!(!is_generic_launcher("ffxiv_dx11.exe"));
        assert!(!is_generic_launcher("discord"));
        assert!(!is_generic_launcher("spotify"));
    }

    #[test]
    fn verifies_process_descendant_check() {
        let our_pid = std::process::id();
        assert!(are_processes_related(our_pid, our_pid));
        assert!(!are_processes_related(0, our_pid));
        assert!(!are_processes_related(our_pid, 0));
        assert!(!are_processes_related(1, 1));
    }

    #[test]
    fn resolves_pid_by_binary_with_wine_cmdline_and_truncated_comm() {
        let procs = vec![
            ProcEntry {
                pid: 100,
                comm: "steam".into(),
                cmdline: "/usr/bin/steam\0".into(),
            },
            ProcEntry {
                pid: 200,
                comm: "ZenlessZoneZero".into(), // 15-char kernel truncation
                cmdline:
                    "Z:\\SteamLibrary\\steamapps\\common\\ZenlessZoneZero\\ZenlessZoneZero.exe\0"
                        .into(),
            },
            ProcEntry {
                pid: 234,
                comm: "ffxiv_dx11.exe".into(),
                cmdline:
                    "Z:\\SteamLibrary\\steamapps\\common\\FINAL FANTASY XIV\\game\\ffxiv_dx11.exe\0"
                        .into(),
            },
        ];

        // Should match exact binary name despite backslashes and .exe
        assert_eq!(
            resolve_pid_by_binary(&procs, "ZenlessZoneZero.exe"),
            Some(200)
        );
        assert_eq!(
            resolve_pid_by_binary(&procs, "Z:\\path\\ZenlessZoneZero.exe"),
            Some(200)
        );
        assert_eq!(resolve_pid_by_name(&procs, "ZenlessZoneZero"), Some(200));
        assert_eq!(resolve_pid_by_binary(&procs, "ffxiv_dx11.exe"), Some(234));
    }

    #[test]
    fn resolve_pid_by_name_requires_two_character_search_key() {
        let procs = vec![ProcEntry {
            pid: 100,
            comm: "vim".into(),
            cmdline: "/usr/bin/vim\0".into(),
        }];
        assert_eq!(resolve_pid_by_name(&procs, ""), None);
        assert_eq!(resolve_pid_by_name(&procs, "v"), None);
        assert_eq!(resolve_pid_by_name(&procs, "vi"), Some(100));
    }

    #[test]
    fn resolve_pid_by_name_ignores_leading_whitespace_words() {
        let procs = vec![ProcEntry {
            pid: 100,
            comm: "firefox".into(),
            cmdline: "/usr/bin/firefox\0".into(),
        }];
        // The first whitespace-separated word is the search key.
        assert_eq!(resolve_pid_by_name(&procs, "  firefox"), Some(100));
    }

    #[test]
    fn resolve_pid_by_binary_is_ambiguous_when_two_processes_match() {
        let procs = vec![
            ProcEntry {
                pid: 100,
                comm: "node".into(),
                cmdline: "/usr/bin/node\0".into(),
            },
            ProcEntry {
                pid: 200,
                comm: "node".into(),
                cmdline: "/usr/bin/node\0".into(),
            },
        ];
        assert_eq!(resolve_pid_by_binary(&procs, "node"), None);
    }

    #[test]
    fn resolve_pid_by_name_returns_first_match_even_if_ambiguous() {
        let procs = vec![
            ProcEntry {
                pid: 100,
                comm: "node".into(),
                cmdline: "/usr/bin/node\0".into(),
            },
            ProcEntry {
                pid: 200,
                comm: "node".into(),
                cmdline: "/usr/bin/node\0".into(),
            },
        ];
        assert_eq!(resolve_pid_by_name(&procs, "node"), Some(100));
    }

    #[test]
    fn is_valid_pid_checks_proc_existence() {
        assert!(is_valid_pid(
            std::process::id()
                .try_into()
                .unwrap_or_else(|e| panic!("pid fits i32: {e}"))
        ));
        assert!(!is_valid_pid(999_999_999));
        assert!(!is_valid_pid(0));
        assert!(!is_valid_pid(-1));
    }

    #[test]
    fn relates_process_to_its_parent() {
        let our_pid = std::process::id();
        // SAFETY: getppid() always returns a valid parent pid for this process.
        let parent = unsafe { libc::getppid() }
            .try_into()
            .unwrap_or_else(|e| panic!("parent pid fits u32: {e}"));
        assert!(are_processes_related(our_pid, parent));
        assert!(are_processes_related(parent, our_pid));
    }

    #[test]
    fn unrelated_pids_are_not_related() {
        let our_pid = std::process::id();
        // A pid a few thousand above ours cannot share an ancestor chain with
        // us: our chain ends at the shell (a session daemon, excluded).
        assert!(!are_processes_related(our_pid, our_pid + 5000));
        assert!(!are_processes_related(our_pid + 5000, our_pid));
    }
}
