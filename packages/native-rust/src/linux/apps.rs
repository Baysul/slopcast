use super::procinfo::{
    client_sec_pid, is_valid_pid, iter_proc, resolve_pid_by_binary, resolve_pid_by_name,
};
use super::{CAPTURE_NODE_NAME, mpris, pw_init, sync_registry};
use crate::AudioApp;
use napi::Result as NapiResult;
use pipewire::spa::utils::dict::DictRef;
use pipewire::types::ObjectType;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

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

#[allow(
    clippy::too_many_lines,
    reason = "PipeWire registry event handling for client PID collection is inherently long and operates as a single logical unit; splitting would harm readability"
)]
fn collect_client_pids(
    core: &pipewire::core::CoreRc,
    main_loop: &pipewire::main_loop::MainLoopRc,
    registry: &pipewire::registry::Registry,
    apps: &Rc<RefCell<Vec<AudioApp>>>,
) -> HashMap<u32, u32> {
    let client_pids: Rc<RefCell<HashMap<u32, u32>>> = Rc::new(RefCell::new(HashMap::new()));
    let cp = client_pids.clone();
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

    let procs = Rc::new(iter_proc());
    let procs_cb = procs.clone();

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
                            .then(|| resolve_pid_by_name(&procs_cb, app_name))
                            .flatten()
                    });
                if let Some(pid) = pid {
                    cp.borrow_mut().insert(global.id, pid.cast_unsigned());
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
                            &procs_cb,
                            props.get("application.process.binary").unwrap_or(""),
                        )
                    })
                    .or_else(|| resolve_pid_by_name(&procs_cb, stream_name))
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
    client_pids.take()
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

pub(crate) fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
    pipewire::init();
    let pw = pw_init().map_err(|e| napi::Error::from_reason(format!("PipeWire init: {e}")))?;
    let registry = pw
        .core
        .get_registry()
        .map_err(|e| napi::Error::from_reason(format!("Registry: {e}")))?;
    let apps = Rc::new(RefCell::new(Vec::<AudioApp>::new()));
    collect_client_pids(&pw.core, &pw.main_loop, &registry, &apps);
    let mut apps = apps.take();
    annotate_mpris_titles(&mut apps);
    Ok(apps)
}

/// Full property dictionaries of every live `Stream/Output/Audio` node —
/// registry props merged with bound-node info props, the same view `pw-dump`
/// prints. Debugging aid for auto-resolve misses: the renderer logs these when
/// a capture starts so the captured window can be matched against real nodes.
pub(crate) fn dump_audio_sources() -> NapiResult<Vec<HashMap<String, String>>> {
    pipewire::init();
    let pw = pw_init().map_err(|e| napi::Error::from_reason(format!("PipeWire init: {e}")))?;
    let registry = pw
        .core
        .get_registry()
        .map_err(|e| napi::Error::from_reason(format!("Registry: {e}")))?;
    let nodes: Rc<RefCell<Vec<(u32, HashMap<String, String>)>>> = Rc::new(RefCell::new(Vec::new()));
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
