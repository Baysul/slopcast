use std::collections::HashMap;
use zbus::blocking::{Connection, Proxy};
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

        let Ok(proxy) =
            Proxy::new(&conn, name.as_str(), "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2")
        else {
            continue;
        };
        let Ok(identity) = proxy.get_property::<String>("Identity") else {
            continue;
        };

        let pid = dbus_get_pid(&conn, &name);
        let desktop_entry: Option<String> = proxy.get_property("DesktopEntry").ok();
        let Ok(player_proxy) = Proxy::new(
            &conn,
            name.as_str(),
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
        ) else {
            players.push(MprisPlayer { pid, identity, desktop_entry, playing: false, title: None });
            continue;
        };
        let playback_status: Option<String> = player_proxy.get_property("PlaybackStatus").ok();
        let title: Option<String> = player_proxy
            .get_property::<HashMap<String, OwnedValue>>("Metadata")
            .ok()
            .and_then(|m| {
                m.get("xesam:title")
                    .and_then(|v| match &**v {
                        Value::Str(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            });
        let playing = playback_status.as_deref() == Some("Playing");

        players.push(MprisPlayer { pid, identity, desktop_entry, playing, title });
    }

    players
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
