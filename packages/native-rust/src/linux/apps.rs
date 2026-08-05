use super::procinfo::{
    client_sec_pid, is_system_or_session_daemon, is_valid_pid, iter_proc, resolve_pid_by_binary,
    resolve_pid_by_name,
};
use super::{CAPTURE_NODE_NAME, mpris, pw_init, sync_registry};
use crate::AudioApp;
use pipewire::spa::utils::dict::DictRef;
use pipewire::types::ObjectType;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Mutex;

/// Serializes `PipeWire` enumeration: the renderer polls `getAudioApps` every
/// 3s on tokio blocking threads, and `libpipewire` contexts/registries are not
/// safe to touch concurrently. The global `pipewire::init()` once is completed
/// at startup (`ensure_pipewire_init`); this gate orders the per-call sessions.
static PW_ACCESS: Mutex<()> = Mutex::new(());

/// Best-effort window/tab title for an audio stream node, used by the UI to tell
/// same-named applications apart. Browsers put the tab title in `media.name`;
/// values that just restate the app name or a generic role are not titles.
fn stream_window_title(props: &DictRef, app_name: &str) -> Option<String> {
    const GENERIC: [&str; 11] = [
        "playback",
        "playback stream",
        "playback streams",
        "playstream",
        "audio stream",
        "audiostream",
        "output",
        "output stream",
        "record",
        "audio",
        "stream",
    ];
    for key in [
        "media.name",
        "media.title",
        "window.title",
        "node.description",
    ] {
        let Some(value) = props.get(key).filter(|v| !v.is_empty()) else {
            continue;
        };
        if value == app_name || GENERIC.contains(&value.to_lowercase().as_str()) {
            continue;
        }
        return Some(value.into());
    }
    None
}

/// True when a `PipeWire` client should never be offered as an audio capture
/// target: it is us, a session daemon, or a name that can never map to a
/// user-meaningful audio app (`Steam` itself, `WEBRTC VoiceEngine` utility
/// clients, …). Pid 0/1 and pids that do not fit the negative-id encoding
/// (`id = -pid`) are rejected too.
fn is_skip_client(pid: u32, name: &str, our_pid: u32) -> bool {
    const SKIP_NAMES: [&str; 7] = [
        "slopcast",
        "pipewire",
        "wireplumber",
        "libcanberra",
        "pulseaudio",
        "steam",
        "webrtc voiceengine",
    ];
    if pid <= 1 || pid > i32::MAX as u32 {
        return true;
    }
    if pid == our_pid {
        return true;
    }
    if name.trim().is_empty() {
        return true;
    }
    let lower = name.to_lowercase();
    if SKIP_NAMES.iter().any(|k| lower.contains(k)) {
        return true;
    }
    is_system_or_session_daemon(pid)
}

/// Appends `PipeWire` clients that are connected to the daemon but currently
/// have no `Stream/Output/Audio` node — e.g. Spotify while paused. They are
/// selectable as process-id targets (`id = -pid`); the capture session links
/// their audio the moment it starts playing. Clients whose pid already owns a
/// stream node are skipped (the stream entries carry the rich metadata).
fn append_idle_clients(apps: &mut Vec<AudioApp>, clients: HashMap<u32, (u32, String)>) {
    let our_pid = std::process::id();
    let stream_pids: HashSet<u32> = apps
        .iter()
        .filter_map(|app| (app.process_id > 0).then_some(app.process_id.cast_unsigned()))
        .collect();
    // Prefer the friendliest name per pid: the shortest one sorts first
    // (e.g. "Chromium" before "Chromium input").
    let mut idle: Vec<(u32, String)> = clients.into_values().collect();
    idle.sort_by(|(pid_a, name_a), (pid_b, name_b)| {
        (pid_a, name_a.len()).cmp(&(pid_b, name_b.len()))
    });
    let mut seen_pids = HashSet::new();
    for (pid, name) in idle {
        if !seen_pids.insert(pid)
            || is_skip_client(pid, &name, our_pid)
            || stream_pids.contains(&pid)
        {
            continue;
        }
        let pid_signed = pid.cast_signed();
        apps.push(AudioApp {
            id: -pid_signed,
            name,
            process_id: pid_signed,
            bundle_id: None,
            window_title: None,
            client_id: None,
            media_title: None,
        });
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "PipeWire registry event handling for client PID collection is inherently long and operates as a single logical unit; splitting would harm readability"
)]
fn collect_client_pids(
    core: &pipewire::core::CoreRc,
    main_loop: &pipewire::main_loop::MainLoopRc,
    registry: &pipewire::registry::Registry,
    apps: &Rc<RefCell<Vec<AudioApp>>>,
) -> (HashMap<u32, u32>, HashMap<u32, (u32, String)>) {
    let client_pids: Rc<RefCell<HashMap<u32, u32>>> = Rc::new(RefCell::new(HashMap::new()));
    let client_info: Rc<RefCell<HashMap<u32, (u32, String)>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let cp = client_pids.clone();
    let ci = client_info.clone();
    let ap = apps.clone();
    // Node info props (e.g. `media.name`, where browsers put the tab title) are
    // not part of the registry advertisement — they only arrive after binding
    // the node. Audio stream nodes are bound here and kept until the second
    // sync round below has delivered their info events.
    let bound_nodes: Rc<RefCell<Vec<(pipewire::node::Node, pipewire::node::NodeListener)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let bindings = bound_nodes.clone();
    let bind_registry: Rc<RefCell<Option<pipewire::registry::RegistryRc>>> =
        Rc::new(RefCell::new(None));
    let reg_cell = bind_registry.clone();
    let core_rc = core.clone();

    let proc_list = Rc::new(iter_proc());
    let proc_list_cb = proc_list.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props else { return };

            if global.type_ == ObjectType::Client {
                let app_name = props.get("application.name").unwrap_or("");
                let pid = client_sec_pid(props)
                    .or_else(|| {
                        props
                            .get("application.process.id")
                            .and_then(|v| v.parse::<i32>().ok())
                            .filter(|p| is_valid_pid(*p))
                    })
                    .or_else(|| {
                        (!app_name.is_empty())
                            .then(|| resolve_pid_by_name(&proc_list_cb, app_name))
                            .flatten()
                    });
                if let Some(pid) = pid {
                    cp.borrow_mut().insert(global.id, pid.cast_unsigned());
                    if !app_name.is_empty() {
                        ci.borrow_mut()
                            .insert(global.id, (pid.cast_unsigned(), app_name.to_string()));
                    }
                }
            }

            let media_class = props.get("media.class").unwrap_or("");
            let stream_name = props
                .get("application.name")
                .or_else(|| props.get("node.name"))
                .or_else(|| props.get("media.name"))
                .unwrap_or("");

            if media_class == "Stream/Output/Audio"
                && !stream_name.is_empty()
                && !stream_name.contains(CAPTURE_NODE_NAME)
            {
                let our_pid = std::process::id().cast_signed();
                let pid = props
                    .get("application.process.id")
                    .and_then(|v| v.parse::<i32>().ok())
                    .filter(|p| is_valid_pid(*p))
                    .or_else(|| {
                        let cid = props.get("client.id").and_then(|c| c.parse::<u32>().ok())?;
                        let p = *cp.borrow().get(&cid)?;
                        is_valid_pid(p.cast_signed()).then_some(p.cast_signed())
                    })
                    .or_else(|| {
                        resolve_pid_by_binary(
                            &proc_list_cb,
                            props.get("application.process.binary").unwrap_or(""),
                        )
                    })
                    .or_else(|| resolve_pid_by_name(&proc_list_cb, stream_name))
                    .unwrap_or(0);

                if pid == our_pid || stream_name.to_lowercase().contains("slopcast") {
                    return;
                }

                let mut list = ap.borrow_mut();
                if !list.iter().any(|a| a.id == global.id.cast_signed()) {
                    let client_id = props
                        .get("client.id")
                        .and_then(|v| v.parse::<u32>().ok())
                        .filter(|id| *id > 0);

                    list.push(AudioApp {
                        id: global.id.cast_signed(),
                        name: stream_name.into(),
                        process_id: pid,
                        bundle_id: None,
                        window_title: stream_window_title(props, stream_name),
                        client_id: client_id.map(u32::cast_signed),
                        media_title: None,
                    });
                    drop(list);

                    if reg_cell.borrow().is_none() {
                        *reg_cell.borrow_mut() = core_rc.get_registry_rc().ok();
                    }
                    let Some(node) = reg_cell
                        .borrow()
                        .as_ref()
                        .and_then(|reg| reg.bind::<pipewire::node::Node, _>(global).ok())
                    else {
                        return;
                    };
                    let node_id = global.id;
                    let apps_info = ap.clone();
                    let listener = node
                        .add_listener_local()
                        .info(move |info| {
                            let Some(props) = info.props() else { return };
                            let mut list = apps_info.borrow_mut();
                            let Some(app) = list.iter_mut().find(|a| a.id == node_id.cast_signed())
                            else {
                                return;
                            };
                            let title = stream_window_title(props, &app.name);
                            app.window_title = title;
                        })
                        .register();
                    bindings.borrow_mut().push((node, listener));
                }
            }
        })
        .register();

    sync_registry(core, main_loop);
    // Second round trip: bound node proxies deliver their info events.
    sync_registry(core, main_loop);
    (client_pids.take(), client_info.take())
}

/// Normalize a string for fuzzy matching: lowercase, strip non-alphanumeric.
fn norm(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Annotate audio apps with MPRIS now-playing titles.
/// MPRIS players are matched to apps by PID (when the player's bus-owner PID
/// matches the audio stream's `process_id`), then by fuzzy name containment
/// (identity/desktop-entry vs app name). Among matching players, the one with
/// `PlaybackStatus == "Playing"` wins; otherwise the first is used.
fn annotate_mpris_titles(apps: &mut [AudioApp]) {
    let players = mpris::list_players();
    if players.is_empty() {
        return;
    }
    for app in apps.iter_mut() {
        let candidates: Vec<&mpris::MprisPlayer> = players
            .iter()
            .filter(|p| {
                if p.pid
                    .is_some_and(|pid| pid > 0 && pid == app.process_id.cast_unsigned())
                {
                    return true;
                }
                let app_norm = norm(&app.name);
                let id_norm = norm(&p.identity);
                if contains_fuzzy(&app_norm, &id_norm) {
                    return true;
                }
                if let Some(de) = &p.desktop_entry {
                    let de_norm = norm(de);
                    if contains_fuzzy(&app_norm, &de_norm) {
                        return true;
                    }
                }
                false
            })
            .collect();
        let best = candidates
            .iter()
            .copied()
            .find(|p| p.playing)
            .or_else(|| candidates.first().copied());
        if let Some(player) = best
            && let Some(title) = &player.title
        {
            app.media_title = Some(title.into());
        }
    }
}

/// True when either string subsumes the other (min length 3).
fn contains_fuzzy(a: &str, b: &str) -> bool {
    if a.len() < 3 || b.len() < 3 {
        return false;
    }
    a.contains(b) || b.contains(a)
}

pub(crate) fn list_audio_applications() -> Result<Vec<AudioApp>, String> {
    let _gate = PW_ACCESS
        .lock()
        .map_err(|_| String::from("PipeWire access lock poisoned"))?;
    pipewire::init();
    let pw = pw_init().map_err(|e| format!("PipeWire init: {e}"))?;
    let registry = pw
        .core
        .get_registry()
        .map_err(|e| format!("Registry: {e}"))?;
    let apps = Rc::new(RefCell::new(Vec::<AudioApp>::new()));
    let (_, client_info) = collect_client_pids(&pw.core, &pw.main_loop, &registry, &apps);
    let mut apps = apps.take();
    append_idle_clients(&mut apps, client_info);
    annotate_mpris_titles(&mut apps);
    Ok(apps)
}

/// Full property dictionaries of every live `Stream/Output/Audio` node —
/// registry props merged with bound-node info props, the same view `pw-dump`
/// prints. Debugging aid for auto-resolve misses: the renderer logs these when
/// a capture starts so the captured window can be matched against real nodes.
type NodePropList = Vec<(u32, HashMap<String, String>)>;

pub(crate) fn dump_audio_sources() -> Result<Vec<HashMap<String, String>>, String> {
    let _gate = PW_ACCESS
        .lock()
        .map_err(|_| String::from("PipeWire access lock poisoned"))?;
    pipewire::init();
    let pw = pw_init().map_err(|e| format!("PipeWire init: {e}"))?;
    let registry = pw
        .core
        .get_registry()
        .map_err(|e| format!("Registry: {e}"))?;
    let nodes: Rc<RefCell<NodePropList>> = Rc::new(RefCell::new(Vec::new()));
    let bindings: Rc<RefCell<Vec<(pipewire::node::Node, pipewire::node::NodeListener)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let bind_core = pw.core.clone();
    let nodes_cb = nodes.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props else { return };
            if props.get("media.class") != Some("Stream/Output/Audio") {
                return;
            }
            let node_id = global.id;
            let merged: HashMap<String, String> = props
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let Some(node) = bind_core
                .get_registry_rc()
                .ok()
                .and_then(|reg| reg.bind::<pipewire::node::Node, _>(global).ok())
            else {
                return;
            };
            let nodes_cell = nodes_cb.clone();
            let listener = node
                .add_listener_local()
                .info(move |info| {
                    let Some(info_props) = info.props() else {
                        return;
                    };
                    let mut list = nodes_cell.borrow_mut();
                    let Some((_, map)) = list.iter_mut().find(|(id, _)| *id == node_id) else {
                        return;
                    };
                    for (k, v) in info_props.iter() {
                        map.insert(k.to_string(), v.to_string());
                    }
                })
                .register();
            bindings.borrow_mut().push((node, listener));
            nodes_cb.borrow_mut().push((node_id, merged));
        })
        .register();

    sync_registry(&pw.core, &pw.main_loop);
    // Second round trip: bound node proxies deliver their info events.
    sync_registry(&pw.core, &pw.main_loop);
    Ok(nodes.take().into_iter().map(|(_, map)| map).collect())
}

#[cfg(test)]
mod tests {
    use super::{append_idle_clients, is_skip_client};
    use crate::AudioApp;
    use std::collections::HashMap;

    fn app(process_id: i32) -> AudioApp {
        AudioApp {
            id: process_id,
            name: "stream".into(),
            process_id,
            bundle_id: None,
            window_title: None,
            client_id: None,
            media_title: None,
        }
    }

    #[test]
    fn skip_client_rejects_self_daemons_and_unusable_pids() {
        let our_pid = std::process::id();
        assert!(is_skip_client(our_pid, "Spotify", our_pid));
        assert!(is_skip_client(0, "Spotify", our_pid));
        assert!(is_skip_client(1, "init", our_pid));
        assert!(is_skip_client(i32::MAX as u32 + 1, "Spotify", our_pid));
        assert!(is_skip_client(100, "   ", our_pid));
        // A dead pid resolves to a session daemon in the /proc walk.
        assert!(is_skip_client(999_999_999, "Spotify", our_pid));
    }

    #[test]
    fn skip_client_rejects_noise_clients_but_keeps_real_apps() {
        // A pid that is definitely not the test process and never appears as
        // a session daemon in /proc: our own live pid with a different
        // "our_pid" argument.
        let live_pid = std::process::id();
        let other_pid = u32::MAX - 1;
        for name in [
            "slopcast",
            "Slopcast-Window-Audio",
            "pipewire",
            "wireplumber",
            "libcanberra",
            "pulseaudio",
            "Steam",
            "Steam Voice Settings",
            "WEBRTC VoiceEngine",
        ] {
            assert!(
                is_skip_client(live_pid, name, other_pid),
                "{name} must be skipped"
            );
        }
        for name in ["Spotify", "Firefox", "Discord", "ZenlessZoneZero.exe"] {
            assert!(
                !is_skip_client(live_pid, name, other_pid),
                "{name} must be offered as a capture target"
            );
        }
    }

    #[test]
    fn idle_clients_become_negative_pid_targets() {
        let mut apps = vec![app(42)];
        let clients = HashMap::from([(1, (43u32, "Spotify".to_string()))]);
        append_idle_clients(&mut apps, clients);
        let idle = apps
            .iter()
            .find(|a| a.name == "Spotify")
            .unwrap_or_else(|| {
                panic!("idle Spotify must be listed");
            });
        assert_eq!(idle.id, -43);
        assert_eq!(idle.process_id, 43);
    }

    #[test]
    fn idle_clients_skip_pids_with_active_streams() {
        let mut apps = vec![app(42)];
        let clients = HashMap::from([(1, (42u32, "Spotify".to_string()))]);
        append_idle_clients(&mut apps, clients);
        assert_eq!(apps.len(), 1, "a stream-owning pid must not be duplicated");
    }

    #[test]
    fn idle_clients_prefer_the_shortest_name_per_pid() {
        let mut apps = Vec::new();
        let clients = HashMap::from([
            (1, (50u32, "Chromium input".to_string())),
            (2, (50u32, "Chromium".to_string())),
        ]);
        append_idle_clients(&mut apps, clients);
        let idle = apps.iter().find(|a| a.process_id == 50).unwrap_or_else(|| {
            panic!("pid 50 must be listed");
        });
        assert_eq!(idle.name, "Chromium");
    }
}
