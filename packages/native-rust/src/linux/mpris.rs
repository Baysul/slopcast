use std::collections::HashMap;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

pub(crate) struct MprisPlayer {
    pub(crate) pid: Option<u32>,
    pub(crate) identity: String,
    pub(crate) desktop_entry: Option<String>,
    pub(crate) playing: bool,
    pub(crate) title: Option<String>,
}

pub(crate) fn list_players() -> Vec<MprisPlayer> {
    let Ok(conn) = Connection::session() else {
        eprintln!("[mpris] D-Bus session bus unavailable");
        return vec![];
    };

    let Ok(names) = dbus_list_names(&conn) else {
        eprintln!("[mpris] Failed to list D-Bus names");
        return vec![];
    };

    let mut players = Vec::new();
    for name in names {
        if !name.starts_with("org.mpris.MediaPlayer2.") {
            continue;
        }

        let main_props = dbus_get_all_properties(
            &conn,
            &name,
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2",
        );
        let Some(identity) = main_props.get("Identity").and_then(|v| match &**v {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        }) else {
            continue;
        };

        let pid = dbus_get_pid(&conn, &name);
        let desktop_entry = main_props.get("DesktopEntry").and_then(|v| match &**v {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        });

        let player_props = dbus_get_all_properties(
            &conn,
            &name,
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
        );
        let playback_status = player_props.get("PlaybackStatus").and_then(|v| match &**v {
            Value::Str(s) => Some(s.as_str()),
            _ => None,
        });

        let title = player_props.get("Metadata").and_then(|v| match &**v {
            Value::Dict(dict) => {
                for (key, val) in dict.iter() {
                    if let Value::Str(k) = key {
                        if k.as_str() == "xesam:title" {
                            if let Value::Str(s) = val {
                                if !s.is_empty() {
                                    return Some(s.to_string());
                                }
                            }
                        }
                    }
                }
                None
            }
            _ => None,
        });

        let playing = playback_status == Some("Playing");

        players.push(MprisPlayer {
            pid,
            identity,
            desktop_entry,
            playing,
            title,
        });
    }

    players
}

fn dbus_get_all_properties(
    conn: &Connection,
    bus_name: &str,
    path: &str,
    interface: &str,
) -> HashMap<String, OwnedValue> {
    let Ok(reply) = conn.call_method(
        Some(bus_name),
        path,
        Some("org.freedesktop.DBus.Properties"),
        "GetAll",
        &(interface,),
    ) else {
        return HashMap::new();
    };
    reply
        .body()
        .deserialize::<HashMap<String, OwnedValue>>()
        .unwrap_or_default()
}

fn dbus_list_names(conn: &Connection) -> Result<Vec<String>, zbus::Error> {
    let reply = conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "ListNames",
        &(),
    )?;
    reply.body().deserialize()
}

fn dbus_get_pid(conn: &Connection, bus_name: &str) -> Option<u32> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetConnectionUnixProcessID",
            &bus_name,
        )
        .ok()?;
    reply.body().deserialize::<u32>().ok()
}
