use native_livekit::{
    NativeCodecInfo, NativeTelemetry, connect_livekit_room, disconnect_livekit_room,
};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectRoomArgs {
    pub url: String,
    pub token: String,
    pub room_name: String,
    pub identity: String,
}

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if the room cannot be connected.
pub async fn connect_native_room(args: ConnectRoomArgs) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        connect_livekit_room(args.url, args.token, args.room_name, args.identity)
    })
    .await
    .map_err(|e| format!("connect room task failed: {e}"))?
}

#[tauri::command(rename_all = "camelCase")]
/// # Errors
/// Returns an error if room teardown fails.
pub async fn disconnect_native_room() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(disconnect_livekit_room)
        .await
        .map_err(|e| format!("disconnect room task failed: {e}"))?
}

#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn is_native_room_connected() -> bool {
    native_livekit::is_livekit_room_connected()
}

#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn has_native_room_session() -> bool {
    native_livekit::has_livekit_room_session()
}

#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn get_spectator_count() -> u32 {
    native_livekit::get_spectator_count()
}

#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub async fn get_native_telemetry() -> NativeTelemetry {
    tauri::async_runtime::spawn_blocking(native_livekit::get_native_telemetry)
        .await
        .unwrap_or_default()
}

#[must_use]
#[tauri::command(rename_all = "camelCase")]
pub fn get_native_supported_codecs() -> Vec<NativeCodecInfo> {
    native_livekit::get_native_supported_codecs()
}
