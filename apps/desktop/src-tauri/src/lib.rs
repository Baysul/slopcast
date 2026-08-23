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

#[cfg(target_os = "linux")]
use std::path::Path;

use tauri::Manager;
use tauri::http;

const _FRONTEND_STAMP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/slopcast-frontend-stamp"));

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
    let handle = app.handle().clone();
    let host = host.to_string();
    std::thread::spawn(move || {
        if let Err(e) = serve_frontend(&host, port, &handle) {
            log::error!("[bootstrap] frontend server failed: {e}");
        }
    });
}

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

#[allow(
    clippy::too_many_lines,
    reason = "app bootstrap is inherently sequential"
)]
pub fn run() {
    #[cfg(target_os = "linux")]
    if std::env::var("APPDIR").is_ok()
        && std::env::var("WAYLAND_DISPLAY").is_ok()
        && std::env::var("GDK_BACKEND").as_deref() == Ok("x11")
    {
        // Packaged AppImages can force X11 on Wayland; remove that override for the custom titlebar.
        // SAFETY: this runs before GTK or any other thread reads the environment.
        unsafe { std::env::remove_var("GDK_BACKEND") };
    }

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {}))
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .build(),
        )
        .command_line_args([
            ("--no-first-run", None),
            ("--no-default-browser-check", None),
            ("--disable-default-apps", None),
            ("--disable-component-update", None),
            ("--disable-background-networking", None),
            ("--disable-breakpad", None),
            ("--enable-gpu-rasterization", None),
            ("--renderer-process-limit", Some("1")),
            ("--disk-cache-size", Some("67108864")),
            ("--log-level", Some("2")),
        ]);

    let builder = builder.register_uri_scheme_protocol("frame", |_app, _request| {
        // The renderer fetches this custom scheme cross-origin, so CORS is required.
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
    let builder = e2e::with_command_line_args(builder);

    let app = builder
        .setup(|app| {
            #[cfg(dev)]
            fallback_to_embedded_without_dev_server(app);
            native_livekit::arm_pipewire_shims();
            native_rust::ensure_pipewire_init();
            #[cfg(target_os = "linux")]
            {
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
            room::has_native_room_session,
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
            let _ = native_rust::stop_audio_capture();
            let _ = native_rust::stop_audio_metering();
            let _ = native_livekit::stop_video_track();
            let _ = native_livekit::stop_desktop_capture();
        }
    });
}
