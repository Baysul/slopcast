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
//! A `Synthetic` source (used by the headless e2e and manual probes) feeds
//! generated test-pattern BGRA frames through the exact same conversion and
//! publish path, so the whole encode → SFU → spectator chain is testable
//! without a portal picker.
//!
//! Threading: the capture thread owns the portal session and the preview poll
//! loop; the `pw_stream` engine runs its own main loop on a separate thread,
//! whose frame callback feeds the shared conversion buffer (mutex-guarded, as
//! before). The picker wait polls the stop flag, so `stop()` never waits on a
//! user that ignores the picker.

/// Preview callback type: `(width, height, base64_data, pts_us)`.
type PreviewFrameCallback = Box<dyn Fn(u32, u32, String, i64) + Send + Sync>;

#[cfg(target_os = "linux")]
mod engine {
    use super::PreviewFrameCallback;
    use arc_swap::ArcSwapOption;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use livekit::webrtc::native::yuv_helper;
    use livekit::webrtc::prelude::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation};
    use native_rust::video_capture::{PwVideoCapture, ScreenCastPortal, StartOutcome};
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
    /// Raw I420 preview frames handed to the preview callback.
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

    /// Preview frame size the captured stream is downscaled to before the raw
    /// I420 payload is shipped to the renderer. The renderer draws the planes
    /// directly (no image codec), so this is a bandwidth/size tradeoff, not a
    /// quality floor: at the default 60 fps a 640×360 I420 frame is ~345 KB.
    const PREVIEW_WIDTH: u32 = 640;
    const PREVIEW_HEIGHT: u32 = 360;
    /// Preview cadence before a video track is published (no stream fps to
    /// target yet); once a track is live the preview targets the stream fps.
    const PREVIEW_FALLBACK_FPS: u32 = 30;

    /// Preview callback: invoked with base64 I420 planes while a capture
    /// session is active. The engine stays Tauri-unaware — the Tauri backend
    /// wires this to its `preview-frame` event.
    static PREVIEW_CALLBACK: ArcSwapOption<PreviewFrameCallback> = ArcSwapOption::const_empty();
    /// Bumped once per converted frame; the poll loop emits only when it
    /// advances, so stale frames are never re-encoded between capture ticks.
    static PREVIEW_GENERATION: AtomicU64 = AtomicU64::new(0);

    /// Registers the preview-frame callback, replacing any previously
    /// registered one. `None` is stored by `clear_preview_callback` when the
    /// app is done.
    pub(crate) fn set_preview_callback(callback: PreviewFrameCallback) {
        PREVIEW_CALLBACK.store(Some(Arc::new(callback)));
    }

    /// Clears the registered preview-frame callback.
    pub(crate) fn clear_preview_callback() {
        PREVIEW_CALLBACK.store(None);
    }

    /// The published video track's encoder target: `(width, height, fps)` of
    /// the last `start_video_track` config. The capture engine scales frames
    /// down to these dimensions before pushing them to the encoder (libwebrtc
    /// has no explicit target size of its own, so a 4K source would otherwise
    /// be encoded at 4K with the user's 1080p bitrate), and the preview loop
    /// emits at this framerate so the preview matches the stream.
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

    /// Scratch buffers reused across preview emissions (allocated once per
    /// session).
    struct PreviewBuffers {
        /// Snapshot of the latest frame's three I420 planes, copied out of the
        /// shared mutex so the expensive plane processing below never blocks
        /// the capture thread (which writes the same buffer in `on_frame`).
        yuv: Vec<u8>,
        plane_y: Vec<u8>,
        plane_u: Vec<u8>,
        plane_v: Vec<u8>,
        payload: Vec<u8>,
    }

    /// Box-downscales one 1-byte-plane image: every destination pixel averages
    /// the source rectangle it covers. When the source is smaller than the
    /// destination (upscaling), the covered range can be empty and falls back
    /// to a nearest-neighbour copy. Used for both the BGRA encode target and
    /// the I420 preview planes.
    fn box_downscale_plane(
        src: &[u8],
        src_w: u32,
        src_h: u32,
        dst: &mut [u8],
        dst_w: u32,
        dst_h: u32,
    ) {
        let src_w = src_w as usize;
        let src_h = src_h as usize;
        let dst_w = dst_w as usize;
        let dst_h = dst_h as usize;
        for dst_y in 0..dst_h {
            let y0 = (dst_y * src_h) / dst_h;
            let y1 = ((dst_y + 1) * src_h) / dst_h;
            for dst_x in 0..dst_w {
                let x0 = (dst_x * src_w) / dst_w;
                let x1 = ((dst_x + 1) * src_w) / dst_w;
                let d = dst_y * dst_w + dst_x;
                if x0 == x1 || y0 == y1 {
                    dst[d] = src[y0.min(src_h - 1) * src_w + x0.min(src_w - 1)];
                    continue;
                }
                let mut sum = 0u64;
                for y in y0..y1 {
                    for x in x0..x1 {
                        sum += u64::from(src[y * src_w + x]);
                    }
                }
                let count = ((y1 - y0) * (x1 - x0)) as u64;
                dst[d] = u8::try_from(sum / count).unwrap_or(255);
            }
        }
    }

    /// Box-downscales a 4-channel (BGRA/RGBA) image: every destination pixel
    /// averages the source rectangle it covers per channel position. When the
    /// source is smaller than the destination, the covered range can be empty
    /// and falls back to a nearest-neighbour copy.
    fn box_downscale_bgra(
        src: &[u8],
        src_w: u32,
        src_h: u32,
        dst: &mut [u8],
        dst_w: u32,
        dst_h: u32,
    ) {
        let src_w = src_w as usize;
        let src_h = src_h as usize;
        let dst_w = dst_w as usize;
        let dst_h = dst_h as usize;
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

    /// Copies the latest converted frame out of the shared mutex as fast as
    /// possible (a raw plane memcpy, ~1 ms at 1080p), then downscales the
    /// planes and ships them as a base64 I420 payload. Independent of
    /// `VIDEO_SOURCE`, so the pre-roll preview works before any track is
    /// published. Returns whether a frame was emitted.
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
        // Target the stream's framerate once a track is live; fall back to a
        // fixed cadence during pre-roll.
        let fps = SCALE_TARGET
            .load_full()
            .map_or(PREVIEW_FALLBACK_FPS, |target| target.fps.max(1));
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
        bufs.yuv.resize(y_len + u_len + v_len, 0);
        bufs.yuv[..y_len].copy_from_slice(&plane_y[..y_len]);
        bufs.yuv[y_len..y_len + u_len].copy_from_slice(&plane_u[..u_len]);
        bufs.yuv[y_len + u_len..].copy_from_slice(&plane_v[..v_len]);
        let pts_us = guard.timestamp_us;
        drop(guard);

        // The buffer is tightly packed (stride == width), so the planes can be
        // downscaled independently and re-packed.
        let dst_w = width.min(PREVIEW_WIDTH);
        let dst_h = height.min(PREVIEW_HEIGHT);
        let chroma_w = width.div_ceil(2);
        let chroma_h = height.div_ceil(2);
        let dst_chroma_w = dst_w.div_ceil(2);
        let dst_chroma_h = dst_h.div_ceil(2);
        let (plane_y, rest) = bufs.yuv.split_at(y_len);
        let (plane_u, plane_v) = rest.split_at(u_len);
        bufs.plane_y.resize((dst_w * dst_h) as usize, 0);
        bufs.plane_u
            .resize((dst_chroma_w * dst_chroma_h) as usize, 0);
        bufs.plane_v
            .resize((dst_chroma_w * dst_chroma_h) as usize, 0);
        box_downscale_plane(plane_y, width, height, &mut bufs.plane_y, dst_w, dst_h);
        box_downscale_plane(
            plane_u,
            chroma_w,
            chroma_h,
            &mut bufs.plane_u,
            dst_chroma_w,
            dst_chroma_h,
        );
        box_downscale_plane(
            plane_v,
            chroma_w,
            chroma_h,
            &mut bufs.plane_v,
            dst_chroma_w,
            dst_chroma_h,
        );

        bufs.payload.clear();
        bufs.payload.extend_from_slice(&bufs.plane_y);
        bufs.payload.extend_from_slice(&bufs.plane_u);
        bufs.payload.extend_from_slice(&bufs.plane_v);
        let data = STANDARD.encode(bufs.payload.as_slice());
        STATS_PREVIEW_SENT.fetch_add(1, Ordering::Relaxed);
        if let Some(callback) = PREVIEW_CALLBACK.load_full() {
            callback(dst_w, dst_h, data, pts_us);
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
    fn convert_and_push(width: u32, height: u32, bgra: &[u8], preview_source: &SharedFrameBuffer) {
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
        source.capture_frame(&frame_buffer);
        STATS_PUSHED.fetch_add(1, Ordering::Relaxed);
    }

    /// Builds the frame callback shared by the portal engine and the
    /// synthetic generator: counts the frame, scales it down to the published
    /// track's resolution when larger, then converts and pushes it.
    fn make_on_frame(preview_source: SharedFrameBuffer) -> FrameCallback {
        let mut scale_scratch: Vec<u8> = Vec::new();
        Box::new(move |width, height, bgra, _pts_us| {
            STATS_DEQUEUED.fetch_add(1, Ordering::Relaxed);
            if width == 0 || height == 0 {
                STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
                return;
            }
            // libwebrtc's encoder has no explicit target size (VideoEncoding
            // carries only bitrate/fps), so without this a 4K source would be
            // encoded at 4K with the bitrate the user configured for 1080p.
            let (w, h, data) = match SCALE_TARGET.load_full() {
                Some(target) if width > target.width || height > target.height => {
                    let needed = (target.width * target.height * 4) as usize;
                    if scale_scratch.len() != needed {
                        scale_scratch.resize(needed, 0);
                    }
                    box_downscale_bgra(
                        bgra,
                        width,
                        height,
                        &mut scale_scratch,
                        target.width,
                        target.height,
                    );
                    (target.width, target.height, scale_scratch.as_slice())
                }
                _ => (width, height, bgra),
            };
            convert_and_push(w, h, data, &preview_source);
        })
    }

    /// Monotonic microsecond timestamp for video frames, anchored at first use.
    fn monotonic_us() -> i64 {
        use std::sync::OnceLock;
        static ANCHOR: OnceLock<Instant> = OnceLock::new();
        let anchor = ANCHOR.get_or_init(Instant::now);
        i64::try_from(anchor.elapsed().as_micros()).unwrap_or(0)
    }

    /// Starts the `pw_stream` engine for a negotiated portal stream: opens
    /// the isolated `PipeWire` remote on the portal fd and captures the
    /// stream's node, delivering linear BGRA frames to `on_frame`.
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

    /// What feeds the capture pipeline: the portal screencast (real screens)
    /// or a synthetic test pattern (headless e2e / probes).
    #[derive(Debug, Clone, Copy)]
    pub(crate) enum CaptureSource {
        Portal,
        Synthetic { width: u32, height: u32, fps: u32 },
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

        let mut portal = None;
        let mut capture = None;
        let mut generator = None;

        match source {
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
            yuv: Vec::new(),
            plane_y: Vec::new(),
            plane_u: Vec::new(),
            plane_v: Vec::new(),
            payload: Vec::new(),
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
        if let Some(mut capture) = capture {
            capture.stop();
        }
        if let Some(handle) = generator {
            let _ = handle.join();
        }
        // Dropping the portal closes the session (and its screencast node).
        drop(portal);
        CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
    }

    /// Starts the desktop capturer on its own thread. Returns once the portal
    /// session is created; the native portal picker then appears and frames
    /// flow after the user selects a source.
    ///
    /// # Errors
    ///
    /// Returns an error if a capture session is already active, the thread
    /// cannot be spawned, or the portal session fails to initialize within
    /// five seconds.
    pub(crate) fn start() -> Result<bool, String> {
        start_with(CaptureSource::Portal)
    }

    /// Starts the synthetic test-pattern capture (headless e2e and probes):
    /// generated BGRA frames feed the exact same conversion and publish path
    /// as the portal engine, with no picker and no Wayland requirement.
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
    /// while no track was active; `preview_frames_sent` counts base64 I420
    /// preview frames emitted via the preview callback; `capture_errors`
    /// counts portal/engine failures.
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

        /// Pins the plane downscale path: an exact 2× box average must equal
        /// the arithmetic mean of its 2×2 source block.
        #[test]
        fn box_downscale_plane_averages_exact_blocks() {
            let src: Vec<u8> = (0..64).collect(); // 8x8
            let mut dst = vec![0u8; 16]; // 4x4
            box_downscale_plane(&src, 8, 8, &mut dst, 4, 4);
            assert_eq!(dst[0], 4); // (0+1+8+9)/4 = 4
            assert_eq!(dst[1], 6); // (2+3+10+11)/4 = 6
            assert_eq!(dst[4], 20); // (16+17+24+25)/4 = 20
            assert_eq!(dst[15], 58); // (54+55+62+63)/4 = 58
        }

        /// Non-divisible ratios (e.g. 1080p → 360) must still produce the
        /// right output size without panics and with the last rows covered.
        #[test]
        fn box_downscale_plane_handles_uneven_ratios() {
            let src = vec![128u8; 1920 * 1080];
            let mut dst = vec![0u8; 640 * 360];
            box_downscale_plane(&src, 1920, 1080, &mut dst, 640, 360);
            assert!(dst.iter().all(|&v| v == 128));
        }

        /// Upscaling (source smaller than target) falls back to a
        /// nearest-neighbour copy without panicking.
        #[test]
        fn box_downscale_plane_upscale_falls_back_to_nearest() {
            let src = vec![7u8, 9, 11, 13]; // 2x2
            let mut dst = vec![0u8; 16]; // 4x4
            box_downscale_plane(&src, 2, 2, &mut dst, 4, 4);
            assert_eq!(dst[0], 7);
            assert_eq!(dst[15], 13);
        }

        /// The BGRA variant averages each channel position independently.
        #[test]
        fn box_downscale_bgra_averages_per_channel() {
            // 2x2 pixels: BGRA each.
            let src = vec![
                1u8, 2, 3, 4, //
                5, 6, 7, 8, //
                9, 10, 11, 12, //
                13, 14, 15, 16,
            ];
            let mut dst = vec![0u8; 4];
            box_downscale_bgra(&src, 2, 2, &mut dst, 1, 1);
            assert_eq!(dst, vec![7, 8, 9, 10]); // per-channel mean
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

        /// Headless probe of the whole synthetic pipeline: frames must flow
        /// through conversion (preview generation advances) even with no
        /// video track published. Run with:
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
    }
}

#[cfg(target_os = "linux")]
pub(crate) use engine::{
    clear_preview_callback, clear_scale_target, is_active, set_preview_callback, set_scale_target,
    start, start_synthetic_capture, stats, stop,
};

/// Non-Linux build stub: the in-house engine is Wayland-only and the app
/// degrades to audio-only outside Wayland, so the capture API is a no-op here.
#[cfg(not(target_os = "linux"))]
mod stub {
    use super::PreviewFrameCallback;

    pub(crate) fn start() -> Result<bool, String> {
        Ok(false)
    }

    pub(crate) fn start_synthetic_capture(_config: &crate::CaptureConfig) -> Result<bool, String> {
        Ok(false)
    }

    pub(crate) fn stop() -> bool {
        true
    }

    pub(crate) fn is_active() -> bool {
        false
    }

    pub(crate) fn stats() -> crate::DesktopCaptureStats {
        crate::DesktopCaptureStats::default()
    }

    pub(crate) fn set_preview_callback(_callback: PreviewFrameCallback) {}

    pub(crate) fn clear_preview_callback() {}

    pub(crate) fn set_scale_target(_width: u32, _height: u32, _fps: u32) {}

    pub(crate) fn clear_scale_target() {}
}

#[cfg(not(target_os = "linux"))]
pub(crate) use stub::{
    clear_preview_callback, clear_scale_target, is_active, set_preview_callback, set_scale_target,
    start, start_synthetic_capture, stats, stop,
};
