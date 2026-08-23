use livekit::webrtc::desktop_capturer::{DesktopCapturer, DesktopCapturerOptions};

fn main() {
    let _new: fn(DesktopCapturerOptions) -> Option<DesktopCapturer> = DesktopCapturer::new;
    std::hint::black_box(&_new);

    native_rust::ensure_pipewire_init();
    match native_rust::list_audio_applications() {
        Ok(apps) => println!("PROBE B: OK — {} audio apps", apps.len()),
        Err(e) => println!("PROBE B: enumeration failed: {e}"),
    }
}
