use super::apps::list_audio_applications;
use super::procinfo::{are_processes_related, is_generic_launcher};
use super::{kwin, pw_init, sync_registry};
use crate::AudioApp;
use napi::Result as NapiResult;
use pipewire::spa::utils::dict::DictRef;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub(crate) fn resolve_audio_app_for_x11_window(window_id: u32) -> Option<AudioApp> {
    // SAFETY: null selects the default display from $DISPLAY; failure returns
    // null, which is checked immediately.
    let display = unsafe { x11::xlib::XOpenDisplay(std::ptr::null()) };
    if display.is_null() {
        return None;
    }

    let atom_name = match std::ffi::CString::new("_NET_WM_PID") {
        Ok(name) => name,
        Err(_) => {
            // SAFETY: balances the XOpenDisplay above; display is valid.
            unsafe { x11::xlib::XCloseDisplay(display) };
            return None;
        }
    };
    // SAFETY: `display` is a valid open display and `atom_name` is a valid
    // NUL-terminated C string; both outlive the call.
    let atom = unsafe { x11::xlib::XInternAtom(display, atom_name.as_ptr(), 1) };
    if atom == 0 {
        // SAFETY: balances the XOpenDisplay above; `display` is valid.
        unsafe { x11::xlib::XCloseDisplay(display) };
        return None;
    }

    let mut actual_type: x11::xlib::Atom = 0;
    let mut actual_format: std::os::raw::c_int = 0;
    let mut nitems: std::os::raw::c_ulong = 0;
    let mut bytes_after: std::os::raw::c_ulong = 0;
    let mut prop: *mut u8 = std::ptr::null_mut();

    // SAFETY: `display` and `atom` are valid; every out-pointer references a
    // live stack variable and `prop` starts null, so a failed call leaves
    // nothing to free.
    let status = unsafe {
        x11::xlib::XGetWindowProperty(
            display,
            x11::xlib::Window::from(window_id),
            atom,
            0,
            1,
            0,
            x11::xlib::XA_CARDINAL,
            &raw mut actual_type,
            &raw mut actual_format,
            &raw mut nitems,
            &raw mut bytes_after,
            &raw mut prop,
        )
    };

    let pid = if status == 0 && !prop.is_null() && nitems > 0 && actual_format == 32 {
        #[allow(
            clippy::cast_ptr_alignment,
            reason = "Xlib guarantees 4-byte alignment for 32-bit format data"
        )]
        // SAFETY: the call succeeded with one 32-bit item returned, so `prop`
        // points to at least one Xlib-allocated u32.
        Some(unsafe { *(prop as *const u32) })
    } else {
        None
    };

    if !prop.is_null() {
        // SAFETY: `prop` was allocated by Xlib in the XGetWindowProperty above.
        unsafe { x11::xlib::XFree(prop.cast::<std::ffi::c_void>()) };
    }
    // SAFETY: balances the XOpenDisplay above; `display` is still valid.
    unsafe { x11::xlib::XCloseDisplay(display) };

    let pid = pid?;
    let apps = list_audio_applications().ok()?;
    apps.into_iter().find(|app| {
        let app_pid = app.process_id.cast_unsigned();
        are_processes_related(app_pid, pid)
    })
}

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
    let apps = list_audio_applications().ok()?;
    // The suffix is the captured window's desktop file name; KWin reports the
    // owning PID and window caption for it over D-Bus.
    if let Some(win) = kwin::resolve_window(suffix) {
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
        for app in &apps {
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

        // Layer 2: Match by non-generic window process candidates.
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
            if let Some(app) = crate::find_best_audio_match(&apps, candidate) {
                return Some(app);
            }
        }

        // Layer 3: Window caption match.
        if !win.caption.is_empty()
            && let Some(app) = crate::find_best_audio_match(&apps, &win.caption)
        {
            return Some(app);
        }
    }

    // Layer 4: Match desktop file name suffix if not generic.
    if !is_generic_launcher(suffix) {
        if let Some(app) = crate::find_best_audio_match(&apps, suffix) {
            return Some(app);
        }
    }

    None
}

/// Snapshot of the `PipeWire` video nodes relevant to an active portal capture.
#[derive(Default)]
struct VideoScan {
    de: Option<&'static str>,
    source_type: Option<&'static str>,
    media_name: Option<String>,
    video_node_count: u32,
    /// Highest `object.serial` observed among screencast nodes — ensures the
    /// active stream (most recently created) is chosen over lingering nodes.
    highest_serial: u64,
    /// `(object.serial, media.name)` per KDE screencast node — the serial
    /// orders streams by creation time.
    kde_media_names: Vec<(u64, String)>,
    capture_names: Vec<String>,
    screencast_node_id: Option<u32>,
    /// xdg-desktop-portal screencast metadata (`portal.screencast.*`) of the
    /// captured window, read off the screencast video node.
    portal_props: Option<HashMap<String, String>>,
}

/// Collect only the xdg-desktop-portal metadata keys (`portal.screencast.*`)
/// from the screencast video node's registry + info props — the portal's own
/// record of the captured window, without dumping the whole PipeWire node.
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
    let reg_cell = bind_registry.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global({
            let scan = scan.clone();
            let bind_core = bind_core.clone();
            let reg_cell = reg_cell.clone();
            let bindings = bindings.clone();
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
                let scan_info = scan.clone();
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

fn resolve_from_video_scan(scan: &VideoScan) -> Option<AudioApp> {
    // 1. For KDE screencast streams (`kwin-screencast-*`), evaluate strictly
    // the single active (most recently created) stream based on object.serial.
    if !scan.kde_media_names.is_empty() {
        let mut kde_names: Vec<&(u64, String)> = scan.kde_media_names.iter().collect();
        kde_names.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        let active_mn = &kde_names[0].1;

        // If the active stream is a monitor or region, return None directly.
        if let Some(suffix) = active_mn.strip_prefix("kwin-screencast-") {
            let class = classify_kde_screencast(suffix);
            if class == KdeScreencast::Monitor || class == KdeScreencast::Region {
                return None;
            }
        }

        // Resolve audio only for the active stream. If it returns None (e.g. VSCodium
        // has no audio process), return None directly — do NOT loop through older
        // lingering screencast streams (e.g. Steam/FFXIV).
        return resolve_kde_screencast_audio(active_mn);
    }

    // 2. Monitors and regions for non-KDE environments are screen displays — return None.
    if scan.source_type == Some("monitor") || scan.source_type == Some("region") {
        return None;
    }

    // 3. For GNOME / XDG portal screencast streams, match against running audio apps.
    let apps = list_audio_applications().ok()?;
    scan.capture_names
        .iter()
        .find_map(|name| crate::find_best_audio_match(&apps, name))
}

pub(crate) fn resolve_audio_app_for_captured_window() -> Option<AudioApp> {
    let scan = inspect_video_graph()?;
    resolve_from_video_scan(&scan)
}

pub(crate) fn get_capture_context() -> NapiResult<crate::CaptureContext> {
    let scan = inspect_video_graph()
        .ok_or_else(|| napi::Error::from_reason("PipeWire video node introspection failed"))?;
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
        portal_props: scan.portal_props,
        window_pid,
        window_caption,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        KdeScreencast, VideoScan, classify_kde_screencast, extract_portal_window_name_from_map,
        resolve_from_video_scan,
    };
    use crate::AudioApp;
    use std::collections::HashMap;

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
    fn resolve_from_video_scan_does_not_fall_through_kde_window_to_unrelated_capture_names() {
        // KDE scan for a window without matching audio (e.g. VSCodium), where capture_names
        // accidentally captured Spotify from another video node.
        let scan = VideoScan {
            de: Some("kde"),
            source_type: Some("window"),
            media_name: Some("kwin-screencast-codium".to_string()),
            video_node_count: 10,
            highest_serial: 200,
            kde_media_names: vec![(200, "kwin-screencast-codium".to_string())],
            capture_names: vec!["Spotify".to_string()],
            screencast_node_id: Some(50),
            portal_props: None,
        };
        assert!(resolve_from_video_scan(&scan).is_none());
    }

    #[test]
    fn resolve_from_video_scan_evaluates_only_newest_stream_ignoring_older_lingering_streams() {
        // Active selection is VSCodium (serial 200), older lingering stream is Steam/FFXIV (serial 100).
        let scan = VideoScan {
            de: Some("kde"),
            source_type: Some("window"),
            media_name: Some("kwin-screencast-codium".to_string()),
            video_node_count: 2,
            highest_serial: 200,
            kde_media_names: vec![
                (100, "kwin-screencast-steam_app_default".to_string()),
                (200, "kwin-screencast-codium".to_string()),
            ],
            capture_names: vec![],
            screencast_node_id: Some(150),
            portal_props: None,
        };
        // Active stream is codium (no audio) — must return None without evaluating the older steam stream.
        assert!(resolve_from_video_scan(&scan).is_none());
    }
}
