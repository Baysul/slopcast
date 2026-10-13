use crate::AppHandle;
use crate::preferences::{ACTIONS, persist_preferences, read_preferences};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(target_os = "linux")]
use std::sync::Arc;
use std::sync::Mutex;
#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShortcutReport {
    pub backend: String,
    pub status: String,
    pub bindings: BTreeMap<String, String>,
    pub can_configure: bool,
}

#[derive(Default)]
pub(crate) struct ShortcutService {
    operation: tauri::async_runtime::Mutex<()>,
    report: Mutex<ShortcutReport>,
    recording: AtomicBool,
    #[cfg(target_os = "linux")]
    portal: tauri::async_runtime::Mutex<Option<PortalBinding>>,
    #[cfg(target_os = "linux")]
    portal_generation: AtomicU64,
}

#[cfg(target_os = "linux")]
struct PortalBinding {
    proxy: Arc<ashpd::desktop::global_shortcuts::GlobalShortcuts>,
    session: Arc<ashpd::desktop::Session<ashpd::desktop::global_shortcuts::GlobalShortcuts>>,
    listener: tauri::async_runtime::JoinHandle<()>,
    is_closed: Arc<AtomicBool>,
}

pub(crate) fn is_wayland() -> bool {
    cfg!(target_os = "linux")
        && std::env::var_os("WAYLAND_DISPLAY").is_some_and(|display| !display.is_empty())
}

pub(crate) fn dispatch(app: &AppHandle, shortcut: &Shortcut, state: ShortcutState) {
    if state != ShortcutState::Pressed {
        return;
    }
    let service = app.state::<ShortcutService>();
    if service.recording.load(Ordering::Relaxed) {
        return;
    }
    let Ok(report) = service.report.lock() else {
        return;
    };
    let action = report.bindings.iter().find_map(|(action, chord)| {
        let registered = chord.parse::<Shortcut>().ok()?;
        (registered.id() == shortcut.id()).then_some(action.as_str())
    });
    if let Some(action) = action {
        crate::controls::emit_action(app, action);
    }
}

#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command injection requires an owned AppHandle"
)]
pub(crate) fn set_shortcut_recording(app: AppHandle, recording: bool) {
    app.state::<ShortcutService>()
        .recording
        .store(recording, Ordering::Relaxed);
}

fn validate(bindings: &BTreeMap<String, String>) -> Result<(), String> {
    if bindings.len() != ACTIONS.len()
        || ACTIONS
            .iter()
            .any(|(action, _, _)| !bindings.contains_key(*action))
    {
        return Err("Provide one shortcut entry for every action".into());
    }
    let mut seen = BTreeSet::new();
    for chord in bindings.values().filter(|chord| !chord.is_empty()) {
        let shortcut = chord
            .parse::<Shortcut>()
            .map_err(|error| format!("Invalid shortcut: {error}"))?;
        let parts = chord.split('+').collect::<Vec<_>>();
        let has_control = parts.contains(&"Control") || parts.contains(&"Command");
        let has_second_modifier = parts.contains(&"Shift") || parts.contains(&"Alt");
        if !has_control || !has_second_modifier {
            return Err(
                "Use Control or Command with Shift or Alt to avoid reserved editing shortcuts"
                    .into(),
            );
        }
        if is_wayland() && parts.contains(&"Command") {
            return Err("The Super key is reserved by your desktop. Use Control instead.".into());
        }
        if !seen.insert(shortcut.id()) {
            return Err("Each action needs a different shortcut".into());
        }
    }
    Ok(())
}

pub(crate) fn validate_telemetry_shortcut(
    app: &AppHandle,
    chord: &str,
    bindings: &BTreeMap<String, String>,
) -> Result<(), String> {
    if chord.is_empty() {
        return Ok(());
    }
    if !chord
        .split('+')
        .any(|part| matches!(part, "Control" | "Command"))
    {
        return Err("Include Control or Command in the telemetry shortcut".into());
    }

    if chord.ends_with("+Comma") && !chord.contains("Shift+") && !chord.contains("Alt+") {
        return Err("That shortcut is already assigned to Settings".into());
    }

    let shortcut = chord
        .parse::<Shortcut>()
        .map_err(|error| format!("Invalid telemetry shortcut: {error}"))?;
    let service = app.state::<ShortcutService>();
    let report = service.report.lock().map_err(|error| error.to_string())?;
    let global_bindings = bindings.values().chain(report.bindings.values());
    let conflicts = global_bindings
        .filter_map(|binding| binding.parse::<Shortcut>().ok())
        .any(|global| global.id() == shortcut.id());
    if conflicts {
        return Err("That shortcut is already assigned to a global action".into());
    }

    Ok(())
}

fn publish_report(app: &AppHandle, report: ShortcutReport) -> Result<ShortcutReport, String> {
    let service = app.state::<ShortcutService>();
    let mut current = service.report.lock().map_err(|error| error.to_string())?;
    *current = report.clone();
    app.emit("shortcuts-changed", &report)
        .map_err(|error| error.to_string())?;

    Ok(report)
}

#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command injection requires an owned AppHandle"
)]
pub(crate) fn get_shortcut_status(app: AppHandle) -> Result<ShortcutReport, String> {
    let service = app.state::<ShortcutService>();
    let report = service.report.lock().map_err(|error| error.to_string())?;

    Ok(report.clone())
}

pub(crate) async fn initialize(app: AppHandle) {
    let result = async {
        let service = app.state::<ShortcutService>();
        let _operation = service.operation.lock().await;
        let preferences = read_preferences(&app)?;
        apply(&app, &preferences.shortcuts).await
    }
    .await;
    if let Err(error) = result {
        log::error!("Global shortcut registration failed: {error}");
        let report = ShortcutReport {
            backend: if is_wayland() { "portal" } else { "native" }.into(),
            status: format!("Global shortcuts unavailable: {error}. Open Settings to retry."),
            ..ShortcutReport::default()
        };
        if let Err(error) = publish_report(&app, report) {
            log::error!("Cannot publish shortcut status: {error}");
        }
    }
}

fn persist_bindings(app: &AppHandle, bindings: BTreeMap<String, String>) -> Result<(), String> {
    let controls = app.state::<crate::controls::DesktopControls>();
    let _preferences = controls
        .preferences_lock
        .lock()
        .map_err(|error| error.to_string())?;
    let mut preferences = read_preferences(app)?;
    preferences.shortcuts = bindings;

    persist_preferences(app, &preferences)?;
    if let Err(error) = app.emit("desktop-preferences-changed", &preferences) {
        log::error!("Cannot broadcast saved preferences: {error}");
    }

    Ok(())
}

#[tauri::command]
pub(crate) async fn save_shortcuts(
    app: AppHandle,
    bindings: BTreeMap<String, String>,
) -> Result<ShortcutReport, String> {
    validate(&bindings)?;
    validate_telemetry_shortcut(&app, &read_preferences(&app)?.telemetry_shortcut, &bindings)?;

    let service = app.state::<ShortcutService>();
    let _operation = service.operation.lock().await;
    let old = read_preferences(&app)?.shortcuts;
    #[cfg(target_os = "linux")]
    let old_report = service
        .report
        .lock()
        .map_err(|error| error.to_string())?
        .clone();
    #[cfg(target_os = "linux")]
    let old_generation = service.portal_generation.load(Ordering::Relaxed);
    #[cfg(target_os = "linux")]
    let previous = if is_wayland() {
        service.portal.lock().await.take()
    } else {
        None
    };
    let report = match apply(&app, &bindings).await {
        Ok(report) => report,
        Err(error) => {
            #[cfg(target_os = "linux")]
            if is_wayland() {
                *service.portal.lock().await = previous;
            }
            return Err(error);
        }
    };

    if let Err(error) = persist_bindings(&app, bindings) {
        #[cfg(target_os = "linux")]
        if is_wayland() {
            let failed = service.portal.lock().await.take();
            service
                .portal_generation
                .store(old_generation, Ordering::Relaxed);
            let mut old_report = old_report;
            if previous
                .as_ref()
                .is_some_and(|binding| binding.is_closed.load(Ordering::Relaxed))
            {
                old_report.bindings.clear();
                old_report.status =
                    "The desktop shortcut service disconnected. Open Settings to reconnect.".into();
            }
            *service.portal.lock().await = previous;
            if let Some(failed) = failed {
                failed.listener.abort();
                if let Err(cleanup) = failed.session.close().await {
                    log::error!("Cannot close uncommitted shortcut session: {cleanup}");
                }
            }
            publish_report(&app, old_report)?;
            return Err(error);
        }
        if let Err(rollback) = apply(&app, &old).await {
            return Err(format!(
                "{error}. Previous shortcut registration could not be restored: {rollback}"
            ));
        }
        return Err(error);
    }
    #[cfg(target_os = "linux")]
    if let Some(previous) = previous {
        previous.listener.abort();
        if let Err(error) = previous.session.close().await {
            log::warn!("Previous shortcut session was already unavailable: {error}");
        }
    }

    Ok(report)
}

async fn apply(
    app: &AppHandle,
    bindings: &BTreeMap<String, String>,
) -> Result<ShortcutReport, String> {
    validate(bindings)?;
    #[cfg(target_os = "linux")]
    if is_wayland() {
        return bind_portal(app, bindings).await;
    }
    let old = get_shortcut_status(app.clone())?.bindings;
    let active = bindings
        .iter()
        .filter(|(_, chord)| !chord.is_empty())
        .map(|(action, chord)| (action.clone(), chord.clone()))
        .collect::<BTreeMap<_, _>>();
    let old_chords = old.values().collect::<BTreeSet<_>>();
    let new_chords = active.values().collect::<BTreeSet<_>>();
    let mut added = Vec::new();
    for chord in new_chords.difference(&old_chords) {
        if let Err(error) = app.global_shortcut().register(chord.as_str()) {
            for added in added {
                app.global_shortcut()
                    .unregister(added)
                    .map_err(|rollback| format!("{error}; rollback failed: {rollback}"))?;
            }
            return Err(format!("Shortcut {chord} is unavailable: {error}"));
        }
        added.push(chord.as_str());
    }
    for chord in old_chords.difference(&new_chords) {
        if let Err(error) = app.global_shortcut().unregister(chord.as_str()) {
            for old in &old_chords {
                if !app.global_shortcut().is_registered(old.as_str()) {
                    app.global_shortcut()
                        .register(old.as_str())
                        .map_err(|rollback| rollback.to_string())?;
                }
            }
            for added in added {
                app.global_shortcut()
                    .unregister(added)
                    .map_err(|rollback| rollback.to_string())?;
            }
            return Err(error.to_string());
        }
    }

    publish_report(
        app,
        ShortcutReport {
            backend: "native".into(),
            status: "Global shortcuts are active, even when Slopcast is not focused.".into(),
            bindings: active,
            can_configure: false,
        },
    )
}

#[cfg(target_os = "linux")]
fn portal_trigger(chord: &str) -> String {
    chord
        .replace("Control", "CTRL")
        .replace("Shift", "SHIFT")
        .replace("Alt", "ALT")
        .replace("Key", "")
        .replace("Digit", "")
}

#[cfg(target_os = "linux")]
async fn request_portal_shortcuts(
    proxy: &ashpd::desktop::global_shortcuts::GlobalShortcuts,
    session: &ashpd::desktop::Session<ashpd::desktop::global_shortcuts::GlobalShortcuts>,
    bindings: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    use ashpd::desktop::global_shortcuts::NewShortcut;
    let shortcuts = ACTIONS
        .iter()
        .filter_map(|(action, description, _)| {
            let chord = bindings.get(*action)?;
            if chord.is_empty() {
                return None;
            }
            Some(
                NewShortcut::new(*action, *description)
                    .preferred_trigger(Some(portal_trigger(chord).as_str())),
            )
        })
        .collect::<Vec<_>>();
    if shortcuts.is_empty() {
        return Ok(BTreeMap::new());
    }

    let request = proxy
        .bind_shortcuts(
            session,
            &shortcuts,
            None,
            ashpd::desktop::global_shortcuts::BindShortcutsOptions::default(),
        )
        .await
        .map_err(|error| format!("Shortcut approval was cancelled or failed: {error}"))?;
    let response = request
        .response()
        .map_err(|error| format!("Shortcut approval was cancelled or failed: {error}"))?;

    Ok(response
        .shortcuts()
        .iter()
        .map(|shortcut| (shortcut.id().into(), shortcut.trigger_description().into()))
        .collect())
}

#[cfg(target_os = "linux")]
enum PortalEvent {
    Activated(String),
    Changed(BTreeMap<String, String>),
    Closed,
}

#[cfg(target_os = "linux")]
async fn watch_portal(
    app: AppHandle,
    proxy: Arc<ashpd::desktop::global_shortcuts::GlobalShortcuts>,
    session: Arc<ashpd::desktop::Session<ashpd::desktop::global_shortcuts::GlobalShortcuts>>,
    generation: u64,
    ready: tauri::async_runtime::Sender<Result<(), String>>,
    is_closed: Arc<AtomicBool>,
) {
    use futures_util::StreamExt;
    let setup = async {
        let activated = proxy
            .receive_activated()
            .await
            .map_err(|error| error.to_string())?;
        let changed = proxy
            .receive_shortcuts_changed()
            .await
            .map_err(|error| error.to_string())?;
        let closed = session
            .receive_closed()
            .await
            .map_err(|error| error.to_string())?;
        // ashpd keeps Session::path private; its public Serialize contract is the object path.
        let path: String = serde_json::from_value(
            serde_json::to_value(session.as_ref()).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok::<_, String>((activated, changed, closed, path))
    }
    .await;
    let (activated, changed, closed, path) = match setup {
        Ok(streams) => streams,
        Err(error) => {
            if let Err(error) = ready.send(Err(error)).await {
                log::error!("Cannot report shortcut listener failure: {error}");
            }
            return;
        }
    };
    let activations = activated
        .filter_map(|event| {
            let action =
                (event.session_handle().as_str() == path).then(|| event.shortcut_id().to_string());
            std::future::ready(action)
        })
        .map(PortalEvent::Activated);
    let updates = changed
        .filter_map(|event| {
            let bindings = (event.session_handle().as_str() == path).then(|| {
                event
                    .shortcuts()
                    .iter()
                    .map(|shortcut| (shortcut.id().into(), shortcut.trigger_description().into()))
                    .collect()
            });
            std::future::ready(bindings)
        })
        .map(PortalEvent::Changed);
    let ended = closed.map(|_| PortalEvent::Closed);
    let events =
        futures_util::stream::select(futures_util::stream::select(activations, updates), ended);
    futures_util::pin_mut!(events);

    if ready.send(Ok(())).await.is_err() {
        return;
    }
    while let Some(event) = events.next().await {
        let service = app.state::<ShortcutService>();
        if service.portal_generation.load(Ordering::Relaxed) != generation
            && !matches!(event, PortalEvent::Closed)
        {
            continue;
        }
        match event {
            PortalEvent::Activated(action) => {
                if !service.recording.load(Ordering::Relaxed) {
                    crate::controls::emit_action(&app, &action);
                }
            }
            PortalEvent::Changed(bindings) => match get_shortcut_status(app.clone()) {
                Ok(mut report) => {
                    report.bindings = bindings;
                    if let Err(error) = publish_report(&app, report) {
                        log::error!("Cannot update shortcut bindings: {error}");
                    }
                }
                Err(error) => log::error!("Cannot read shortcut bindings: {error}"),
            },
            PortalEvent::Closed => break,
        }
    }
    is_closed.store(true, Ordering::Relaxed);
    let service = app.state::<ShortcutService>();
    if service.portal_generation.load(Ordering::Relaxed) != generation {
        return;
    }
    log::warn!("GlobalShortcuts portal session disconnected");
    match get_shortcut_status(app.clone()) {
        Ok(mut report) => {
            report.bindings.clear();
            report.status =
                "The desktop shortcut service disconnected. Open Settings to reconnect.".into();
            if let Err(error) = publish_report(&app, report) {
                log::error!("Cannot update shortcut status: {error}");
            }
        }
        Err(error) => log::error!("Cannot read shortcut status: {error}"),
    }
}

#[cfg(target_os = "linux")]
async fn create_portal_connection(app: &AppHandle) -> Result<ashpd::zbus::Connection, String> {
    let portal_id = "org.slopcast.Slopcast";
    let app_id = portal_id
        .parse()
        .map_err(|error: ashpd::Error| error.to_string())?;
    let applications = app
        .path()
        .data_dir()
        .map_err(|error| error.to_string())?
        .join("applications");
    let executable_path = std::env::current_exe().map_err(|error| error.to_string())?;
    let executable = executable_path
        .to_str()
        .ok_or("The executable path is not UTF-8")?
        .replace('\\', "\\\\\\\\")
        .replace('"', "\\\\\"")
        .replace('`', "\\\\`")
        .replace('$', "\\\\$")
        .replace('%', "%%");
    let desktop_entry = format!(
        "[Desktop Entry]\nType=Application\nName=Slopcast\nExec=\"{executable}\"\nIcon=slopcast\nNoDisplay=true\n"
    );

    // Direct binary launches still need desktop metadata for portal permission attribution.
    std::fs::create_dir_all(&applications).map_err(|error| error.to_string())?;
    std::fs::write(
        applications.join(format!("{portal_id}.desktop")),
        desktop_entry,
    )
    .map_err(|error| error.to_string())?;
    let connection = ashpd::zbus::Connection::session()
        .await
        .map_err(|error| error.to_string())?;
    ashpd::register_host_app_with_connection(connection.clone(), app_id)
        .await
        .map_err(|error| format!("Cannot identify Slopcast to the desktop portal: {error}"))?;

    Ok(connection)
}

#[cfg(target_os = "linux")]
async fn bind_portal(
    app: &AppHandle,
    bindings: &BTreeMap<String, String>,
) -> Result<ShortcutReport, String> {
    use ashpd::desktop::global_shortcuts::GlobalShortcuts;
    let service = app.state::<ShortcutService>();
    let mut current = service.portal.lock().await;
    let connection = create_portal_connection(app).await?;
    let proxy = Arc::new(
        GlobalShortcuts::with_connection(connection)
            .await
            .map_err(|error| {
                format!("Your desktop must provide the GlobalShortcuts portal: {error}")
            })?,
    );
    let session = Arc::new(
        proxy
            .create_session(ashpd::desktop::CreateSessionOptions::default())
            .await
            .map_err(|error| error.to_string())?,
    );
    let generation = service.portal_generation.load(Ordering::Relaxed) + 1;
    let is_closed = Arc::new(AtomicBool::new(false));
    let (ready, mut waiting) = tauri::async_runtime::channel(1);
    let listen = watch_portal(
        app.clone(),
        Arc::clone(&proxy),
        Arc::clone(&session),
        generation,
        ready,
        Arc::clone(&is_closed),
    );
    let listener = tauri::async_runtime::spawn(listen);
    let registration = async {
        waiting
            .recv()
            .await
            .ok_or("Shortcut listener did not start")??;
        let registered = request_portal_shortcuts(&proxy, &session, bindings).await?;
        if is_closed.load(Ordering::Relaxed) {
            return Err("The desktop closed the shortcut session before approval completed".into());
        }

        Ok(registered)
    }
    .await;
    let response = match registration {
        Ok(bindings) => bindings,
        Err(error) => {
            listener.abort();
            if let Err(cleanup) = session.close().await {
                log::error!("Cannot close cancelled portal session: {cleanup}");
            }
            return Err(error);
        }
    };
    let mut report = ShortcutReport {
        backend: "portal".into(),
        status: String::new(),
        bindings: response,
        can_configure: proxy.version() >= 2,
    };

    service
        .portal_generation
        .store(generation, Ordering::Relaxed);
    if let Some(old) = current.take() {
        old.listener.abort();
        if let Err(error) = old.session.close().await {
            log::warn!("Previous shortcut session was already unavailable: {error}");
        }
    }
    if is_closed.load(Ordering::Relaxed) {
        report.bindings.clear();
        report.status =
            "The desktop shortcut service disconnected. Open Settings to reconnect.".into();
    }
    *current = Some(PortalBinding {
        proxy,
        session,
        listener,
        is_closed,
    });

    publish_report(app, report)
}

#[tauri::command]
pub(crate) async fn configure_portal_shortcuts(app: AppHandle) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let service = app.state::<ShortcutService>();
        let _operation = service.operation.lock().await;
        let binding = service.portal.lock().await;
        let binding = binding.as_ref().ok_or("Connect global shortcuts first")?;
        return binding
            .proxy
            .configure_shortcuts(
                &binding.session,
                None,
                ashpd::desktop::global_shortcuts::ConfigureShortcutsOptions::default(),
            )
            .await
            .map_err(|error| error.to_string());
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Err("Desktop shortcut configuration is only available on Wayland".into())
    }
}
