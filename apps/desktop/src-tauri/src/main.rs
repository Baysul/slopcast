#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(
    unused_crate_dependencies,
    reason = "dependencies are used by slopcast_lib"
)]

fn main() {
    #[cfg(target_os = "linux")]
    if !cfg!(debug_assertions)
        && let Err(error) = slopcast_lib::configure_native_output()
    {
        eprintln!("Could not open Slopcast's runtime log: {error}");
    }
    let context = tauri::generate_context!();
    #[cfg(target_os = "linux")]
    configure_gstreamer_scanner(&context);
    if let Err(error) = slopcast_lib::run(context) {
        // Native runtime stderr is file-backed in release builds. Keep an app startup
        // failure visible to the person launching it, with a nonzero exit status.
        println!("Could not start Slopcast: {error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
fn configure_gstreamer_scanner(context: &tauri::Context<tauri_runtime_cef::CefRuntime>) {
    let environment = tauri::utils::Env::default();
    let resource_dir = if cfg!(dev) {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    } else {
        let Ok(path) = tauri::utils::platform::resource_dir(context.package_info(), &environment)
        else {
            return;
        };
        path
    };
    // AppImage's sharun launcher finds shared/bin relative to its own location.
    // The copy nested in resources cannot locate the real scanner executable.
    let scanner = environment
        .appdir
        .map(|appdir| std::path::PathBuf::from(appdir).join("bin/gst-plugin-scanner"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| resource_dir.join("resources/gstreamer-plugins/gst-plugin-scanner"));
    if scanner.is_file() {
        // GStreamer can discover rswebrtc during gst_init, before load_plugins
        // preloads its dependencies. Use the bundled scanner for that initial scan.
        // SAFETY: main calls this before starting CEF, Tauri, or any worker threads.
        unsafe { std::env::set_var("GST_PLUGIN_SCANNER_1_0", scanner) };
    }
}
