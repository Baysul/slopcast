//! Slopcast's native desktop shell.
//!
//! Bootstrap only: resolve configuration and settings, initialize the native media runtime, start
//! the process-lifetime [`Presenter`], then hand the presenter to the GPUI application.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
#[cfg(feature = "e2e")]
mod e2e;
mod instance;
mod logging;
#[cfg(target_os = "linux")]
mod portal;
mod services;
mod settings;
mod tray;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;

use desktop_core::config::AppConfig;
use desktop_core::media;
use desktop_core::platform::platform_info;
use desktop_core::presenter::{Presenter, PresenterOptions};
use desktop_core::storage::SettingsStorage;
use gpui_kit::*;

fn main() {
    init_logging();

    // GStreamer discovers plugins during gst_init, before `load_gstreamer_plugins` preloads their
    // dependencies, so the bundled scanner is selected before anything starts.
    #[cfg(target_os = "linux")]
    configure_gstreamer_scanner();

    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(error) => fail(&format!("Cannot load the Slopcast configuration: {error}")),
    };
    let storage = match SettingsStorage::open_default() {
        Ok(storage) => Arc::new(storage),
        Err(error) => fail(&format!("Cannot open the Slopcast settings: {error}")),
    };
    let (signals, receiver) = async_channel::bounded(128);
    let directory = std::env::var_os("SLOPCAST_SETTINGS_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|path| path.join("slopcast")))
        .unwrap_or_else(|| fail("Cannot locate the settings directory"));
    let instance = match instance::Instance::acquire(&directory, signals.clone()) {
        Ok(Some(instance)) => instance,
        Ok(None) => return,
        Err(error) => fail(&error),
    };
    if let Err(error) = media::init_native(gstreamer_plugin_dir().as_deref()) {
        log::error!("Native media runtime unavailable: {error}");
    }
    let presenter = match Presenter::start(PresenterOptions {
        config,
        storage,
        platform: platform_info(),
    }) {
        Ok(presenter) => presenter,
        Err(error) => fail(&format!("Cannot start the Slopcast presenter: {error}")),
    };

    application()
        .with_quit_mode(QuitMode::Explicit)
        .with_assets(assets::AllAssets)
        .run(move |cx| {
            let fonts = [
                std::borrow::Cow::Borrowed(
                    include_bytes!("../resources/fonts/Inter.ttc").as_slice(),
                ),
                std::borrow::Cow::Borrowed(
                    include_bytes!("../resources/fonts/Lora.ttc").as_slice(),
                ),
                std::borrow::Cow::Borrowed(
                    include_bytes!("../resources/fonts/JetBrainsMono.ttc").as_slice(),
                ),
            ];
            if let Err(error) = cx.text_system().add_fonts(fonts.to_vec()) {
                log::warn!("Cannot load bundled fonts: {error}");
            }
            init(cx);
            // Match the former renderer’s -webkit-font-smoothing: antialiased.
            cx.set_text_rendering_mode(TextRenderingMode::Grayscale);
            app::open(cx, presenter, signals, receiver, instance);
        });
}

fn init_logging() {
    logging::init();
}

fn fail(message: &str) -> ! {
    log::error!("{message}");
    eprintln!("{message}");
    std::process::exit(1)
}

/// The bundled GStreamer plugin directory, or `None` when the system runtime is used.
fn gstreamer_plugin_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("SLOPCAST_GSTREAMER_DIR").filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let executable = std::env::current_exe().ok()?;
    let installed = executable.parent()?.join("resources/gstreamer-plugins");
    if installed.is_dir() {
        return Some(installed);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = root.join("../../target").canonicalize().ok();
    let development_binary = target.is_some_and(|target| {
        executable.parent().is_some_and(|parent| {
            parent == target.join("debug") || parent == target.join("release")
        })
    });
    let development = root.join("resources/gstreamer-plugins");
    (development_binary && development.is_dir()).then_some(development)
}

#[cfg(target_os = "linux")]
fn configure_gstreamer_scanner() {
    let Some(scanner) = gstreamer_plugin_dir()
        .map(|dir| dir.join("gst-plugin-scanner"))
        .filter(|path| path.is_file())
    else {
        return;
    };
    // SAFETY: main runs this before GPUI, GStreamer or any worker thread starts.
    unsafe { std::env::set_var("GST_PLUGIN_SCANNER_1_0", scanner) };
}
