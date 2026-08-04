//! Probe A: link `livekit` (which statically links libwebrtc) + `native-rust`
//! (pipewire-rs) but NEVER reference `DesktopCapturer` from our code.
//!
//! Purpose: if libwebrtc's capturer module is pulled into the final binary
//! regardless of our usage, its hidden-weak `pw_*` dlopen shims capture
//! pipewire-rs's `pw_init` reference and `pipewire::init()` SIGSEGVs here
//! (no arming call). If the member is NOT pulled, this exits 0 and
//! enumerates audio apps cleanly.

fn main() {
    native_rust::ensure_pipewire_init();
    match native_rust::list_audio_applications() {
        Ok(apps) => println!("PROBE A: OK — pipewire init + enumeration worked, {} audio apps", apps.len()),
        Err(e) => {
            println!("PROBE A: enumeration failed: {e}");
            std::process::exit(2);
        }
    }
}
