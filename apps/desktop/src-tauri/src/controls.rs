use crate::AppHandle;
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_runtime_cef::CefRuntime;

#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent renderer flags control separate tray actions and API endpoint editing"
)]
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct ControlState {
    room_open: bool,
    capture_stage: String,
    is_muted: bool,
    is_busy: bool,
    room_endpoint: Option<String>,
    endpoint_controls_disabled: bool,
    endpoint_validation_revision: u32,
}

struct TrayItems {
    start: MenuItem<CefRuntime>,
    stop: MenuItem<CefRuntime>,
    close: MenuItem<CefRuntime>,
    mute: MenuItem<CefRuntime>,
}

impl TrayItems {
    fn update(&self, state: &ControlState) -> tauri::Result<()> {
        self.start
            .set_text(if state.capture_stage == "previewing" {
                "Go live"
            } else {
                "Start screenshare…"
            })?;
        self.start
            .set_enabled(state.capture_stage != "live" && !state.is_busy)?;
        self.stop
            .set_enabled(state.capture_stage != "idle" || state.is_busy)?;
        self.close.set_enabled(state.room_open && !state.is_busy)?;
        self.mute.set_text(if state.is_muted {
            "Unmute audio"
        } else {
            "Mute audio"
        })?;

        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct DesktopControls {
    state: Mutex<ControlState>,
    tray_items: Mutex<Option<TrayItems>>,
    tray_enabled: Mutex<bool>,
    tray_pulse: Mutex<Option<crate::tray_pulse::TrayPulse>>,
    pub preferences_lock: Mutex<()>,
}

pub(crate) fn show_main(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or("Main window unavailable")?;

    window.show().map_err(|error| error.to_string())?;
    window.unminimize().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command injection requires an owned AppHandle"
)]
pub(crate) fn show_main_window(app: AppHandle) -> Result<(), String> {
    show_main(&app)
}

#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command injection requires an owned AppHandle"
)]
pub(crate) fn get_control_state(app: AppHandle) -> Result<ControlState, String> {
    let controls = app.state::<DesktopControls>();
    let state = controls.state.lock().map_err(|error| error.to_string())?;

    Ok(state.clone())
}

pub(crate) fn emit_action(app: &AppHandle, action: &str) {
    if matches!(action, "switch-source" | "close-room" | "quit")
        && let Err(error) = show_main(app)
    {
        log::error!("Cannot show Slopcast: {error}");
    }
    if let Err(error) = app.emit_to("main", "desktop-action", action) {
        log::error!("Desktop action {action} could not be delivered: {error}");
    }
}

#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command injection requires an owned AppHandle"
)]
pub(crate) fn open_settings(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("settings") {
        window.show().map_err(|error| error.to_string())?;
        window.unminimize().map_err(|error| error.to_string())?;
        return window.set_focus().map_err(|error| error.to_string());
    }

    WebviewWindowBuilder::new(
        &app,
        "settings",
        WebviewUrl::App("index.html?settings".into()),
    )
    .title("Slopcast Settings")
    .inner_size(720.0, 760.0)
    .min_inner_size(420.0, 420.0)
    .resizable(true)
    .decorations(false)
    .center()
    .build()
    .map_err(|error| error.to_string())?;

    Ok(())
}

fn tray_menu(app: &AppHandle) -> tauri::Result<(Menu<CefRuntime>, TrayItems)> {
    let show = MenuItem::with_id(app, "show", "Show Slopcast", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let start = MenuItem::with_id(
        app,
        "start-share",
        "Start screenshare…",
        false,
        None::<&str>,
    )?;
    let stop = MenuItem::with_id(app, "stop-share", "Stop screenshare", false, None::<&str>)?;
    let close = MenuItem::with_id(app, "close-room", "Close room…", false, None::<&str>)?;
    let mute = MenuItem::with_id(app, "toggle-mute", "Mute audio", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Slopcast…", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let end_separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &settings,
            &separator,
            &start,
            &stop,
            &close,
            &mute,
            &end_separator,
            &quit,
        ],
    )?;

    Ok((
        menu,
        TrayItems {
            start,
            stop,
            close,
            mute,
        },
    ))
}

pub(crate) fn set_tray_enabled(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let controls = app.state::<DesktopControls>();
    let mut is_enabled = controls
        .tray_enabled
        .lock()
        .map_err(|error| error.to_string())?;
    if let Some(tray) = app.tray_by_id("slopcast") {
        tray.set_visible(enabled)
            .map_err(|error| error.to_string())?;
    } else if enabled {
        let state = controls.state.lock().map_err(|error| error.to_string())?;
        let (menu, items) = tray_menu(app).map_err(|error| error.to_string())?;
        items.update(&state).map_err(|error| error.to_string())?;
        let icon = app
            .default_window_icon()
            .ok_or("Application icon unavailable")?;
        TrayIconBuilder::with_id("slopcast")
            .icon(icon.clone())
            .tooltip("Slopcast")
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(|app, event| match event.id().as_ref() {
                "show" => {
                    if let Err(error) = show_main(app) {
                        log::error!("Cannot show Slopcast: {error}");
                    }
                }
                "settings" => {
                    if let Err(error) = open_settings(app.clone()) {
                        log::error!("Cannot open Settings: {error}");
                    }
                }
                action => emit_action(app, action),
            })
            .on_tray_icon_event(|tray, event| {
                if matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                ) && let Err(error) = show_main(tray.app_handle())
                {
                    log::error!("Cannot show Slopcast: {error}");
                }
            })
            .build(app)
            .map_err(|error| error.to_string())?;
        *controls
            .tray_items
            .lock()
            .map_err(|error| error.to_string())? = Some(items);
    }
    if !enabled {
        show_main(app)?;
    }
    *is_enabled = enabled;
    drop(is_enabled);
    update_tray_pulse(app)?;

    Ok(())
}

#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command injection requires an owned AppHandle"
)]
pub(crate) fn update_control_state(app: AppHandle, state: ControlState) -> Result<(), String> {
    let controls = app.state::<DesktopControls>();
    let mut current = controls.state.lock().map_err(|error| error.to_string())?;
    let items = controls
        .tray_items
        .lock()
        .map_err(|error| error.to_string())?;

    // Linux cannot replace an existing tray menu; update its retained items instead.
    if let Some(items) = items.as_ref() {
        items.update(&state).map_err(|error| error.to_string())?;
    }
    *current = state;
    app.emit("desktop-control-state-changed", &*current)
        .map_err(|error| error.to_string())?;
    drop(items);
    drop(current);
    update_tray_pulse(&app)?;

    Ok(())
}

fn update_tray_pulse(app: &AppHandle) -> Result<(), String> {
    let controls = app.state::<DesktopControls>();
    let mut pulse = controls
        .tray_pulse
        .lock()
        .map_err(|error| error.to_string())?;
    let is_enabled = *controls
        .tray_enabled
        .lock()
        .map_err(|error| error.to_string())?;
    let is_live = controls
        .state
        .lock()
        .map_err(|error| error.to_string())?
        .capture_stage
        == "live";

    if is_enabled && is_live {
        if pulse.is_none() {
            *pulse = Some(crate::tray_pulse::TrayPulse::start(app)?);
        }
    } else if pulse.take().is_some() {
        let icon = app
            .default_window_icon()
            .ok_or("Application icon unavailable")?;
        if let Some(tray) = app.tray_by_id("slopcast") {
            tray.set_icon(Some(icon.clone()))
                .map_err(|error| error.to_string())?;
            tray.set_tooltip(Some("Slopcast"))
                .map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

/// A hidden or minimized window gives no sign that the screen is still being shared.
pub(crate) fn notify_if_streaming(app: &AppHandle) {
    let controls = app.state::<DesktopControls>();
    let is_live = match controls.state.lock() {
        Ok(state) => state.capture_stage == "live",
        Err(error) => {
            log::error!("Cannot read control state: {error}");
            false
        }
    };
    if !is_live {
        return;
    }

    // Showing blocks on the notification service, which must not stall the event loop.
    std::thread::spawn(|| {
        let result = notify_rust::Notification::new()
            .appname("Slopcast")
            .summary("Your stream is still live")
            .body("Slopcast is sharing your screen in the background. Open it to stop sharing.")
            .show();
        if let Err(error) = result {
            log::error!("Cannot show the streaming notification: {error}");
        }
    });
}

pub(crate) fn close_main(app: &AppHandle) {
    let controls = app.state::<DesktopControls>();
    let enabled = match controls.tray_enabled.lock() {
        Ok(enabled) => *enabled,
        Err(error) => {
            log::error!("Cannot read tray state: {error}");
            false
        }
    };

    if enabled {
        if let Some(window) = app.get_webview_window("main")
            && let Err(error) = window.hide()
        {
            log::error!("Cannot hide Slopcast: {error}");
        }
        notify_if_streaming(app);
    } else {
        emit_action(app, "quit");
    }
}

#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri command injection requires an owned AppHandle"
)]
pub(crate) fn quit_app(app: AppHandle) {
    app.exit(0);
}
