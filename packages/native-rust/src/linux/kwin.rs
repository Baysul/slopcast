use std::sync::mpsc;
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::interface;

const HELPER_PATH: &str = "/org/slopcast/KWinHelper";
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

const KWIN_SCRIPT: &str = r#"
var target = "__TARGET_KEY__";
var targetStem = target.replace(/\.exe$/, "");
var list = typeof workspace.windowList === "function" ? workspace.windowList() : workspace.clientList();

function findMatch(exactOnly) {
    for (var i = 0; i < list.length; i++) {
        var w = list[i];
        var df = ("" + (w.desktopFileName || "")).toLowerCase();
        var rc = ("" + (w.resourceClass || "")).toLowerCase();
        var rn = ("" + (w.resourceName || "")).toLowerCase();
        var dfStem = df.replace(/\.exe$/, "");
        var rcStem = rc.replace(/\.exe$/, "");
        var rnStem = rn.replace(/\.exe$/, "");
        var title = w.title || w.caption || "";

        var exact = df === target || rc === target || rn === target ||
            dfStem === targetStem || rcStem === targetStem || rnStem === targetStem;

        if (exact) {
            return { pid: w.pid, title: title };
        }

        if (!exactOnly && targetStem.length >= 3) {
            if ((dfStem.length >= 3 && (dfStem.indexOf(targetStem) !== -1 || targetStem.indexOf(dfStem) !== -1)) ||
                (rcStem.length >= 3 && (rcStem.indexOf(targetStem) !== -1 || targetStem.indexOf(rcStem) !== -1)) ||
                (rnStem.length >= 3 && (rnStem.indexOf(targetStem) !== -1 || targetStem.indexOf(rnStem) !== -1))) {
                return { pid: w.pid, title: title };
            }
        }
    }
    return null;
}

var found = findMatch(true) || findMatch(false);
if (found) {
    callDBus("__BUS_NAME__", "/org/slopcast/KWinHelper", "org.slopcast.KWinHelper", "report", found.pid + "\n" + found.title);
}
"#;

pub(crate) struct WindowMatch {
    pub pid: u32,
    pub caption: String,
}

struct Helper {
    tx: mpsc::Sender<(u32, String)>,
}

#[interface(name = "org.slopcast.KWinHelper")]
impl Helper {
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

struct ScriptFile(std::path::PathBuf);

impl Drop for ScriptFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(crate) fn resolve_window(key: &str) -> Option<WindowMatch> {
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
