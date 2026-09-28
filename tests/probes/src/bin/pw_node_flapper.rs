//! Emulates a browser audio stream node that disappears and reappears in the
//! same process (Firefox pause/resume behavior): creates a Stream/Output/Audio
//! node, destroys it, recreates it, then loops.

use pipewire::properties::properties;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

fn pw_init() -> Result<(pipewire::core::CoreRc, pipewire::main_loop::MainLoopRc), String> {
    pipewire::init();
    let main_loop =
        pipewire::main_loop::MainLoopRc::new(None).map_err(|e| format!("MainLoop: {e}"))?;
    let context =
        pipewire::context::ContextRc::new(&main_loop, None).map_err(|e| format!("Context: {e}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| format!("Connect: {e}"))?;
    Ok((core, main_loop))
}

fn create_app_node(core: &pipewire::core::Core) -> Result<pipewire::node::Node, String> {
    core.create_object::<pipewire::node::Node>(
        "adapter",
        &properties! {
            "factory.name" => "support.null-audio-sink",
            "node.name" => "Flapper-App-Audio",
            "node.description" => "Flapper App Audio",
            "media.class" => "Stream/Output/Audio",
            "application.name" => "Flapper",
            "audio.position" => "FL,FR",
            "object.linger" => "false",
        },
    )
    .map_err(|e| format!("create_object: {e}"))
}

fn sync_registry(core: &pipewire::core::Core, main_loop: &pipewire::main_loop::MainLoopRc) {
    let sync_complete = Rc::new(RefCell::new(false));
    let sync_complete_clone = Rc::clone(&sync_complete);
    let Some(pending) = core.sync(0).ok() else {
        return;
    };
    let _listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pipewire::core::PW_ID_CORE && seq == pending {
                *sync_complete_clone.borrow_mut() = true;
            }
        })
        .register();
    for _ in 0..100 {
        if *sync_complete.borrow() {
            break;
        }
        main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(50)));
    }
}

#[allow(
    clippy::unwrap_used,
    reason = "probe binary: panics on setup errors are the point"
)]
fn main() {
    let (core, main_loop) = pw_init().unwrap_or_else(|e| panic!("pw_init: {e}"));
    let registry = core.get_registry().unwrap_or_else(|e| panic!("registry: {e}"));
    let _listener = registry.add_listener_local().global(|_g| {}).register();
    sync_registry(&core, &main_loop);

    loop {
        let node = create_app_node(&core).unwrap_or_else(|e| panic!("create node: {e}"));
        println!("[flapper] node created");
        sync_registry(&core, &main_loop);
        std::thread::sleep(Duration::from_secs(4));
        drop(node);
        println!("[flapper] node destroyed");
        sync_registry(&core, &main_loop);
        std::thread::sleep(Duration::from_secs(4));
    }
}
