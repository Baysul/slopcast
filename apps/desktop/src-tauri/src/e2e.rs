//! E2E-only wiring: registers the embedded `WebDriver` plugins
//! under the `e2e` cargo feature. Production builds exclude this module, so
//! the unauthenticated localhost `WebDriver` surface never ships.

/// Adds the WDIO plugins to the app builder:
/// `tauri-plugin-wdio-webdriver` embeds the W3C `WebDriver` HTTP server
/// (`TAURI_WEBDRIVER_PORT`, default 4445) inside the app, and
/// `tauri-plugin-wdio` powers `browser.tauri.execute` plus log forwarding.
/// Plugins must be registered before `Builder::build`, hence the builder
/// pass-through instead of the old setup hook.
pub fn with_plugins<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .plugin(tauri_plugin_wdio_webdriver::init())
        .plugin(tauri_plugin_wdio::init())
}
