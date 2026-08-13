//! Tauri backend for the Slopcast desktop presenter.

pub mod audio;
pub mod capture;
pub mod config;
pub mod context;
pub mod dto;
pub mod platform;
pub mod room;
pub mod settings;

pub type AppHandle = tauri::AppHandle<tauri::Cef>;
pub type App = tauri::App<tauri::Cef>;

#[cfg(feature = "e2e")]
mod e2e;

use std::path::Path;

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
fn fallback_to_embedded_without_dev_server(app: &App) {
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
    log::info!("[bootstrap] dev server unreachable ({dev_url}) — serving frontend from disk");
    // A dev build does not embed the frontend (tauri serves `devUrl` instead),
    // so the `tauri://` protocol has no assets ("asset not found: index.html"
    // blank window). The window's URL is `devUrl` — which is where the IPC
    // bridge is injected — so serve the built `frontendDist` over HTTP on
    // that exact port. The existing page load then succeeds with the bridge
    // intact, no navigation needed.
    let handle = app.handle().clone();
    let host = host.to_string();
    std::thread::spawn(move || {
        if let Err(e) = serve_frontend(&host, port, &handle) {
            log::error!("[bootstrap] frontend server failed: {e}");
        }
    });
    // The server binds on a background thread; the window's devUrl load
    // retries until it's up.
}

/// Minimal HTTP/1.1 static file server (dev-only, no new deps). Serves the
/// app's frontend via `AssetResolver` (which reads `frontendDist` from disk
/// in dev, embedded assets in release) so the window's devUrl page and its
/// relative asset requests all resolve.
#[cfg(dev)]
fn serve_frontend(host: &str, port: u16, handle: &AppHandle) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind((host, port))?;
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        let handle = (*handle).clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let path = request
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/")
                .split('?')
                .next()
                .unwrap_or("/");
            let path = path.trim_start_matches('/');
            let path = if path.is_empty() { "index.html" } else { path };
            let asset = handle.asset_resolver().get(path.to_string());
            let (status, body, mime) = match asset {
                Some(a) => ("200 OK", a.bytes, a.mime_type),
                None => (
                    "404 Not Found",
                    b"not found".to_vec(),
                    "text/plain".to_string(),
                ),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        });
    }
    Ok(())
}

/// Builds and runs the Tauri application: plugins, managed state, the audio
/// callback wiring, the command surface and the exit-time cleanup that
/// tears down capture state.
/// # Panics
///
/// Panics if the `frame` protocol handler builder fails (should never
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
        )
        // CEF/Chromium startup + resource tuning. These reach every child
        // process (renderer/GPU/utility) via `on_before_command_line_processing`.
        .command_line_args([
            // No first-run / default-app / update chatter; nothing to update.
            ("--no-first-run", None),
            ("--no-default-browser-check", None),
            ("--disable-default-apps", None),
            ("--disable-component-update", None),
            ("--disable-background-networking", None),
            // No crash reporter in production; a crashed process is a hard
            // failure our e2e diagnostics surface, not a telemetry event.
            ("--disable-breakpad", None),
            // Raster on the GPU process. The startup probe already rejects
            // software rendering, so this is a no-op where it can't be honored.
            ("--enable-gpu-rasterization", None),
            // One window -> one renderer is enough; trims per-process overhead.
            ("--renderer-process-limit", Some("1")),
            // Bound the disk cache the runtime creates under ~/.cache.
            ("--disk-cache-size", Some("67108864")), // 64 MiB
            // CEF logs to ./debug.log from the CWD; keep it to errors only.
            ("--log-level", Some("2")), // LOGSEVERITY_ERROR
        ]);

    // Custom URI scheme for the preview frames: the renderer fetches
    // `http://frame.localhost/frame.bin?t=…` directly — no tauri IPC, no channel, no
    // ordering. The handler reads from a shared slot (updated by the
    // capture callback) and returns the bytes as-is.
    //
    // The response must carry `Access-Control-Allow-Origin`: the frame
    // scheme is cross-origin to the app page, so CORS applies and every
    // fetch fails with "Load failed" without it. Tauri's own `ipc://`
    // responses set the same header
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
            // Arm libwebrtc's bundled PipeWire dlopen shims before any
            // native-rust PipeWire call. Our code no longer pulls them (the
            // in-house engine replaced `DesktopCapturer`), but the peer
            // connection factory keeps libwebrtc's PipeWire video capture
            // module linked, which drags the hidden-weak `pw_*` shims in;
            // they jump through NULL until `InitializePipewire` arms them
            // (SIGSEGV at startup when unarmed).
            native_livekit::arm_pipewire_shims();
            native_rust::ensure_pipewire_init();
            #[cfg(target_os = "linux")]
            {
                // Bundled layouts (deb/AppImage) place resources directly in
                // the resource dir (`/usr/lib/slopcast`); cargo-built binaries
                // (`tauri build --no-bundle`, `tauri dev`) place them under
                // `<exe_dir>/resources`. Try both so the same code serves the
                // packaged app and the locally built one.
                let resource_dir = app.path().resource_dir()?;
                let exe_dir = std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(Path::to_path_buf));
                let mut candidates = vec![resource_dir.join("gstreamer-plugins")];
                if let Some(exe_dir) = exe_dir {
                    candidates.push(exe_dir.join("resources/gstreamer-plugins"));
                }
                let plugin_dir = candidates
                    .into_iter()
                    .find(|dir| dir.join("libgstrswebrtc.so").is_file())
                    .ok_or_else(|| {
                        std::io::Error::other(
                            "Bundled GStreamer plugin directory not found (looked in the resource and executable dirs)",
                        )
                    })?;
                native_livekit::load_gstreamer_plugins(&plugin_dir)
                    .map_err(std::io::Error::other)?;
            }
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
