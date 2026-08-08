//! Tauri backend for the Slopcast desktop presenter.

pub mod audio;
pub mod capture;
pub mod config;
pub mod context;
pub mod dto;
pub mod platform;
pub mod room;
pub mod settings;

#[cfg(feature = "e2e")]
mod e2e;

use tauri::Manager;
use tauri::http;

// Build-script hook: `build.rs` rewrites this stamp whenever any renderer
// asset changes, so this `include_bytes!` dependency recompiles the crate and
// forces `generate_context!` to re-embed the frontend (otherwise new hashed
// bundles leave a stale embed behind -> blank window).
const _FRONTEND_STAMP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/slopcast-frontend-stamp"));

/// Dev builds load the vite dev server (`devUrl`); a standalone debug binary
/// with no server running shows a dead "Could not connect to localhost" page
/// instead of the app. Probe the dev server once and fall back to the
/// embedded frontend assets so the UI always loads, however the binary was
/// launched.
#[cfg(dev)]
fn fallback_to_embedded_without_dev_server(app: &tauri::App) {
    use std::net::ToSocketAddrs;
    let Some(dev_url) = app.config().build.dev_url.as_ref() else {
        return;
    };
    let Some(host) = dev_url.host_str() else {
        return;
    };
    let Some(port) = dev_url.port_or_known_default() else {
        return;
    };
    let reachable = (host, port).to_socket_addrs().is_ok_and(|mut addrs| {
        addrs.any(|addr| {
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(250))
                .is_ok()
        })
    });
    if reachable {
        return;
    }
    log::info!("[bootstrap] dev server unreachable ({dev_url}) — loading embedded frontend");
    if let Some(window) = app.get_webview_window("main")
        && let Ok(url) = tauri::Url::parse("tauri://localhost/index.html")
    {
        let _ = window.navigate(url);
    }
}

/// Builds and runs the Tauri application: plugins, managed state, the audio
/// callback wiring, the command surface and the exit-time cleanup that
/// tears down capture state.
/// # Panics
///
/// Panics if the `frame://` protocol handler builder fails (should never
/// happen with a valid header value).
#[allow(
    clippy::too_many_lines,
    reason = "app bootstrap is inherently sequential"
)]
pub fn run() {
    // linuxdeploy-plugin-gtk's AppImage hook (apprun-hooks/linuxdeploy-plugin-gtk.sh)
    // exports `GDK_BACKEND=x11` — a stale tauri#8541 workaround — sending the app
    // to XWayland, where tao's CSD fix (Wayland-only) never engages and KWin draws
    // a native titlebar on top of the custom one. Undo it when running from an
    // AppImage on a Wayland session so GTK auto-detects the backend; on X11
    // sessions or non-AppImage launches this is a no-op.
    #[cfg(target_os = "linux")]
    if std::env::var("APPDIR").is_ok()
        && std::env::var("WAYLAND_DISPLAY").is_ok()
        && std::env::var("GDK_BACKEND").as_deref() == Ok("x11")
    {
        // SAFETY: single-threaded here (top of `main`, before the event loop or
        // any thread spawns) and before GTK reads the variable.
        unsafe { std::env::remove_var("GDK_BACKEND") };
    }

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {}))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        // Info level: the plugin's default (Trace) forwards every
        // `log::debug!` from libwebrtc's ICE/connection threads to the
        // console, flooding it with per-connection STUN spam while streaming.
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .build(),
        );

    // Custom URI scheme for the preview frames: the renderer fetches
    // `frame://frame.bin?t=…` directly — no tauri IPC, no channel, no
    // ordering. The handler reads from a shared slot (updated by the
    // capture callback) and returns the bytes as-is.
    //
    // The response must carry `Access-Control-Allow-Origin`: WebKitGTK
    // enforces CORS on custom-scheme fetches from the `tauri://localhost`
    // page, and every fetch fails with "Load failed" without it (verified
    // on 2.52.5). Tauri's own `ipc://` responses set the same header
    // (tauri/src/ipc/protocol.rs); only webview code can reach a custom
    // scheme, so `*` adds no exposure beyond what the page already has.
    let builder = builder.register_uri_scheme_protocol("frame", |_app, _request| {
        let body = crate::capture::LATEST_FRAME
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_default();
        // SAFETY: a valid header name/value + Vec<u8> body always succeeds.
        match http::Response::builder()
            .header(http::header::CONTENT_TYPE, "application/octet-stream")
            .header(http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(body)
        {
            Ok(r) => r,
            Err(_) => unreachable!("valid header and body"),
        }
    });

    #[cfg(feature = "e2e")]
    let builder = e2e::with_plugins(builder);

    let app = builder
        .setup(|app| {
            #[cfg(dev)]
            fallback_to_embedded_without_dev_server(app);
            // WebKitGTK ships with smooth scrolling disabled by default
            // (Chromium always had it on), so
            // wheel/trackpad scrolls jump in discrete steps instead of
            // interpolating per compositor frame. The app reads as running
            // at a low framerate for that reason; restore per-frame
            // scrolling so the page tracks the display's refresh rate.
            #[cfg(target_os = "linux")]
            {
                use webkit2gtk::{SettingsExt, WebViewExt};
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.with_webview(|webview| {
                        if let Some(settings) = webview.inner().settings() {
                            settings.set_enable_smooth_scrolling(true);
                        }
                    });
                }
            }
            // Arm libwebrtc's bundled PipeWire dlopen shims before any
            // native-rust PipeWire call. Our code no longer pulls them (the
            // in-house engine replaced `DesktopCapturer`), but the peer
            // connection factory keeps libwebrtc's PipeWire video capture
            // module linked, which drags the hidden-weak `pw_*` shims in;
            // they jump through NULL until `InitializePipewire` arms them
            // (SIGSEGV at startup when unarmed).
            native_livekit::arm_pipewire_shims();
            native_rust::ensure_pipewire_init();
            app.manage(context::CaptureContextCache::default());
            app.manage(config::AppConfigState::load()?);
            audio::register_audio_callbacks(app.handle());
            capture::register_preview_frame_callback();
            capture::register_capture_ended_callback(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            config::get_app_config,
            platform::get_platform_info,
            platform::probe_gpu_info,
            audio::get_audio_apps,
            audio::dump_audio_sources,
            audio::start_audio_capture,
            audio::stop_audio_capture,
            audio::switch_audio_capture,
            audio::start_audio_metering,
            audio::stop_audio_metering,
            audio::resolve_audio_source,
            context::get_capture_context,
            context::inspect_capture_context,
            settings::get_stream_settings,
            settings::save_stream_settings,
            settings::get_onboarding_completed,
            settings::set_onboarding_completed,
            room::connect_native_room,
            room::disconnect_native_room,
            room::is_native_room_connected,
            room::get_spectator_count,
            room::get_native_telemetry,
            room::get_native_supported_codecs,
            capture::start_native_capture,
            capture::start_synthetic_capture,
            capture::update_native_video,
            capture::stop_native_capture,
            capture::stop_video_capture,
            capture::is_native_capture_active,
            capture::get_video_capture_stats,
            capture::start_capture_preview,
            capture::go_live,
            capture::get_capture_sources,
            capture::set_preview_viewport,
            capture::clear_preview_viewport,
            capture::bench_register_channel,
            capture::bench_push_frames,
        ])
        .build(tauri::generate_context!())
        .map_err(|e| eprintln!("failed to build tauri application: {e}"))
        .ok();

    let Some(app) = app else {
        return;
    };

    app.run(|_app_handle, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            // Exit-time capture teardown.
            let _ = native_rust::stop_audio_capture();
            let _ = native_rust::stop_audio_metering();
            let _ = native_livekit::stop_video_track();
            let _ = native_livekit::stop_desktop_capture();
        }
    });
}
