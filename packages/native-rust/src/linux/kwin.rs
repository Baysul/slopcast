//! KDE window UUID → PID resolution via the `KWin` scripting D-Bus interface.
//!
//! `KWin` names its window screencast streams `kwin-screencast-<uuid>`, where the
//! UUID is the `KWin` window's `internalId`. Window metadata (including the PID)
//! is only exposed to `KWin`'s own scripts, so we load a one-shot script that
//! finds the window and reports the PID back through a D-Bus method call to an
//! object registered on our own bus name — the same mechanism `kdotool` uses,
//! without the external binary dependency.

use std::sync::mpsc;
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::interface;

const HELPER_PATH: &str = "/org/slopcast/KWinHelper";
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

const KWIN_SCRIPT: &str = r#"
var target = "__TARGET_UUID__";
var list = typeof workspace.windowList === "function" ? workspace.windowList() : workspace.clientList();
for (var i = 0; i < list.length; i++) {
    var w = list[i];
    var id = ("" + w.internalId).replace(/[{}]/g, "").toLowerCase();
    if (id === target) {
        callDBus("__BUS_NAME__", "/org/slopcast/KWinHelper", "org.slopcast.KWinHelper", "report", "" + w.pid);
        break;
    }
}
"#;

struct Helper {
    tx: mpsc::Sender<u32>,
}

#[interface(name = "org.slopcast.KWinHelper")]
impl Helper {
    // The PID travels as a string: KWin's `callDBus` maps JS numbers to varying
    // D-Bus integer types, while a string always arrives as 's'.
    #[zbus(name = "report")]
    fn report(&self, pid: &str) {
        if let Ok(pid) = pid.parse::<u32>() {
            let _ = self.tx.send(pid);
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

/// Resolve the PID owning the `KWin` window with the given internal UUID.
/// Best-effort: `None` on any D-Bus, scripting, or timeout failure.
pub(crate) fn window_uuid_to_pid(uuid: &str) -> Option<u32> {
    // The UUID is interpolated into JavaScript, so reject anything that could
    // break out of the string literal.
    if uuid.is_empty()
        || !uuid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '{' | '}' | '-'))
    {
        return None;
    }
    let target = uuid.replace(['{', '}'], "").to_lowercase();

    let conn = Connection::session().ok()?;
    let (tx, rx) = mpsc::channel::<u32>();
    conn.object_server().at(HELPER_PATH, Helper { tx }).ok()?;
    let bus_name = format!("org.slopcast.KWinHelper{}", std::process::id());
    conn.request_name(bus_name.as_str()).ok()?;

    let script_stem = format!("slopcast-kwin-helper-{}", std::process::id());
    let script_path = std::env::temp_dir().join(format!("{script_stem}.js"));
    let script = KWIN_SCRIPT
        .replace("__BUS_NAME__", &bus_name)
        .replace("__TARGET_UUID__", &target);
    std::fs::write(&script_path, script).ok()?;
    let _script_file = ScriptFile(script_path.clone());

    load_and_run_script(&conn, &script_path)?;
    let pid = rx.recv_timeout(REPLY_TIMEOUT).ok();

    unload_script(&conn, &script_stem);
    pid
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
