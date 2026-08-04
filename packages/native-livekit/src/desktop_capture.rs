//! Desktop video capture engine via libwebrtc's `DesktopCapturer`.
//!
//! The `LiveKit` SDK's `DesktopCapturer` (Generic source type) is the same
//! `PipeWire` portal capturer Chromium ships: it opens its own
//! xdg-desktop-portal `ScreenCast` session (the native picker dialog appears
//! on Wayland), negotiates DMA-BUF modifiers with the compositor, and reads
//! every frame through EGL import — tiled/compressed buffers included —
//! delivering plain CPU-accessible BGRA. Frames are converted to `I420` and
//! handed to the active `NativeVideoSource`, so the renderer never touches
//! capture memory.
//!
//! The capture thread mirrors the SDK's screensharing example: poll
//! `capture_frame()` on a ~60 Hz cadence and convert whatever the callback
//! delivers. The portal session dies with the capturer on stop.

use arc_swap::ArcSwapOption;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use livekit::webrtc::desktop_capturer::{
    CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions, DesktopFrame,
};
use livekit::webrtc::native::yuv_helper;
use livekit::webrtc::prelude::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation};

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::VIDEO_SOURCE;

pub(crate) struct DesktopCaptureSession {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

pub(crate) static CAPTURE_STATE: Mutex<Option<DesktopCaptureSession>> = Mutex::new(None);
pub(crate) static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Frames handed to the video source after a successful convert. The e2e test
/// requires this to advance while streaming — a stalled counter means the
/// capture pipeline is dead (e.g. the portal picker was cancelled).
pub(crate) static STATS_PUSHED: AtomicU64 = AtomicU64::new(0);
/// Frames delivered by the capturer callback (before conversion).
pub(crate) static STATS_DEQUEUED: AtomicU64 = AtomicU64::new(0);
/// Frames dropped because no video track was active or the frame was invalid.
pub(crate) static STATS_DROPPED: AtomicU64 = AtomicU64::new(0);
/// Transient/permanent capture errors reported by the capturer.
pub(crate) static STATS_ERRORS: AtomicU64 = AtomicU64::new(0);
/// Base64 JPEG preview frames handed to the preview callback (MIGRATION §9.1).
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

/// Preview frame size the captured stream is downscaled to before JPEG
/// encoding (~1 MB/s local IPC at 15 fps, MIGRATION §9.1).
const PREVIEW_WIDTH: u32 = 640;
const PREVIEW_HEIGHT: u32 = 360;
const PREVIEW_JPEG_QUALITY: u8 = 80;
/// ~15 fps emission cadence.
const PREVIEW_INTERVAL: Duration = Duration::from_millis(66);

/// Preview callback type: `(width, height, base64_data, pts_us)`.
type PreviewFrameCallback = Box<dyn Fn(u32, u32, String, i64) + Send + Sync>;

/// Preview callback: invoked from the capture thread with base64 JPEG frames
/// while a capture session is active. The engine stays Tauri-unaware — the
/// Tauri backend wires this to its `preview-frame` event.
static PREVIEW_CALLBACK: ArcSwapOption<PreviewFrameCallback> = ArcSwapOption::const_empty();
/// Bumped once per converted frame; the poll loop emits only when it advances,
/// so stale frames are never re-encoded between capture ticks.
static PREVIEW_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Registers the preview-frame callback, replacing any previously registered
/// one. `None` is stored by `clear_preview_callback` when the app is done.
pub fn set_preview_callback(callback: PreviewFrameCallback) {
    PREVIEW_CALLBACK.store(Some(Arc::new(callback)));
}

/// Clears the registered preview-frame callback.
pub fn clear_preview_callback() {
    PREVIEW_CALLBACK.store(None);
}

/// Scratch buffers reused across preview encodes (allocated once per session).
struct PreviewBuffers {
    argb: Vec<u8>,
    scaled: Vec<u8>,
    rgb: Vec<u8>,
    jpeg: Vec<u8>,
}

/// Encodes and dispatches a preview frame (MIGRATION §9.1) when a new frame
/// arrived since the previous emit and the ~15 fps interval has elapsed.
/// Independent of `VIDEO_SOURCE`, so the pre-roll preview works before any
/// track is published. Returns whether a frame was emitted.
fn emit_preview_frame(
    preview_source: &Mutex<VideoFrame<I420Buffer>>,
    bufs: &mut PreviewBuffers,
    last_generation: &mut u64,
    next_at: &mut Instant,
) -> bool {
    let generation = PREVIEW_GENERATION.load(Ordering::Relaxed);
    if generation == *last_generation || *next_at > Instant::now() {
        return false;
    }
    *last_generation = generation;
    *next_at = Instant::now() + PREVIEW_INTERVAL;
    let Ok(guard) = preview_source.lock() else {
        return false;
    };
    let Some((width, height, data, pts_us)) = encode_preview_frame(
        &guard,
        &mut bufs.argb,
        &mut bufs.scaled,
        &mut bufs.rgb,
        &mut bufs.jpeg,
    ) else {
        return false;
    };
    STATS_PREVIEW_SENT.fetch_add(1, Ordering::Relaxed);
    if let Some(callback) = PREVIEW_CALLBACK.load_full() {
        callback(width, height, data, pts_us);
    }
    true
}

/// Box-downscales a 4-channel ARGB image to the fixed preview size: every
/// destination pixel averages the source rectangle it covers. When the source
/// is smaller than the preview (upscaling), the covered range can be empty and
/// falls back to a nearest-neighbour copy.
fn box_downscale_argb(src: &[u8], src_width: u32, src_height: u32, dst: &mut [u8]) {
    let src_w = src_width as usize;
    let src_h = src_height as usize;
    let dst_w = PREVIEW_WIDTH as usize;
    let dst_h = PREVIEW_HEIGHT as usize;
    for dst_y in 0..dst_h {
        let y0 = (dst_y * src_h) / dst_h;
        let y1 = ((dst_y + 1) * src_h) / dst_h;
        for dst_x in 0..dst_w {
            let x0 = (dst_x * src_w) / dst_w;
            let x1 = ((dst_x + 1) * src_w) / dst_w;
            let d = (dst_y * dst_w + dst_x) * 4;
            if x0 == x1 || y0 == y1 {
                let i = (y0.min(src_h - 1) * src_w + x0.min(src_w - 1)) * 4;
                dst[d..d + 4].copy_from_slice(&src[i..i + 4]);
                continue;
            }
            let mut sums = [0u64; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * src_w + x) * 4;
                    for (sum, pixel) in sums.iter_mut().zip(&src[i..i + 4]) {
                        *sum += u64::from(*pixel);
                    }
                }
            }
            let count = ((y1 - y0) * (x1 - x0)) as u64;
            for (sum, out) in sums.into_iter().zip(&mut dst[d..d + 4]) {
                *out = u8::try_from(sum / count).unwrap_or(255);
            }
        }
    }
}

/// Encodes the latest converted I420 frame as a base64 JPEG preview payload.
/// Reuses the `yuv_helper` converters (I420→ARGB, ARGB→RGB24); only the box
/// downscale is ours.
fn encode_preview_frame(
    frame: &VideoFrame<I420Buffer>,
    argb_buf: &mut Vec<u8>,
    scaled_buf: &mut Vec<u8>,
    rgb_buf: &mut Vec<u8>,
    jpeg_buf: &mut Vec<u8>,
) -> Option<(u32, u32, String, i64)> {
    let width = frame.buffer.width();
    let height = frame.buffer.height();
    if width == 0 || height == 0 {
        return None;
    }

    let (stride_y, stride_u, stride_v) = frame.buffer.strides();
    let (plane_y, plane_u, plane_v) = frame.buffer.data();
    argb_buf.resize(width as usize * height as usize * 4, 0);
    yuv_helper::i420_to_argb(
        plane_y,
        stride_y,
        plane_u,
        stride_u,
        plane_v,
        stride_v,
        argb_buf,
        width * 4,
        i32::try_from(width).unwrap_or(0),
        i32::try_from(height).unwrap_or(0),
    );

    scaled_buf.resize(PREVIEW_WIDTH as usize * PREVIEW_HEIGHT as usize * 4, 0);
    box_downscale_argb(argb_buf, width, height, scaled_buf);

    rgb_buf.resize(PREVIEW_WIDTH as usize * PREVIEW_HEIGHT as usize * 3, 0);
    yuv_helper::argb_to_rgb24(
        scaled_buf,
        PREVIEW_WIDTH * 4,
        rgb_buf,
        PREVIEW_WIDTH * 3,
        i32::try_from(PREVIEW_WIDTH).unwrap_or(0),
        i32::try_from(PREVIEW_HEIGHT).unwrap_or(0),
    );

    jpeg_buf.clear();
    let encoder = jpeg_encoder::Encoder::new(&mut *jpeg_buf, PREVIEW_JPEG_QUALITY);
    if let Err(err) = encoder.encode(
        rgb_buf,
        u16::try_from(PREVIEW_WIDTH).unwrap_or(0),
        u16::try_from(PREVIEW_HEIGHT).unwrap_or(0),
        jpeg_encoder::ColorType::Rgb,
    ) {
        eprintln!("[desktop-capture] preview JPEG encode failed: {err:?}");
        return None;
    }

    let data = STANDARD.encode(jpeg_buf.as_slice());
    Some((PREVIEW_WIDTH, PREVIEW_HEIGHT, data, frame.timestamp_us))
}

/// Monotonic microsecond timestamp for video frames, anchored at first use.
fn monotonic_us() -> i64 {
    use std::sync::OnceLock;
    static ANCHOR: OnceLock<Instant> = OnceLock::new();
    let anchor = ANCHOR.get_or_init(Instant::now);
    i64::try_from(anchor.elapsed().as_micros()).unwrap_or(0)
}

/// The capture thread: owns the capturer, pumps the portal's `GDBus`
/// callbacks and polls frames. The portal machinery must dispatch on THIS
/// thread, never on the process-global default `GMainContext` — in the
/// Electron main process that context is shared with Chromium's GTK
/// integration, so portal callbacks could otherwise fire on a foreign thread
/// and race the capturer's own state (`StartScreenCastStream` touches the
/// same unlocked members `CaptureFrame` reads every poll). A dedicated
/// thread-default context, acquired for the thread's lifetime, makes the
/// whole portal flow single-threaded.
fn run_desktop_capture(stop: Arc<AtomicBool>, ready_tx: mpsc::Sender<Result<(), String>>) {
    let glib_ctx = glib::MainContext::new();
    let on_error_tx = ready_tx.clone();
    let glib_ctx_ref = &glib_ctx;
    if let Err(err) = glib_ctx.with_thread_default(move || {
        run_capture_with_glib(&stop, &ready_tx, glib_ctx_ref);
    }) {
        let _ = on_error_tx.send(Err(format!("Failed to own GLib context: {err}")));
    }
}

fn run_capture_with_glib(
    stop: &Arc<AtomicBool>,
    ready_tx: &mpsc::Sender<Result<(), String>>,
    glib_ctx: &glib::MainContext,
) {
    let options = DesktopCapturerOptions::new(DesktopCaptureSourceType::Generic);
    let Some(mut capturer) = DesktopCapturer::new(options) else {
        let _ = ready_tx.send(Err(
            "Failed to create desktop capturer (PipeWire portal unavailable)".into(),
        ));
        return;
    };
    // Wayland: the delegated capturer reports a placeholder source; the real
    // selection happens in the portal picker opened by `start_capture`.
    let source = capturer.get_source_list().into_iter().next();

    let frame_buffer = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us: 0,
        frame_metadata: None,
        buffer: I420Buffer::new(1, 1),
    };

    // The converted frame is shared with the preview emitter in the poll
    // loop. Both run on this single capture thread — the callback only fires
    // from `glib_ctx.iteration` / `capture_frame`, never concurrently — so the
    // mutex never actually contends; it exists to give the 'static callback
    // closure and the loop a shared owner for the buffer.
    let preview_source = Arc::new(Mutex::new(frame_buffer));
    let callback_source = Arc::clone(&preview_source);

    // Log an error only when its kind changes: the pre-stream polls report
    // Temporary on every tick while the portal picker is open, which would
    // otherwise spam the console.
    let mut last_error_kind: Option<CaptureError> = None;
    let callback = move |result: Result<DesktopFrame, CaptureError>| {
        let frame = match result {
            Ok(frame) => frame,
            Err(err) => {
                STATS_ERRORS.fetch_add(1, Ordering::Relaxed);
                if last_error_kind.as_ref() != Some(&err) {
                    eprintln!("[desktop-capture] capture error: {err:?}");
                    last_error_kind = Some(err);
                }
                return;
            }
        };
        last_error_kind = None;
        STATS_DEQUEUED.fetch_add(1, Ordering::Relaxed);

        let width = frame.width();
        let height = frame.height();
        let stride = frame.stride();
        if width <= 0 || height <= 0 || stride == 0 {
            STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let width_u32 = u32::try_from(width).unwrap_or(0);
        let height_u32 = u32::try_from(height).unwrap_or(0);
        let Ok(mut frame_buffer) = callback_source.lock() else {
            STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if frame_buffer.buffer.width() != width_u32 || frame_buffer.buffer.height() != height_u32 {
            frame_buffer.buffer = I420Buffer::new(width_u32, height_u32);
        }
        let (stride_y, stride_u, stride_v) = frame_buffer.buffer.strides();
        let (plane_y, plane_u, plane_v) = frame_buffer.buffer.data_mut();
        yuv_helper::argb_to_i420(
            frame.data(),
            stride,
            plane_y,
            stride_y,
            plane_u,
            stride_u,
            plane_v,
            stride_v,
            width,
            height,
        );
        frame_buffer.timestamp_us = monotonic_us();
        PREVIEW_GENERATION.fetch_add(1, Ordering::Relaxed);
        STATS_LAST_WIDTH.store(width_u32, Ordering::Relaxed);
        STATS_LAST_HEIGHT.store(height_u32, Ordering::Relaxed);

        let Some(source) = VIDEO_SOURCE.load_full() else {
            STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        };
        source.capture_frame(&frame_buffer);
        STATS_PUSHED.fetch_add(1, Ordering::Relaxed);
    };

    capturer.start_capture(source, callback);
    CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
    let _ = ready_tx.send(Ok(()));

    // Reused scratch buffers for the preview encoder (allocated once).
    let mut preview_bufs = PreviewBuffers {
        argb: Vec::new(),
        scaled: Vec::new(),
        rgb: Vec::new(),
        jpeg: Vec::new(),
    };
    let mut last_preview_generation = 0u64;
    let mut next_preview_at = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        // Dispatch pending portal callbacks on this thread.
        while glib_ctx.pending() {
            glib_ctx.iteration(false);
        }
        capturer.capture_frame();
        emit_preview_frame(
            &preview_source,
            &mut preview_bufs,
            &mut last_preview_generation,
            &mut next_preview_at,
        );
        thread::sleep(Duration::from_millis(16));
    }
    CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
}

/// Starts the desktop capturer on its own thread. Returns once the capturer is
/// running; on Wayland the native portal picker then appears and frames flow
/// after the user selects a source.
///
/// # Errors
///
/// Returns an error if a capture session is already active, the thread cannot
/// be spawned, or the capturer fails to initialize within five seconds.
pub(crate) fn start() -> Result<bool, String> {
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
        .spawn(move || run_desktop_capture(thread_stop, ready_tx))
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

/// Returns `true` while the capturer is running (the portal picker may still
/// be awaiting a selection).
pub(crate) fn is_active() -> bool {
    CAPTURE_ACTIVE.load(Ordering::Relaxed)
}

/// Per-stage counters for the desktop capture pipeline, reset on every
/// `start_desktop_capture`. `frames_dequeued` counts frames received from the
/// capturer; `frames_pushed` counts those converted to `I420` and delivered
/// to the video track; `frames_dropped` counts frames skipped while no track
/// was active; `preview_frames_sent` counts base64 JPEG frames emitted via
/// the preview callback; `capture_errors` counts capturer-reported failures.
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

    /// Manual control probe: runs the capture engine in a plain Rust process
    /// (no Electron, no room) so a SIGTRAP can be attributed to libwebrtc's
    /// portal/PipeWire stack itself instead of its integration with the
    /// Electron main process. Run with:
    ///
    /// ```sh
    /// cargo test --release -p native-livekit -- --ignored capture_engine_probe --nocapture
    /// ```
    ///
    /// A portal picker appears; select a source, then observe the counters.
    /// The process exits itself after 20 s.
    #[test]
    #[ignore = "manual probe: requires a Wayland session with a picker to answer"]
    fn capture_engine_probe() {
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
            if !is_active() {
                eprintln!("[probe] capture went inactive early");
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
    }
}
