use super::apps::list_audio_applications;
use super::procinfo::{are_processes_related, is_generic_launcher, iter_proc, resolve_pid_by_name};
use super::{kwin, pw_init, sync_registry};
use crate::AudioApp;
use pipewire::spa::utils::dict::DictRef;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

fn extract_portal_window_name_from_map<F>(get_prop: F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    for &key in &[
        "portal.screencast.application",
        "portal.screencast.title",
        "window.name",
        "pipewire.access.portal.app_id",
    ] {
        if let Some(v) = get_prop(key).filter(|v| !v.is_empty()) {
            return Some(v);
        }
    }

    if let Some(v) = get_prop("media.name")
        .filter(|v| !v.is_empty() && !v.contains("pipewire") && v != "kwin_wayland")
    {
        return Some(v);
    }

    if let Some(v) = get_prop("node.name").filter(|v| !v.is_empty()) {
        let lower = v.to_lowercase();
        if lower != "kwin_wayland" && lower != "gnome-shell" && !lower.contains("pipewire") {
            return Some(v);
        }
    }

    if let Some(v) = get_prop("application.name").filter(|v| !v.is_empty()) {
        let lower = v.to_lowercase();
        if lower != "xdg-desktop-portal"
            && lower != "kwin_wayland"
            && lower != "gnome-shell"
            && !lower.contains("pipewire")
        {
            return Some(v);
        }
    }

    None
}

fn portal_window_name(props: &DictRef) -> Option<String> {
    extract_portal_window_name_from_map(|key| props.get(key).map(Into::into))
}

/// What a `kwin-screencast-<suffix>` stream captures, derived from the suffix.
/// `KWin` names window streams after the window's desktop file name, monitor
/// streams after the output (`DP-1`, `HDMI-A-1`, `eDP-1`, …), and region
/// streams after the geometry (`x,y WxH`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KdeScreencast {
    Window,
    Monitor,
    Region,
}

fn classify_kde_screencast(suffix: &str) -> KdeScreencast {
    // An empty suffix is a window whose desktop file name is empty (or a
    // restored portal session) — monitor and region names are never empty.
    if suffix.is_empty() {
        return KdeScreencast::Window;
    }
    if suffix.contains(',') {
        return KdeScreencast::Region;
    }
    // Output names end in a digit group after a dash (`DP-3`, `HDMI-A-1`),
    // while desktop file names carry dots or underscores (`org.kde.dolphin`,
    // `steam_app_default`) or no dash at all (`codium`, `signal`).
    let output_like = suffix.split('-').nth(1).is_some()
        && suffix
            .rsplit('-')
            .next()
            .is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        && !suffix.contains(['.', '_']);
    if output_like {
        KdeScreencast::Monitor
    } else {
        KdeScreencast::Window
    }
}

fn resolve_kde_screencast_audio(media_name: &str) -> Option<AudioApp> {
    let suffix = media_name.strip_prefix("kwin-screencast-")?;
    // Empty suffix means KWin didn't encode a window identity (e.g. restored
    // portal session with persist) — can't resolve any specific app. Monitors
    // and regions never map to a single application.
    if suffix.is_empty() || classify_kde_screencast(suffix) != KdeScreencast::Window {
        return None;
    }
    let win = kwin::resolve_window(suffix)?;
    if let Ok(apps) = list_audio_applications()
        && let Some(app) = match_kde_window_to_apps(&apps, &win, suffix)
    {
        return Some(app);
    }
    // Layer 5: the window's own process as a PID target — captures apps with no
    // active stream yet (e.g. Spotify while paused).
    window_pid_fallback(&win, suffix)
}

/// Layers 1–4 of the KDE audio resolution: match the captured window against
/// the *active* audio streams (process hierarchy, binary, name, caption,
/// desktop file name).
#[allow(
    clippy::too_many_lines,
    reason = "four ordered match strategies are clearer as one cascade than as separate helpers"
)]
fn match_kde_window_to_apps(
    apps: &[AudioApp],
    win: &kwin::WindowMatch,
    suffix: &str,
) -> Option<AudioApp> {
    // Layer 1: Process hierarchy & related process tree match — check if
    // the audio process and window owner process are identical, parent/child,
    // or share a launcher/container ancestor (e.g. Proton/Wine, Steam/bwrap).
    if let Some(app) = apps.iter().find(|app| {
        let app_pid = app.process_id.cast_unsigned();
        are_processes_related(app_pid, win.pid)
    }) {
        return Some(app.clone());
    }

    // Layer 1b: Match audio app by checking process binary/cmdline of running
    // audio apps against the suffix or window caption (normalizing Windows backslashes
    // and .exe extensions).
    let clean_suffix = suffix.trim_end_matches(".exe").trim_end_matches(".EXE");
    for app in apps {
        let app_pid = app.process_id;
        if app_pid <= 0 {
            continue;
        }
        if let Ok(cmdline) = std::fs::read_to_string(format!("/proc/{app_pid}/cmdline")) {
            let norm_cmd = cmdline.replace('\\', "/");
            let exe = std::path::Path::new(norm_cmd.split('\0').next().unwrap_or(""))
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .trim_end_matches(".exe")
                .trim_end_matches(".EXE");
            if !exe.is_empty()
                && (exe.eq_ignore_ascii_case(clean_suffix)
                    || (!win.caption.is_empty()
                        && win.caption.to_lowercase().contains(&exe.to_lowercase())))
            {
                return Some(app.clone());
            }
        }
    }

    // Layer 2: window-process candidates (comm / cmdline binary).
    let comm = std::fs::read_to_string(format!("/proc/{}/comm", win.pid)).ok();
    let cmdline = std::fs::read_to_string(format!("/proc/{}/cmdline", win.pid)).ok();
    let mut candidates: Vec<String> = Vec::new();
    if let Some(ref comm) = comm {
        let name = comm.trim();
        if !name.is_empty() && !is_generic_launcher(name) {
            candidates.push(name.to_string());
        }
    }
    if let Some(ref cmdline) = cmdline {
        let norm_cmd = cmdline.replace('\\', "/");
        let binary = norm_cmd.split('\0').next().unwrap_or("").trim();
        if !binary.is_empty()
            && let Some(stem) = std::path::Path::new(binary)
                .file_stem()
                .and_then(|s| s.to_str())
            && !stem.is_empty()
            && !is_generic_launcher(stem)
            && !candidates.iter().any(|c| c == stem)
        {
            candidates.push(stem.to_string());
        }
    }
    for candidate in &candidates {
        if let Some(app) = crate::find_best_audio_match(apps, candidate) {
            return Some(app);
        }
    }

    // Layer 3: Window caption match.
    if !win.caption.is_empty()
        && let Some(app) = crate::find_best_audio_match(apps, &win.caption)
    {
        return Some(app);
    }

    // Layer 4: desktop file name suffix.
    if !is_generic_launcher(suffix)
        && let Some(app) = crate::find_best_audio_match(apps, suffix)
    {
        return Some(app);
    }

    None
}

/// The friendliest display name for a process: its comm unless that is a
/// generic launcher (steam, wine64-preloader, …), falling back to `fallback`.
fn process_display_name(pid: u32, fallback: &str) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty() && !is_generic_launcher(c))
        .unwrap_or_else(|| fallback.to_string())
}

/// A process-id capture target (`id = -pid`) for an app that is running but
/// currently silent. Pid 0/1 and pids that do not fit the negative-id
/// encoding are rejected.
fn pid_fallback_app(pid: i32, name: String) -> Option<AudioApp> {
    if pid <= 1 {
        return None;
    }
    Some(AudioApp {
        id: -pid,
        name,
        process_id: pid,
        bundle_id: None,
        window_title: None,
        client_id: None,
        media_title: None,
    })
}

/// Layer 5 for KDE window captures: the window's own process as a PID target.
fn window_pid_fallback(win: &kwin::WindowMatch, suffix: &str) -> Option<AudioApp> {
    let pid = i32::try_from(win.pid).ok()?;
    let name = if win.caption.is_empty() {
        process_display_name(win.pid, suffix)
    } else {
        process_display_name(win.pid, &win.caption)
    };
    pid_fallback_app(pid, name)
}

/// The last dot-segment of a reverse-domain app id (`org.mozilla.firefox` →
/// `firefox`). Only names with at least two dots qualify — binary names like
/// `ffxiv_dx11.exe` (a single dot) stay untouched.
fn shorten_portal_app_id(name: &str) -> Option<&str> {
    let short = name.split('.').next_back()?;
    (name.matches('.').count() >= 2 && short.len() >= 3).then_some(short)
}

/// Resolves the process id for a portal-reported window application id:
/// exact name/binary match first, then the last dot-segment (`org.gnome.…`).
fn resolve_pid_for_portal_app(name: &str) -> Option<i32> {
    let procs = iter_proc();
    if let Some(pid) = resolve_pid_by_name(&procs, name) {
        return Some(pid);
    }
    let short = shorten_portal_app_id(name)?;
    resolve_pid_by_name(&procs, short)
}

/// Snapshot of the `PipeWire` video nodes relevant to an active portal capture.
#[derive(Default)]
struct VideoScan {
    de: Option<&'static str>,
    source_type: Option<&'static str>,
    media_name: Option<String>,
    video_node_count: u32,
    /// Highest `object.serial` seen — serials order streams by creation time, so
    /// this picks the active stream over lingering ones.
    highest_serial: u64,
    /// `(object.serial, media.name)` per KDE screencast node.
    kde_media_names: Vec<(u64, String)>,
    capture_names: Vec<String>,
    screencast_node_id: Option<u32>,
    /// xdg-desktop-portal metadata (`portal.screencast.*`) of the captured window.
    portal_props: Option<HashMap<String, String>>,
}

/// Collect only the xdg-desktop-portal metadata keys (`portal.screencast.*`)
/// from the screencast video node's registry + info props.
fn merge_portal_props(
    out: &mut Option<HashMap<String, String>>,
    registry: &HashMap<String, String>,
    info: &DictRef,
) {
    let merged = registry
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .chain(info.iter())
        .filter(|(k, _)| k.starts_with("portal.screencast."))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    *out = Some(merged);
}

#[allow(
    clippy::too_many_lines,
    reason = "single-pass PipeWire registry scan whose bindings must stay inline with their capture-scan mutations"
)]
fn inspect_video_graph() -> Option<VideoScan> {
    pipewire::init();
    let pw = pw_init().ok()?;
    let registry = pw.core.get_registry().ok()?;

    let scan = Rc::new(RefCell::new(VideoScan::default()));
    let bindings: Rc<RefCell<Vec<(pipewire::node::Node, pipewire::node::NodeListener)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let bind_core = pw.core.clone();
    let bind_registry: Rc<RefCell<Option<pipewire::registry::RegistryRc>>> =
        Rc::new(RefCell::new(None));
    let reg_cell = bind_registry;

    let _reg_listener = registry
        .add_listener_local()
        .global({
            let scan = Rc::clone(&scan);
            let bind_core = bind_core;
            let reg_cell = reg_cell;
            let bindings = bindings;
            move |global| {
                let Some(props) = global.props else { return };
                let media_class = props.get("media.class").unwrap_or("");
                // Only producer/output nodes carry capture-source metadata;
                // consumer nodes (Stream/Input/Video) are not relevant.
                if !media_class.starts_with("Video/")
                    && !media_class.starts_with("Stream/Output/Video")
                {
                    return;
                }
                let scan_info = Rc::clone(&scan);
                let mut scan = scan.borrow_mut();
                scan.video_node_count += 1;

                let has_reg = reg_cell.borrow().is_some();
                if !has_reg {
                    *reg_cell.borrow_mut() = bind_core.get_registry_rc().ok();
                }
                let reg_binding = reg_cell.borrow();
                let Some(reg) = reg_binding.as_ref() else {
                    return;
                };
                let Ok(node) = reg.bind::<pipewire::node::Node, _>(global) else {
                    return;
                };
                let media_class_owned: String = media_class.into();
                let node_id = global.id;
                let registry_props: HashMap<String, String> = props
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                let listener = node
                    .add_listener_local()
                    .info(move |info| {
                        let Some(p) = info.props() else { return };
                        let mut scan = scan_info.borrow_mut();
                        let mn = p.get("media.name").unwrap_or("");
                        let serial = p
                            .get("object.serial")
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(0);

                        if let Some(suffix) = mn.strip_prefix("kwin-screencast-") {
                            scan.de = Some("kde");
                            if !scan.kde_media_names.iter().any(|(_, s)| s == mn) {
                                scan.kde_media_names.push((serial, mn.into()));
                            }
                            if serial >= scan.highest_serial {
                                scan.highest_serial = serial;
                                scan.media_name = Some(mn.into());
                                scan.source_type = Some(match classify_kde_screencast(suffix) {
                                    KdeScreencast::Window => "window",
                                    KdeScreencast::Monitor => "monitor",
                                    KdeScreencast::Region => "region",
                                });
                                scan.screencast_node_id = Some(node_id);
                                merge_portal_props(&mut scan.portal_props, &registry_props, p);
                            }
                            return;
                        }
                        if p.get("portal.screencast.application")
                            .is_some_and(|v| !v.is_empty())
                        {
                            scan.de = Some("gnome");
                            if serial >= scan.highest_serial {
                                scan.highest_serial = serial;
                                scan.media_name =
                                    if mn.is_empty() { None } else { Some(mn.into()) };
                                scan.source_type = Some("window");
                                scan.screencast_node_id = Some(node_id);
                                merge_portal_props(&mut scan.portal_props, &registry_props, p);
                            }
                            if let Some(name) = portal_window_name(p)
                                && !scan.capture_names.contains(&name)
                            {
                                scan.capture_names.push(name);
                            }
                        } else if media_class_owned == "Video/Source" && scan.source_type.is_none()
                        {
                            let has_app_meta =
                                p.get("application.name").is_some_and(|v| !v.is_empty())
                                    || p.get("pipewire.access.portal.app_id")
                                        .is_some_and(|v| !v.is_empty())
                                    || p.get("window.name").is_some_and(|v| !v.is_empty());
                            if !has_app_meta && serial >= scan.highest_serial {
                                scan.highest_serial = serial;
                                scan.source_type = Some("monitor");
                            }
                        }
                    })
                    .register();
                bindings.borrow_mut().push((node, listener));
            }
        })
        .register();

    sync_registry(&pw.core, &pw.main_loop);
    // Second round for bound nodes to deliver their info events.
    sync_registry(&pw.core, &pw.main_loop);
    Some(scan.take())
}

/// The active KDE screencast stream: the most recently created one, by
/// `object.serial` (`KWin` can leave older, lingering streams listed).
fn active_kde_media_name(kde_media_names: &[(u64, String)]) -> Option<&str> {
    kde_media_names
        .iter()
        .max_by_key(|(serial, _)| *serial)
        .map(|(_, name)| name.as_str())
}

fn resolve_from_video_scan(scan: &VideoScan) -> Option<AudioApp> {
    // 1. For KDE screencast streams (`kwin-screencast-*`), evaluate strictly
    // the single active (most recently created) stream based on object.serial.
    if let Some(active_mn) = active_kde_media_name(&scan.kde_media_names) {
        // If the active stream is a monitor or region, return None directly.
        if let Some(suffix) = active_mn.strip_prefix("kwin-screencast-") {
            let class = classify_kde_screencast(suffix);
            if class == KdeScreencast::Monitor || class == KdeScreencast::Region {
                return None;
            }
        }

        // Resolve only the active stream; never fall through to older lingering ones.
        return resolve_kde_screencast_audio(active_mn);
    }

    // 2. Monitors and regions for non-KDE environments are screen displays — return None.
    if scan.source_type == Some("monitor") || scan.source_type == Some("region") {
        return None;
    }

    // 3. GNOME / XDG portal streams: match the capture names against running apps.
    if let Ok(apps) = list_audio_applications()
        && let Some(app) = scan
            .capture_names
            .iter()
            .find_map(|name| crate::find_best_audio_match(&apps, name))
    {
        return Some(app);
    }

    // 4. Portal app with no active stream: resolve its id to a running process.
    scan.capture_names.iter().find_map(|name| {
        let pid = resolve_pid_for_portal_app(name)?;
        let display = process_display_name(pid.cast_unsigned(), name);
        pid_fallback_app(pid, display)
    })
}

pub(crate) fn resolve_audio_app_for_captured_window() -> Option<AudioApp> {
    let scan = inspect_video_graph()?;
    resolve_from_video_scan(&scan)
}

pub(crate) fn get_capture_context() -> Result<crate::CaptureContext, String> {
    let scan = inspect_video_graph()
        .ok_or_else(|| "PipeWire video node introspection failed".to_string())?;
    let node_id = scan.screencast_node_id;
    let app = resolve_from_video_scan(&scan);
    let (window_pid, window_caption) = if scan.de == Some("kde") {
        scan.media_name
            .as_deref()
            .and_then(|mn| mn.strip_prefix("kwin-screencast-"))
            .filter(|suffix| classify_kde_screencast(suffix) == KdeScreencast::Window)
            .and_then(kwin::resolve_window)
            .map_or((None, None), |w| {
                (Some(w.pid.cast_signed()), Some(w.caption))
            })
    } else {
        (None, None)
    };
    Ok(crate::CaptureContext {
        de: scan.de.unwrap_or("unknown").into(),
        source_type: scan.source_type.unwrap_or("unknown").into(),
        media_name: scan.media_name,
        video_node_count: scan.video_node_count.cast_signed(),
        app,
        screencast_node_id: node_id,
        // `object.serial` values stay far below 2^53, so the f64 conversion
        // is exact for every serial that can occur in practice.
        #[allow(
            clippy::cast_precision_loss,
            reason = "object.serial is monotonically increasing from 1 and stays below 2^53 in any real session"
        )]
        highest_serial: (scan.highest_serial != 0).then_some(scan.highest_serial as f64),
        portal_props: scan.portal_props,
        window_pid,
        window_caption,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        KdeScreencast, VideoScan, active_kde_media_name, classify_kde_screencast,
        extract_portal_window_name_from_map, match_kde_window_to_apps, pid_fallback_app,
        process_display_name, resolve_from_video_scan, shorten_portal_app_id, window_pid_fallback,
    };
    use crate::AudioApp;
    use std::collections::HashMap;

    #[test]
    fn pid_fallback_app_encodes_pid_as_negative_id() {
        let app = pid_fallback_app(1234, "Spotify".into()).unwrap_or_else(|| panic!("pid app"));
        assert_eq!(app.id, -1234);
        assert_eq!(app.process_id, 1234);
        assert_eq!(app.name, "Spotify");
    }

    #[test]
    fn pid_fallback_app_rejects_system_pids() {
        assert!(pid_fallback_app(0, "x".into()).is_none());
        assert!(pid_fallback_app(1, "x".into()).is_none());
    }

    #[test]
    fn window_pid_fallback_prefers_process_comm_over_caption() {
        // Our own live process: comm is the test binary (not a generic
        // launcher), so it must win over the caption.
        let pid = std::process::id();
        let win = super::super::kwin::WindowMatch {
            pid,
            caption: "Some Window Caption".into(),
        };
        let app = window_pid_fallback(&win, "suffix").unwrap_or_else(|| panic!("pid app"));
        let comm =
            std::fs::read_to_string("/proc/self/comm").unwrap_or_else(|e| panic!("comm: {e}"));
        assert_eq!(app.name, comm.trim());
        assert_eq!(app.id, -(i32::try_from(pid).unwrap_or(i32::MAX)));
        assert_eq!(app.process_id, i32::try_from(pid).unwrap_or(i32::MAX));
    }

    #[test]
    fn process_display_name_falls_back_for_unknown_processes() {
        assert_eq!(process_display_name(999_999_999, "fallback"), "fallback");
        assert!(!process_display_name(std::process::id(), "fallback").is_empty());
    }

    #[test]
    fn shorten_portal_app_id_only_shortens_reverse_domain_names() {
        assert_eq!(
            shorten_portal_app_id("org.mozilla.firefox"),
            Some("firefox")
        );
        assert_eq!(
            shorten_portal_app_id("org.gnome.Nautilus"),
            Some("Nautilus")
        );
        assert_eq!(shorten_portal_app_id("com.spotify.Client"), Some("Client"));
        // Single-dot names are binaries (or plain names) — never shortened.
        assert_eq!(shorten_portal_app_id("ffxiv_dx11.exe"), None);
        assert_eq!(shorten_portal_app_id("spotify"), None);
        assert_eq!(shorten_portal_app_id("org"), None);
        // Too-short segments are not usable search keys.
        assert_eq!(shorten_portal_app_id("org.foo.x"), None);
    }

    #[test]
    fn matches_pipewire_portal_screencast_node_properties() {
        // Sample PipeWire properties from xdg-desktop-portal / getDisplayMedia() window pickers
        let mut ffxiv_props = HashMap::new();
        ffxiv_props.insert("portal.screencast.title", "FINAL FANTASY XIV".to_string());
        ffxiv_props.insert(
            "portal.screencast.application",
            "ffxiv_dx11.exe".to_string(),
        );
        ffxiv_props.insert("window.name", "FINAL FANTASY XIV".to_string());

        let name = extract_portal_window_name_from_map(|k| ffxiv_props.get(k).cloned());
        assert_eq!(name.as_deref(), Some("ffxiv_dx11.exe"));

        let mut zzz_props = HashMap::new();
        zzz_props.insert("window.name", "Zenless Zone Zero".to_string());
        zzz_props.insert("application.name", "ZenlessZoneZero.exe".to_string());

        let name = extract_portal_window_name_from_map(|k| zzz_props.get(k).cloned());
        assert_eq!(name.as_deref(), Some("Zenless Zone Zero"));

        let mut portal_wrapper = HashMap::new();
        portal_wrapper.insert("application.name", "xdg-desktop-portal".to_string());
        portal_wrapper.insert("node.name", "pipewire_system".to_string());

        let name = extract_portal_window_name_from_map(|k| portal_wrapper.get(k).cloned());
        assert_eq!(name, None);
    }

    #[test]
    fn matches_pipewire_getdisplaymedia_video_scan_objects() {
        let apps = vec![
            AudioApp {
                id: 234,
                name: "ffxiv_dx11.exe".to_string(),
                process_id: 54321,
                bundle_id: None,
                window_title: Some("Playback".to_string()),
                client_id: Some(230),
                media_title: None,
            },
            AudioApp {
                id: 101,
                name: "ZenlessZoneZero.exe".to_string(),
                process_id: 60000,
                bundle_id: None,
                window_title: None,
                client_id: Some(100),
                media_title: None,
            },
        ];

        // Simulated PipeWire VideoScan from a getDisplayMedia() portal screencast
        let scan = VideoScan {
            de: Some("kde"),
            source_type: Some("window"),
            media_name: Some("kwin-screencast-ffxiv_dx11.exe".to_string()),
            video_node_count: 1,
            highest_serial: 100,
            kde_media_names: vec![(100, "kwin-screencast-ffxiv_dx11.exe".to_string())],
            capture_names: vec!["FINAL FANTASY XIV".to_string()],
            screencast_node_id: Some(50),
            portal_props: None,
        };

        let matched = scan
            .capture_names
            .iter()
            .find_map(|name| crate::find_best_audio_match(&apps, name));
        assert_eq!(matched.map(|a| a.id), Some(234));

        // Matching Zenless Zone Zero by window name
        let matched_zzz = crate::find_best_audio_match(&apps, "Zenless Zone Zero");
        assert_eq!(matched_zzz.map(|a| a.id), Some(101));
    }

    #[test]
    fn classifies_kde_window_names() {
        // KWin names window streams after the window's desktop file name.
        for suffix in [
            "codium",
            "signal",
            "discord",
            "org.kde.dolphin",
            "brave-origin",
            "spotify-launcher",
            "com.mitchellh.ghostty",
            "io.ente.auth",
            "gitbutler-tauri",
            "steam_app_default",
            // Window with no desktop file name at all.
            "",
        ] {
            assert_eq!(
                classify_kde_screencast(suffix),
                KdeScreencast::Window,
                "{suffix:?} must classify as a window"
            );
        }
    }

    #[test]
    fn classifies_kde_monitor_names() {
        // KWin names monitor streams after the output connector.
        for suffix in ["DP-1", "DP-3", "HDMI-A-1", "eDP-1", "DVI-D-1", "Virtual-1"] {
            assert_eq!(
                classify_kde_screencast(suffix),
                KdeScreencast::Monitor,
                "{suffix:?} must classify as a monitor"
            );
        }
    }

    #[test]
    fn classifies_kde_region_name() {
        assert_eq!(
            classify_kde_screencast("0,0 1920x1080"),
            KdeScreencast::Region
        );
    }

    #[test]
    fn classifies_kde_multi_digit_output_names() {
        for suffix in ["DP-10", "DP-12", "HDMI-A-10", "eDP-2", "DisplayPort-0"] {
            assert_eq!(
                classify_kde_screencast(suffix),
                KdeScreencast::Monitor,
                "{suffix:?} must classify as a monitor"
            );
        }
    }

    #[test]
    fn classifies_dash_digit_suffix_with_dot_or_underscore_as_window() {
        // Desktop-file-like names that *look* like outputs (dash + digits) but
        // carry a dot/underscore must stay window-classified.
        for suffix in ["org.kde.foo-1", "steam_app_123", "app-2_test"] {
            assert_eq!(
                classify_kde_screencast(suffix),
                KdeScreencast::Window,
                "{suffix:?} must classify as a window"
            );
        }
    }

    #[test]
    fn portal_window_name_prefers_portal_keys_over_media_name() {
        let props = HashMap::from([
            (
                "portal.screencast.application",
                "ffxiv_dx11.exe".to_string(),
            ),
            ("portal.screencast.title", "FINAL FANTASY XIV".to_string()),
            ("media.name", "Playback".to_string()),
        ]);
        let name = extract_portal_window_name_from_map(|k| props.get(k).cloned());
        assert_eq!(name.as_deref(), Some("ffxiv_dx11.exe"));
    }

    #[test]
    fn portal_window_name_skips_generic_pipewire_names() {
        let props = HashMap::from([
            ("media.name", "pipewire-screencast".to_string()),
            ("node.name", "pipewire_system".to_string()),
            ("application.name", "xdg-desktop-portal".to_string()),
        ]);
        assert_eq!(
            extract_portal_window_name_from_map(|k| props.get(k).cloned()),
            None
        );
    }

    #[test]
    fn portal_window_name_accepts_media_name_when_not_generic() {
        let props = HashMap::from([("media.name", "firefox".to_string())]);
        let name = extract_portal_window_name_from_map(|k| props.get(k).cloned());
        assert_eq!(name.as_deref(), Some("firefox"));
    }

    #[test]
    fn portal_window_name_rejects_kwin_gnome_application_names() {
        for app in ["kwin_wayland", "gnome-shell", "xdg-desktop-portal"] {
            let props = HashMap::from([("application.name", app.to_string())]);
            assert_eq!(
                extract_portal_window_name_from_map(|k| props.get(k).cloned()),
                None,
                "{app:?} must be rejected"
            );
        }
    }

    #[test]
    fn resolve_from_video_scan_rejects_monitors_and_regions() {
        let monitor_scan = VideoScan {
            de: Some("kde"),
            source_type: Some("monitor"),
            media_name: Some("kwin-screencast-DP-3".to_string()),
            video_node_count: 18,
            highest_serial: 224,
            kde_media_names: vec![(224, "kwin-screencast-DP-3".to_string())],
            capture_names: vec!["Spotify".to_string()],
            screencast_node_id: Some(224),
            portal_props: None,
        };
        assert!(resolve_from_video_scan(&monitor_scan).is_none());

        let region_scan = VideoScan {
            de: Some("kde"),
            source_type: Some("region"),
            media_name: Some("kwin-screencast-0,0 1920x1080".to_string()),
            video_node_count: 5,
            highest_serial: 225,
            kde_media_names: vec![(225, "kwin-screencast-0,0 1920x1080".to_string())],
            capture_names: vec!["Spotify".to_string()],
            screencast_node_id: Some(225),
            portal_props: None,
        };
        assert!(resolve_from_video_scan(&region_scan).is_none());
    }

    #[test]
    fn kde_window_resolution_never_falls_through_to_unrelated_audio_apps() {
        // A KDE window capture resolves strictly by the captured window's
        // identity. An unrelated running audio app — a different process,
        // e.g. Spotify playing in another window, which may appear in
        // `capture_names` from another video node — must never be picked,
        // even when the captured app has no active audio stream: the
        // resolution ends at the captured window's own PID target (Layer 5),
        // never at the unrelated app.
        let win = super::super::kwin::WindowMatch {
            pid: std::process::id(),
            caption: "VSCodium".into(),
        };
        let unrelated =
            pid_fallback_app(999_999, "Spotify".into()).unwrap_or_else(|| panic!("app"));
        let resolved = match_kde_window_to_apps(&[unrelated], &win, "codium")
            .or_else(|| window_pid_fallback(&win, "codium"))
            .unwrap_or_else(|| panic!("window PID fallback must resolve"));
        assert_ne!(
            resolved.name.to_lowercase(),
            "spotify",
            "unrelated capture names must never win the KDE resolution"
        );
        let self_pid = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
        assert_eq!(
            resolved.process_id, self_pid,
            "resolution targets the captured window's own process"
        );
    }

    #[test]
    fn active_kde_stream_is_the_newest_ignoring_older_lingering_streams() {
        // KWin can leave lingering screencast streams behind (an older
        // capture's node is eventually destroyed, but it may still be
        // listed); the active stream is the most recently created one —
        // highest `object.serial` — regardless of list order.
        let names = vec![
            (100, "kwin-screencast-steam_app_default".to_string()),
            (200, "kwin-screencast-codium".to_string()),
        ];
        assert_eq!(
            active_kde_media_name(&names),
            Some("kwin-screencast-codium")
        );
        let shuffled = vec![
            (200, "kwin-screencast-codium".to_string()),
            (100, "kwin-screencast-steam_app_default".to_string()),
        ];
        assert_eq!(
            active_kde_media_name(&shuffled),
            Some("kwin-screencast-codium")
        );
        assert_eq!(active_kde_media_name(&[]), None);
    }
}
