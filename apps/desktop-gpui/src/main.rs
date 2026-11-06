//! Slopcast's native desktop shell.
//!
//! Bootstrap only: resolve configuration and settings, initialize the native media runtime, start
//! the process-lifetime [`Presenter`], then hand the presenter to the GPUI application.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;

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

    application().with_assets(assets::Assets).run(move |cx| {
        init(cx);
        app::open(cx, presenter);
    });
}

fn init_logging() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,zbus=warn,tracing::span=warn"),
    )
    .init();
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
    if cfg!(debug_assertions) {
        let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/desktop/src-tauri/resources/gstreamer-plugins");
        if development.is_dir() {
            return Some(development);
        }
    }
    let installed = std::env::current_exe()
        .ok()?
        .parent()?
        .join("resources/gstreamer-plugins");
    installed.is_dir().then_some(installed)
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
