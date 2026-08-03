use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};
use std::thread;

use zbus::MessageStream;
use zbus::export::ordered_stream::OrderedStreamExt;
use zbus::names::{InterfaceName, MemberName};
use zbus::zvariant::{OwnedValue, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MprisPlayer {
    pub(crate) pid: Option<u32>,
    pub(crate) identity: String,
    pub(crate) desktop_entry: Option<String>,
    pub(crate) playing: bool,
    pub(crate) title: Option<String>,
}

#[derive(Clone, Debug)]
struct CachedPlayer {
    well_known_name: String,
    unique_name: Option<String>,
    player: MprisPlayer,
}

#[derive(Default)]
struct MprisState {
    players: HashMap<String, CachedPlayer>,
    unique_to_well_known: HashMap<String, String>,
}

impl MprisState {
    fn insert(&mut self, cached: CachedPlayer) {
        if let Some(ref uniq) = cached.unique_name {
            self.unique_to_well_known
                .insert(uniq.clone(), cached.well_known_name.clone());
        }
        self.players.insert(cached.well_known_name.clone(), cached);
    }

    fn remove(&mut self, name: &str) {
        if name.starts_with("org.mpris.MediaPlayer2.")
            && let Some(removed) = self.players.remove(name)
            && let Some(uniq) = removed.unique_name
        {
            self.unique_to_well_known.remove(&uniq);
        } else if name.starts_with(':')
            && let Some(well_known) = self.unique_to_well_known.remove(name)
        {
            self.players.remove(&well_known);
        }
    }

    fn get_players(&self) -> Vec<MprisPlayer> {
        self.players.values().map(|c| c.player.clone()).collect()
    }

    fn has_sender(&self, sender: Option<&str>) -> bool {
        let Some(s) = sender else { return false };
        if s.starts_with("org.mpris.MediaPlayer2.") {
            self.players.contains_key(s)
        } else {
            self.unique_to_well_known.contains_key(s)
        }
    }

    fn update_properties(
        &mut self,
        sender: Option<&str>,
        iface: &str,
        changed: HashMap<String, OwnedValue>,
    ) {
        let well_known = sender.and_then(|s| {
            if s.starts_with("org.mpris.MediaPlayer2.") {
                Some(s.to_string())
            } else {
                self.unique_to_well_known.get(s).cloned()
            }
        });

        let Some(wk) = well_known else { return };
        let Some(cached) = self.players.get_mut(&wk) else {
            return;
        };

        if iface == "org.mpris.MediaPlayer2" {
            if let Some(v) = changed.get("Identity")
                && let Value::Str(s) = &**v
            {
                cached.player.identity = s.to_string();
            }
            if let Some(v) = changed.get("DesktopEntry")
                && let Value::Str(s) = &**v
            {
                cached.player.desktop_entry = Some(s.to_string());
            }
        } else if iface == "org.mpris.MediaPlayer2.Player" {
            if let Some(v) = changed.get("PlaybackStatus")
                && let Value::Str(s) = &**v
            {
                cached.player.playing = s.as_str() == "Playing";
            }
            if let Some(v) = changed.get("Metadata") {
                cached.player.title = extract_title_from_value(v);
            }
        }
    }
}

static MONITOR: LazyLock<Arc<RwLock<MprisState>>> = LazyLock::new(|| {
    let state = Arc::new(RwLock::new(MprisState::default()));
    let state_clone = Arc::clone(&state);
    thread::spawn(move || {
        zbus::block_on(run_mpris_monitor(state_clone));
    });
    state
});

pub(crate) fn list_players() -> Vec<MprisPlayer> {
    let Ok(guard) = MONITOR.read() else {
        return vec![];
    };
    guard.get_players()
}

async fn run_mpris_monitor(state: Arc<RwLock<MprisState>>) {
    let Ok(conn) = zbus::Connection::session().await else {
        eprintln!("[mpris] D-Bus session bus unavailable");
        return;
    };

    let rule_name_owner = "type='signal',interface='org.freedesktop.DBus',member='NameOwnerChanged',path='/org/freedesktop/DBus'";
    let rule_props = "type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path='/org/mpris/MediaPlayer2'";

    let _ = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "AddMatch",
            &(rule_name_owner,),
        )
        .await;

    let _ = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "AddMatch",
            &(rule_props,),
        )
        .await;

    if let Ok(names) = dbus_list_names(&conn).await {
        let initial_players = fetch_all_players(&conn, &names).await;
        if let Ok(mut guard) = state.write() {
            for player in initial_players {
                guard.insert(player);
            }
        }
    }

    let mut stream = MessageStream::from(&conn);
    while let Some(Ok(msg)) = stream.next().await {
        let header = msg.header();
        let iface = header.interface().map(InterfaceName::as_str);
        let member = header.member().map(MemberName::as_str);

        match (iface, member) {
            (Some("org.freedesktop.DBus"), Some("NameOwnerChanged")) => {
                if let Ok((name, old_owner, new_owner)) =
                    msg.body().deserialize::<(String, String, String)>()
                {
                    handle_name_owner_changed(&conn, &state, name, old_owner, new_owner).await;
                }
            }
            (Some("org.freedesktop.DBus.Properties"), Some("PropertiesChanged")) => {
                let sender = header.sender().map(ToString::to_string);
                if let Ok((iface, changed_props, _invalidated)) =
                    msg.body()
                        .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
                {
                    handle_properties_changed(
                        &conn,
                        &state,
                        sender.as_deref(),
                        &iface,
                        changed_props,
                    )
                    .await;
                }
            }
            _ => {}
        }
    }
}

async fn handle_name_owner_changed(
    conn: &zbus::Connection,
    state: &Arc<RwLock<MprisState>>,
    name: String,
    old_owner: String,
    new_owner: String,
) {
    if !name.starts_with("org.mpris.MediaPlayer2.") {
        return;
    }

    if new_owner.is_empty() {
        if let Ok(mut guard) = state.write() {
            guard.remove(&name);
            if !old_owner.is_empty() {
                guard.remove(&old_owner);
            }
        }
    } else if let Some(cached) = fetch_player_info(conn, &name).await
        && let Ok(mut guard) = state.write()
    {
        if !old_owner.is_empty() {
            guard.remove(&old_owner);
        }
        guard.insert(cached);
    }
}

async fn handle_properties_changed(
    conn: &zbus::Connection,
    state: &Arc<RwLock<MprisState>>,
    sender: Option<&str>,
    iface: &str,
    changed_props: HashMap<String, OwnedValue>,
) {
    let is_known = if let Ok(guard) = state.read() {
        guard.has_sender(sender)
    } else {
        false
    };

    if is_known {
        if let Ok(mut guard) = state.write() {
            guard.update_properties(sender, iface, changed_props);
        }
    } else if let Some(s) = sender
        && s.starts_with("org.mpris.MediaPlayer2.")
        && let Some(cached) = fetch_player_info(conn, s).await
        && let Ok(mut guard) = state.write()
    {
        guard.insert(cached);
    }
}

async fn fetch_all_players(conn: &zbus::Connection, names: &[String]) -> Vec<CachedPlayer> {
    let futures: Vec<_> = names
        .iter()
        .filter(|n| n.starts_with("org.mpris.MediaPlayer2."))
        .map(|n| fetch_player_info(conn, n))
        .collect();

    futures_util::future::join_all(futures)
        .await
        .into_iter()
        .flatten()
        .collect()
}

async fn fetch_player_info(conn: &zbus::Connection, bus_name: &str) -> Option<CachedPlayer> {
    if !bus_name.starts_with("org.mpris.MediaPlayer2.") {
        return None;
    }

    let main_props_fut = dbus_get_all_properties(
        conn,
        bus_name,
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2",
    );
    let player_props_fut = dbus_get_all_properties(
        conn,
        bus_name,
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
    );
    let pid_fut = dbus_get_pid(conn, bus_name);
    let owner_fut = dbus_get_name_owner(conn, bus_name);

    let (main_props, player_props, pid, unique_name) =
        futures_util::join!(main_props_fut, player_props_fut, pid_fut, owner_fut);

    let identity = main_props.get("Identity").and_then(|v| match &**v {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    })?;

    let desktop_entry = main_props.get("DesktopEntry").and_then(|v| match &**v {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    });

    let playback_status = player_props.get("PlaybackStatus").and_then(|v| match &**v {
        Value::Str(s) => Some(s.as_str()),
        _ => None,
    });
    let playing = playback_status == Some("Playing");

    let title = player_props
        .get("Metadata")
        .and_then(|v| extract_title_from_value(v));

    Some(CachedPlayer {
        well_known_name: bus_name.to_string(),
        unique_name,
        player: MprisPlayer {
            pid,
            identity,
            desktop_entry,
            playing,
            title,
        },
    })
}

async fn dbus_get_all_properties(
    conn: &zbus::Connection,
    bus_name: &str,
    path: &str,
    interface: &str,
) -> HashMap<String, OwnedValue> {
    let Ok(reply) = conn
        .call_method(
            Some(bus_name),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &(interface,),
        )
        .await
    else {
        return HashMap::new();
    };
    reply
        .body()
        .deserialize::<HashMap<String, OwnedValue>>()
        .unwrap_or_default()
}

async fn dbus_list_names(conn: &zbus::Connection) -> Result<Vec<String>, zbus::Error> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListNames",
            &(),
        )
        .await?;
    reply.body().deserialize()
}

async fn dbus_get_pid(conn: &zbus::Connection, bus_name: &str) -> Option<u32> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetConnectionUnixProcessID",
            &(bus_name,),
        )
        .await
        .ok()?;
    reply.body().deserialize::<u32>().ok()
}

async fn dbus_get_name_owner(conn: &zbus::Connection, bus_name: &str) -> Option<String> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &(bus_name,),
        )
        .await
        .ok()?;
    reply.body().deserialize::<String>().ok()
}

fn extract_title_from_value(val: &Value) -> Option<String> {
    let Value::Dict(dict) = val else {
        return None;
    };
    for (key, val) in dict.iter() {
        let Value::Str(k) = key else {
            continue;
        };
        if k.as_str() != "xesam:title" {
            continue;
        }
        match val {
            Value::Str(s) if !s.is_empty() => return Some(s.to_string()),
            Value::Value(inner) => {
                let Value::Str(s) = &**inner else {
                    continue;
                };
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
            _ => {}
        }
    }
    None
}
