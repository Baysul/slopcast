//! Desktop video capture engine: in-house portal + `pw_stream` + EGL readback.
//!
//! Replaces libwebrtc's `DesktopCapturer` (SCREEN-CAPTURE-INHOUSE.md Phase D).
//! The capture thread runs the `ScreenCast` portal handshake (zbus) and then
//! hands the portal fd + node id to the `pw_stream` engine
//! (`native_rust::video_capture`), whose EGL/DMA-BUF readback delivers linear
//! BGRA frames to the same conversion callback as before: `argb_to_i420` →
//! `VideoFrame<I420Buffer>` → the active `NativeVideoSource`. The portal
//! picker opens inside the portal `Start` phase, so the session is reported
//! "running" before the user answers it; frames flow once a source is picked.
//!
//! A `Synthetic` source (used by the headless e2e, manual probes and the
//! preview benchmark) feeds generated test-pattern BGRA frames through the
//! exact same conversion and publish path, so the whole encode → SFU →
//! spectator chain is testable without a portal picker. It is available on
//! every platform, so the preview pipeline (convert → downscale → JPEG →
//! channel) is fully exercised on Windows too; only the portal arm is
//! Linux-specific (`ScreenCastPortal`/`PwVideoCapture` come from native-rust,
//! a Linux-only dependency). Windows desktop capture (WGC via libwebrtc's
//! `DesktopCapturer`) is the documented follow-up — the preview pipeline
//! needs no further changes when it lands.
//!
//! Preview transport: the captured I420 planes are converted to RGBA with
//! libyuv (`I420ToABGR` — the only fourcc with `gl.RGBA` memory order) and
//! encoded to JPEG with libjpeg-turbo (`turbojpeg`). Previews are scaled to
//! fit the renderer's preview card — OBS-style "scale to the window"
//! (`GetScaleAndCenterPos` fit math, libyuv downscale, aspect preserved) —
//! and emitted at the stream's framerate, so the IPC channel only carries
//! what the card can show. The Tauri backend forwards the bytes over a raw
//! IPC channel (no JSON, no base64) with an 8-byte `pts_us` header so the
//! renderer can measure end-to-end latency.
//!
//! Threading: the capture thread owns the portal session and the preview poll
//! loop; the `pw_stream` engine runs its own main loop on a separate thread,
//! whose frame callback feeds the shared conversion buffer (mutex-guarded, as
//! before). The picker wait polls the stop flag, so `stop()` never waits on a
//! user that ignores the picker.

/// Preview callback type: `(jpeg_bytes, pts_us)`. The engine stays
/// Tauri-unaware — the Tauri backend forwards these bytes to the renderer's
/// raw IPC channel.
type PreviewFrameCallback = Box<dyn Fn(Vec<u8>, i64) + Send + Sync>;

use arc_swap::ArcSwapOption;
use livekit::webrtc::native::yuv_helper;
use livekit::webrtc::prelude::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use turbojpeg::{Compressor, PixelFormat};

#[cfg(target_os = "linux")]
use native_rust::video_capture::{PwVideoCapture, ScreenCastPortal, StartOutcome};

use crate::{CaptureConfig, VIDEO_SOURCE};

pub(crate) struct DesktopCaptureSession {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

pub(crate) static CAPTURE_STATE: Mutex<Option<DesktopCaptureSession>> = Mutex::new(None);
pub(crate) static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Frames handed to the video source after a successful convert. The e2e
/// test requires this to advance while streaming — a stalled counter means
/// the capture pipeline is dead (e.g. the portal picker was cancelled).
pub(crate) static STATS_PUSHED: AtomicU64 = AtomicU64::new(0);
/// Frames delivered by the capture source (portal engine or synthetic
/// generator), before conversion.
pub(crate) static STATS_DEQUEUED: AtomicU64 = AtomicU64::new(0);
/// Frames dropped because no video track was active or the frame was invalid.
pub(crate) static STATS_DROPPED: AtomicU64 = AtomicU64::new(0);
/// Capture session failures (portal errors, cancelled picker, engine start).
pub(crate) static STATS_ERRORS: AtomicU64 = AtomicU64::new(0);
/// JPEG preview frames handed to the preview callback.
pub(crate) static STATS_PREVIEW_SENT: AtomicU64 = AtomicU64::new(0);
pub(crate) static STATS_LAST_WIDTH: AtomicU32 = AtomicU32::new(0);
pub(crate) static STATS_LAST_HEIGHT: AtomicU32 = AtomicU32::new(0);

pub(crate) fn reset_stats() {
    STATS_PUSHED.store(0, Ordering::Relaxed);
    STATS_DEQUEUED.store(0, Ordering::Relaxed);
    STATS_DROPPED.store(0, Ordering::Relaxed);
    STATS_ERRORS.store(0, Ordering::Relaxed);
    STATS_PREVIEW_SENT.store(0, Ordering::Relaxed);
    STATS_LAST_WIDTH.store(0, Ordering::Relaxed);
    STATS_LAST_HEIGHT.store(0, Ordering::Relaxed);
}

/// Preview cadence before a video track is published (no stream fps to
/// target yet).
const PREVIEW_FALLBACK_FPS: u32 = 30;
/// Upper bound for the live preview cadence — a source faster than this
/// (e.g. a 120 Hz monitor) would otherwise flood the renderer channel.
const PREVIEW_MAX_FPS: u32 = 60;
/// JPEG quality for the preview encode (libjpeg-turbo, 4:2:0 subsampling).
const PREVIEW_JPEG_QUALITY: i32 = 90;

/// Preview callback: invoked with JPEG bytes while a capture session is
/// active. The engine stays Tauri-unaware — the Tauri backend wires this to
/// the renderer's preview channel.
static PREVIEW_CALLBACK: ArcSwapOption<PreviewFrameCallback> = ArcSwapOption::const_empty();
/// Bumped once per converted frame; the poll loop emits only when it
/// advances, so stale frames are never re-encoded between capture ticks.
static PREVIEW_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Registers the preview callback, replacing any previously registered one.
/// `None` is stored by `clear_preview_callback` when the app is done.
pub(crate) fn set_preview_callback(callback: PreviewFrameCallback) {
    PREVIEW_CALLBACK.store(Some(Arc::new(callback)));
}

/// Clears the registered preview callback.
pub(crate) fn clear_preview_callback() {
    PREVIEW_CALLBACK.store(None);
}

/// The published video track's encoder target: `(width, height, fps)` of the
/// last `start_video_track` config. The capture engine scales frames down to
/// these dimensions before pushing them to the encoder (libwebrtc has no
/// explicit target size of its own, so a 4K source would otherwise be encoded
/// at 4K with the user's 1080p bitrate), and the preview loop emits at this
/// framerate so the preview matches the stream.
#[derive(Debug, Clone, Copy)]
struct ScaleTarget {
    width: u32,
    height: u32,
    fps: u32,
}

static SCALE_TARGET: ArcSwapOption<ScaleTarget> = ArcSwapOption::const_empty();

/// Updates the encoder scale target; called when a video track publishes.
pub(crate) fn set_scale_target(width: u32, height: u32, fps: u32) {
    SCALE_TARGET.store(Some(Arc::new(ScaleTarget { width, height, fps })));
}

/// Clears the encoder scale target; called when the video track stops.
pub(crate) fn clear_scale_target() {
    SCALE_TARGET.store(None);
}

/// The renderer's preview viewport size `(width, height)` in device pixels
/// (the card the preview canvas is drawn into). The preview emitter scales
/// every frame to fit inside it — OBS-style "scale to the window" — instead
/// of shipping full-resolution JPEGs through the IPC channel.
static PREVIEW_VIEWPORT: ArcSwapOption<(u32, u32)> = ArcSwapOption::const_empty();

/// Reports the preview viewport size; the renderer calls this on mount and
/// whenever the preview card is resized.
pub(crate) fn set_preview_viewport(width: u32, height: u32) {
    if width == 0 || height == 0 {
        clear_preview_viewport();
        return;
    }
    PREVIEW_VIEWPORT.store(Some(Arc::new((width, height))));
}

/// Clears the reported viewport (renderer unmounted) — previews fall back to
/// the source resolution.
pub(crate) fn clear_preview_viewport() {
    PREVIEW_VIEWPORT.store(None);
}

/// OBS's `GetScaleAndCenterPos` (obs-studio `frontend/utility/display-helpers.hpp`):
/// the size the source occupies inside the viewport when scaled to fit,
/// preserving its aspect ratio. Returns `None` when the source already fits
/// inside the viewport — previews are never upscaled past the source, the
/// renderer's CSS does that.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "OBS truncates the fitted size with int(); all inputs are non-negative u32s, so the f64 products are non-negative and bounded by the viewport dimensions"
)]
fn fit_preview_size(
    base_w: u32,
    base_h: u32,
    viewport_w: u32,
    viewport_h: u32,
) -> Option<(u32, u32)> {
    if base_w == 0 || base_h == 0 || viewport_w == 0 || viewport_h == 0 {
        return None;
    }
    let viewport_aspect = f64::from(viewport_w) / f64::from(viewport_h);
    let base_aspect = f64::from(base_w) / f64::from(base_h);
    let (fit_w, fit_h) = if viewport_aspect > base_aspect {
        // The viewport is wider than the source: fit by height.
        ((f64::from(viewport_h) * base_aspect) as u32, viewport_h)
    } else {
        // The viewport is taller (or equal): fit by width.
        (viewport_w, (f64::from(viewport_w) / base_aspect) as u32)
    };
    if fit_w >= base_w && fit_h >= base_h {
        return None;
    }
    Some((fit_w.max(1), fit_h.max(1)))
}

/// Scratch buffers reused across preview emissions (allocated once per
/// session).
struct PreviewBuffers {
    /// Snapshot of the latest frame's I420 planes at the source resolution,
    /// copied out of the shared mutex so the expensive plane processing below
    /// never blocks the capture thread (which writes the same buffer in
    /// `on_frame`). Reallocated only when the source resolution changes.
    src: Option<I420Buffer>,
    /// The planes actually encoded: the source-resolution snapshot when the
    /// source fits the viewport, or the viewport-fitted libyuv downscale.
    /// Tightly packed (stride == width), same layout as `src`.
    out: Vec<u8>,
    /// Converted to tightly packed RGBA via libyuv `I420ToABGR` (the only
    /// fourcc with `gl.RGBA` memory order).
    rgba: Vec<u8>,
    /// Reused JPEG output buffer (resized to the compressor's guaranteed
    /// maximum, then truncated to the actual encoded length).
    jpeg: Vec<u8>,
    /// Lazily created libjpeg-turbo compressor; the error is cached so a
    /// broken JPEG stack is logged once, not every frame.
    compressor: Option<Result<Compressor, String>>,
}

/// Copies the latest converted frame out of the shared mutex as fast as
/// possible (a raw plane memcpy, ~3 ms at 1080p), then — when the reported
/// preview viewport is smaller than the source — scales it down to fit the
/// viewport with libyuv (OBS preview behavior: the source is scaled to the
/// window, aspect preserved). The I420 planes are converted to RGBA with
/// libyuv and encoded to a JPEG with libjpeg-turbo. Independent of
/// `VIDEO_SOURCE`, so the pre-roll preview works before any track is
/// published. Returns whether a frame was emitted.
#[allow(
    clippy::too_many_lines,
    reason = "one linear preview emission (snapshot → scale → convert → encode → callback); splitting it would scatter the buffer bookkeeping"
)]
fn emit_preview_frame(
    preview_source: &SharedFrameBuffer,
    bufs: &mut PreviewBuffers,
    last_generation: &mut u64,
    next_at: &mut Instant,
) -> bool {
    let generation = PREVIEW_GENERATION.load(Ordering::Relaxed);
    if generation == *last_generation || *next_at > Instant::now() {
        return false;
    }
    *last_generation = generation;
    // Pre-roll previews run at a fixed cadence; once a track is live the
    // preview matches the stream fps (capped — a source faster than 60 fps
    // would otherwise flood the renderer channel).
    let fps = SCALE_TARGET
        .load_full()
        .map_or(PREVIEW_FALLBACK_FPS, |target| {
            target.fps.clamp(1, PREVIEW_MAX_FPS)
        });
    *next_at = Instant::now() + Duration::from_millis(u64::from(1000 / fps));
    let Ok(guard) = preview_source.lock() else {
        return false;
    };
    let width = guard.buffer.width();
    let height = guard.buffer.height();
    if width == 0 || height == 0 {
        return false;
    }
    let (stride_y, stride_u, stride_v) = guard.buffer.strides();
    let (plane_y, plane_u, plane_v) = guard.buffer.data();
    let uv_height = height.div_ceil(2);
    let y_len = (stride_y * height) as usize;
    let u_len = (stride_u * uv_height) as usize;
    let v_len = (stride_v * uv_height) as usize;
    let src = bufs
        .src
        .get_or_insert_with(|| I420Buffer::new(width, height));
    if src.width() != width || src.height() != height {
        *src = I420Buffer::new(width, height);
    }
    let (src_plane_y, src_plane_u, src_plane_v) = src.data_mut();
    src_plane_y[..y_len].copy_from_slice(&plane_y[..y_len]);
    src_plane_u[..u_len].copy_from_slice(&plane_u[..u_len]);
    src_plane_v[..v_len].copy_from_slice(&plane_v[..v_len]);
    let pts_us = guard.timestamp_us;
    drop(guard);
    // OBS-style fit (`GetScaleAndCenterPos`): scale the source down to the
    // reported viewport when it does not fit; otherwise ship the source
    // resolution untouched (the renderer's CSS upscales it).
    let (out_w, out_h) = if let Some((fit_w, fit_h)) = PREVIEW_VIEWPORT
        .load_full()
        .and_then(|vp| fit_preview_size(width, height, vp.0, vp.1))
    {
        let scaled = src.scale(
            i32::try_from(fit_w).unwrap_or(0),
            i32::try_from(fit_h).unwrap_or(0),
        );
        let (py, pu, pv) = scaled.data();
        let total = py.len() + pu.len() + pv.len();
        bufs.out.resize(total, 0);
        bufs.out[..py.len()].copy_from_slice(py);
        bufs.out[py.len()..py.len() + pu.len()].copy_from_slice(pu);
        bufs.out[py.len() + pu.len()..].copy_from_slice(pv);
        (fit_w, fit_h)
    } else {
        bufs.out.resize(y_len + u_len + v_len, 0);
        bufs.out[..y_len].copy_from_slice(&src_plane_y[..y_len]);
        bufs.out[y_len..y_len + u_len].copy_from_slice(&src_plane_u[..u_len]);
        bufs.out[y_len + u_len..].copy_from_slice(&src_plane_v[..v_len]);
        (width, height)
    };
    // `out` is tightly packed (stride == width), so the planes split cleanly.
    let out_chroma_w = out_w.div_ceil(2);
    let out_chroma_h = out_h.div_ceil(2);
    let luma_len = (out_w * out_h) as usize;
    let chroma_len = (out_chroma_w * out_chroma_h) as usize;
    let (plane_y, rest) = bufs.out.split_at(luma_len);
    let (plane_u, plane_v) = rest.split_at(chroma_len);

    // libyuv converts the (possibly viewport-fitted) I420 planes to tightly
    // packed RGBA (memory order R,G,B,A via I420ToABGR — libyuv's "RGBA"
    // fourcc is [A,B,G,R]) — the exact byte order libjpeg-turbo's
    // PixelFormat::RGBA expects, so the JPEG encode needs no channel
    // shuffling.
    bufs.rgba.resize((out_w * out_h * 4) as usize, 0);
    yuv_helper::i420_to_abgr(
        plane_y,
        out_w,
        plane_u,
        out_chroma_w,
        plane_v,
        out_chroma_w,
        &mut bufs.rgba,
        out_w * 4,
        i32::try_from(out_w).unwrap_or(0),
        i32::try_from(out_h).unwrap_or(0),
    );

    if bufs.compressor.is_none() {
        let result = Compressor::new()
            .and_then(|mut compressor| {
                compressor
                    .set_quality(PREVIEW_JPEG_QUALITY)
                    .map(|()| compressor)
            })
            .map_err(|e| e.to_string());
        if let Err(e) = &result {
            log::error!("[desktop-capture] JPEG compressor init failed: {e}");
        }
        bufs.compressor = Some(result);
    }
    let Some(Ok(compressor)) = bufs.compressor.as_mut() else {
        return false;
    };
    let Ok(max_len) = compressor.buf_len(
        usize::try_from(out_w).unwrap_or(0),
        usize::try_from(out_h).unwrap_or(0),
    ) else {
        return false;
    };
    bufs.jpeg.resize(max_len, 0);
    let Ok(written) = compressor.compress_to_slice(
        turbojpeg::Image {
            pixels: bufs.rgba.as_slice(),
            width: usize::try_from(out_w).unwrap_or(0),
            pitch: usize::try_from(out_w * 4).unwrap_or(0),
            height: usize::try_from(out_h).unwrap_or(0),
            format: PixelFormat::RGBA,
        },
        &mut bufs.jpeg,
    ) else {
        return false;
    };
    bufs.jpeg.truncate(written);

    STATS_PREVIEW_SENT.fetch_add(1, Ordering::Relaxed);
    if let Some(callback) = PREVIEW_CALLBACK.load_full() {
        callback(bufs.jpeg.clone(), pts_us);
    }
    true
}

/// The shared conversion buffer: the capture source (pw or generator
/// thread) writes it, the preview emitter reads it. The mutex never
/// contends in practice — both holds are brief — but it gives the
/// `'static` callback closure and the loop a shared owner for the buffer.
type SharedFrameBuffer = Arc<Mutex<VideoFrame<I420Buffer>>>;
/// The capture-source frame callback: `(width, height, bgra, pts_us)`.
type FrameCallback = Box<dyn FnMut(u32, u32, &[u8], i64) + Send>;

/// Converts a linear BGRA frame into the shared I420 buffer, bumps the
/// preview generation and pushes the frame into the active video source
/// (if any). Shared by the portal engine and the synthetic generator.
///
/// The preview reads the shared buffer at the source resolution; when the
/// encoder target is smaller, a libyuv-scaled copy of the I420 planes is
/// pushed to the video source instead, so the encoder never receives a
/// frame larger than the published track's dimensions.
fn convert_and_push(
    width: u32,
    height: u32,
    bgra: &[u8],
    preview_source: &SharedFrameBuffer,
    scale_target: Option<ScaleTarget>,
) {
    let Ok(mut frame_buffer) = preview_source.lock() else {
        STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if frame_buffer.buffer.width() != width || frame_buffer.buffer.height() != height {
        frame_buffer.buffer = I420Buffer::new(width, height);
    }
    let (stride_y, stride_u, stride_v) = frame_buffer.buffer.strides();
    let (plane_y, plane_u, plane_v) = frame_buffer.buffer.data_mut();
    // The engine delivers tightly packed rows (stride == width * 4).
    yuv_helper::argb_to_i420(
        bgra,
        width * 4,
        plane_y,
        stride_y,
        plane_u,
        stride_u,
        plane_v,
        stride_v,
        i32::try_from(width).unwrap_or(0),
        i32::try_from(height).unwrap_or(0),
    );
    frame_buffer.timestamp_us = monotonic_us();
    PREVIEW_GENERATION.fetch_add(1, Ordering::Relaxed);
    STATS_LAST_WIDTH.store(width, Ordering::Relaxed);
    STATS_LAST_HEIGHT.store(height, Ordering::Relaxed);

    let Some(source) = VIDEO_SOURCE.load_full() else {
        STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    match scale_target.filter(|target| width > target.width || height > target.height) {
        // libwebrtc's encoder has no explicit target size (VideoEncoding
        // carries only bitrate/fps), so without this a 4K source would be
        // encoded at 4K with the bitrate the user configured for 1080p.
        // I420Buffer::scale is libwebrtc's libyuv-backed scaler.
        Some(target) => {
            let scaled = frame_buffer.buffer.scale(
                i32::try_from(target.width).unwrap_or(0),
                i32::try_from(target.height).unwrap_or(0),
            );
            let scaled_frame = VideoFrame {
                rotation: VideoRotation::VideoRotation0,
                timestamp_us: frame_buffer.timestamp_us,
                frame_metadata: None,
                buffer: scaled,
            };
            source.capture_frame(&scaled_frame);
        }
        None => source.capture_frame(&frame_buffer),
    }
    STATS_PUSHED.fetch_add(1, Ordering::Relaxed);
}

/// Builds the frame callback shared by the portal engine and the
/// synthetic generator: counts the frame, converts it to I420 and pushes
/// it — scaled down to the published track's resolution when larger.
fn make_on_frame(preview_source: SharedFrameBuffer) -> FrameCallback {
    Box::new(move |width, height, bgra, _pts_us| {
        STATS_DEQUEUED.fetch_add(1, Ordering::Relaxed);
        if width == 0 || height == 0 {
            STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let scale_target = SCALE_TARGET.load_full().map(|t| *t);
        convert_and_push(width, height, bgra, &preview_source, scale_target);
    })
}

/// Monotonic microsecond timestamp for video frames, anchored at first use.
fn monotonic_us() -> i64 {
    use std::sync::OnceLock;
    static ANCHOR: OnceLock<Instant> = OnceLock::new();
    let anchor = ANCHOR.get_or_init(Instant::now);
    i64::try_from(anchor.elapsed().as_micros()).unwrap_or(0)
}

/// Starts the `pw_stream` engine for a negotiated portal stream: opens the
/// isolated `PipeWire` remote on the portal fd and captures the stream's
/// node, delivering linear BGRA frames to `on_frame`.
#[cfg(target_os = "linux")]
fn start_pw_capture(
    portal: &ScreenCastPortal,
    stream: &native_rust::video_capture::PortalStream,
    on_frame: native_rust::video_capture::VideoFrameCallback,
) -> Result<PwVideoCapture, String> {
    // When the portal reports no size, offer no fixed size either — a
    // fixed value that does not match the source's resolution fails the
    // format intersection.
    let (width, height) = stream.size.map_or((0, 0), |(w, h)| {
        (u32::try_from(w).unwrap_or(0), u32::try_from(h).unwrap_or(0))
    });
    let fd = portal.open_pipewire_remote()?;
    PwVideoCapture::start(Some(fd), stream.node_id, width, height, 0, on_frame)
}

/// What feeds the capture pipeline: the portal screencast (real screens,
/// Linux-only) or a synthetic test pattern (headless e2e / probes /
/// benchmarks, all platforms).
#[derive(Debug, Clone, Copy)]
enum CaptureSource {
    #[cfg(target_os = "linux")]
    Portal,
    Synthetic {
        width: u32,
        height: u32,
        fps: u32,
    },
}

/// Fills `bgra` with a deterministic test pattern: eight vertical color
/// bars with a white box moving left→right that wraps around, so frame
/// contents differ from frame to frame (proves motion to the spectator
/// and shows up as non-black content).
fn synthetic_frame(width: u32, height: u32, frame_index: u64, bgra: &mut [u8]) {
    const BARS: [[u8; 3]; 8] = [
        [255, 255, 255], // white
        [255, 255, 0],   // yellow
        [0, 255, 255],   // cyan
        [0, 255, 0],     // green
        [255, 0, 255],   // magenta
        [255, 0, 0],     // red
        [0, 0, 255],     // blue
        [0, 0, 0],       // black
    ];
    let width = usize::try_from(width).unwrap_or(0);
    let height = usize::try_from(height).unwrap_or(0);
    let bar_width = width.div_ceil(BARS.len());
    for row in bgra.chunks_exact_mut(width * 4).take(height) {
        for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
            let bar = (x / bar_width).min(BARS.len() - 1);
            // BGRA byte order, alpha forced to 255.
            let [red, green, blue] = BARS[bar];
            pixel.copy_from_slice(&[blue, green, red, 255]);
        }
    }
    // Moving white box: 1/8 of the frame, wrapping at the right edge.
    let box_w = (width / 8).max(1);
    let box_h = (height / 8).max(1);
    let travel = width - box_w;
    let x0 = (u64::try_from(travel)
        .unwrap_or(0)
        .saturating_mul(frame_index % 128)
        / 128) as usize;
    let y0 = height / 8;
    for y in y0..(y0 + box_h).min(height) {
        let row = &mut bgra[(y * width + x0) * 4..(y * width + x0 + box_w) * 4];
        for pixel in row.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[255, 255, 255, 255]);
        }
    }
}

/// The capture thread: owns the portal session (Portal) or the synthetic
/// generator thread, plus the preview poll loop.
///
/// Phase A (portal handshake) runs here; the ready signal is sent after
/// `SelectSources` and before `Start`, because the native picker opens
/// inside `Start` and the caller must see the session as running while
/// the user chooses a source. A cancelled picker or a failed engine start
/// leaves the session idling (old behavior): no frames, one counted
/// error, the user stops manually.
#[allow(
    clippy::too_many_lines,
    reason = "one linear capture lifecycle (source acquisition → preview loop → teardown); splitting it would scatter the failure accounting"
)]
fn run_capture(
    stop: &Arc<AtomicBool>,
    ready_tx: &mpsc::Sender<Result<(), String>>,
    source: CaptureSource,
) {
    // The converted frame is shared between the source callback (pw or
    // generator thread) and the preview emitter (this thread). The mutex
    // never contends in practice — both holds are brief — but it gives
    // the 'static callback closure and the loop a shared owner for the
    // buffer.
    let frame_buffer = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us: 0,
        frame_metadata: None,
        buffer: I420Buffer::new(1, 1),
    };
    let preview_source = Arc::new(Mutex::new(frame_buffer));
    let on_frame = make_on_frame(Arc::clone(&preview_source));

    #[cfg(target_os = "linux")]
    let mut portal: Option<ScreenCastPortal> = None;
    #[cfg(target_os = "linux")]
    let mut capture: Option<PwVideoCapture> = None;
    let mut generator = None;

    match source {
        #[cfg(target_os = "linux")]
        CaptureSource::Portal => {
            let connection = match zbus::blocking::Connection::session() {
                Ok(connection) => connection,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("Portal session bus: {e}")));
                    return;
                }
            };
            let portal_session = match ScreenCastPortal::connect(connection) {
                Ok(portal) => portal,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            if let Err(e) = portal_session.select_sources() {
                let _ = ready_tx.send(Err(e));
                return;
            }

            CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
            let _ = ready_tx.send(Ok(()));

            // Phases B + C: `pw_stream` on the portal fd, EGL readback → on_frame.
            match portal_session.start(Some(stop.as_ref())) {
                Ok(StartOutcome::Stream(stream)) => {
                    if !stop.load(Ordering::Relaxed) {
                        match start_pw_capture(&portal_session, &stream, Box::new(on_frame)) {
                            Ok(engine) => capture = Some(engine),
                            Err(e) => {
                                STATS_ERRORS.fetch_add(1, Ordering::Relaxed);
                                log::error!("[desktop-capture] capture engine failed: {e}");
                            }
                        }
                    }
                }
                Ok(StartOutcome::Cancelled) => {
                    STATS_ERRORS.fetch_add(1, Ordering::Relaxed);
                    log::error!("[desktop-capture] portal picker cancelled");
                }
                Err(e) => {
                    STATS_ERRORS.fetch_add(1, Ordering::Relaxed);
                    log::error!("[desktop-capture] portal error: {e}");
                }
            }
            portal = Some(portal_session);
        }
        CaptureSource::Synthetic { width, height, fps } => {
            CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
            let _ = ready_tx.send(Ok(()));

            let fps = fps.max(1);
            let interval = Duration::from_micros(1_000_000 / u64::from(fps));
            let generator_stop = Arc::clone(stop);
            let handle = thread::Builder::new()
                .name("synthetic-capture".into())
                .spawn(move || {
                    let mut scratch = vec![0u8; (width * height * 4) as usize];
                    let mut on_frame = on_frame;
                    let mut frame_index = 0u64;
                    while !generator_stop.load(Ordering::Relaxed) {
                        let started = Instant::now();
                        synthetic_frame(width, height, frame_index, &mut scratch);
                        on_frame(width, height, &scratch, monotonic_us());
                        frame_index += 1;
                        let elapsed = started.elapsed();
                        if elapsed < interval {
                            thread::sleep(interval.saturating_sub(elapsed));
                        }
                    }
                });
            match handle {
                Ok(handle) => generator = Some(handle),
                Err(e) => {
                    STATS_ERRORS.fetch_add(1, Ordering::Relaxed);
                    log::error!("[desktop-capture] synthetic thread spawn failed: {e}");
                }
            }
        }
    }

    // Reused scratch buffers for the preview emitter (allocated once).
    let mut preview_bufs = PreviewBuffers {
        src: None,
        out: Vec::new(),
        rgba: Vec::new(),
        jpeg: Vec::new(),
        compressor: None,
    };
    let mut last_preview_generation = 0u64;
    let mut next_preview_at = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        emit_preview_frame(
            &preview_source,
            &mut preview_bufs,
            &mut last_preview_generation,
            &mut next_preview_at,
        );
        thread::sleep(Duration::from_millis(8));
    }
    #[cfg(target_os = "linux")]
    if let Some(mut capture) = capture {
        capture.stop();
    }
    if let Some(handle) = generator {
        let _ = handle.join();
    }
    // Dropping the portal closes the session (and its screencast node).
    #[cfg(target_os = "linux")]
    drop(portal);
    CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
}

/// Starts the desktop capturer on its own thread. On Linux, returns once
/// the portal session is created; the native portal picker then appears and
/// frames flow after the user selects a source. On other platforms there is
/// no capture source yet (Windows WGC is the documented follow-up), so this
/// reports video as unavailable (`Ok(false)`) while the synthetic source
/// and the whole preview pipeline remain usable.
///
/// # Errors
///
/// Returns an error if a capture session is already active, the thread
/// cannot be spawned, or the portal session fails to initialize within
/// five seconds.
pub(crate) fn start() -> Result<bool, String> {
    #[cfg(target_os = "linux")]
    {
        start_with(CaptureSource::Portal)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(false)
    }
}

/// Starts the synthetic test-pattern capture (headless e2e, probes and the
/// preview benchmark): generated BGRA frames feed the exact same conversion
/// and publish path as the portal engine, with no picker and no Wayland
/// requirement. Available on every platform, so the preview pipeline can be
/// exercised on Windows too.
///
/// # Errors
///
/// Returns an error if a capture session is already active or the
/// generator thread cannot be spawned.
pub(crate) fn start_synthetic_capture(config: &CaptureConfig) -> Result<bool, String> {
    start_with(CaptureSource::Synthetic {
        width: config.width,
        height: config.height,
        fps: config.fps,
    })
}

fn start_with(source: CaptureSource) -> Result<bool, String> {
    let mut guard = CAPTURE_STATE
        .lock()
        .map_err(|e| format!("Desktop capture state lock poisoned: {e}"))?;
    if guard.is_some() {
        return Err("Desktop capture already active".into());
    }

    reset_stats();
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread_stop = stop.clone();
    let handle = thread::Builder::new()
        .name("desktop-capture".into())
        .spawn(move || run_capture(&thread_stop, &ready_tx, source))
        .map_err(|e| format!("Thread spawn: {e}"))?;

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {
            *guard = Some(DesktopCaptureSession {
                stop,
                join: Some(handle),
            });
            Ok(true)
        }
        Ok(Err(reason)) => {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
            Err(reason)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
            Err("Timed out starting desktop capture".into())
        }
    }
}

/// Stops the active desktop capture session (closing its portal stream).
pub(crate) fn stop() -> bool {
    let session = {
        let Ok(mut guard) = CAPTURE_STATE.lock() else {
            return true;
        };
        guard.take()
    };
    let Some(mut session) = session else {
        return true;
    };
    session.stop.store(true, Ordering::Relaxed);
    if let Some(handle) = session.join.take() {
        let _ = handle.join();
    }
    true
}

/// Returns `true` while the capturer is running (the portal picker may
/// still be awaiting a selection).
pub(crate) fn is_active() -> bool {
    CAPTURE_ACTIVE.load(Ordering::Relaxed)
}

/// Per-stage counters for the desktop capture pipeline, reset on every
/// `start_desktop_capture`. `frames_dequeued` counts frames received from
/// the source; `frames_pushed` counts those converted to `I420` and
/// delivered to the video track; `frames_dropped` counts frames skipped
/// while no track was active; `preview_frames_sent` counts JPEG preview
/// frames emitted via the preview callback; `capture_errors` counts
/// portal/engine failures.
pub(crate) fn stats() -> crate::DesktopCaptureStats {
    crate::DesktopCaptureStats {
        frames_dequeued: STATS_DEQUEUED.load(Ordering::Relaxed).cast_signed(),
        frames_pushed: STATS_PUSHED.load(Ordering::Relaxed).cast_signed(),
        frames_dropped: STATS_DROPPED.load(Ordering::Relaxed).cast_signed(),
        capture_errors: STATS_ERRORS.load(Ordering::Relaxed).cast_signed(),
        preview_frames_sent: STATS_PREVIEW_SENT.load(Ordering::Relaxed).cast_signed(),
        last_width: i64::from(STATS_LAST_WIDTH.load(Ordering::Relaxed)),
        last_height: i64::from(STATS_LAST_HEIGHT.load(Ordering::Relaxed)),
    }
}

#[cfg(test)]
mod probe {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    /// The encode-target scale path uses libwebrtc's libyuv-backed
    /// `I420Buffer::scale`: a uniform-color 8×8 frame scaled 2:1 must keep
    /// its dimensions and its dominant color (a broken scaler would either
    /// panic, resize wrong or shift the color).
    #[test]
    fn i420_scale_preserves_dimensions_and_mean_color() {
        let mut src = I420Buffer::new(8, 8);
        {
            let (plane_y, plane_u, plane_v) = src.data_mut();
            for byte in &mut *plane_y {
                *byte = 100;
            }
            for byte in &mut *plane_u {
                *byte = 90;
            }
            for byte in &mut *plane_v {
                *byte = 240;
            }
        }
        let scaled = src.scale(4, 4);
        assert_eq!((scaled.width(), scaled.height()), (4, 4));
        let (plane_y, plane_u, plane_v) = scaled.data();
        assert!(
            plane_y.iter().all(|&v| v == 100),
            "luma must survive the scale"
        );
        assert!(plane_u.iter().all(|&v| v == 90));
        assert!(plane_v.iter().all(|&v| v == 240));
    }

    /// The encode-target scale must never be applied when the source is
    /// smaller than the target (upscaling would waste encoder bitrate on
    /// interpolated pixels).
    #[test]
    fn scale_target_is_only_applied_to_larger_sources() {
        let needs_scale = |src_w: u32, src_h: u32, t: ScaleTarget| {
            (src_w > t.width || src_h > t.height).then_some(t)
        };
        let target = ScaleTarget {
            width: 1920,
            height: 1080,
            fps: 60,
        };
        assert!(
            needs_scale(3840, 2160, target).is_some(),
            "4K must scale down"
        );
        assert!(
            needs_scale(1920, 1080, target).is_none(),
            "exact match must not scale"
        );
        assert!(
            needs_scale(1920, 1040, target).is_none(),
            "smaller-than-target sources must pass through untouched"
        );
        assert!(
            needs_scale(1280, 720, target).is_none(),
            "720p must never be upscaled to 1080p"
        );
    }

    /// OBS `GetScaleAndCenterPos` parity: a 1920×1080 source in a wider
    /// viewport fits by height; in a taller/narrower viewport it fits by
    /// width. The result keeps the source aspect within the viewport bounds.
    #[test]
    fn fit_preview_size_matches_obs_contain_math() {
        // Viewport wider than the source (2:1 vs 16:9): fit by height.
        assert_eq!(fit_preview_size(1920, 1080, 2000, 1000), Some((1777, 1000)));
        // Viewport narrower than the source (16:10 vs 16:9): fit by width.
        assert_eq!(fit_preview_size(1920, 1080, 1600, 1000), Some((1600, 900)));
        // Viewport with a different aspect: fit by width keeps the aspect.
        assert_eq!(fit_preview_size(1920, 1080, 900, 1000), Some((900, 506)));
    }

    /// The preview is never upscaled past the source — the renderer's CSS
    /// does that — and degenerate viewports are ignored.
    #[test]
    fn fit_preview_size_never_upscales_and_guards_zeroes() {
        // Same-aspect viewport smaller than the source: downscale to fit.
        assert_eq!(fit_preview_size(1920, 1080, 1280, 720), Some((1280, 720)));
        // Source smaller than the viewport: no downscale needed.
        assert_eq!(fit_preview_size(1280, 720, 1920, 1080), None);
        assert_eq!(fit_preview_size(1280, 720, 4000, 3000), None);
        // Zero viewport or source dimensions are ignored.
        assert_eq!(fit_preview_size(0, 0, 1280, 720), None);
        assert_eq!(fit_preview_size(1920, 1080, 0, 720), None);
        assert_eq!(fit_preview_size(1920, 1080, 1280, 0), None);
    }

    /// The synthetic pattern is deterministic, non-uniform and moves with
    /// the frame index.
    #[test]
    fn synthetic_pattern_is_deterministic_and_moving() {
        let (frame_w, frame_h) = (128u32, 64u32);
        let mut first_frame = vec![0u8; (frame_w * frame_h * 4) as usize];
        let mut same_index = vec![0u8; (frame_w * frame_h * 4) as usize];
        synthetic_frame(frame_w, frame_h, 0, &mut first_frame);
        synthetic_frame(frame_w, frame_h, 0, &mut same_index);
        assert_eq!(
            first_frame, same_index,
            "same index must produce identical frames"
        );
        // Color bars are not uniform.
        let first = &first_frame[0..4];
        let mid = &first_frame[(frame_w as usize / 2 * 4)..(frame_w as usize / 2 * 4) + 4];
        assert_ne!(first, mid, "bars must differ across the frame");
        // The moving box changes position between indices.
        let mut later_frame = vec![0u8; (frame_w * frame_h * 4) as usize];
        synthetic_frame(frame_w, frame_h, 10, &mut later_frame);
        assert_ne!(
            first_frame, later_frame,
            "frame contents must move with the index"
        );
        // Alpha is fully opaque everywhere.
        assert!(first_frame.chunks_exact(4).all(|pixel| pixel[3] == 255));
    }

    /// The preview JPEG encode round-trips through libjpeg-turbo with the
    /// dominant colors intact: red top half, blue bottom half. Pins the
    /// `PixelFormat::RGBA` byte order (a swapped format would turn the
    /// blocks cyan/magenta).
    #[test]
    fn preview_jpeg_round_trip_keeps_dominant_colors() {
        const W: usize = 64;
        const H: usize = 48;
        let mut rgba = vec![0u8; W * H * 4];
        for (i, pixel) in rgba.chunks_exact_mut(4).enumerate() {
            if i / W < H / 2 {
                pixel.copy_from_slice(&[255, 0, 0, 255]);
            } else {
                pixel.copy_from_slice(&[0, 0, 255, 255]);
            }
        }

        let mut compressor = Compressor::new().unwrap_or_else(|e| panic!("compressor: {e}"));
        compressor
            .set_quality(PREVIEW_JPEG_QUALITY)
            .unwrap_or_else(|e| panic!("quality: {e}"));
        let max_len = compressor
            .buf_len(W, H)
            .unwrap_or_else(|e| panic!("buf_len: {e}"));
        let mut jpeg = vec![0u8; max_len];
        let written = compressor
            .compress_to_slice(
                turbojpeg::Image {
                    pixels: rgba.as_slice(),
                    width: W,
                    pitch: W * 4,
                    height: H,
                    format: PixelFormat::RGBA,
                },
                &mut jpeg,
            )
            .unwrap_or_else(|e| panic!("compress: {e}"));
        jpeg.truncate(written);
        assert!(
            jpeg.len() < rgba.len() / 4,
            "JPEG must be far smaller than the raw RGBA frame"
        );

        let decoded = turbojpeg::decompress(&jpeg, PixelFormat::RGBA)
            .unwrap_or_else(|e| panic!("decompress: {e}"));
        assert_eq!(decoded.width, W);
        assert_eq!(decoded.height, H);
        let pixel_at = |x: usize, y: usize| {
            let i = (y * decoded.pitch + x * 4) as usize;
            (
                decoded.pixels[i],
                decoded.pixels[i + 1],
                decoded.pixels[i + 2],
            )
        };
        let (r, g, b) = pixel_at(W / 2, H / 4);
        assert!(
            r > 200 && g < 80 && b < 80,
            "top half must stay red, got ({r},{g},{b})"
        );
        let (r, g, b) = pixel_at(W / 2, 3 * H / 4);
        assert!(
            b > 200 && r < 80 && g < 80,
            "bottom half must stay blue, got ({r},{g},{b})"
        );
    }

    /// Headless probe of the whole synthetic pipeline: frames must flow
    /// through conversion and the JPEG preview emitter must fire even with
    /// no video track published. Run with:
    ///
    /// ```sh
    /// cargo test -p native-livekit --release -- --ignored synthetic_capture_probe --nocapture
    /// ```
    #[test]
    #[ignore = "probe: runs the synthetic capture thread for a few seconds"]
    fn synthetic_capture_probe() {
        let config = CaptureConfig {
            width: 1280,
            height: 720,
            fps: 30,
            video_codec: None,
            max_bitrate: None,
        };
        let started = start_synthetic_capture(&config).unwrap_or_else(|e| panic!("start: {e}"));
        assert!(started);
        std::thread::sleep(Duration::from_millis(1500));
        let s = stats();
        stop();
        assert!(s.frames_dequeued > 0, "synthetic frames must flow");
        assert!(
            s.preview_frames_sent > 0,
            "preview emitter must run on synthetic frames"
        );
        assert_eq!((s.last_width, s.last_height), (1280, 720));
    }

    /// Manual control probe: runs the whole in-house pipeline (portal →
    /// `pw_stream` → EGL readback → conversion) through the public capture
    /// API in a plain Rust process. Run with:
    ///
    /// ```sh
    /// cargo test --release -p native-livekit -- --ignored capture_engine_probe --nocapture
    /// ```
    ///
    /// A portal picker appears; select a source, then observe the
    /// counters. The process exits itself after 20 s; a cancelled picker
    /// (or engine failure) counts a capture error and ends the probe early.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "manual probe: requires a Wayland session with a picker to answer"]
    fn capture_engine_probe() {
        // The Tauri app arms libwebrtc's PipeWire dlopen shims at startup
        // (setup wiring); a bare test binary must do the same, or
        // `pipewire::init` jumps a NULL shim and SIGSEGVs (see the
        // probe_a/probe_b pinning of that collision).
        crate::arm_pipewire_shims();
        let started = start().unwrap_or_else(|e| panic!("start failed: {e}"));
        assert!(started);
        eprintln!("[probe] capture started — answer the portal picker");
        for second in 0..20 {
            std::thread::sleep(Duration::from_secs(1));
            let s = stats();
            eprintln!(
                "[probe] t={second}s active={} pushed={} dequeued={} dropped={} errors={} preview={} size={}x{}",
                is_active(),
                s.frames_pushed,
                s.frames_dequeued,
                s.frames_dropped,
                s.capture_errors,
                s.preview_frames_sent,
                s.last_width,
                s.last_height,
            );
            if s.capture_errors > 0 {
                eprintln!("[probe] capture errored (cancelled picker or engine failure)");
                break;
            }
        }
        stop();
        eprintln!(
            "[probe] stopped — final: pushed={} dequeued={} dropped={} errors={} preview={}",
            STATS_PUSHED.load(Ordering::Relaxed),
            STATS_DEQUEUED.load(Ordering::Relaxed),
            STATS_DROPPED.load(Ordering::Relaxed),
            STATS_ERRORS.load(Ordering::Relaxed),
            STATS_PREVIEW_SENT.load(Ordering::Relaxed),
        );
        if STATS_PUSHED.load(Ordering::Relaxed) == 0 {
            eprintln!("[probe] WARNING: no frames pushed — run pw_video_probe first");
        }
    }

    /// Phase 1 benchmark: the per-frame cost of the old base64-RGBA
    /// preview payload vs a raw byte copy vs the JPEG encode, at 640×360.
    /// Run with:
    ///
    /// ```sh
    /// cargo test --release -p native-livekit -- --ignored preview_payload_bench --nocapture
    /// ```
    #[test]
    #[ignore = "bench: per-frame preview payload cost comparison; needs --release for meaningful numbers"]
    fn preview_payload_bench() {
        const W: usize = 640;
        const H: usize = 360;
        const ITERATIONS: usize = 300;
        // Deterministic pseudo-random pixels (xorshift) — flat/gray content
        // would compress to a handful of KB and flatter the JPEG numbers.
        let mut rgba = vec![0u8; W * H * 4];
        {
            let mut state = 0x1234_5678_9abc_def0u64;
            for byte in &mut rgba {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = (state & 0xFF) as u8;
            }
        }

        let mut encoded_len = 0;
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            encoded_len = STANDARD.encode(&rgba).len();
        }
        report(
            "base64-RGBA (old payload)",
            start.elapsed(),
            encoded_len,
            ITERATIONS,
        );

        let mut dst = vec![0u8; W * H * 4];
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            dst.copy_from_slice(&rgba);
            std::hint::black_box(&dst);
        }
        report("raw byte copy", start.elapsed(), dst.len(), ITERATIONS);

        // Reused compressor + buffer, exactly like the production emitter.
        let mut compressor = Compressor::new().unwrap_or_else(|e| panic!("compressor: {e}"));
        compressor
            .set_quality(PREVIEW_JPEG_QUALITY)
            .unwrap_or_else(|e| panic!("quality: {e}"));
        let max_len = compressor
            .buf_len(W, H)
            .unwrap_or_else(|e| panic!("buf_len: {e}"));
        let mut jpeg = vec![0u8; max_len];
        let mut jpeg_len = 0;
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            jpeg_len = compressor
                .compress_to_slice(
                    turbojpeg::Image {
                        pixels: rgba.as_slice(),
                        width: W,
                        pitch: W * 4,
                        height: H,
                        format: PixelFormat::RGBA,
                    },
                    &mut jpeg,
                )
                .unwrap_or_else(|e| panic!("compress: {e}"));
        }
        report(
            "turbojpeg q=90 (reused buffer)",
            start.elapsed(),
            jpeg_len,
            ITERATIONS,
        );
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "benchmark display only; frame byte counts are far below f64's 2^52 exact range"
    )]
    fn report(label: &str, total: Duration, bytes_per_frame: usize, iterations: usize) {
        let per_frame = total / u32::try_from(iterations).unwrap_or(1);
        let mb_per_s = (bytes_per_frame as f64) / (per_frame.as_secs_f64() * 1_000_000.0);
        eprintln!(
            "[bench] {label}: {per_frame:?}/frame, {bytes_per_frame} B/frame, {mb_per_s:.1} MB/s"
        );
    }
}
