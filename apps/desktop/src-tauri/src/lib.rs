//! Tauri backend for the Slopcast desktop presenter.
//!
//! Replaces the Electron main process: every preload IPC channel maps to a
//! command here (see MIGRATION.md §5), audio/video capture and the `LiveKit`
//! room run entirely in Rust via the `native-rust` / `native-livekit` crates.

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

/// Builds and runs the Tauri application: plugins, managed state, the audio
/// callback wiring, the command surface and the lifecycle cleanup that
/// mirrors Electron's `before-quit` handler.
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {}))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_log::Builder::new().build());

    #[cfg(feature = "e2e")]
    let builder = e2e::with_plugins(builder);

    let app = builder
        .setup(|app| {
            // Arm libwebrtc's bundled PipeWire dlopen shims before any
            // native-rust PipeWire call: the hidden-weak `pw_*` shims capture
            // pipewire-rs's references at link time and jump through NULL
            // until `InitializePipewire` dlopens libpipewire (SIGSEGV observed
            // at startup when unarmed).
            native_livekit::arm_pipewire_shims();
            native_rust::ensure_pipewire_init();
            app.manage(context::CaptureContextCache::default());
            app.manage(config::AppConfigState::load()?);
            audio::register_audio_callbacks(app.handle());
            capture::register_preview_callback(app.handle());
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
            capture::start_native_capture,
            capture::update_native_video,
            capture::stop_native_capture,
            capture::stop_video_capture,
            capture::is_native_capture_active,
            capture::get_video_capture_stats,
            capture::start_capture_preview,
            capture::go_live,
        ])
        .build(tauri::generate_context!())
        .map_err(|e| eprintln!("failed to build tauri application: {e}"))
        .ok();

    let Some(app) = app else {
        return;
    };

    app.run(|_app_handle, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            // Mirrors Electron's `before-quit` cleanup in main/index.ts.
            let _ = native_rust::stop_audio_capture();
            let _ = native_rust::stop_audio_metering();
            let _ = native_livekit::stop_video_track();
            let _ = native_livekit::stop_desktop_capture();
        }
    });
}
