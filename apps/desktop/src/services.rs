//! Desktop services dispatch the same commands as the visible presenter.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use desktop_core::presenter::{DesktopAction, Presenter, Snapshot};
use desktop_core::shortcuts::{Chord, ShortcutReport};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};

#[derive(Clone)]
pub enum Signal {
    ShowMain,
    Settings,
    Action(DesktopAction),
    TrayReady(bool),
    ShortcutsBound(u64, BTreeMap<String, String>, ShortcutReport),
    ShortcutStatus(u64, ShortcutReport),
    Error(String),
}

pub struct Services {
    signals: async_channel::Sender<Signal>,
    #[cfg(target_os = "linux")]
    runtime: tokio::runtime::Handle,
    #[cfg(target_os = "linux")]
    wayland: bool,
    native: Option<GlobalHotKeyManager>,
    running: Arc<AtomicBool>,
    event_thread: Option<std::thread::JoinHandle<()>>,
    bindings: BTreeMap<String, HotKey>,
    generation: u64,
    actions: Arc<Mutex<BTreeMap<u32, DesktopAction>>>,
    #[cfg(target_os = "linux")]
    portal: Option<tokio::sync::oneshot::Sender<()>>,
    #[cfg(target_os = "linux")]
    pending_portal: Option<tokio::sync::oneshot::Sender<()>>,
    #[cfg(target_os = "linux")]
    tray: tokio::task::JoinHandle<()>,
    #[cfg(target_os = "windows")]
    tray: Option<crate::tray::WindowsTray>,
}

impl Services {
    pub fn new(presenter: &Presenter, signals: async_channel::Sender<Signal>) -> Self {
        let view = presenter.snapshot();
        #[cfg(target_os = "linux")]
        let runtime = presenter.runtime();
        let actions = Arc::new(Mutex::new(BTreeMap::<u32, DesktopAction>::new()));
        let native_actions = Arc::clone(&actions);
        let events = signals.clone();
        let running = Arc::new(AtomicBool::new(true));
        let active = Arc::clone(&running);
        let event_thread = if view.platform.is_wayland {
            None
        } else {
            match std::thread::Builder::new()
                .name("desktop-shortcuts".into())
                .spawn(move || {
                    while active.load(Ordering::Acquire) {
                        if let Ok(event) = GlobalHotKeyEvent::receiver()
                            .recv_timeout(std::time::Duration::from_millis(100))
                        {
                            log::debug!("Global shortcut event: {} {:?}", event.id, event.state);
                            if event.state == HotKeyState::Pressed
                                && let Ok(actions) = native_actions.lock()
                                && let Some(action) = actions.get(&event.id)
                            {
                                let _ = events.try_send(Signal::Action(*action));
                            }
                        }
                    }
                }) {
                Ok(thread) => Some(thread),
                Err(error) => {
                    let _ = signals.try_send(Signal::Error(format!(
                        "Shortcut listener unavailable: {error}"
                    )));
                    None
                }
            }
        };
        let mut services = Self {
            signals: signals.clone(),
            #[cfg(target_os = "linux")]
            runtime: runtime.clone(),
            #[cfg(target_os = "linux")]
            wayland: view.platform.is_wayland,
            native: None,
            running,
            event_thread,
            bindings: BTreeMap::new(),
            generation: 0,
            actions,
            #[cfg(target_os = "linux")]
            portal: None,
            #[cfg(target_os = "linux")]
            pending_portal: None,
            #[cfg(target_os = "linux")]
            tray: runtime.spawn(crate::tray::run(presenter.subscribe(), signals.clone())),
            #[cfg(target_os = "windows")]
            tray: None,
        };
        services.configure_shortcuts(view.preferences.shortcuts.clone());
        services.sync(&view);
        services
    }

    pub fn sync(&mut self, view: &Snapshot) {
        #[cfg(target_os = "windows")]
        {
            if view.preferences.tray_enabled && self.tray.is_none() {
                match crate::tray::WindowsTray::new(self.signals.clone()) {
                    Ok(tray) => {
                        self.tray = Some(tray);
                        let _ = self.signals.try_send(Signal::TrayReady(true));
                    }
                    Err(error) => {
                        let _ = self.signals.try_send(Signal::Error(error));
                    }
                }
            } else if !view.preferences.tray_enabled && self.tray.take().is_some() {
                let _ = self.signals.try_send(Signal::TrayReady(false));
            }
            if let Some(tray) = &self.tray {
                tray.update(view);
            }
        }
        #[cfg(not(target_os = "windows"))]
        let _ = view;
    }

    pub fn configure_shortcuts(&mut self, requested: BTreeMap<String, String>) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        #[cfg(target_os = "linux")]
        if self.wayland {
            if let Some(cancel) = self.pending_portal.take() {
                let _ = cancel.send(());
            }
            let (cancel, cancelled) = tokio::sync::oneshot::channel();
            self.pending_portal = Some(cancel);
            let signals = self.signals.clone();
            self.runtime.spawn(crate::portal::run(
                generation, requested, signals, cancelled,
            ));
            return;
        }
        let report = match self.register_native(&requested) {
            Ok(bindings) => Signal::ShortcutsBound(
                generation,
                requested,
                ShortcutReport {
                    backend: "native".into(),
                    status: "Global shortcuts are active, even when Slopcast is not focused."
                        .into(),
                    bindings,
                    can_configure: false,
                },
            ),
            Err(error) => {
                log::warn!("Shortcut registration failed: {error}");
                Signal::ShortcutStatus(
                    generation,
                    ShortcutReport {
                        backend: "native".into(),
                        status: format!(
                            "Global shortcuts unavailable: {error}. Retry in Settings."
                        ),
                        ..Default::default()
                    },
                )
            }
        };
        let _ = self.signals.try_send(report);
    }

    pub fn accepts_shortcuts(&self, generation: u64) -> bool {
        self.generation == generation
    }

    pub fn commit_shortcuts(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(next) = self.pending_portal.take()
            && let Some(previous) = self.portal.replace(next)
        {
            let _ = previous.send(());
        }
    }

    pub fn rollback_shortcuts(&mut self, previous: &BTreeMap<String, String>) {
        #[cfg(target_os = "linux")]
        if self.wayland {
            if let Some(cancel) = self.pending_portal.take() {
                let _ = cancel.send(());
            }
            return;
        }
        if let Err(error) = self.register_native(previous) {
            let _ = self.signals.try_send(Signal::Error(format!(
                "Cannot restore the previous global shortcuts: {error}"
            )));
        }
    }

    fn register_native(
        &mut self,
        requested: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>, String> {
        if self.native.is_none() {
            self.native = Some(GlobalHotKeyManager::new().map_err(|e| e.to_string())?);
        }
        let manager = self.native.as_ref().ok_or("Shortcut manager unavailable")?;
        let next = requested
            .iter()
            .filter(|(_, chord)| !chord.is_empty())
            .map(|(action, chord)| {
                let parsed = Chord::parse(chord)?;
                let key = parsed
                    .to_storage()
                    .parse::<HotKey>()
                    .map_err(|e| e.to_string())?;
                Ok((action.clone(), key))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let mut added = Vec::new();
        for key in next.values() {
            if !self.bindings.values().any(|old| old.id() == key.id()) {
                if let Err(error) = manager.register(*key) {
                    for key in added {
                        let _ = manager.unregister(key);
                    }
                    return Err(error.to_string());
                }
                log::debug!("Registered native shortcut {}", key.id());
                added.push(*key);
            }
        }
        for key in self.bindings.values() {
            if !next.values().any(|next| next.id() == key.id()) {
                let _ = manager.unregister(*key);
            }
        }
        let mut actions = self.actions.lock().map_err(|e| e.to_string())?;
        actions.clear();
        for (id, key) in &next {
            if let Some(action) = DesktopAction::from_id(id) {
                actions.insert(key.id(), action);
            }
        }
        self.bindings = next;
        Ok(requested
            .iter()
            .filter(|(_, key)| !key.is_empty())
            .map(|(id, key)| (id.clone(), key.clone()))
            .collect())
    }
}

impl Drop for Services {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            if let Some(cancel) = self.portal.take() {
                let _ = cancel.send(());
            }
            if let Some(cancel) = self.pending_portal.take() {
                let _ = cancel.send(());
            }
            self.tray.abort();
        }
        self.running.store(false, Ordering::Release);
        if let Some(thread) = self.event_thread.take() {
            let _ = thread.join();
        }
    }
}
