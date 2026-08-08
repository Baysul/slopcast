//! Room commands — driving the native `LiveKit` publisher in
//! `native-livekit`.

use native_livekit::{
    NativeCodecInfo, NativeTelemetry, connect_livekit_room, disconnect_livekit_room,
};

/// Arguments for `connect_native_room`.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectRoomArgs {
    pub url: String,
    pub token: String,
}

/// Connects to a `LiveKit` room and publishes the screenshare audio track.
///
/// # Errors
///
/// Returns an error if a room is already connected or the worker thread
/// cannot be spawned.
#[tauri::command(rename_all = "camelCase")]
pub async fn connect_native_room(args: ConnectRoomArgs) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || connect_livekit_room(args.url, args.token))
        .await
        .map_err(|e| format!("connect room task failed: {e}"))?
}

/// Disconnects from the live room, stopping the video track and desktop
/// capture and tearing down the worker thread.
///
/// # Errors
///
/// Returns an error if the room state lock is poisoned.
#[tauri::command(rename_all = "camelCase")]
pub async fn disconnect_native_room() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(disconnect_livekit_room)
        .await
        .map_err(|e| format!("disconnect room task failed: {e}"))?
}

/// Returns whether the native `LiveKit` room is connected.
#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn is_native_room_connected() -> bool {
    native_livekit::is_livekit_room_connected()
}

/// Returns the current spectator (remote participant) count.
#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn get_spectator_count() -> u32 {
    native_livekit::get_spectator_count()
}

/// Collects the latest libwebrtc stats for the local published tracks; keeps
/// the worker's 500 ms answer-timeout fallback.
#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub async fn get_native_telemetry() -> NativeTelemetry {
    tauri::async_runtime::spawn_blocking(native_livekit::get_native_telemetry)
        .await
        .unwrap_or_default()
}

/// Returns the codecs the native encoder stack (bundled libwebrtc) can
/// encode with. This is the authoritative list for the renderer's codec
/// picker — the webview's `RTCRtpSender.getCapabilities` reflects the
/// WebKitGTK/GStreamer stack, which is never used for encoding.
#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn get_native_supported_codecs() -> Vec<NativeCodecInfo> {
    native_livekit::get_native_supported_codecs()
}
