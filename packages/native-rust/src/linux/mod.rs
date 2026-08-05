mod apps;
mod capture;
#[allow(
    dead_code,
    reason = "Phase C lands standalone; consumed by the Phase B/D capture wiring"
)]
mod egl;
mod graph;
mod kwin;
mod metering;
mod mpris;
#[allow(
    dead_code,
    reason = "Phase A lands standalone; consumed by the Phase D capture wiring"
)]
mod portal;
mod procinfo;
#[allow(
    dead_code,
    reason = "Phase B lands standalone; consumed by the Phase D capture wiring"
)]
mod pw_video;
mod screencast;

pub(crate) use apps::{dump_audio_sources, list_audio_applications};
pub(crate) use capture::{
    is_audio_capture_active, start_audio_capture, stop_audio_capture, switch_audio_capture,
};
pub(crate) use metering::{
    clear_wave_callback, set_wave_callback, start_audio_metering, stop_audio_metering,
};
pub(crate) use screencast::{get_capture_context, resolve_audio_app_for_captured_window};

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

const CAPTURE_NODE_NAME: &str = "Slopcast-Window-Audio";
const CAPTURE_NODE_DESCRIPTION: &str = "Slopcast-Window-Audio";
const ADAPTER_FACTORY: &str = "adapter";
const LINK_FACTORY: &str = "link-factory";

struct PwCtx {
    main_loop: pipewire::main_loop::MainLoopRc,
    core: pipewire::core::CoreRc,
}

/// Runs the global `PipeWire` library init exactly once on the main thread,
/// before the event loop serves IPC. libwebrtc's statically-linked `pw_*`
/// dlopen shims capture pipewire-rs's references in the same binary; the Tauri
/// backend arms them (`native_livekit::arm_pipewire_shims`) before calling this
/// so `pw_init` reaches the real libpipewire. Completing the once here keeps
/// later worker-thread calls (the renderer polls `getAudioApps` every 3s) on
/// the fast path.
pub(crate) fn ensure_pipewire_init() {
    pipewire::init();
}

fn pw_init() -> Result<PwCtx, String> {
    let main_loop =
        pipewire::main_loop::MainLoopRc::new(None).map_err(|e| format!("MainLoop: {e}"))?;
    let context =
        pipewire::context::ContextRc::new(&main_loop, None).map_err(|e| format!("Context: {e}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| format!("Connect: {e}"))?;
    Ok(PwCtx { main_loop, core })
}

fn sync_registry(core: &pipewire::core::Core, main_loop: &pipewire::main_loop::MainLoopRc) {
    let done = Rc::new(RefCell::new(false));
    let done_clone = done.clone();
    let Some(pending) = core.sync(0).ok() else {
        return;
    };
    let main_loop_weak = main_loop.downgrade();
    let _listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pipewire::core::PW_ID_CORE && seq == pending {
                *done_clone.borrow_mut() = true;
                if let Some(ml) = main_loop_weak.upgrade() {
                    ml.quit();
                }
            }
        })
        .register();
    for _ in 0..100 {
        if *done.borrow() {
            break;
        }
        main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(50)));
    }
}
