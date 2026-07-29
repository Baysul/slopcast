//! KDE window lookup via the `KWin` scripting D-Bus interface.
//!
//! `KWin` names its screencast streams `kwin-screencast-<objectName>`, where
//! the object name is the window's `desktopFileName` for window captures and
//! the output name (`DP-1`, `HDMI-A-1`, …) for monitor captures — never the
//! window UUID (see `ScreencastManager::streamWindow`/`streamOutput`).
//! Window metadata (PID, caption) is only exposed to `KWin`'s own scripts, so
//! we load a one-shot script that finds the window by desktop file name
//! (falling back to resource class for X11/XWayland clients) and reports the
//! PID and caption back through a D-Bus method call to an object registered
//! on our own bus name — the same mechanism `kdotool` uses, without the
//! external binary dependency.

use std::sync::mpsc;
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::interface;

const HELPER_PATH: &str = "/org/slopcast/KWinHelper";
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

const KWIN_SCRIPT: &str = r#"
var target = "__TARGET_KEY__";
var list = typeof workspace.windowList === "function" ? workspace.windowList() : workspace.clientList();
for (var i = 0; i < list.length; i++) {
    var w = list[i];
    var df = ("" + (w.desktopFileName || "")).toLowerCase();
    var rc = ("" + (w.resourceClass || "")).toLowerCase();
    if (df === target || rc === target) {
        callDBus("__BUS_NAME__", "/org/slopcast/KWinHelper", "org.slopcast.KWinHelper", "report", w.pid + "\n" + w.caption);
        break;
    }
}
"#;

/// A `KWin` window matched by desktop file name or resource class.
pub(crate) struct WindowMatch {
    pub pid: u32,
    pub caption: String,
}

struct Helper {
    tx: mpsc::Sender<(u32, String)>,
}

#[interface(name = "org.slopcast.KWinHelper")]
impl Helper {
    // PID and caption travel as one string: KWin's `callDBus` maps JS numbers
    // to varying D-Bus integer types, while a string always arrives as 's'.
    #[zbus(name = "report")]
    fn report(&self, payload: &str) {
        let Some((pid, caption)) = payload.split_once('\n') else {
            return;
        };
        if let Ok(pid) = pid.parse::<u32>() {
            let _ = self.tx.send((pid, caption.to_string()));
        }
    }
}

/// Removes the temporary `KWin` script file when the resolution attempt ends,
/// however it ends.
struct ScriptFile(std::path::PathBuf);

impl Drop for ScriptFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Resolve the `KWin` window whose desktop file name (or resource class)
/// equals `key`. Best-effort: `None` on any D-Bus, scripting, or timeout
/// failure.
pub(crate) fn resolve_window(key: &str) -> Option<WindowMatch> {
    // The key is interpolated into JavaScript, so reject anything that could
    // break out of the string literal.
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return None;
    }
    let target = key.to_lowercase();

    let conn = Connection::session().ok()?;
    let (tx, rx) = mpsc::channel::<(u32, String)>();
    conn.object_server().at(HELPER_PATH, Helper { tx }).ok()?;
    let bus_name = format!("org.slopcast.KWinHelper{}", std::process::id());
    conn.request_name(bus_name.as_str()).ok()?;

    let script_stem = format!("slopcast-kwin-helper-{}", std::process::id());
    let script_path = std::env::temp_dir().join(format!("{script_stem}.js"));
    let script = KWIN_SCRIPT
        .replace("__BUS_NAME__", &bus_name)
        .replace("__TARGET_KEY__", &target);
    std::fs::write(&script_path, script).ok()?;
    let _script_file = ScriptFile(script_path.clone());

    load_and_run_script(&conn, &script_path)?;
    let found = rx.recv_timeout(REPLY_TIMEOUT).ok();

    unload_script(&conn, &script_stem);
    found.map(|(pid, caption)| WindowMatch { pid, caption })
}

fn load_and_run_script(conn: &Connection, path: &std::path::Path) -> Option<()> {
    let path_str = path.to_str()?;
    // Replies are ignored on purpose: the loadScript return type differs
    // between KWin 5 and 6, and a failure surfaces as a missing report.
    conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "loadScript",
        &(path_str,),
    )
    .ok()?;
    conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "start",
        &(),
    )
    .ok()?;
    Some(())
}

fn unload_script(conn: &Connection, stem: &str) {
    let _ = conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "unloadScript",
        &(stem,),
    );
}
