//! Desktop video capture engine: libwebrtc `DesktopCapturer` — the `PipeWire`
//! capturer (XDG portal picker) on Linux, WGC on Windows — plus a synthetic
//! test pattern everywhere.
//!
//! On Linux the engine is `linux_capture` (`LinuxDesktopCapture`): libwebrtc's
//! own `PipeWire` capturer (`livekit::webrtc::desktop_capturer`, bundled in the
//! m144 prebuilt), which replaces the former in-house portal + `pw_stream` +
//! EGL engine (SCREEN-CAPTURE-INHOUSE.md, now superseded). The portal picker
//! opens inside `start_capture`, so the session is reported "running" before
//! the user answers it; frames flow once a source is picked. The portal's
//! async `GDBus` handshake is dispatched on the capture thread (see
//! `LinuxDesktopCapture`).
//!
//! On Windows the same pipeline is fed by `wgc_capture` (`WgcCapture`, a
//! `CaptureSource::Wgc` arm): libwebrtc's `DesktopCapturer` with the WGC
//! backend, reachable through the same bundled bindings, polled at the
//! encoder target fps from the shared capture loop. The renderer's in-app
//! picker supplies the `(kind, id)` selection — Windows has no system
//! picker.
//!
//! A `Synthetic` source (used by the headless e2e, manual probes and the
//! preview benchmark) feeds generated test-pattern BGRA frames through the
//! exact same conversion and publish path, so the whole encode → SFU →
//! spectator chain is testable without a portal picker. It is available on
//! every platform, so the preview pipeline (convert → downscale → JPEG →
//! channel) is fully exercised on Windows too; macOS remains video-less
//! (`Ok(false)` from `start()` — no capture route).
//!
//! Preview transport: the capture engines deliver tightly packed BGRA (rows
//! re-packed from the capturer's possibly padded stride). The preview thread
//! keeps the newest I420 planes, scales them to fit the renderer's preview
//! card — OBS-style "scale to the window" (aspect preserved, never upscaled)
//! — and emits the BGRA payload at the stream's framerate, so the IPC
//! channel only carries what the card can show. The Tauri backend forwards
//! the bytes over a raw IPC channel (no JSON, no base64) with a 16-byte
//! little-endian header (`u64 pts_us`, `u32 width`, `u32 height`) so the
//! renderer can size its texture and measure end-to-end latency; the
//! renderer uploads the BGRA pixels into a persistent GPU texture as-is (no
//! per-frame decode, no channel shuffling).
//!
//! Threading: the capture thread owns the engine and the paced delivery
//! loop; a dedicated preview thread emits frames at its own cadence (it only
//! reads the newest stashed I420 planes under a brief lock), so the
//! scale/copy work can never delay a delivery. `stop()` is polled by the
//! loop itself, so it never waits on a user that ignores the picker.

/// Preview callback type: `(payload, pts_us)`, where `payload` is the raw
/// channel frame — 16-byte little-endian header (`u64 pts_us`, `u32 width`,
/// `u32 height`) followed by tightly packed BGRA rows. The engine stays
/// Tauri-unaware — the Tauri backend forwards these bytes to the renderer's
/// raw IPC channel.
type PreviewFrameCallback = Box<dyn Fn(Vec<u8>, i64) + Send + Sync>;

/// Capture-ended callback type: invoked once when the portal session closes
/// while the capture is running — the compositor ended the stream (e.g. the
/// presenter closed the captured window). The Tauri backend forwards this to
/// the renderer as the `capture-ended` event, which tears the share down.
type CaptureEndedCallback = Box<dyn Fn() + Send + Sync>;

use arc_swap::ArcSwapOption;
use livekit::webrtc::native::yuv_helper;
use livekit::webrtc::prelude::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

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
    CAPTURE_ENDED_EMITTED.store(false, Ordering::Relaxed);
}

/// Preview cadence before a video track is published (no stream fps to
/// target yet).
const PREVIEW_FALLBACK_FPS: u32 = 30;
/// Upper bound for the live preview cadence — a source faster than this
/// (e.g. a 120 Hz monitor) would otherwise flood the renderer channel.
const PREVIEW_MAX_FPS: u32 = 60;
/// Frames the delivery pacer holds between capture and encode. Sized so a
/// source burst (the portal engine reads back inline in the `PipeWire`
/// process callback, so delivery is naturally bursty) is absorbed without
/// adding more than a frame of latency; drop-oldest keeps content fresh.
const PACER_CAPACITY: usize = 4;

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

/// Invoked once per capture when the portal session closes unexpectedly
/// (the compositor ended the stream — e.g. the captured window was closed).
static CAPTURE_ENDED_CALLBACK: ArcSwapOption<CaptureEndedCallback> = ArcSwapOption::const_empty();
/// Guards the ended callback so a session fires it exactly once.
static CAPTURE_ENDED_EMITTED: AtomicBool = AtomicBool::new(false);

/// Registers the capture-ended callback, replacing any previously
/// registered one.
pub(crate) fn set_capture_ended_callback(callback: CaptureEndedCallback) {
    CAPTURE_ENDED_CALLBACK.store(Some(Arc::new(callback)));
}

/// Fires the capture-ended callback exactly once per capture session when
/// the captured source is gone: WGC `ERROR_PERMANENT` (captured window
/// closed) or the `PipeWire` portal session closing (compositor ended the
/// stream). The flag is reset per session in `reset_stats`.
pub(crate) fn fire_capture_ended_once() {
    if !CAPTURE_ENDED_EMITTED.swap(true, Ordering::Relaxed)
        && let Some(callback) = CAPTURE_ENDED_CALLBACK.load_full()
    {
        callback();
    }
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

/// Poll cadence for engines that pace themselves (the WGC engine and the
/// Linux `PipeWire` engine): the encoder target fps when a track is live
/// (capped at the preview ceiling), else the fallback preview cadence.
/// Re-read per poll via `ArcSwap`, so going live mid-capture speeds the
/// capture up without a restart.
pub(crate) fn capture_poll_fps() -> u32 {
    SCALE_TARGET
        .load_full()
        .map_or(PREVIEW_FALLBACK_FPS, |target| {
            target.fps.clamp(1, PREVIEW_MAX_FPS)
        })
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
struct PreviewScaleBufs {
    /// I420 planes copied out of the stash under a brief lock (packed
    /// Y, U, V with `y_len = w*h` and `uv_len = w*h/4` each).
    planes: Vec<u8>,
    /// Final channel payload: 16-byte header + the fitted BGRA pixels.
    out: Vec<u8>,
    /// Reusable I420 frame rebuilt from `planes`; recreated only when the
    /// source dimensions change. The old per-emission `I420Buffer::new`
    /// allocated a full-source-res buffer (≈2.25 MB @1080p, ≈12 MB @4K)
    /// on every emitted frame.
    src: Option<I420Buffer>,
}

/// The newest captured I420 frame, stashed by the capture callback for the
/// preview thread (which reads it without touching the paced queue). The
/// planes Vec keeps its allocation across frames. The three length fields
/// mirror the *actual* plane slice lengths — `I420Buffer::new` sizes the
/// chroma planes as `ceil(w/2) × ceil(h/2)`, which exceeds `w*h/4`
/// arithmetic whenever either dimension is odd (mid-resize frames, e.g.
/// 1280×693); sizing the copy by arithmetic panicked with a
/// `copy_from_slice` length mismatch.
struct PreviewI420 {
    width: u32,
    height: u32,
    pts_us: i64,
    y_len: usize,
    u_len: usize,
    v_len: usize,
    planes: Vec<u8>,
}

static PREVIEW_I420: Mutex<Option<PreviewI420>> = Mutex::new(None);

/// Snapshot the newest captured frame and ship it to the renderer as a
/// raw channel payload: `[pts_us u64 LE][width u32 LE][height u32 LE][tight
/// BGRA rows]`, scaled to fit the reported preview viewport (OBS-style
/// "scale to the window", aspect preserved, never upscaled). Independent of
/// `VIDEO_SOURCE`, so the pre-roll preview works before any track is
/// published. Returns whether a frame was emitted.
#[allow(
    clippy::too_many_lines,
    reason = "one linear preview emission (snapshot → fit → scale → callback); splitting it would scatter the buffer bookkeeping"
)]
fn emit_preview_frame(
    bufs: &mut PreviewScaleBufs,
    last_generation: &mut u64,
    next_at: &mut Instant,
) -> bool {
    let now = Instant::now();
    if *next_at > now {
        return false;
    }
    // Re-arm before processing, so a stale generation (no new captured
    // frame) can never stall the loop's wake-up: the deadline is always
    // in the future after this call.
    // Pre-roll previews run at a fixed cadence; once a track is live the
    // preview matches the stream fps (capped — a source faster than 60 fps
    // would otherwise flood the renderer channel).
    let fps = SCALE_TARGET
        .load_full()
        .map_or(PREVIEW_FALLBACK_FPS, |target| {
            target.fps.clamp(1, PREVIEW_MAX_FPS)
        });
    *next_at = now + Duration::from_millis(u64::from(1000 / fps));
    let generation = PREVIEW_GENERATION.load(Ordering::Relaxed);
    if generation == *last_generation {
        return false;
    }
    *last_generation = generation;

    let (width, height, pts_us, y_len, u_len, v_len) = {
        let Ok(guard) = PREVIEW_I420.lock() else {
            return false;
        };
        let Some(entry) = guard.as_ref() else {
            return false;
        };
        // Copy the stashed planes out under the brief lock; the scale and
        // conversion below run outside it, so a slow emission can never
        // stall the capture callback's stash write (the lock is shared).
        bufs.planes.clear();
        bufs.planes.extend_from_slice(&entry.planes);
        (
            entry.width,
            entry.height,
            entry.pts_us,
            entry.y_len,
            entry.u_len,
            entry.v_len,
        )
    };
    if width == 0 || height == 0 {
        return false;
    }
    // Emit only when the renderer has reported a viewport: with no card
    // mounted (or a zero-sized one) nobody can display the frame, and
    // falling back to the source resolution shipped full-res frames into
    // the channel — 4K at 60 fps ≈ 33 MB/frame into Tauri's unbounded
    // channel queue, which a stalled webview never drains (OOM-scale).
    let Some((out_w, out_h)) = PREVIEW_VIEWPORT
        .load_full()
        .and_then(|vp| fit_preview_size(width, height, vp.0, vp.1))
    else {
        return false;
    };

    // Rebuild the I420 frame from the stashed planes and scale it with
    // libyuv (SIMD), then convert to the BGRA the renderer's texture
    // expects — same colors, a fraction of the cost of the old scalar
    // bilinear pass, and none of it under the shared lock. The source
    // buffer is reused across emissions and recreated only on a source
    // dimension change.
    if bufs
        .src
        .as_ref()
        .is_none_or(|s| s.width() != width || s.height() != height)
    {
        bufs.src = Some(I420Buffer::new(width, height));
    }
    let src = bufs
        .src
        .as_mut()
        .unwrap_or_else(|| unreachable!("src was just assigned above"));
    {
        // Same allocator and (width, height) as the stash's source ⇒ the
        // reconstructed slices match its lengths; clamp + zero-fill anyway,
        // because a panic here would abort the process (the `PipeWire`
        // process callback cannot unwind).
        let (plane_y, plane_u, plane_v) = src.data_mut();
        let n = plane_y.len().min(y_len);
        plane_y[..n].copy_from_slice(&bufs.planes[..n]);
        plane_y[n..].fill(0);
        let n = plane_u.len().min(u_len);
        plane_u[..n].copy_from_slice(&bufs.planes[y_len..y_len + n]);
        plane_u[n..].fill(0);
        let n = plane_v.len().min(v_len);
        plane_v[..n].copy_from_slice(&bufs.planes[y_len + u_len..y_len + u_len + n]);
        plane_v[n..].fill(0);
    }
    let scaled = src.scale(
        i32::try_from(out_w).unwrap_or(0),
        i32::try_from(out_h).unwrap_or(0),
    );

    // The channel payload: 16-byte little-endian header + the fitted BGRA
    // rows. The renderer parses width/height to size its texture and uploads
    // the pixels as-is (bgra8unorm — no channel shuffling anywhere).
    let payload_len = out_w as usize * out_h as usize * 4;
    bufs.out.clear();
    bufs.out.resize(16 + payload_len, 0);
    bufs.out[..8].copy_from_slice(&pts_us.to_le_bytes());
    bufs.out[8..12].copy_from_slice(&out_w.to_le_bytes());
    bufs.out[12..16].copy_from_slice(&out_h.to_le_bytes());
    let (plane_y, plane_u, plane_v) = scaled.data();
    let (stride_y, stride_u, stride_v) = scaled.strides();
    // libyuv's I420ToARGB writes `[B, G, R, A]` — the renderer's BGRA
    // texture order. (`I420ToBGRA` writes `[A, R, G, B]`: libyuv names
    // formats by FOURCC value, rotated from the intuitive spelling.)
    yuv_helper::i420_to_argb(
        plane_y,
        stride_y,
        plane_u,
        stride_u,
        plane_v,
        stride_v,
        &mut bufs.out[16..],
        out_w * 4,
        i32::try_from(out_w).unwrap_or(0),
        i32::try_from(out_h).unwrap_or(0),
    );

    STATS_PREVIEW_SENT.fetch_add(1, Ordering::Relaxed);
    if let Some(callback) = PREVIEW_CALLBACK.load_full() {
        callback(std::mem::take(&mut bufs.out), pts_us);
    }
    true
}

/// Frames awaiting paced delivery to the video source. The capture source
/// converts and pushes; the capture loop pops one frame per
/// encoder-target interval so the encoder (and the RTP timestamps it
/// derives from arrival time) sees even pacing even when the source
/// delivers in bursts.
struct FramePacer {
    queue: Mutex<VecDeque<VideoFrame<I420Buffer>>>,
    /// Maximum frames held; the oldest is dropped when a push overflows.
    capacity: usize,
}

impl FramePacer {
    fn new(capacity: usize) -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    /// Queues a converted frame, dropping the oldest queued frame when
    /// full (drop-oldest: keep the freshest content, bound latency).
    fn push(&self, frame: VideoFrame<I420Buffer>) {
        let Ok(mut queue) = self.queue.lock() else {
            STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if queue.len() >= self.capacity {
            let _ = queue.pop_front();
            STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
        }
        queue.push_back(frame);
    }

    /// Pops the oldest queued frame for delivery to the encoder.
    fn pop(&self) -> Option<VideoFrame<I420Buffer>> {
        self.queue.lock().ok()?.pop_front()
    }
}

/// The capture-source frame callback: `(width, height, bgra, pts_us)`.
pub(crate) type FrameCallback = Box<dyn FnMut(u32, u32, &[u8], i64) + Send>;

/// Converts a linear BGRA frame into a fresh, immutable I420 buffer.
///
/// Every frame gets its own buffer: the encoder consumes frames
/// asynchronously on its own queue (the cadence adapter posts them by
/// value, `VideoStreamEncoder` encodes on the encoder thread), so a
/// reused buffer would be torn or duplicated mid-read — blocky smearing
/// during motion, independent of codec. The conversion is the only
/// per-frame pass; the handed-out buffer is never written again.
fn convert_frame(width: u32, height: u32, bgra: &[u8]) -> VideoFrame<I420Buffer> {
    let mut out = I420Buffer::new(width, height);
    let (stride_y, stride_u, stride_v) = out.strides();
    let (plane_y, plane_u, plane_v) = out.data_mut();
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
    PREVIEW_GENERATION.fetch_add(1, Ordering::Relaxed);
    STATS_LAST_WIDTH.store(width, Ordering::Relaxed);
    STATS_LAST_HEIGHT.store(height, Ordering::Relaxed);
    VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us: monotonic_us(),
        frame_metadata: None,
        buffer: out,
    }
}

/// Builds the frame callback shared by the portal engine, the WGC engine
/// and the synthetic generator: counts the frame, converts it to I420 and
/// queues it for the paced delivery loop.
fn make_on_frame(pacer: Arc<FramePacer>) -> FrameCallback {
    Box::new(move |width, height, bgra, _pts_us| {
        STATS_DEQUEUED.fetch_add(1, Ordering::Relaxed);
        if width == 0 || height == 0 {
            STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let frame = convert_frame(width, height, bgra);
        // Stash the newest frame's I420 planes for the preview thread
        // (skipped when no preview is subscribed — 1080p planes are
        // ~2.25 MB, a third of the old BGRA copy). The stash write is a
        // brief memcpy under the lock; the emitter copies out the same way,
        // so neither side ever holds the lock for a scale or convert.
        if PREVIEW_CALLBACK.load_full().is_some()
            && let Ok(mut slot) = PREVIEW_I420.lock()
        {
            let entry = slot.get_or_insert_with(|| PreviewI420 {
                width,
                height,
                pts_us: 0,
                y_len: 0,
                u_len: 0,
                v_len: 0,
                planes: Vec::new(),
            });
            entry.width = width;
            entry.height = height;
            entry.pts_us = monotonic_us();
            // Size from the actual plane slices: the chroma planes hold
            // `ceil(w/2) × ceil(h/2)` samples, which is more than `w*h/4`
            // for odd dimensions (mid-resize frames).
            let (plane_y, plane_u, plane_v) = frame.buffer.data();
            entry.y_len = plane_y.len();
            entry.u_len = plane_u.len();
            entry.v_len = plane_v.len();
            entry
                .planes
                .resize(entry.y_len + entry.u_len + entry.v_len, 0);
            entry.planes[..entry.y_len].copy_from_slice(plane_y);
            entry.planes[entry.y_len..entry.y_len + entry.u_len].copy_from_slice(plane_u);
            entry.planes[entry.y_len + entry.u_len..].copy_from_slice(plane_v);
        }
        pacer.push(frame);
    })
}

/// Monotonic microsecond timestamp for video frames, anchored at first use.
pub(crate) fn monotonic_us() -> i64 {
    use std::sync::OnceLock;
    static ANCHOR: OnceLock<Instant> = OnceLock::new();
    let anchor = ANCHOR.get_or_init(Instant::now);
    i64::try_from(anchor.elapsed().as_micros()).unwrap_or(0)
}

/// What feeds the capture pipeline: the libwebrtc `PipeWire` capturer (real
/// screens/windows through the portal picker, Linux-only), the WGC capturer
/// (real screens/windows, Windows-only) or a synthetic test pattern (headless
/// e2e / probes / benchmarks, all platforms).
#[derive(Debug, Clone, Copy)]
enum CaptureSource {
    #[cfg(target_os = "linux")]
    Desktop,
    #[cfg(target_os = "windows")]
    Wgc {
        kind: crate::WgcSourceKind,
        id: u64,
    },
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

/// The capture thread: owns the `PipeWire` portal engine (Linux), the WGC
/// engine (Windows) or the synthetic generator thread, plus the preview poll
/// loop.
///
/// On Linux the portal picker opens inside `start_capture` (libwebrtc's
/// async portal handshake), so the ready signal is sent right after the
/// engine starts and the caller sees the session as running while the user
/// chooses a source. A failed engine start sends the error back over
/// `ready_tx`; a session that dies later (portal failure, captured window
/// closed) surfaces as `ERROR_PERMANENT` → the `capture-ended` event.
#[allow(
    clippy::too_many_lines,
    reason = "one linear capture lifecycle (source acquisition → preview loop → teardown); splitting it would scatter the failure accounting"
)]
fn run_capture(
    stop: &Arc<AtomicBool>,
    ready_tx: &mpsc::Sender<Result<(), String>>,
    source: CaptureSource,
) {
    // Paced delivery: the source thread converts and queues frames; this
    // loop pops one frame per encoder-target interval, so the encoder
    // (and the RTP timestamps it derives from arrival time) sees even
    // pacing even when the source delivers in bursts. The same queue is
    // the preview's window onto the newest frame.
    let pacer = Arc::new(FramePacer::new(PACER_CAPACITY));
    let on_frame = make_on_frame(Arc::clone(&pacer));

    #[cfg(target_os = "linux")]
    let mut desktop: Option<crate::linux_capture::LinuxDesktopCapture> = None;
    #[cfg(target_os = "windows")]
    let mut wgc: Option<crate::wgc_capture::WgcCapture> = None;
    let mut generator = None;

    match source {
        #[cfg(target_os = "linux")]
        CaptureSource::Desktop => {
            match crate::linux_capture::LinuxDesktopCapture::start(on_frame) {
                Ok(engine) => {
                    CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
                    let _ = ready_tx.send(Ok(()));
                    desktop = Some(engine);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            }
        }
        #[cfg(target_os = "windows")]
        CaptureSource::Wgc { kind, id } => {
            match crate::wgc_capture::WgcCapture::start(kind, id, on_frame) {
                Ok(engine) => {
                    CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
                    let _ = ready_tx.send(Ok(()));
                    wgc = Some(engine);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            }
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

    // The preview emitter runs on its own thread: it reads the newest
    // queued frame and encodes a JPEG at its own cadence, so the
    // (expensive) scale/ABGR/JPEG work can never delay a paced delivery
    // in the loop below. A failed spawn only disables the preview.
    let preview_stop = Arc::clone(stop);
    let preview_handle =
        match thread::Builder::new()
            .name("preview-emitter".into())
            .spawn(move || {
                let mut preview_bufs = PreviewScaleBufs {
                    planes: Vec::new(),
                    out: Vec::new(),
                    src: None,
                };
                let mut last_preview_generation = 0u64;
                let mut next_preview_at = Instant::now();
                // TEMP-DEBUG: per-second emission rate + avg cost + RSS.
                let mut dbg_last = Instant::now();
                let mut dbg_emitted = 0u64;
                let mut dbg_time = Duration::ZERO;
                while !preview_stop.load(Ordering::Relaxed) {
                    let t0 = Instant::now();
                    let emitted = emit_preview_frame(
                        &mut preview_bufs,
                        &mut last_preview_generation,
                        &mut next_preview_at,
                    );
                    if emitted {
                        dbg_emitted += 1;
                        dbg_time += t0.elapsed();
                    }
                    // Sleep until the next preview deadline (capped so the stop
                    // flag stays responsive).
                    let wake = next_preview_at.min(Instant::now() + Duration::from_millis(100));
                    thread::sleep(wake.saturating_duration_since(Instant::now()));
                    if dbg_last.elapsed() >= Duration::from_secs(1) {
                        // TEMP-DEBUG: resident (field 2) + shared (field 3)
                        // pages — SIZE (field 1) is mostly reserved virtual.
                        let mem = std::fs::read_to_string("/proc/self/statm")
                            .ok()
                            .and_then(|s| {
                                let mut it = s.split_whitespace();
                                let rss = it.nth(1)?.parse::<u64>().ok()?;
                                let shared = it.next()?.parse::<u64>().ok()?;
                                Some((rss * 4 / 1024, shared * 4 / 1024))
                            })
                            .unwrap_or((0, 0));
                        // TEMP-DEBUG counters stay far below 2^53, so the
                        // u64 → f64 conversion loses no precision.
                        #[allow(
                            clippy::cast_precision_loss,
                            reason = "debug counters stay far below 2^53"
                        )]
                        let avg_ms =
                            dbg_time.as_secs_f64() * 1000.0 / dbg_emitted.max(1) as f64;
                        log::info!(
                            "[TEMP] preview emitted={dbg_emitted} avg={avg_ms:.1}ms rss={}MB shm={}MB vp={}x{} src={}x{}",
                            mem.0,
                            mem.1,
                            PREVIEW_VIEWPORT
                                .load_full()
                                .map_or(0, |vp| vp.0),
                            PREVIEW_VIEWPORT
                                .load_full()
                                .map_or(0, |vp| vp.1),
                            STATS_LAST_WIDTH.load(Ordering::Relaxed),
                            STATS_LAST_HEIGHT.load(Ordering::Relaxed),
                        );
                        dbg_emitted = 0;
                        dbg_time = Duration::ZERO;
                        dbg_last = Instant::now();
                    }
                }
            }) {
            Ok(handle) => Some(handle),
            Err(e) => {
                STATS_ERRORS.fetch_add(1, Ordering::Relaxed);
                log::error!("[desktop-capture] preview thread spawn failed: {e}");
                None
            }
        };
    let mut next_delivery_at = Instant::now();
    // The last frame actually pushed to the video source. When the pacer is
    // empty on a delivery tick (the portal capture is event-driven — KWin
    // records a frame only when the compositor renders or the cursor moves,
    // so a low-activity screen delivers far fewer frames than the refresh
    // rate), it is re-pushed with a fresh timestamp so the RTP cadence stays
    // even instead of stalling: the encoder compresses the duplicates to
    // near-nothing and the receiver never sees a gap. This mirrors the WGC
    // engine, whose capturer already re-delivers the last frame on static
    // content.
    let mut last_frame: Option<VideoFrame<I420Buffer>> = None;
    // Rate-limits the slow-scale log below (at most one line per 3 s).
    // Initialized so the first slow scale always logs.
    let mut last_slow_scale = Instant::now()
        .checked_sub(Duration::from_secs(3))
        .unwrap_or(Instant::now());
    // TEMP-DEBUG: capture-side rate counters.
    let mut dbg_last = Instant::now();
    let mut dbg_polls = 0u64;
    let mut dbg_last_dequeued = STATS_DEQUEUED.load(Ordering::Relaxed);

    while !stop.load(Ordering::Relaxed) {
        // Paced delivery tick: one queued frame per encoder-target
        // interval (fallback cadence before a track is live), so RTP
        // pacing at the receiver stays even. Both engines are polled on
        // the same tick — they self-pace inside, so extra polls are no-ops.
        let now = Instant::now();
        if now >= next_delivery_at {
            #[cfg(target_os = "windows")]
            if let Some(engine) = wgc.as_mut() {
                engine.poll();
            }
            #[cfg(target_os = "linux")]
            if let Some(engine) = desktop.as_mut() {
                engine.poll();
                dbg_polls += 1;
            }
            match pacer.pop() {
                Some(mut frame) => {
                    if let Some(source) = VIDEO_SOURCE.load_full() {
                        // libwebrtc's encoder has no explicit target size
                        // (VideoEncoding carries only bitrate/fps), so
                        // without this a 4K source would be encoded at 4K
                        // with the bitrate the user configured for 1080p —
                        // and a smaller source would stream at its own
                        // resolution instead of the configured one. Scale
                        // every frame to the configured dimensions, up or
                        // down (OBS-style canvas scaling). I420Buffer::scale
                        // is libwebrtc's libyuv-backed scaler.
                        let push_frame = match SCALE_TARGET.load_full().map(|t| *t).filter(
                            |target| {
                                frame.buffer.width() != target.width
                                    || frame.buffer.height() != target.height
                            },
                        ) {
                            Some(target) => {
                                let scale_start = Instant::now();
                                let scaled = frame.buffer.scale(
                                    i32::try_from(target.width).unwrap_or(0),
                                    i32::try_from(target.height).unwrap_or(0),
                                );
                                let scale_ms = scale_start.elapsed().as_secs_f64() * 1000.0;
                                if scale_ms >= 5.0
                                    && last_slow_scale.elapsed() >= Duration::from_secs(3)
                                {
                                    last_slow_scale = Instant::now();
                                    log::info!(
                                        "[desktop-capture] slow target scale: {scale_ms:.1} ms ({}x{} -> {}x{})",
                                        frame.buffer.width(),
                                        frame.buffer.height(),
                                        target.width,
                                        target.height,
                                    );
                                }
                                VideoFrame {
                                    rotation: VideoRotation::VideoRotation0,
                                    timestamp_us: frame.timestamp_us,
                                    frame_metadata: None,
                                    buffer: scaled,
                                }
                            }
                            None => frame,
                        };
                        source.capture_frame(&push_frame);
                        last_frame = Some(push_frame);
                        STATS_PUSHED.fetch_add(1, Ordering::Relaxed);
                    } else {
                        // Seed `last_frame` with the newest frame even
                        // without a live track. Go Live swaps `VIDEO_SOURCE`
                        // in while the pacer is usually empty (KWin records
                        // only on damage or cursor move), and the keepalive
                        // arm below needs a frame to re-push — without this
                        // the stream stays empty until the source produces
                        // motion, which in practice means waiting for audio
                        // to start playing.
                        last_frame = Some(frame);
                        STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
                    }
                }
                None => {
                    // Keepalive: an empty pacer usually means the compositor
                    // simply had nothing to record, not that delivery should
                    // stop. Both engines re-deliver their last frame while
                    // running (WGC's frame pool, the `PipeWire` capturer's
                    // `latest_available_frame_`), but a dropped corrupt
                    // buffer or a gap between the picker answer and the
                    // first frame can still leave a delivery tick empty —
                    // re-push the last frame (same immutable buffer,
                    // timestamp refreshed) so arrival pacing (and therefore
                    // the receiver's RTP cadence) stays even.
                    if let Some(source) = VIDEO_SOURCE.load_full()
                        && let Some(last) = last_frame.as_mut()
                    {
                        last.timestamp_us = monotonic_us();
                        source.capture_frame(last);
                    }
                }
            }
            let fps = SCALE_TARGET
                .load_full()
                .map_or(PREVIEW_FALLBACK_FPS, |target| {
                    target.fps.clamp(1, PREVIEW_MAX_FPS)
                });
            let interval = Duration::from_micros(1_000_000 / u64::from(fps));
            // Deliveries are spaced exactly one interval apart. After a
            // stall the next deadline is a full interval after the frame
            // just delivered — never `now`, which would pop a second
            // frame immediately: the encoder stamps both with
            // near-identical RTP times and the receiver sees a frame
            // pair flash, then a gap (the rubberbanding pattern).
            next_delivery_at = now + interval;
        }
        // Sleep until the next delivery deadline (capped so the portal
        // session check and the stop flag stay responsive).
        let wake = next_delivery_at.min(now + Duration::from_millis(100));
        thread::sleep(wake.saturating_duration_since(Instant::now()));
        if dbg_last.elapsed() >= Duration::from_secs(1) {
            let dq = STATS_DEQUEUED.load(Ordering::Relaxed);
            log::info!(
                "[TEMP] capture polls={dbg_polls} dequeued_delta={} gen={}",
                dq - dbg_last_dequeued,
                PREVIEW_GENERATION.load(Ordering::Relaxed),
            );
            dbg_polls = 0;
            dbg_last_dequeued = dq;
            dbg_last = Instant::now();
        }
    }
    #[cfg(target_os = "linux")]
    drop(desktop);
    if let Some(handle) = generator {
        let _ = handle.join();
    }
    if let Some(handle) = preview_handle {
        let _ = handle.join();
    }
    // Dropping the WGC engine destroys the libwebrtc capturer (stopping its
    // capture session) before the COM apartment is released.
    #[cfg(target_os = "windows")]
    drop(wgc);
    CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
}

/// Starts the desktop capturer on its own thread. On Linux, returns once
/// the libwebrtc `PipeWire` capturer is started; the portal picker then
/// appears and frames flow after the user selects a source. On Windows the
/// renderer's picker selection is required — use `start_windows` — and on
/// other platforms there is no capture source yet, so this reports video as
/// unavailable (`Ok(false)`) while the synthetic source and the whole
/// preview pipeline remain usable.
///
/// # Errors
///
/// Returns an error if a capture session is already active, the thread
/// cannot be spawned, or the capturer fails to initialize within five
/// seconds.
#[allow(
    clippy::unnecessary_wraps,
    reason = "non-Linux arm is a constant Ok(false) stub; the Result signature carries the Linux portal errors"
)]
pub(crate) fn start() -> Result<bool, String> {
    #[cfg(target_os = "linux")]
    {
        start_with(CaptureSource::Desktop)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(false)
    }
}

/// Starts the WGC desktop capturer for the chosen source (Windows-only; the
/// renderer's in-app picker supplies the selection). Returns once the
/// capture session is running; frames flow from the first paced poll.
///
/// # Errors
///
/// Returns an error if a capture session is already active, the thread
/// cannot be spawned, or the capturer fails to initialize within five
/// seconds.
#[cfg(target_os = "windows")]
pub(crate) fn start_windows(kind: crate::WgcSourceKind, id: u64) -> Result<bool, String> {
    start_with(CaptureSource::Wgc { kind, id })
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
            // Release the state lock before joining: the error-path join
            // waits for the capture thread, which returns promptly once the
            // engine start failed; `stop()`/`disconnect_livekit_room` block
            // on this lock, so a hung engine would otherwise freeze them
            // (and, via `LIVEKIT`, the audio callback) permanently.
            drop(guard);
            let _ = handle.join();
            Err(reason)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            drop(guard);
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

    /// The encode target is applied whenever the source dimensions differ
    /// from it — downscaling for larger sources, upscaling for smaller ones
    /// (OBS-style canvas scaling) — and skipped only on an exact match.
    #[test]
    fn scale_target_is_applied_whenever_source_differs_from_target() {
        let needs_scale = |src_w: u32, src_h: u32, t: ScaleTarget| {
            (src_w != t.width || src_h != t.height).then_some(t)
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
            needs_scale(1920, 1040, target).is_some(),
            "height-shorter source must scale to the exact target"
        );
        assert!(
            needs_scale(1280, 720, target).is_some(),
            "720p must scale up to the configured 1080p"
        );
        assert!(
            needs_scale(960, 520, target).is_some(),
            "small windows must scale up to the configured resolution"
        );
        assert!(
            needs_scale(1920, 1080, target).is_none(),
            "exact match must not scale"
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

    /// The preview pipeline (I420 stash → libyuv scale → BGRA payload)
    /// keeps the dominant colors intact: red top half, blue bottom half.
    /// Pins the channel handling (a swapped order would turn the blocks
    /// cyan/magenta).
    #[test]
    fn i420_preview_path_keeps_dominant_colors() {
        const W: u32 = 128;
        const H: u32 = 96;
        let mut bgra = vec![0u8; (W * H * 4) as usize];
        for (i, pixel) in bgra.chunks_exact_mut(4).enumerate() {
            if i / (W as usize) < (H as usize) / 2 {
                pixel.copy_from_slice(&[0, 0, 255, 255]); // BGRA: red
            } else {
                pixel.copy_from_slice(&[255, 0, 0, 255]); // BGRA: blue
            }
        }

        let mut src = I420Buffer::new(W, H);
        {
            let (stride_y, stride_u, stride_v) = src.strides();
            let (plane_y, plane_u, plane_v) = src.data_mut();
            yuv_helper::argb_to_i420(
                &bgra,
                W * 4,
                plane_y,
                stride_y,
                plane_u,
                stride_u,
                plane_v,
                stride_v,
                i32::try_from(W).unwrap_or(0),
                i32::try_from(H).unwrap_or(0),
            );
        }
        let scaled = src.scale(
            i32::try_from(W / 2).unwrap_or(0),
            i32::try_from(H / 2).unwrap_or(0),
        );
        let mut dst = vec![0u8; ((W / 2) * (H / 2) * 4) as usize];
        let (plane_y, plane_u, plane_v) = scaled.data();
        let (stride_y, stride_u, stride_v) = scaled.strides();
        yuv_helper::i420_to_argb(
            plane_y,
            stride_y,
            plane_u,
            stride_u,
            plane_v,
            stride_v,
            &mut dst,
            (W / 2) * 4,
            i32::try_from(W / 2).unwrap_or(0),
            i32::try_from(H / 2).unwrap_or(0),
        );

        let pixel_at = |x: usize, y: usize| {
            let i = (y * (W as usize / 2) + x) * 4;
            (dst[i], dst[i + 1], dst[i + 2])
        };
        let (b, g, r) = pixel_at((W / 4) as usize, (H / 8) as usize);
        assert!(
            r > 200 && g < 80 && b < 80,
            "top half must stay red, got ({r},{g},{b})"
        );
        let (b, g, r) = pixel_at((W / 4) as usize, (3 * H / 8) as usize);
        assert!(
            b > 200 && r < 80 && g < 80,
            "bottom half must stay blue, got ({r},{g},{b})"
        );
    }

    /// The preview pipeline must preserve horizontal orientation: a mirror
    /// anywhere in convert → stash → scale → payload would swap left and
    /// right halves. (The renderer samples mirrored bar positions in e2e, but
    /// the conversion layer is the unit-testable boundary — a flip here would
    /// ship to the spectator too.)
    #[test]
    fn i420_preview_path_preserves_horizontal_orientation() {
        const W: u32 = 128;
        const H: u32 = 96;
        let mut bgra = vec![0u8; (W * H * 4) as usize];
        for (i, pixel) in bgra.chunks_exact_mut(4).enumerate() {
            if i % (W as usize) < (W as usize) / 2 {
                pixel.copy_from_slice(&[0, 0, 255, 255]); // BGRA: red — left half
            } else {
                pixel.copy_from_slice(&[255, 0, 0, 255]); // BGRA: blue — right half
            }
        }

        let mut src = I420Buffer::new(W, H);
        {
            let (stride_y, stride_u, stride_v) = src.strides();
            let (plane_y, plane_u, plane_v) = src.data_mut();
            yuv_helper::argb_to_i420(
                &bgra,
                W * 4,
                plane_y,
                stride_y,
                plane_u,
                stride_u,
                plane_v,
                stride_v,
                i32::try_from(W).unwrap_or(0),
                i32::try_from(H).unwrap_or(0),
            );
        }
        let scaled = src.scale(
            i32::try_from(W / 2).unwrap_or(0),
            i32::try_from(H / 2).unwrap_or(0),
        );
        let mut dst = vec![0u8; ((W / 2) * (H / 2) * 4) as usize];
        let (plane_y, plane_u, plane_v) = scaled.data();
        let (stride_y, stride_u, stride_v) = scaled.strides();
        yuv_helper::i420_to_argb(
            plane_y,
            stride_y,
            plane_u,
            stride_u,
            plane_v,
            stride_v,
            &mut dst,
            (W / 2) * 4,
            i32::try_from(W / 2).unwrap_or(0),
            i32::try_from(H / 2).unwrap_or(0),
        );

        let pixel_at = |x: usize, y: usize| {
            let i = (y * (W as usize / 2) + x) * 4;
            (dst[i], dst[i + 1], dst[i + 2])
        };
        let (b, g, r) = pixel_at((W / 8) as usize, (H / 4) as usize);
        assert!(
            r > 200 && g < 80 && b < 80,
            "left half must stay red (no horizontal flip), got ({r},{g},{b})"
        );
        let (b, g, r) = pixel_at((3 * W / 8) as usize, (H / 4) as usize);
        assert!(
            b > 200 && r < 80 && g < 80,
            "right half must stay blue (no horizontal flip), got ({r},{g},{b})"
        );
    }

    /// The stash → rebuild path must round-trip odd dimensions exactly: the
    /// chroma planes hold `ceil(w/2) × ceil(h/2)` samples, more than the
    /// `w*h/4` arithmetic (mid-resize frames, e.g. 1280×693 — sizing by
    /// arithmetic panicked with a `copy_from_slice` length mismatch).
    #[test]
    fn i420_stash_roundtrip_handles_odd_dimensions() {
        const W: u32 = 1280;
        const H: u32 = 693;
        let mut src = I420Buffer::new(W, H);
        {
            let (plane_y, plane_u, plane_v) = src.data_mut();
            for (i, byte) in plane_y.iter_mut().enumerate() {
                *byte = u8::try_from(i % 251).unwrap_or(0);
            }
            for (i, byte) in plane_u.iter_mut().enumerate() {
                *byte = u8::try_from(i % 97).unwrap_or(0);
            }
            for (i, byte) in plane_v.iter_mut().enumerate() {
                *byte = u8::try_from(i % 61).unwrap_or(0);
            }
        }
        let (plane_y, plane_u, plane_v) = src.data();
        let (y_len, u_len, v_len) = (plane_y.len(), plane_u.len(), plane_v.len());
        // The trap this test pins: ceil rounding makes the chroma planes
        // bigger than `w*h/4` for odd heights.
        assert!(u_len > ((W * H) / 4) as usize);
        let mut planes = vec![0u8; y_len + u_len + v_len];
        planes[..y_len].copy_from_slice(plane_y);
        planes[y_len..y_len + u_len].copy_from_slice(plane_u);
        planes[y_len + u_len..].copy_from_slice(plane_v);

        // Emitter side: same allocator + same dims ⇒ identical lengths.
        let mut rebuilt = I420Buffer::new(W, H);
        {
            let (plane_y, plane_u, plane_v) = rebuilt.data_mut();
            assert_eq!(plane_y.len(), y_len);
            assert_eq!(plane_u.len(), u_len);
            assert_eq!(plane_v.len(), v_len);
            plane_y.copy_from_slice(&planes[..y_len]);
            plane_u.copy_from_slice(&planes[y_len..y_len + u_len]);
            plane_v.copy_from_slice(&planes[y_len + u_len..]);
        }
        assert_eq!(src.data(), rebuilt.data());
    }

    /// Headless probe of the whole synthetic pipeline: frames must flow
    /// through conversion and the preview emitter must fire even with
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
        // The stash write and the emission are gated on a registered
        // preview callback (the Tauri backend registers one in the app; a
        // bare probe must do the same), and the emitter skips frames until
        // a viewport is reported (no card = nothing to display, and
        // full-res fallback emission was an OOM vector) — so the probe
        // registers both, like the renderer does.
        set_preview_callback(Box::new(|_bytes, _pts_us| {}));
        set_preview_viewport(640, 360);
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

        // The production preview: I420 stash copy → libyuv scale → BGRA
        // payload (reused buffers, exactly like the emitter).
        let mut src = I420Buffer::new(u32::try_from(W).unwrap_or(0), u32::try_from(H).unwrap_or(0));
        {
            let (stride_y, stride_u, stride_v) = src.strides();
            let (plane_y, plane_u, plane_v) = src.data_mut();
            yuv_helper::argb_to_i420(
                &rgba,
                u32::try_from(W * 4).unwrap_or(0),
                plane_y,
                stride_y,
                plane_u,
                stride_u,
                plane_v,
                stride_v,
                i32::try_from(W).unwrap_or(0),
                i32::try_from(H).unwrap_or(0),
            );
        }
        let mut planes = Vec::new();
        {
            let y_len = W * H;
            let uv_len = y_len / 4;
            planes.resize(y_len + 2 * uv_len, 0);
            let (plane_y, plane_u, plane_v) = src.data();
            planes[..y_len].copy_from_slice(plane_y);
            planes[y_len..y_len + uv_len].copy_from_slice(plane_u);
            planes[y_len + uv_len..].copy_from_slice(plane_v);
        }
        let mut payload = Vec::new();
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            let mut src =
                I420Buffer::new(u32::try_from(W).unwrap_or(0), u32::try_from(H).unwrap_or(0));
            {
                let y_len = W * H;
                let uv_len = y_len / 4;
                let (plane_y, plane_u, plane_v) = src.data_mut();
                plane_y.copy_from_slice(&planes[..y_len]);
                plane_u.copy_from_slice(&planes[y_len..y_len + uv_len]);
                plane_v.copy_from_slice(&planes[y_len + uv_len..]);
            }
            let scaled = src.scale(
                i32::try_from(W / 2).unwrap_or(0),
                i32::try_from(H / 2).unwrap_or(0),
            );
            let (plane_y, plane_u, plane_v) = scaled.data();
            let (stride_y, stride_u, stride_v) = scaled.strides();
            payload.resize(16 + (W / 2) * (H / 2) * 4, 0);
            yuv_helper::i420_to_argb(
                plane_y,
                stride_y,
                plane_u,
                stride_u,
                plane_v,
                stride_v,
                &mut payload[16..],
                u32::try_from(W / 2).unwrap_or(0) * 4,
                i32::try_from(W / 2).unwrap_or(0),
                i32::try_from(H / 2).unwrap_or(0),
            );
            std::hint::black_box(&payload);
        }
        report(
            "i420 scale+convert 640x360 -> 320x180",
            start.elapsed(),
            payload.len(),
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
