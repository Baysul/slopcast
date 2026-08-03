mod apps;
mod capture;
mod dmabuf;
mod graph;
mod kwin;
mod metering;
mod mpris;
mod procinfo;
mod screencast;
mod video;

pub(crate) use apps::{dump_audio_sources, list_audio_applications};
pub(crate) use capture::{
    is_audio_capture_active, start_audio_capture, stop_audio_capture, switch_audio_capture,
};
pub(crate) use dmabuf::{clear_dmabuf_callback, invoke_dmabuf_callback, set_dmabuf_callback};
pub(crate) use metering::{
    clear_audio_wave_callback, set_audio_wave_callback, start_audio_metering, stop_audio_metering,
};
pub(crate) use napi::Result as NapiResult;
pub(crate) use screencast::{
    get_capture_context, resolve_audio_app_for_captured_window, resolve_audio_app_for_x11_window,
};
pub(crate) use video::{start_video_capture, stop_video_capture};

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
