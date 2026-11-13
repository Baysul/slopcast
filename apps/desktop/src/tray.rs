//! Native tray adapters: D-Bus on Linux, a Win32 icon/menu on Windows.

use crate::services::Signal;
use desktop_core::presenter::{CaptureStage, DesktopAction, Snapshot};

fn menu_actions(view: &Snapshot) -> Vec<(&'static str, Signal, bool)> {
    vec![
        ("Show Slopcast", Signal::ShowMain, true),
        ("Settings…", Signal::Settings, true),
        (
            "Start sharing…",
            Signal::Action(DesktopAction::StartShare),
            view.can_start_share(),
        ),
        (
            "Stop sharing",
            Signal::Action(DesktopAction::StopShare),
            view.capture != CaptureStage::Idle,
        ),
        (
            "Close room…",
            Signal::Action(DesktopAction::CloseRoom),
            view.room.is_some() && !view.is_busy(),
        ),
        (
            if view.muted {
                "Unmute audio"
            } else {
                "Mute audio"
            },
            Signal::Action(DesktopAction::ToggleMute),
            true,
        ),
        ("Quit Slopcast…", Signal::Action(DesktopAction::Quit), true),
    ]
}

fn icon() -> Result<(Vec<u8>, u32, u32), String> {
    let image = image::load_from_memory(include_bytes!("../resources/icons/32x32.png"))
        .map_err(|e| e.to_string())?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Ok((image.into_vec(), width, height))
}

#[cfg(target_os = "linux")]
struct LinuxTray {
    view: Snapshot,
    signals: async_channel::Sender<Signal>,
    icon: ksni::Icon,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for LinuxTray {
    fn id(&self) -> String {
        "slopcast".into()
    }
    fn title(&self) -> String {
        if self.view.capture == CaptureStage::Live {
            "Slopcast · Live"
        } else {
            "Slopcast"
        }
        .into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![self.icon.clone()]
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.signals.try_send(Signal::ShowMain);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        menu_actions(&self.view)
            .into_iter()
            .map(|(label, action, enabled)| {
                ksni::menu::StandardItem {
                    label: label.into(),
                    enabled,
                    activate: Box::new(move |tray: &mut Self| {
                        let _ = tray.signals.try_send(action.clone());
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect()
    }
}

#[cfg(target_os = "linux")]
pub async fn run(
    mut snapshots: tokio::sync::watch::Receiver<Snapshot>,
    signals: async_channel::Sender<Signal>,
) {
    use ksni::TrayMethods;
    let mut handle: Option<ksni::Handle<LinuxTray>> = None;
    let mut attempted = false;
    loop {
        let view = snapshots.borrow_and_update().clone();
        if view.preferences.tray_enabled {
            if handle.is_none() && !attempted {
                attempted = true;
                let tray = icon().map(|(mut data, width, height)| {
                    for pixel in data.as_chunks_mut::<4>().0 {
                        pixel.rotate_right(1);
                    }
                    LinuxTray {
                        view: view.clone(),
                        signals: signals.clone(),
                        icon: ksni::Icon {
                            width: width as i32,
                            height: height as i32,
                            data,
                        },
                    }
                });
                match tray {
                    Ok(tray) => match tray.spawn().await {
                        Ok(tray) => {
                            handle = Some(tray);
                            let _ = signals.send(Signal::TrayReady(true)).await;
                        }
                        Err(error) => {
                            log::warn!("Cannot register the system tray: {error}");
                            let _ = signals.send(Signal::TrayReady(false)).await;
                            let _ = signals.send(Signal::Error("System tray unavailable. Enable a tray host in your desktop, or keep Slopcast open. Closing the window will ask to quit.".into())).await;
                        }
                    },
                    Err(error) => {
                        let _ = signals.send(Signal::Error(error)).await;
                    }
                }
            }
            if let Some(tray) = &handle {
                if tray.is_closed() {
                    handle = None;
                    let _ = signals.send(Signal::TrayReady(false)).await;
                } else {
                    tray.update(|tray| tray.view = view).await;
                }
            }
        } else {
            attempted = false;
            if let Some(tray) = handle.take() {
                tray.shutdown().await;
            }
            let _ = signals.send(Signal::TrayReady(false)).await;
        }
        if handle.is_some() {
            tokio::select! {
                result = snapshots.changed() => if result.is_err() { break; },
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
            }
        } else if snapshots.changed().await.is_err() {
            break;
        }
    }
    if let Some(tray) = handle {
        tray.shutdown().await;
    }
}

#[cfg(target_os = "windows")]
pub struct WindowsTray {
    _icon: tray_icon::TrayIcon,
    items: Vec<tray_icon::menu::MenuItem>,
}

#[cfg(target_os = "windows")]
impl WindowsTray {
    pub fn new(signals: async_channel::Sender<Signal>) -> Result<Self, String> {
        use tray_icon::{
            Icon, TrayIconBuilder,
            menu::{Menu, MenuEvent, MenuItem},
        };
        let menu = Menu::new();
        let names = [
            "Show Slopcast",
            "Settings…",
            "Start sharing…",
            "Stop sharing",
            "Close room…",
            "Mute audio",
            "Quit Slopcast…",
        ];
        let actions = [
            Signal::ShowMain,
            Signal::Settings,
            Signal::Action(DesktopAction::StartShare),
            Signal::Action(DesktopAction::StopShare),
            Signal::Action(DesktopAction::CloseRoom),
            Signal::Action(DesktopAction::ToggleMute),
            Signal::Action(DesktopAction::Quit),
        ];
        let mut handlers = std::collections::HashMap::new();
        let mut items = Vec::new();
        for (label, action) in names.into_iter().zip(actions) {
            let item = MenuItem::new(label, true, None);
            menu.append(&item).map_err(|e| e.to_string())?;
            handlers.insert(item.id().clone(), action);
            items.push(item);
        }
        let activation = signals.clone();
        tray_icon::TrayIconEvent::set_event_handler(Some(
            move |event: tray_icon::TrayIconEvent| {
                if matches!(
                    event,
                    tray_icon::TrayIconEvent::Click {
                        button: tray_icon::MouseButton::Left,
                        button_state: tray_icon::MouseButtonState::Up,
                        ..
                    }
                ) {
                    let _ = activation.try_send(Signal::ShowMain);
                }
            },
        ));
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(action) = handlers.get(&event.id) {
                let _ = signals.try_send(action.clone());
            }
        }));
        let (rgba, width, height) = icon()?;
        let icon = TrayIconBuilder::new()
            .with_tooltip("Slopcast")
            .with_menu_on_left_click(false)
            .with_menu(Box::new(menu))
            .with_icon(Icon::from_rgba(rgba, width, height).map_err(|e| e.to_string())?)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { _icon: icon, items })
    }
    pub fn update(&self, view: &Snapshot) {
        for (item, (label, _, enabled)) in self.items.iter().zip(menu_actions(view)) {
            item.set_text(label);
            item.set_enabled(enabled);
        }
    }
}
