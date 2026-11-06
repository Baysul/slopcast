//! Adapter over the native media engines (`native-livekit`, `native-rust`).
//!
//! Every function here may block (portal dialogs, PipeWire, GStreamer state changes) and must
//! run off the UI thread. Native callbacks are process-global, so they are registered once into
//! the [`MediaHub`] and fan out from there.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use native_livekit::CaptureConfig;
use tokio::sync::Notify;

use crate::audio::{AudioApp, NO_AUDIO, WaveDeduplicator};
use crate::platform::{synthetic_capture, video_capture_available};
use crate::settings::{StreamSettings, resolution_dims};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Screen,
    Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSelection {
    pub kind: SourceKind,
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSource {
    pub id: u64,
    pub title: String,
    pub display_id: i64,
    pub kind: SourceKind,
}

/// One decoded preview frame: tightly packed BGRA rows.
pub struct PreviewFrame {
    pub pts_us: i64,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

const HEADER_BYTES: usize = 16;

/// Parses the native preview payload: a 16-byte little-endian header (u64 timestamp,
/// u32 width, u32 height) followed by `width * height` BGRA pixels.
#[must_use]
pub fn parse_preview_payload(mut payload: Vec<u8>) -> Option<PreviewFrame> {
    if payload.len() <= HEADER_BYTES {
        return None;
    }
    let pts = u64::from_le_bytes(payload[0..8].try_into().ok()?);
    let width = u32::from_le_bytes(payload[8..12].try_into().ok()?);
    let height = u32::from_le_bytes(payload[12..16].try_into().ok()?);
    let expected = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?
        .checked_add(HEADER_BYTES)?;
    if width == 0 || height == 0 || payload.len() != expected {
        return None;
    }
    payload.drain(..HEADER_BYTES);
    Some(PreviewFrame {
        pts_us: i64::try_from(pts).unwrap_or(i64::MAX),
        width,
        height,
        bgra: payload,
    })
}

/// Latest-value slots for high-rate native output. Consumers take the newest value only.
#[derive(Default)]
pub struct MediaHub {
    frame: Mutex<Option<PreviewFrame>>,
    frame_ready: Notify,
    waves: Mutex<Option<Vec<native_rust::AudioAppWave>>>,
    waves_ready: Notify,
    accepting_frames: AtomicBool,
    frames_received: std::sync::atomic::AtomicU64,
}

impl MediaHub {
    /// Takes the newest unconsumed preview frame.
    pub fn take_frame(&self) -> Option<PreviewFrame> {
        self.frame
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Resolves when a new preview frame may be available.
    pub async fn frame_ready(&self) {
        self.frame_ready.notified().await;
    }

    pub fn take_waves(&self) -> Option<Vec<native_rust::AudioAppWave>> {
        self.waves
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub async fn waves_ready(&self) {
        self.waves_ready.notified().await;
    }

    /// Frames delivered since the current capture started.
    pub fn frames_received(&self) -> u64 {
        self.frames_received.load(Ordering::Acquire)
    }

    fn clear_frame(&self) {
        *self.frame.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn set_accepting_frames(&self, accepting: bool) {
        self.accepting_frames.store(accepting, Ordering::Release);
        self.frames_received.store(0, Ordering::Release);
        if !accepting {
            self.clear_frame();
        }
        // Wake consumers so they observe a cleared preview promptly.
        self.frame_ready.notify_one();
    }
}

static HUB: OnceLock<Arc<MediaHub>> = OnceLock::new();
static AUDIO_MUTED: AtomicBool = AtomicBool::new(false);
type EndedCallback = Box<dyn Fn() + Send + Sync>;
static CAPTURE_ENDED: Mutex<Option<EndedCallback>> = Mutex::new(None);

#[must_use]
pub fn hub() -> Arc<MediaHub> {
    Arc::clone(HUB.get_or_init(Arc::default))
}

/// Initializes the native media runtime. Call once, before any capture.
///
/// # Errors
/// Returns an error if the bundled GStreamer plugins cannot be loaded.
pub fn init_native(gstreamer_plugin_dir: Option<&Path>) -> Result<(), String> {
    native_livekit::arm_pipewire_shims();
    native_rust::ensure_pipewire_init();
    if let Some(dir) = gstreamer_plugin_dir {
        native_livekit::load_gstreamer_plugins(dir)?;
    }
    Ok(())
}

/// Registers the process-global native callbacks. Replaces any earlier registration.
pub fn register_callbacks(on_capture_ended: EndedCallback) {
    let hub = hub();
    *CAPTURE_ENDED.lock().unwrap_or_else(PoisonError::into_inner) = Some(on_capture_ended);

    let frames = Arc::clone(&hub);
    native_livekit::set_preview_callback(Box::new(move |payload, _pts| {
        if !frames.accepting_frames.load(Ordering::Acquire) {
            return;
        }
        if let Some(frame) = parse_preview_payload(payload) {
            *frames.frame.lock().unwrap_or_else(PoisonError::into_inner) = Some(frame);
            frames.frames_received.fetch_add(1, Ordering::AcqRel);
            frames.frame_ready.notify_one();
        }
    }));
    native_livekit::set_capture_ended_callback(Box::new(|| {
        if let Some(callback) = CAPTURE_ENDED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            callback();
        }
    }));
    native_rust::set_audio_data_callback(Box::new(|mut pcm| {
        if AUDIO_MUTED.load(Ordering::Relaxed) {
            pcm.fill(0);
        }
        let _ = native_livekit::feed_pcm(pcm);
    }));
    let dedup = Mutex::new(WaveDeduplicator::default());
    let waves = hub;
    native_rust::set_wave_callback(Box::new(move |update| {
        if !dedup
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .update(&update)
        {
            return;
        }
        *waves.waves.lock().unwrap_or_else(PoisonError::into_inner) = Some(update);
        waves.waves_ready.notify_one();
    }));
}

pub fn unregister_callbacks() {
    native_livekit::clear_preview_callback();
    native_rust::clear_wave_callback();
    native_rust::clear_audio_data_callback();
    *CAPTURE_ENDED.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Requests preview frames scaled to fit `width`×`height` physical pixels.
pub fn set_preview_viewport(width: u32, height: u32) {
    native_livekit::set_preview_viewport(width, height);
}

/// Stops preview conversion while publication continues (preview hidden).
pub fn clear_preview_viewport() {
    native_livekit::clear_preview_viewport();
}

#[must_use]
pub fn is_muted() -> bool {
    AUDIO_MUTED.load(Ordering::Relaxed)
}

/// Toggles the PCM mute and returns the new state.
pub fn toggle_mute() -> bool {
    !AUDIO_MUTED.fetch_xor(true, Ordering::Relaxed)
}

fn capture_supported() -> bool {
    video_capture_available() || synthetic_capture()
}

#[cfg(target_os = "windows")]
fn start_real_capture(source: Option<SourceSelection>) -> Result<(), String> {
    let selection = source.ok_or("A capture source is required on Windows")?;
    let kind = match selection.kind {
        SourceKind::Screen => native_livekit::WgcSourceKind::Screen,
        SourceKind::Window => native_livekit::WgcSourceKind::Window,
    };
    native_livekit::start_windows_capture(kind, selection.id).map(|_| ())
}

#[cfg(not(target_os = "windows"))]
fn start_real_capture(_source: Option<SourceSelection>) -> Result<(), String> {
    native_livekit::start_desktop_capture().map(|_| ())
}

fn start_source(config: &CaptureConfig, source: Option<SourceSelection>) -> Result<(), String> {
    if synthetic_capture() {
        native_livekit::start_synthetic_capture(config).map(|_| ())
    } else {
        start_real_capture(source)
    }
}

/// The encoder configuration for the current settings.
#[must_use]
pub fn capture_config(settings: &StreamSettings, max_bitrate: f64) -> CaptureConfig {
    let (width, height) = resolution_dims(&settings.resolution);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "sanitized fps is bounded to [1, 60]"
    )]
    let fps = settings.fps.round() as u32;
    CaptureConfig {
        width,
        height,
        fps,
        video_codec: Some(settings.video_codec.clone()),
        max_bitrate: Some(max_bitrate),
        auto_bitrate: settings.auto_bitrate,
    }
}

/// Opens the capture source (the portal picker on Wayland) for preview, without publishing.
///
/// # Errors
/// Returns an error when capture is unsupported or the source was not selected.
pub fn start_preview(
    source: Option<SourceSelection>,
    settings: &StreamSettings,
) -> Result<(), String> {
    if !capture_supported() {
        return Err("Screen capture is not supported on this platform".into());
    }
    hub().set_accepting_frames(true);
    let config = if synthetic_capture() {
        let mut config = capture_config(settings, settings.bitrate_limit);
        config.video_codec = None;
        config.max_bitrate = None;
        config.auto_bitrate = false;
        config
    } else {
        CaptureConfig {
            width: 0,
            height: 0,
            fps: 30,
            ..CaptureConfig::default()
        }
    };
    start_source(&config, source)
}

/// Publishes the previewed source, opening it first when no capture is running.
///
/// # Errors
/// Returns an error if the video track or capture cannot start.
pub fn go_live(config: CaptureConfig, source: Option<SourceSelection>) -> Result<(), String> {
    if !capture_supported() {
        return Err("Screen capture is not supported on this platform".into());
    }
    if native_livekit::is_desktop_capture_active() {
        return native_livekit::start_video_track(config);
    }
    hub().set_accepting_frames(true);
    native_livekit::start_video_track(config.clone())?;
    if let Err(error) = start_source(&config, source) {
        let _ = native_livekit::stop_video_track();
        return Err(error);
    }
    Ok(())
}

/// Starts the video track and capture together (resuming after a room replacement).
///
/// # Errors
/// Returns an error if either component fails; the track is stopped again on failure.
pub fn start_combined(
    config: CaptureConfig,
    source: Option<SourceSelection>,
) -> Result<(), String> {
    if !capture_supported() {
        return Ok(());
    }
    hub().set_accepting_frames(true);
    native_livekit::start_video_track(config.clone())?;
    if let Err(error) = start_source(&config, source) {
        let _ = native_livekit::stop_video_track();
        return Err(error);
    }
    Ok(())
}

/// Reconfigures the live encoder. Returns whether the update applied.
#[must_use]
pub fn update_video(config: CaptureConfig) -> bool {
    capture_supported() && native_livekit::start_video_track(config).is_ok()
}

/// Stops video publication and capture, keeping audio.
///
/// # Errors
/// Returns an error if a component does not stop.
pub fn stop_video_capture() -> Result<(), String> {
    let track = native_livekit::stop_video_track();
    let stopped = native_livekit::stop_desktop_capture();
    hub().set_accepting_frames(false);
    track?;
    if !stopped {
        return Err("Failed to stop desktop capture".into());
    }
    Ok(())
}

/// Stops video, capture and audio capture.
///
/// # Errors
/// Returns an error naming the first component that did not stop.
pub fn stop_native_capture() -> Result<(), String> {
    let video = stop_video_capture();
    let audio = native_rust::stop_audio_capture();
    video?;
    if !audio? {
        return Err("Failed to stop audio capture".into());
    }
    Ok(())
}

/// Moves audio capture from `previous` to `target`; target 0 stops audio.
///
/// # Errors
/// Returns the native error.
pub fn change_audio_target(previous: Option<i32>, target: i32) -> Result<bool, String> {
    let native_target = native_rust::AudioTarget::Id(target);
    if target == NO_AUDIO {
        return native_rust::stop_audio_capture();
    }
    match previous {
        None | Some(NO_AUDIO) => native_rust::start_audio_capture(&native_target),
        Some(_) => native_rust::switch_audio_capture(&native_target),
    }
}

/// # Errors
/// Returns the native enumeration error.
pub fn list_audio_apps() -> Result<Vec<AudioApp>, String> {
    native_rust::list_audio_applications()
        .map(|apps| apps.into_iter().map(AudioApp::from).collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureContextView {
    pub desktop: String,
    pub source_type: String,
    pub media_name: Option<String>,
    pub window_caption: Option<String>,
    pub app: Option<AudioApp>,
}

#[must_use]
pub fn capture_context() -> Option<CaptureContextView> {
    let context = native_rust::get_capture_context()
        .inspect_err(|error| log::warn!("Capture context unavailable: {error}"))
        .ok()?;
    Some(CaptureContextView {
        desktop: context.de,
        source_type: context.source_type,
        media_name: context.media_name,
        window_caption: context.window_caption,
        app: context.app.map(AudioApp::from),
    })
}

/// Finds the application that owns the captured window's audio.
#[must_use]
pub fn resolve_audio_source(
    name_hint: Option<&str>,
) -> (Option<AudioApp>, Option<CaptureContextView>) {
    for attempt in 0..3 {
        if let Some(app) = native_rust::resolve_audio_app_for_captured_window() {
            return (Some(app.into()), None);
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    if let Some(hint) = name_hint.filter(|h| !h.trim().is_empty())
        && let Ok(Some(app)) = native_rust::resolve_audio_app_by_name(hint)
    {
        return (Some(app.into()), None);
    }
    let context = capture_context();
    (context.as_ref().and_then(|c| c.app.clone()), context)
}

/// # Errors
/// Returns the native error.
pub fn start_metering() -> Result<bool, String> {
    native_rust::start_audio_metering()
}

/// # Errors
/// Returns the native error.
pub fn stop_metering() -> Result<bool, String> {
    native_rust::stop_audio_metering()
}

/// Windows capture sources for the in-app picker.
///
/// # Errors
/// Returns an error on platforms that use a system picker instead.
pub fn capture_sources() -> Result<Vec<CaptureSource>, String> {
    #[cfg(target_os = "windows")]
    {
        native_livekit::get_windows_capture_sources().map(|sources| {
            sources
                .into_iter()
                .map(|s| CaptureSource {
                    id: s.id,
                    title: s.title,
                    display_id: s.display_id,
                    kind: match s.kind {
                        native_livekit::WgcSourceKind::Screen => SourceKind::Screen,
                        native_livekit::WgcSourceKind::Window => SourceKind::Window,
                    },
                })
                .collect()
        })
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("Capture sources are chosen with the system picker on this platform".into())
    }
}

/// Logs the live PipeWire audio sources (development diagnostics).
pub fn log_audio_sources() {
    match native_rust::dump_audio_sources() {
        Ok(sources) => {
            log::debug!("[Presenter] live audio sources: {}", sources.len());
            for source in sources {
                log::debug!("[Presenter] audio source props: {source:?}");
            }
        }
        Err(error) => log::debug!("[Presenter] audio sources unavailable: {error}"),
    }
}

/// Stops every native media component. Safe to call repeatedly.
pub fn shutdown() {
    let _ = native_rust::stop_audio_capture();
    let _ = native_rust::stop_audio_metering();
    let _ = native_livekit::stop_video_track();
    let _ = native_livekit::stop_desktop_capture();
    hub().set_accepting_frames(false);
    unregister_callbacks();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(width: u32, height: u32, extra: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&42u64.to_le_bytes());
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend(std::iter::repeat_n(
            7u8,
            (width * height * 4) as usize + extra,
        ));
        bytes
    }

    #[test]
    fn parses_valid_preview_payloads() {
        let frame = parse_preview_payload(payload(2, 3, 0));
        let frame = frame.unwrap_or_else(|| panic!("valid payload"));
        assert_eq!((frame.width, frame.height, frame.pts_us), (2, 3, 42));
        assert_eq!(frame.bgra.len(), 24);
    }

    #[test]
    fn rejects_malformed_preview_payloads() {
        assert!(parse_preview_payload(payload(2, 3, 1)).is_none());
        assert!(parse_preview_payload(payload(0, 3, 0)).is_none());
        assert!(parse_preview_payload(vec![0; 16]).is_none());
        let mut huge = payload(1, 1, 0);
        huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        huge[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_preview_payload(huge).is_none());
    }

    #[test]
    fn mute_toggles() {
        let before = is_muted();
        assert_eq!(toggle_mute(), !before);
        assert_eq!(toggle_mute(), before);
    }
}
