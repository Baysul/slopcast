//! Wayland global shortcuts, attributed to Slopcast's desktop identity.

use crate::services::Signal;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use desktop_core::preferences::ACTIONS;
use desktop_core::shortcuts::{ShortcutReport, portal_trigger};
use futures_util::StreamExt;
use std::collections::BTreeMap;

pub async fn run(
    generation: u64,
    bindings: BTreeMap<String, String>,
    signals: async_channel::Sender<Signal>,
    mut cancelled: tokio::sync::oneshot::Receiver<()>,
) {
    let result = connect().await;
    let (proxy, session) = match result {
        Ok(connection) => connection,
        Err(error) => {
            unavailable(generation, &signals, error).await;
            return;
        }
    };
    let registration = async {
        // Subscribe before approval: some desktops activate immediately after binding.
        let mut activated = proxy.receive_activated().await.map_err(|e| e.to_string())?;
        let mut changed = proxy
            .receive_shortcuts_changed()
            .await
            .map_err(|e| e.to_string())?;
        let mut closed = session.receive_closed().await.map_err(|e| e.to_string())?;
        let path: String =
            serde_json::from_value(serde_json::to_value(&session).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let shortcuts = ACTIONS
            .iter()
            .filter_map(|(id, label, _)| {
                bindings
                    .get(*id)
                    .filter(|chord| !chord.is_empty())
                    .map(|chord| {
                        NewShortcut::new(*id, *label)
                            .preferred_trigger(Some(portal_trigger(chord).as_str()))
                    })
            })
            .collect::<Vec<_>>();
        let active = if shortcuts.is_empty() {
            BTreeMap::new()
        } else {
            proxy
                .bind_shortcuts(&session, &shortcuts, None, Default::default())
                .await
                .map_err(|e| e.to_string())?
                .response()
                .map_err(|e| e.to_string())?
                .shortcuts()
                .iter()
                .map(|s| (s.id().to_string(), s.trigger_description().to_string()))
                .collect()
        };
        let mut report = ShortcutReport {
            backend: "portal".into(),
            status: "Global shortcuts approved by your desktop.".into(),
            bindings: active,
            can_configure: false,
        };
        signals
            .send(Signal::ShortcutsBound(generation, bindings, report.clone()))
            .await
            .map_err(|e| e.to_string())?;
        loop {
            tokio::select! {
                event = activated.next() => match event {
                    Some(event) if event.session_handle().as_str() == path => {
                        if let Some(action) = desktop_core::presenter::DesktopAction::from_id(event.shortcut_id()) {
                            signals.send(Signal::Action(action)).await.map_err(|e|e.to_string())?;
                        }
                    }
                    Some(_) => {},
                    None => break,
                },
                event = changed.next() => match event {
                    Some(event) if event.session_handle().as_str() == path => {
                        report.bindings = event.shortcuts().iter().map(|s|(s.id().to_string(),s.trigger_description().to_string())).collect();
                        let _ = signals.send(Signal::ShortcutStatus(generation, report.clone())).await;
                    }
                    Some(_) => {},
                    None => break,
                },
                _ = closed.next() => return Err("The desktop shortcut session closed. Retry in Settings.".into()),
            }
        }
        Ok::<(), String>(())
    };
    tokio::select! {
        result = registration => { if let Err(error) = result { unavailable(generation,&signals,error).await; } },
        _ = &mut cancelled => {},
    }
    let _ = session.close().await;
}

async fn unavailable(generation: u64, signals: &async_channel::Sender<Signal>, error: String) {
    let _ = signals
        .send(Signal::ShortcutStatus(
            generation,
            ShortcutReport {
                backend: "portal".into(),
                status: format!("Global shortcuts unavailable: {error}"),
                ..Default::default()
            },
        ))
        .await;
}

async fn connect() -> Result<(GlobalShortcuts, ashpd::desktop::Session<GlobalShortcuts>), String> {
    let id = "org.slopcast.Slopcast";
    let applications = dirs::data_dir()
        .ok_or("Desktop data directory unavailable")?
        .join("applications");
    std::fs::create_dir_all(&applications).map_err(|e| e.to_string())?;
    let executable = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
        .replace('%', "%%");
    std::fs::write(applications.join(format!("{id}.desktop")),format!("[Desktop Entry]\nType=Application\nName=Slopcast\nExec=\"{executable}\"\nIcon=slopcast\nNoDisplay=true\n")).map_err(|e|e.to_string())?;
    let connection = ashpd::zbus::Connection::session()
        .await
        .map_err(|e| e.to_string())?;
    ashpd::register_host_app_with_connection(
        connection.clone(),
        id.parse().map_err(|e: ashpd::Error| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    let proxy = GlobalShortcuts::with_connection(connection)
        .await
        .map_err(|e| e.to_string())?;
    let session = proxy
        .create_session(Default::default())
        .await
        .map_err(|e| e.to_string())?;
    Ok((proxy, session))
}
