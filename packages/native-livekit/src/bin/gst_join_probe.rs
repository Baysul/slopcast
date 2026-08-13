#![allow(
    unused_crate_dependencies,
    reason = "temporary scratch probe; the lib's dependencies are used transitively"
)]
// Scratch diagnostic: drives the GStreamer LiveKit publisher directly
// against a live SFU and reports what the room state + telemetry look
// like, so a "presenter never appears in the room" report can be
// reproduced without the Tauri shell. TEMPORARY — deleted after use.
//
// Env: PROBE_URL, PROBE_TOKEN, PROBE_ROOM, PROBE_IDENTITY.

use std::time::Duration;

use native_livekit::{
    CaptureConfig, connect_livekit_room, disconnect_livekit_room, get_native_telemetry,
    is_livekit_room_connected, load_gstreamer_plugins, start_synthetic_capture, start_video_track,
};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set"))
}

fn poll(label: &str, seconds: u64) {
    for i in 0..seconds {
        std::thread::sleep(Duration::from_millis(500));
        let telemetry = get_native_telemetry();
        println!(
            "[{label} {i:02}] connected={} video_bytes_sent={:?} audio_bytes_sent={:?} frames_encoded={:?} rtt_ms={:?}",
            is_livekit_room_connected(),
            telemetry.video_bytes_sent,
            telemetry.audio_bytes_sent,
            telemetry.video_frames_encoded,
            telemetry.rtt_ms,
        );
    }
}

fn main() {
    let plugin_dir = std::path::Path::new(
        "/home/basil/Projects/screen-share/apps/desktop/src-tauri/resources/gstreamer-plugins",
    );
    println!("[probe] loading plugins from {}", plugin_dir.display());
    match load_gstreamer_plugins(plugin_dir) {
        Ok(()) => println!("[probe] plugins loaded"),
        Err(error) => panic!("[probe] plugin load failed: {error}"),
    }

    let url = env("PROBE_URL");
    let token = env("PROBE_TOKEN");
    let room = env("PROBE_ROOM");
    let identity = env("PROBE_IDENTITY");
    println!("[probe] connecting to {url} room={room} identity={identity}");
    match connect_livekit_room(url, token, room, identity) {
        Ok(()) => println!("[probe] connect() returned Ok"),
        Err(error) => panic!("[probe] connect failed: {error}"),
    }
    // The sink's codec-discovery gate waits until EVERY stream (audio and
    // video) is discovered, and audio discovery only starts when real PCM
    // flows on the audio pad. The app streams continuously from connect;
    // the probe must too, so feed 10 ms of silence every 10 ms.
    std::thread::spawn(|| {
        let silence = vec![0i16; 480];
        loop {
            let _ = native_livekit::feed_pcm(silence.clone());
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    poll("audio-only", 8);

    let config = CaptureConfig {
        width: 1280,
        height: 720,
        fps: 30,
        video_codec: Some("h264".into()),
        max_bitrate: Some(8_000_000.0),
    };
    println!("[probe] starting synthetic capture + video track");
    match start_synthetic_capture(&config) {
        Ok(true) => println!("[probe] synthetic capture started"),
        Ok(false) => println!("[probe] synthetic capture returned false"),
        Err(error) => panic!("[probe] synthetic capture failed: {error}"),
    }
    match start_video_track(config) {
        Ok(()) => println!("[probe] start_video_track Ok"),
        Err(error) => panic!("[probe] start_video_track failed: {error}"),
    }
    poll("video", 12);

    println!("[probe] disconnecting");
    let _ = disconnect_livekit_room();
    println!("[probe] done");
}
