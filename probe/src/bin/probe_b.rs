//! Probe B (baseline): same link set as Probe A, but additionally takes a
//! function pointer to `DesktopCapturer::new` — forcing the C++ capturer
//! symbol chain (and with it `pipewire_stubs.o`) into the link, without
//! actually executing anything.
//!
//! Expectation: this binary contains the hidden-weak `pw_*` shims and
//! `pipewire::init()` SIGSEGVs (no arming call) — reproducing the app crash.

use livekit::webrtc::desktop_capturer::{DesktopCapturer, DesktopCapturerOptions};

fn main() {
    // Reference-only: never called, never constructed. This is enough for the
    // linker to pull the capturer chain from libwebrtc's static archive.
    let _new: fn(DesktopCapturerOptions) -> Option<DesktopCapturer> = DesktopCapturer::new;
    std::hint::black_box(&_new);

    native_rust::ensure_pipewire_init();
    match native_rust::list_audio_applications() {
        Ok(apps) => println!("PROBE B: OK — {} audio apps", apps.len()),
        Err(e) => println!("PROBE B: enumeration failed: {e}"),
    }
}
