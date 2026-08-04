//! Phase A: xdg-desktop-portal `ScreenCast` client (zbus, blocking).
//!
//! Mirror of libwebrtc's `screencast_portal.cc` (`m144_release`): the same
//! method sequence (`CreateSession` → `SelectSources` → `Start` →
//! `OpenPipeWireRemote` → `Session::Close`), the same option keys with the
//! same `AvailableCursorModes`/`version` guards, and the same response-code
//! semantics (0 success, 1 user-cancelled, 2 error). The session handle is
//! read from the `Request::Response` results dict, never from the
//! `CreateSession` method reply — the portal types it as `s` (spec-known
//! wart), and libwebrtc consumes it the same way.
//!
//! Threading: every request phase waits for its `Response` signal on a short
//! lived "portal-wait" thread that forwards the parsed body over an `mpsc`
//! channel; the caller applies the timeout (`recv_timeout`, the kwin.rs
//! pattern). zbus's blocking `SignalIterator::next()` cannot be interrupted,
//! so a timed-out wait leaves its thread parked on the bus until the portal
//! eventually answers (or the connection dies with the process) — bounded to
//! the "user ignored the picker" error path, and inert (no CPU) meanwhile.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedFd as ZvOwnedFd, OwnedObjectPath, OwnedValue, Value};

const PORTAL_BUS_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_REQUEST_PREFIX: &str = "/org/freedesktop/portal/desktop/request";
const SCREENCAST_INTERFACE: &str = "org.freedesktop.portal.ScreenCast";
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
const SESSION_INTERFACE: &str = "org.freedesktop.portal.Session";

/// Portal `types` bitmask for "any screen content" (monitor|window).
const CAPTURE_TYPES_ANY: u32 = 0b11;
/// Portal `cursor_mode` value for an embedded (composited) cursor.
const CURSOR_MODE_EMBEDDED: u32 = 0b10;
/// Portal `persist_mode` value for "do not persist" (restore-token wiring is
/// a documented follow-up, see SCREEN-CAPTURE-INHOUSE.md §8).
const PERSIST_MODE_NONE: u32 = 0;

/// Non-interactive request phases (`CreateSession`/`SelectSources`) resolve
/// without any user interaction; 30 s covers a stalled portal daemon.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// `Start` waits for the user to answer the picker; mirror the e2e's
/// `PICKER_TIMEOUT_MS` semantics with a generous ceiling (5 minutes).
const PICKER_TIMEOUT: Duration = Duration::from_mins(5);

/// Parsed `org.freedesktop.portal.Request::Response` signal body: the
/// response code (`u`) and the results dict (`a{sv}`).
type ResponseBody = (u32, HashMap<String, OwnedValue>);
type Options = HashMap<&'static str, Value<'static>>;

/// What `Start` returned: a negotiated stream, or the user cancelling the
/// picker (portal response code 1 — surfaced as cancellation, not an error).
pub(crate) enum StartOutcome {
    Stream(PortalStream),
    Cancelled,
}

/// The single portal stream `Start` negotiated (mirrors libwebrtc: only the
/// first `streams` tuple is consumed; node id is the targeting key, the
/// `pipewire-serial` alternative is a documented follow-up).
pub(crate) struct PortalStream {
    pub node_id: u32,
    pub source_type: Option<u32>,
    pub size: Option<(i32, i32)>,
    pub position: Option<(i32, i32)>,
    pub restore_token: Option<String>,
}

/// An open `ScreenCast` portal session. All phases must run on one thread
/// (the capture thread in the final wiring); `Drop` closes the session.
pub(crate) struct ScreenCastPortal {
    connection: Connection,
    portal: Proxy<'static>,
    session_handle: Option<String>,
    /// Set once the portal emits `Session::Closed` (its own close, or ours) —
    /// teardown then skips the redundant `Close` (libwebrtc parity).
    session_closed: Arc<AtomicBool>,
}

/// Fresh, process-unique portal token. The portal echoes the token back as
/// the last element of the request/session object path, so uniqueness within
/// the process is all that matters (libwebrtc uses a random u32).
fn next_token(prefix: &str) -> String {
    static TOKEN_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let seq = TOKEN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let seed = (u64::from(std::process::id()) << 32) | seq;
    format!("{prefix}_{seed}")
}

/// The request object path the portal will use for a token:
/// `/org/freedesktop/portal/desktop/request/<sender>/<token>` where `<sender>`
/// is our unique bus name with the leading `:` stripped and `.` replaced by
/// `_` (libwebrtc's `PrepareSignalHandle`).
fn portal_handle_path(connection: &Connection, token: &str) -> Result<String, String> {
    let unique = connection
        .unique_name()
        .ok_or_else(|| "session bus has no unique name".to_string())?;
    let sender = unique.trim_start_matches(':').replace('.', "_");
    Ok(format!("{PORTAL_REQUEST_PREFIX}/{sender}/{token}"))
}

fn portal_code_error(phase: &str, code: u32) -> String {
    match code {
        1 => format!("{phase}: user cancelled"),
        _ => format!("{phase}: portal response code {code}"),
    }
}

/// Subscribes to `org.freedesktop.portal.Request::Response` on `request_path`
/// on a short lived thread and forwards the first parsed body. The portal
/// unicasts `Response` exactly once per completed request; the thread exits
/// after the first one, so a caller that times out leaves a parked, inert
/// thread that wakes only when the portal eventually answers.
fn spawn_response_wait(
    connection: &Connection,
    request_path: &str,
) -> Result<mpsc::Receiver<Result<ResponseBody, String>>, String> {
    let (tx, rx) = mpsc::channel();
    let connection = connection.clone();
    let request_path = request_path.to_string();
    thread::Builder::new()
        .name("portal-wait".into())
        .spawn(move || {
            let Ok(proxy) = Proxy::new(
                &connection,
                PORTAL_BUS_NAME,
                request_path.as_str(),
                REQUEST_INTERFACE,
            ) else {
                let _ = tx.send(Err("request proxy".into()));
                return;
            };
            let Ok(mut iter) = proxy.receive_signal("Response") else {
                let _ = tx.send(Err("Response signal subscription".into()));
                return;
            };
            // The match rule scopes the subscription to this exact request
            // path, so the first signal is the one Response we wait for.
            if let Some(msg) = iter.next() {
                let body = msg
                    .body()
                    .deserialize::<ResponseBody>()
                    .map_err(|e| format!("parse Response signal: {e}"));
                let _ = tx.send(body);
            }
        })
        .map_err(|e| format!("portal-wait thread: {e}"))?;
    Ok(rx)
}

impl ScreenCastPortal {
    /// Opens a portal connection, creates the `ScreenCast` session, and
    /// subscribes to `Session::Closed`. On error the session (if created) is
    /// closed by `Drop`.
    pub(crate) fn connect(connection: Connection) -> Result<Self, String> {
        let portal = Proxy::new_owned(
            connection.clone(),
            PORTAL_BUS_NAME,
            PORTAL_OBJECT_PATH,
            SCREENCAST_INTERFACE,
        )
        .map_err(|e| format!("ScreenCast proxy: {e}"))?;
        let mut client = Self {
            connection,
            portal,
            session_handle: None,
            session_closed: Arc::new(AtomicBool::new(false)),
        };
        client.create_session()?;
        client.subscribe_session_closed()?;
        Ok(client)
    }

    fn create_session(&mut self) -> Result<(), String> {
        let request_token = next_token("slopcast");
        let request_path = portal_handle_path(&self.connection, &request_token)?;
        let rx = spawn_response_wait(&self.connection, &request_path)?;

        let mut options: Options = HashMap::new();
        options.insert(
            "session_handle_token",
            Value::new(next_token("slopcast_session")),
        );
        options.insert("handle_token", Value::new(request_token));
        let _reply: (OwnedObjectPath,) = self
            .portal
            .call("CreateSession", &options)
            .map_err(|e| format!("CreateSession call: {e}"))?;

        let (code, results) = rx
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|e| format!("CreateSession: {e}"))??;
        if code != 0 {
            return Err(portal_code_error("CreateSession", code));
        }
        let handle = results
            .get("session_handle")
            .ok_or_else(|| "CreateSession: missing session_handle".into())
            .and_then(|v| {
                v.downcast_ref::<String>()
                    .map_err(|e| format!("CreateSession: session_handle: {e}"))
            })?;
        ObjectPath::try_from(handle.as_str())
            .map_err(|e| format!("CreateSession: session_handle {handle:?} is not a path: {e}"))?;
        self.session_handle = Some(handle);
        Ok(())
    }

    /// Watches for `Session::Closed` on a thread and flips the teardown flag.
    /// The portal emits `Closed` exactly once per session, so this thread
    /// always wakes and exits.
    fn subscribe_session_closed(&self) -> Result<(), String> {
        let Some(handle) = self.session_handle.clone() else {
            return Err("subscribe_session_closed: no session".into());
        };
        let closed = self.session_closed.clone();
        let connection = self.connection.clone();
        thread::Builder::new()
            .name("portal-session".into())
            .spawn(move || {
                let Ok(path) = ObjectPath::try_from(handle.as_str()) else {
                    return;
                };
                let Ok(proxy) = Proxy::new(&connection, PORTAL_BUS_NAME, path, SESSION_INTERFACE)
                else {
                    return;
                };
                let Ok(mut iter) = proxy.receive_signal("Closed") else {
                    return;
                };
                if let Some(_msg) = iter.next() {
                    closed.store(true, Ordering::Relaxed);
                }
            })
            .map_err(|e| format!("portal-session thread: {e}"))?;
        Ok(())
    }

    fn session_path(&self) -> Result<ObjectPath<'_>, String> {
        let handle = self
            .session_handle
            .as_deref()
            .ok_or_else(|| "no active portal session".to_string())?;
        ObjectPath::try_from(handle).map_err(|e| format!("invalid session path: {e}"))
    }

    /// `SelectSources` with libwebrtc's exact options: `types` = any screen
    /// content (monitor|window), `multiple` = false, `cursor_mode` = embedded
    /// only when advertised in `AvailableCursorModes`, `persist_mode` = 0 only
    /// when the portal `version` is ≥ 4.
    pub(crate) fn select_sources(&self) -> Result<(), String> {
        let session = self.session_path()?;
        let request_token = next_token("slopcast");
        let request_path = portal_handle_path(&self.connection, &request_token)?;
        let rx = spawn_response_wait(&self.connection, &request_path)?;

        let mut options: Options = HashMap::new();
        options.insert("types", Value::U32(CAPTURE_TYPES_ANY));
        options.insert("multiple", Value::Bool(false));
        // Setting a cursor mode the portal does not advertise closes the
        // session, hence the bitmask guard (libwebrtc's exact check).
        if let Ok(modes) = self.portal.get_property::<u32>("AvailableCursorModes")
            && modes & CURSOR_MODE_EMBEDDED != 0
        {
            options.insert("cursor_mode", Value::U32(CURSOR_MODE_EMBEDDED));
        }
        // persist_mode/restore_token are v4+ options; passing them to an
        // older portal also closes the session.
        if let Ok(version) = self.portal.get_property::<u32>("version")
            && version >= 4
        {
            options.insert("persist_mode", Value::U32(PERSIST_MODE_NONE));
        }
        options.insert("handle_token", Value::new(request_token));
        let _reply: (OwnedObjectPath,) = self
            .portal
            .call("SelectSources", &(session.as_ref(), &options))
            .map_err(|e| format!("SelectSources call: {e}"))?;

        let (code, _results) = rx
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|e| format!("SelectSources: {e}"))??;
        if code != 0 {
            // Unlike `Start`, a cancelled/errored `SelectSources` is treated
            // as an error (libwebrtc maps any non-zero code to kError here).
            return Err(portal_code_error("SelectSources", code));
        }
        Ok(())
    }

    /// `Start` with an empty `parent_window` (Wayland has no x11 id); blocks
    /// until the user answers the picker (or `PICKER_TIMEOUT` elapses).
    pub(crate) fn start(&self) -> Result<StartOutcome, String> {
        let session = self.session_path()?;
        let request_token = next_token("slopcast");
        let request_path = portal_handle_path(&self.connection, &request_token)?;
        let rx = spawn_response_wait(&self.connection, &request_path)?;

        let options = HashMap::from([("handle_token", Value::new(request_token))]);
        let _reply: (OwnedObjectPath,) = self
            .portal
            .call("Start", &(session.as_ref(), "", &options))
            .map_err(|e| format!("Start call: {e}"))?;

        let (code, results) = rx
            .recv_timeout(PICKER_TIMEOUT)
            .map_err(|e| format!("Start: {e}"))??;
        match code {
            0 => parse_streams(&results).map(StartOutcome::Stream),
            1 => Ok(StartOutcome::Cancelled),
            code => Err(portal_code_error("Start", code)),
        }
    }

    /// Returns the isolated `PipeWire` remote fd for this session, where only
    /// the screencast node is visible. The reply carries an `h`; zbus resolves
    /// the fd index against the message's fd list.
    pub(crate) fn open_pipewire_remote(&self) -> Result<OwnedFd, String> {
        let session = self.session_path()?;
        let options: Options = HashMap::new();
        let (fd,): (ZvOwnedFd,) = self
            .portal
            .call("OpenPipeWireRemote", &(session.as_ref(), &options))
            .map_err(|e| format!("OpenPipeWireRemote call: {e}"))?;
        Ok(fd.into())
    }
}

impl Drop for ScreenCastPortal {
    fn drop(&mut self) {
        // Fire-and-forget Session::Close (libwebrtc's TearDownSession): no
        // reply handling, no blocking wait.
        if self.session_closed.load(Ordering::Relaxed) {
            return;
        }
        let Some(handle) = self.session_handle.clone() else {
            return;
        };
        // Fire-and-forget via a temp session proxy: `send` without awaiting
        // a reply (libwebrtc's TearDownSession does the same).
        let Ok(session) = ObjectPath::try_from(handle.as_str()) else {
            return;
        };
        let Ok(proxy) = Proxy::new(
            &self.connection,
            PORTAL_BUS_NAME,
            session,
            SESSION_INTERFACE,
        ) else {
            return;
        };
        let _ = proxy.call_noreply("Close", &());
    }
}

/// Consumes the first `streams` tuple (node id + props) plus the top-level
/// `restore_token` (libwebrtc's exact parse).
fn parse_streams(results: &HashMap<String, OwnedValue>) -> Result<PortalStream, String> {
    let streams: Vec<(u32, HashMap<String, OwnedValue>)> = results
        .get("streams")
        .ok_or_else(|| "Start: missing streams".to_string())?
        .clone()
        .try_into()
        .map_err(|e: zbus::zvariant::Error| format!("Start: streams: {e}"))?;
    let (node_id, props) = streams
        .first()
        .ok_or_else(|| "Start: empty streams".to_string())?;
    let source_type = props
        .get("source_type")
        .and_then(|v| v.downcast_ref::<u32>().ok());
    let size = props
        .get("size")
        .and_then(|v| <(i32, i32)>::try_from(&**v).ok());
    let position = props
        .get("position")
        .and_then(|v| <(i32, i32)>::try_from(&**v).ok());
    let restore_token = results
        .get("restore_token")
        .and_then(|v| v.downcast_ref::<String>().ok());
    Ok(PortalStream {
        node_id: *node_id,
        source_type,
        size,
        position,
        restore_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    /// Manual control probe (gate 1 of SCREEN-CAPTURE-INHOUSE.md §10): drives
    /// the full portal handshake against the real session bus. A picker
    /// appears; pick any window or monitor. Success prints the node id, the
    /// stream props, and the `PipeWire` fd, then closes the session. Run with:
    ///
    /// ```sh
    /// cargo test -p native-rust --release -- --ignored portal_probe --nocapture
    /// ```
    #[test]
    #[ignore = "manual probe: requires a Wayland session with a portal picker to answer"]
    fn portal_probe() {
        let connection = Connection::session().unwrap_or_else(|e| panic!("session bus: {e}"));
        let portal =
            ScreenCastPortal::connect(connection).unwrap_or_else(|e| panic!("create session: {e}"));
        portal
            .select_sources()
            .unwrap_or_else(|e| panic!("select sources: {e}"));
        match portal.start().unwrap_or_else(|e| panic!("start: {e}")) {
            StartOutcome::Stream(stream) => {
                eprintln!(
                    "[portal-probe] node_id={} source_type={:?} size={:?} position={:?} restore_token={:?}",
                    stream.node_id,
                    stream.source_type,
                    stream.size,
                    stream.position,
                    stream.restore_token,
                );
                let fd = portal
                    .open_pipewire_remote()
                    .unwrap_or_else(|e| panic!("open pipewire remote: {e}"));
                eprintln!("[portal-probe] pipewire fd={}", fd.as_raw_fd());
                std::thread::sleep(Duration::from_secs(2));
                eprintln!("[portal-probe] session closed cleanly");
            }
            StartOutcome::Cancelled => eprintln!("[portal-probe] cancelled by user"),
        }
    }
}
