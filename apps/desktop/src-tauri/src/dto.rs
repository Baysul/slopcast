//! Serde DTOs for the command/event payloads.
//!
//! `native-rust` types carry no serde derives; these mirrors restore the
//! camelCase JSON shapes the preload bridge exposed, so the renderer-facing
//! contract (MIGRATION.md §5) matches the old IPC surface exactly.

use std::collections::HashMap;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioAppDto {
    pub id: i32,
    pub name: String,
    pub process_id: i32,
    pub bundle_id: Option<String>,
    pub window_title: Option<String>,
    pub client_id: Option<i32>,
    pub media_title: Option<String>,
}

impl From<native_rust::AudioApp> for AudioAppDto {
    fn from(app: native_rust::AudioApp) -> Self {
        Self {
            id: app.id,
            name: app.name,
            process_id: app.process_id,
            bundle_id: app.bundle_id,
            window_title: app.window_title,
            client_id: app.client_id,
            media_title: app.media_title,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioAppWaveDto {
    pub id: i32,
    /// 96 interleaved (min, max) amplitude pairs of the last ~85 ms of mono
    /// audio, each value in [-1, 1].
    pub columns: Vec<f64>,
}

impl From<native_rust::AudioAppWave> for AudioAppWaveDto {
    fn from(wave: native_rust::AudioAppWave) -> Self {
        Self {
            id: wave.id,
            columns: wave.columns,
        }
    }
}

/// Wayland video-capture introspection: which desktop environment is
/// streaming, whether the source is a monitor or a window, and the
/// best-matched audio application for the captured source.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureContextDto {
    pub de: String,
    pub source_type: String,
    pub media_name: Option<String>,
    pub video_node_count: i32,
    pub app: Option<AudioAppDto>,
    pub screencast_node_id: Option<u32>,
    /// `object.serial` of the newest `kwin-screencast-*` node — snapshotted
    /// before triggering the portal so lingering or preview streams are never
    /// mistaken for the live capture.
    pub highest_serial: Option<f64>,
    /// xdg-desktop-portal screencast metadata (`portal.screencast.*`) for the
    /// captured window — the portal's own record of what was picked.
    pub portal_props: Option<HashMap<String, String>>,
    /// KWin-resolved owning window PID (KDE window captures only).
    pub window_pid: Option<i32>,
    /// KWin-resolved window caption (KDE window captures only).
    pub window_caption: Option<String>,
}

impl From<&native_rust::CaptureContext> for CaptureContextDto {
    fn from(context: &native_rust::CaptureContext) -> Self {
        Self {
            de: context.de.clone(),
            source_type: context.source_type.clone(),
            media_name: context.media_name.clone(),
            video_node_count: context.video_node_count,
            app: context.app.clone().map(AudioAppDto::from),
            screencast_node_id: context.screencast_node_id,
            highest_serial: context.highest_serial,
            portal_props: context.portal_props.clone(),
            window_pid: context.window_pid,
            window_caption: context.window_caption.clone(),
        }
    }
}

/// One `preview-frame` event payload: a base64 JPEG frame (640×360 @ ~15 fps)
/// rendered by the renderer's preview canvas while capture is active
/// (MIGRATION §9.1).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewFrameDto {
    pub data: String,
    pub width: u32,
    pub height: u32,
    pub pts_us: i64,
}
