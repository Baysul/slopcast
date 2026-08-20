//! E2E-only wiring: opens CEF's remote-debugging endpoint so the
//! Playwright presenter phase can drive the app over the DevTools
//! protocol. Production builds exclude this module, so no debugging
//! surface ships.

/// Adds the CEF remote-debugging command-line flag under the `e2e` cargo
/// feature. The flag lands in CEF's `on_before_command_line_processing`
/// before the browser process initializes, so the DevTools HTTP endpoint
/// (`http://127.0.0.1:9222`) comes up with the app.
pub fn with_command_line_args(builder: tauri::Builder<tauri::Cef>) -> tauri::Builder<tauri::Cef> {
    builder.command_line_args([(
        "--remote-debugging-port".to_string(),
        Some("9222".to_string()),
    )])
}
