//! Desktop video capture engine: libwebrtc `DesktopCapturer` — `PipeWire`
//! (XDG portal picker) on Linux via `linux_capture`, WGC on Windows via
//! `wgc_capture`, plus a synthetic test-pattern source everywhere. All arms
//! feed the same packed-BGRA → I420 → paced-delivery → publish pipeline, so
//! the whole encode → SFU → spectator chain is testable without a portal
//! picker (the headless e2e uses `Synthetic`; macOS has no capture route).
//!
//! Preview transport: the preview thread keeps the newest I420 planes,
//! scales them to the renderer's preview card (aspect preserved, never
//! upscaled), and emits packed BGRA at the stream's framerate so the channel
//! only carries what the card can show — with a 16-byte little-endian header
//! (`u64 pts_us`, `u32 width`, `u32 height`) so the renderer can size its
//! texture and measure end-to-end latency. The renderer uploads the pixels
//! into a persistent GPU texture as-is.
//!
//! Threading: the capture thread owns the engine and the paced delivery
//! loop; a dedicated preview thread emits frames at its own cadence (it only
//! reads the newest stashed I420 planes under a brief lock), so scale/copy
//! work can never delay a delivery. `stop()` is polled by the loop itself,
//! so it never waits on a user that ignores the picker.

/// Preview callback type: `(payload, pts_us)` — `payload` is the raw
/// channel frame: 16-byte little-endian header + tightly packed BGRA rows
/// (see the module doc). The engine stays Tauri-unaware.
type PreviewFrameCallback = Box<dyn Fn(Vec<u8>, i64) + Send + Sync>;

/// Capture-ended callback type: invoked once when the portal session closes
/// while the capture is running — the compositor ended the stream (e.g. the
/// presenter closed the captured window). The Tauri backend forwards this to
/// the renderer as the `capture-ended` event, which tears the share down.
type CaptureEndedCallback = Box<dyn Fn() + Send + Sync>;

use arc_swap::ArcSwapOption;
use livekit::webrtc::native::yuv_helper;
#[cfg(target_os = "linux")]
use livekit::webrtc::prelude::{I420Buffer, VideoBuffer};
#[cfg(not(target_os = "linux"))]
use livekit::webrtc::prelude::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation};
use std::collections::VecDeque;
#[cfg(target_os = "linux")]
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::CaptureConfig;
#[cfg(not(target_os = "linux"))]
use crate::VIDEO_SOURCE;
#[cfg(target_os = "linux")]
use gstreamer as gst;
#[cfg(target_os = "linux")]
use gstreamer_video as gst_video;

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
/// Preview frames handed to the preview callback.
pub(crate) static STATS_PREVIEW_SENT: AtomicU64 = AtomicU64::new(0);
pub(crate) static STATS_KEEPALIVE_ATTEMPTED: AtomicU64 = AtomicU64::new(0);
pub(crate) static STATS_KEEPALIVE_PUSHED: AtomicU64 = AtomicU64::new(0);
pub(crate) static STATS_KEEPALIVE_DROPPED: AtomicU64 = AtomicU64::new(0);
pub(crate) static STATS_LAST_WIDTH: AtomicU32 = AtomicU32::new(0);
pub(crate) static STATS_LAST_HEIGHT: AtomicU32 = AtomicU32::new(0);
/// `FramePacer` counters are separate from generic capture drops so a real-path
/// trace can distinguish source failure from queue overflow.
pub(crate) static STATS_PACER_PUSHES: AtomicU64 = AtomicU64::new(0);
pub(crate) static STATS_PACER_POPS: AtomicU64 = AtomicU64::new(0);
pub(crate) static STATS_PACER_DROPS: AtomicU64 = AtomicU64::new(0);
pub(crate) static STATS_PACER_DEPTH: AtomicU64 = AtomicU64::new(0);
pub(crate) static STATS_PACER_MAX_DEPTH: AtomicU64 = AtomicU64::new(0);
static NEXT_FRAME_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static FRAME_TRACE_ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("SLOPCAST_FRAME_TRACE").is_some());

pub(crate) fn reset_stats() {
    STATS_PUSHED.store(0, Ordering::Relaxed);
    STATS_DEQUEUED.store(0, Ordering::Relaxed);
    STATS_DROPPED.store(0, Ordering::Relaxed);
    STATS_ERRORS.store(0, Ordering::Relaxed);
    STATS_PREVIEW_SENT.store(0, Ordering::Relaxed);
    STATS_KEEPALIVE_ATTEMPTED.store(0, Ordering::Relaxed);
    STATS_KEEPALIVE_PUSHED.store(0, Ordering::Relaxed);
    STATS_KEEPALIVE_DROPPED.store(0, Ordering::Relaxed);
    STATS_LAST_WIDTH.store(0, Ordering::Relaxed);
    STATS_LAST_HEIGHT.store(0, Ordering::Relaxed);
    STATS_PACER_PUSHES.store(0, Ordering::Relaxed);
    STATS_PACER_POPS.store(0, Ordering::Relaxed);
    STATS_PACER_DROPS.store(0, Ordering::Relaxed);
    STATS_PACER_DEPTH.store(0, Ordering::Relaxed);
    STATS_PACER_MAX_DEPTH.store(0, Ordering::Relaxed);
    NEXT_FRAME_SEQUENCE.store(0, Ordering::Relaxed);
    CAPTURE_ENDED_EMITTED.store(false, Ordering::Relaxed);
}

/// Freelist cap for `OwnedI420` plane allocations — the old input pool's
/// size. Steady-state capture reuses this bounded set of allocations: the
/// `GStreamer` pipeline returns each one here when it releases the wrapping
/// buffer, and `OwnedI420::new` pops it back for the next frame.
#[cfg(target_os = "linux")]
const I420_FREELIST_CAP: usize = 24;

/// Recycled `OwnedI420` plane allocations (see `OwnedI420::new`/`Drop`).
#[cfg(target_os = "linux")]
static I420_FREELIST: LazyLock<Mutex<Vec<Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(Vec::with_capacity(I420_FREELIST_CAP)));

/// Discards the cached `OwnedI420` plane allocations. Called on every video
/// pipeline attach: the previous session's pipeline returns its buffers to
/// the freelist during teardown, and this frees them once the new pipeline
/// is in place — mirroring the old input pool's lifetime.
#[cfg(target_os = "linux")]
pub(crate) fn clear_i420_freelist() {
    if let Ok(mut freelist) = I420_FREELIST.lock() {
        freelist.clear();
    }
}

/// I420 plane layout for `OwnedI420`, derived from `gst_video::VideoInfo` so
/// it always matches the layout the encoder's appsrc caps describe (same
/// builder call, same default alignment). The plane `Vec` is sized exactly
/// to `size`.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
struct I420Layout {
    offsets: [usize; 3],
    strides: [i32; 3],
    size: usize,
}

/// An owned, contiguous I420 frame whose plane allocation is handed straight
/// to `GStreamer` as the input buffer (`gst::Buffer::from_mut_slice`, zero
/// copy): the capture engine converts BGRA directly into the allocation, the
/// encoder's buffer wraps the *same* allocation, and `OwnedI420::drop`
/// returns it to `I420_FREELIST` when `GStreamer` releases the buffer — one
/// copy total per frame, where the old path copied `I420Buffer` → pool buffer.
///
/// This is the Linux publish interchange type; Windows keeps the libwebrtc
/// `I420Buffer` path (`SampleBuffer` below).
#[cfg(target_os = "linux")]
pub(crate) struct OwnedI420 {
    width: u32,
    height: u32,
    layout: I420Layout,
    planes: Vec<u8>,
}

#[cfg(target_os = "linux")]
impl OwnedI420 {
    /// The I420 `VideoInfo` this frame's layout derives from. The fps is
    /// left unset, so `VideoConverter::new` accepts it as both source and
    /// destination (see `scale`).
    fn video_info(width: u32, height: u32) -> Result<gst_video::VideoInfo, String> {
        gst_video::VideoInfo::builder(gst_video::VideoFormat::I420, width, height)
            .build()
            .map_err(|error| format!("Failed to build GStreamer I420 video info: {error}"))
    }

    fn layout(width: u32, height: u32) -> Result<I420Layout, String> {
        let info = Self::video_info(width, height)?;
        let offsets = info.offset();
        let strides = info.stride();
        Ok(I420Layout {
            offsets: [offsets[0], offsets[1], offsets[2]],
            strides: [strides[0], strides[1], strides[2]],
            size: info.size(),
        })
    }

    /// Allocates a frame's plane buffer, reusing a cached allocation from
    /// `I420_FREELIST` when one is available (a static screen's keepalive
    /// path reuses them back-to-back).
    fn new(width: u32, height: u32) -> Self {
        let layout = Self::layout(width, height).unwrap_or_else(|error| {
            unreachable!("I420 VideoInfo for {width}x{height} cannot fail to build: {error}")
        });
        let mut planes = if let Ok(mut freelist) = I420_FREELIST.lock() {
            freelist.pop().unwrap_or_default()
        } else {
            log::warn!("I420 freelist lock poisoned; allocating a fresh plane buffer");
            Vec::new()
        };
        planes.resize(layout.size, 0);
        Self {
            width,
            height,
            layout,
            planes,
        }
    }

    /// Plane strides as `u32`, matching the `argb_to_i420`/`i420_to_argb`
    /// signatures. `GStreamer` reports I420 strides as positive `i32` values.
    fn strides(&self) -> (u32, u32, u32) {
        (
            u32::try_from(self.layout.strides[0]).unwrap_or(0),
            u32::try_from(self.layout.strides[1]).unwrap_or(0),
            u32::try_from(self.layout.strides[2]).unwrap_or(0),
        )
    }

    fn data(&self) -> (&[u8], &[u8], &[u8]) {
        (
            &self.planes[self.layout.offsets[0]..self.layout.offsets[1]],
            &self.planes[self.layout.offsets[1]..self.layout.offsets[2]],
            &self.planes[self.layout.offsets[2]..],
        )
    }

    fn data_mut(&mut self) -> (&mut [u8], &mut [u8], &mut [u8]) {
        let (y, u, v) = (
            self.layout.offsets[0],
            self.layout.offsets[1],
            self.layout.offsets[2],
        );
        let (y_and_u, v_plane) = self.planes.split_at_mut(v);
        let (y_and_u_head, u_plane) = y_and_u.split_at_mut(u);
        let (_, y_plane) = y_and_u_head.split_at_mut(y);
        (y_plane, u_plane, v_plane)
    }

    /// Zero-copy I420 scaling via `GStreamer`'s `VideoConverter`: it maps the
    /// plane allocation directly (no copy), and the scaled destination
    /// allocation is recovered from the buffer it wrapped. Consumes the
    /// source frame.
    ///
    /// The converter is cached by (src, dst) dimensions so steady-state
    /// 1080p60 pays for `VideoConverter::new` only once per resolution
    /// change — the delivery hot path already drops frames when it slips
    /// 16 ms (the fixed-deadline `next_delivery_at` skips a missed tick),
    /// so a per-frame converter construction was measured as a recurring
    /// frame-interval overrun that directly surfaced as presenter stutter
    /// at high capture sizes.
    fn scale(self, width: u32, height: u32) -> Result<Self, String> {
        // Fast path: identical dimensions are handled upstream
        // (`scale_to_target`), but `keepalive_sample` still routes 1:1
        // through here when the capture and target resolutions already
        // match — avoid all VideoInfo allocation in that case.
        if self.width == width && self.height == height {
            return Ok(self);
        }
        let src_info = Self::video_info(self.width, self.height)?;
        let dst_info = Self::video_info(width, height)?;
        let converter = Self::cached_converter(&src_info, &dst_info)
            .ok_or_else(|| "Failed to create I420 scaler".to_string())?;

        let mut dest = Self::new(width, height);
        let src_buffer = gst::Buffer::from_mut_slice(self);
        let mut dst_buffer = gst::Buffer::from_mut_slice(dest);
        {
            let src =
                gst_video::VideoFrameRef::from_buffer_ref_readable(src_buffer.as_ref(), &src_info)
                    .map_err(|error| format!("Failed to map I420 scale source: {error}"))?;
            let dst_ref = dst_buffer
                .get_mut()
                .ok_or_else(|| "I420 scale destination is unexpectedly shared".to_string())?;
            let mut dst_frame =
                gst_video::VideoFrameRef::from_buffer_ref_writable(dst_ref, &dst_info)
                    .map_err(|error| format!("Failed to map I420 scale destination: {error}"))?;
            converter.frame_ref(&src, &mut dst_frame);
        }
        dest = dst_buffer
            .try_into_inner::<OwnedI420>()
            .map_err(|_| "I420 scaler destination lost its wrapped allocation".to_string())?;
        Ok(dest)
    }

    fn cached_converter(
        src: &gst_video::VideoInfo,
        dst: &gst_video::VideoInfo,
    ) -> Option<Arc<gst_video::VideoConverter>> {
        // `VideoConverter` wraps a non-clonable GStreamer object (no Clone/Copy),
        // so cache the boxed instance behind an `Arc` keyed on dimensions.
        // A `Mutex` protects the singleton; the hot path only pays for an
        // `Arc::clone` after the first hit. A `LazyLock` would complicate
        // invalidation (resolution changes), and dimensions are small enough
        // to compare cheaply.
        #[allow(
            clippy::type_complexity,
            reason = "single cache entry keyed on four small dimensions; extracted type would obscure the tuple shape"
        )]
        static CACHE: std::sync::Mutex<
            Option<(u32, u32, u32, u32, Arc<gst_video::VideoConverter>)>,
        > = std::sync::Mutex::new(None);
        // Dimensions fully qualify the conversion (format is always I420,
        // fps is unset — see `video_info`). Two sw/u32 pairs are cheaper to
        // compare than two `VideoInfo` values.
        let key = (src.width(), src.height(), dst.width(), dst.height());
        #[allow(
            clippy::collapsible_if,
            reason = "guard + data check are clearer separated here for the cache-hit fast path"
        )]
        if let Ok(cache) = CACHE.lock()
            && let Some((sw, sh, dw, dh, converter)) = cache.as_ref()
            && (*sw, *sh, *dw, *dh) == key
        {
            return Some(Arc::clone(converter));
        }
        let converter = gst_video::VideoConverter::new(src, dst, None).ok()?;
        let wrapped = Arc::new(converter);
        if let Ok(mut cache) = CACHE.lock() {
            *cache = Some((key.0, key.1, key.2, key.3, Arc::clone(&wrapped)));
        }
        Some(wrapped)
    }
}

#[cfg(target_os = "linux")]
impl AsMut<[u8]> for OwnedI420 {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.planes
    }
}

#[cfg(target_os = "linux")]
impl OwnedI420 {
    /// Hands the plane allocation straight to the freelist, skipping the
    /// `Drop` push (which would return it again on drop). Used by the
    /// keepalive warm-up, which wants the allocation resident *without*
    /// an owning buffer instance.
    fn into_planes(mut self) -> Vec<u8> {
        std::mem::take(&mut self.planes)
    }
}

#[cfg(target_os = "linux")]
impl Drop for OwnedI420 {
    fn drop(&mut self) {
        // A `std::mem::take`d allocation (the keepalive warm-up's
        // `into_planes`) leaves an empty shell; never push it back — the
        // freelist must only ever hold real plane allocations.
        if self.planes.is_empty() {
            return;
        }
        if let Ok(mut freelist) = I420_FREELIST.lock()
            && freelist.len() < I420_FREELIST_CAP
        {
            freelist.push(std::mem::take(&mut self.planes));
        }
    }
}

/// The frame handed to the publish path: capture dimensions, the capture
/// clock timestamp, and the I420 plane buffer. On Linux the buffer is an
/// `OwnedI420` allocation that becomes the `GStreamer` input buffer itself; on
/// other platforms it is a libwebrtc `I420Buffer` wrapped in a `VideoFrame`
/// at publish time.
pub(crate) struct VideoSample {
    pub(crate) sequence: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pts_us: i64,
    pub(crate) buffer: SampleBuffer,
}

/// Emits one structured, monotonic-clock record per sender boundary when the
/// temporary real-path diagnostic is enabled. Keeping it opt-in avoids adding
/// logging work to normal capture.
pub(crate) fn trace_frame(
    stage: &str,
    sequence: u64,
    capture_pts_us: i64,
    queue_depth: usize,
    gstreamer_pts_ns: Option<u64>,
    duration_ns: Option<u64>,
) {
    if !*FRAME_TRACE_ENABLED {
        return;
    }

    log::info!(
        "[frame-trace] stage={stage} sequence={sequence} callback_us={capture_pts_us} local_us={} pacer_depth={queue_depth} gstreamer_pts_ns={gstreamer_pts_ns:?} duration_ns={duration_ns:?}",
        monotonic_us(),
    );
}

pub(crate) fn trace_encoder_output(gstreamer_pts_ns: Option<u64>, encoded_count: u64) {
    if !*FRAME_TRACE_ENABLED {
        return;
    }

    log::info!(
        "[frame-trace] stage=encoder-output encoded_count={encoded_count} local_us={} gstreamer_pts_ns={gstreamer_pts_ns:?}",
        monotonic_us(),
    );
}

#[cfg(target_os = "linux")]
pub(crate) type SampleBuffer = OwnedI420;
#[cfg(not(target_os = "linux"))]
pub(crate) type SampleBuffer = I420Buffer;

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

/// Ring A capacity: capture-resolution history slots kept for downstream
/// consumers (the preview emitter and the keepalive delivery arm). Two slots
/// hold the newest captured frame and its immediate predecessor; both readers
/// only ever take the newest. Slots store the *original capture resolution*
/// (e.g. native desktop size), so a dynamic client-side `SCALE_TARGET` change
/// is honored at delivery time instead of being baked in at capture time.
const HISTORY_RING_CAPACITY: usize = 2;

/// Keepalive delivery ring (Ring B) capacity (Windows only): delivery buffers
/// the keepalive arm rotates through when the pacer runs dry. Sized larger
/// than m144's encoder retention window (`kMaxFramesInPreparation` = 10), so
/// no buffer instance is ever re-submitted — or repacked in place — while the
/// asynchronous encode path may still reference it. Linux needs no ring (see
/// `keepalive_sample`).
#[cfg(not(target_os = "linux"))]
const KEEPALIVE_RING_CAPACITY: usize = 16;

/// Preview callback: invoked with a BGRA frame while a capture session is
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

/// The capture-ended callback; `CAPTURE_ENDED_EMITTED` fires it at most
/// once per session.
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
/// On Linux this also warms the I420 freelist with ready-made keepalive
/// buffers at the target size (see `warm_keepalive_freelist`).
pub(crate) fn set_scale_target(width: u32, height: u32, fps: u32) {
    SCALE_TARGET.store(Some(Arc::new(ScaleTarget { width, height, fps })));
    #[cfg(target_os = "linux")]
    warm_keepalive_freelist(width, height);
}

/// Pre-allocates two target-sized `OwnedI420` buffers into `I420_FREELIST`
/// so the first keepalives after a busy stretch never allocate on the
/// delivery thread. Without this, the stillness onset (the last real
/// frames' buffers are still held by the pipeline) paid a 2.25 MB
/// alloc + zero-fill + copy on the delivery path — the same transition
/// where libvpx's rate controller is already wobbling, so the allocation
/// hitch compounded the visible "lazy" stutter.
#[cfg(target_os = "linux")]
fn warm_keepalive_freelist(width: u32, height: u32) {
    if width == 0 || height == 0 {
        return;
    }
    // Allocate *before* taking the lock: `OwnedI420::new` pops from the
    // freelist itself, so holding the mutex across the allocation would
    // self-deadlock on the non-reentrant lock.
    let warmed = [
        OwnedI420::new(width, height).into_planes(),
        OwnedI420::new(width, height).into_planes(),
    ];
    let Ok(mut freelist) = I420_FREELIST.lock() else {
        log::warn!("I420 freelist lock poisoned; skipping keepalive warm-up");
        return;
    };
    for planes in warmed {
        if freelist.len() >= I420_FREELIST_CAP {
            break;
        }
        freelist.push(planes);
    }
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
/// of shipping full-resolution frames through the IPC channel.
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
/// preserving its aspect ratio. Never upscales past the source — when the
/// source already fits inside the viewport the source resolution is returned
/// unchanged (the renderer's CSS does the upscale). `None` only on zero
/// dimensions.
/// Scales a sample's plane buffer to the target dimensions, returning the
/// sample with updated width/height. Consumes the sample: Linux scaling
/// takes ownership of the plane allocation, Windows `I420Buffer::scale`
/// allocates a new buffer.
fn scale_sample(
    mut sample: VideoSample,
    target_width: u32,
    target_height: u32,
) -> Result<VideoSample, String> {
    sample.buffer = scale_buffer(sample.buffer, target_width, target_height)?;
    sample.width = target_width;
    sample.height = target_height;
    Ok(sample)
}

#[cfg(target_os = "linux")]
fn scale_buffer(buffer: OwnedI420, width: u32, height: u32) -> Result<OwnedI420, String> {
    buffer.scale(width, height)
}

#[cfg(not(target_os = "linux"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "non-Linux I420Buffer::scale cannot fail; the Result matches the fallible Linux arm so scale_sample stays uniform"
)]
fn scale_buffer(mut buffer: I420Buffer, width: u32, height: u32) -> Result<I420Buffer, String> {
    Ok(buffer.scale(
        i32::try_from(width).unwrap_or(0),
        i32::try_from(height).unwrap_or(0),
    ))
}

/// Scales a sample to the current `SCALE_TARGET` when its dimensions differ
/// (OBS-style canvas scaling: libwebrtc's encoder has no explicit size, so a
/// 4K source would otherwise stream at 4K with the user's 1080p bitrate).
/// Logs scales slower than 5 ms at most once per 3 s. Errors bubble up for
/// the delivery loop to count as drops.
fn scale_to_target(
    sample: VideoSample,
    target: Option<&ScaleTarget>,
    last_slow_scale: &mut Instant,
) -> Result<VideoSample, String> {
    let Some(target) =
        target.filter(|target| sample.width != target.width || sample.height != target.height)
    else {
        return Ok(sample);
    };

    let scale_start = Instant::now();
    let scaled = scale_sample(sample, target.width, target.height)?;
    let scale_ms = scale_start.elapsed().as_secs_f64() * 1000.0;
    if scale_ms >= 5.0 && last_slow_scale.elapsed() >= Duration::from_secs(3) {
        *last_slow_scale = Instant::now();
        log::info!(
            "[desktop-capture] slow target scale: {scale_ms:.1} ms ({}x{} -> {}x{})",
            scaled.width,
            scaled.height,
            target.width,
            target.height,
        );
    }
    Ok(scaled)
}

#[cfg(target_os = "linux")]
fn publish_video_frame(sample: VideoSample) -> bool {
    crate::gstreamer_publisher::push_video_frame(sample).is_ok()
}

#[cfg(not(target_os = "linux"))]
fn publish_video_frame(sample: VideoSample) -> bool {
    let Some(source) = VIDEO_SOURCE.load_full() else {
        return false;
    };

    source.capture_frame(&VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us: sample.pts_us,
        frame_metadata: None,
        buffer: sample.buffer,
    });
    true
}

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
        // The source already fits inside the viewport: no downscale needed,
        // so emit at the source's own resolution (the renderer's CSS scales
        // it up). `None` is reserved for invalid zero dimensions.
        return Some((base_w, base_h));
    }
    Some((fit_w.max(1), fit_h.max(1)))
}

/// Scratch buffers reused across preview emissions (allocated once per
/// session).
struct PreviewScaleBufs {
    /// I420 planes copied out of the stash under a brief lock.
    planes: Vec<u8>,
    /// Final channel payload: 16-byte header + the fitted BGRA pixels.
    out: Vec<u8>,
    /// Reusable I420 frame rebuilt from `planes`; recreated only when the
    /// source dimensions change.
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
///
/// `width`/`height` are the **original capture resolution** (e.g. the native
/// desktop size), not any delivery/`SCALE_TARGET` size: the preview emitter
/// fits the card, and the keepalive arm scales to the current encoder target
/// at delivery time — so a client-side resolution change is honored without
/// touching the capture thread.
struct PreviewI420 {
    width: u32,
    height: u32,
    pts_us: i64,
    y_len: usize,
    u_len: usize,
    v_len: usize,
    planes: Vec<u8>,
}

/// Ring A: a small rotating history of the most recent capture-resolution
/// I420 frames (the newest plus its immediate predecessor), written by the
/// capture callback under a brief lock and read by the preview emitter and
/// the keepalive delivery arm. Readers copy the newest slot out; the writer
/// only ever pushes — drop-oldest keeps the ring bounded by construction.
///
/// A ring (not a single slot) so the keepalive arm never hands the same
/// buffer instance to the encoder twice within the encoder's asynchronous
/// retention window: Ring B (the delivery ring) rotates *fresh* packed
/// buffers, but its source — this history — must always be a stable,
/// immutable snapshot, which a ring of immutable slots provides.
struct HistoryRing {
    slots: Vec<PreviewI420>,
    /// Write index; `slots[write_idx % len]` is replaced next push.
    write_idx: usize,
    /// Total frames written; readers use it to detect a new snapshot.
    pushed: u64,
}

impl HistoryRing {
    fn new(capacity: usize) -> Self {
        Self {
            slots: (0..capacity)
                .map(|_| PreviewI420 {
                    width: 0,
                    height: 0,
                    pts_us: 0,
                    y_len: 0,
                    u_len: 0,
                    v_len: 0,
                    planes: Vec::new(),
                })
                .collect(),
            write_idx: 0,
            pushed: 0,
        }
    }

    /// Stores a capture-resolution I420 frame directly from the plane
    /// slices. The length fields mirror the actual plane slice lengths;
    /// packing the planes straight into the slot skips the intermediate
    /// packed buffer — a ~2.25 MB allocation *and* a ~2.25 MB copy saved
    /// on every capture frame (the capture callback's hot path).
    fn push_planes(&mut self, width: u32, height: u32, pts_us: i64, y: &[u8], u: &[u8], v: &[u8]) {
        let idx = self.write_idx % self.slots.len();
        let slot = &mut self.slots[idx];
        slot.width = width;
        slot.height = height;
        slot.pts_us = pts_us;
        slot.y_len = y.len();
        slot.u_len = u.len();
        slot.v_len = v.len();
        slot.planes.clear();
        slot.planes.extend_from_slice(y);
        slot.planes.extend_from_slice(u);
        slot.planes.extend_from_slice(v);
        self.write_idx += 1;
        self.pushed += 1;
    }

    /// Returns the newest snapshot (the most recently pushed slot), or
    /// `None` when nothing has been captured yet.
    fn newest(&self) -> Option<&PreviewI420> {
        if self.pushed == 0 {
            return None;
        }
        let idx = (self.write_idx - 1) % self.slots.len();
        Some(&self.slots[idx])
    }
}

static PREVIEW_I420: Mutex<Option<HistoryRing>> = Mutex::new(None);

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
        let Some(ring) = guard.as_ref() else {
            return false;
        };
        // Copy the newest slot's planes out under the brief lock; the scale
        // and conversion below run outside it, so a slow emission can never
        // stall the capture callback's stash write (the lock is shared).
        let Some(entry) = ring.newest() else {
            return false;
        };
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

    // Rebuild the I420 frame from the stashed planes and scale with libyuv,
    // then convert to the BGRA the renderer expects — none of it under the
    // shared lock. The source buffer is reused across emissions, recreated
    // only on a source dimension change.
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
    queue: Mutex<VecDeque<VideoSample>>,
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
    fn push(&self, frame: VideoSample) {
        let Ok(mut queue) = self.queue.lock() else {
            STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if queue.len() >= self.capacity
            && let Some(dropped) = queue.pop_front()
        {
            STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
            STATS_PACER_DROPS.fetch_add(1, Ordering::Relaxed);
            trace_frame(
                "pacer-drop",
                dropped.sequence,
                dropped.pts_us,
                queue.len(),
                None,
                None,
            );
        }
        let sequence = frame.sequence;
        let capture_pts_us = frame.pts_us;
        queue.push_back(frame);
        let depth = queue.len();
        STATS_PACER_PUSHES.fetch_add(1, Ordering::Relaxed);
        STATS_PACER_DEPTH.store(depth as u64, Ordering::Relaxed);
        STATS_PACER_MAX_DEPTH.fetch_max(depth as u64, Ordering::Relaxed);
        trace_frame("pacer-push", sequence, capture_pts_us, depth, None, None);
    }

    /// Pops the oldest queued frame for delivery to the encoder.
    fn pop(&self) -> Option<VideoSample> {
        let Ok(mut queue) = self.queue.lock() else {
            return None;
        };
        let frame = queue.pop_front()?;
        let depth = queue.len();
        STATS_PACER_POPS.fetch_add(1, Ordering::Relaxed);
        STATS_PACER_DEPTH.store(depth as u64, Ordering::Relaxed);
        trace_frame("pacer-pop", frame.sequence, frame.pts_us, depth, None, None);
        Some(frame)
    }
}

/// Ring B: the keepalive delivery ring (Windows only). The capture loop's
/// keepalive arm (pacer empty) must keep the RTP cadence even without fresh
/// frames, but it must never re-submit the same `VideoFrame`/`I420Buffer`
/// instance while the encoder may still hold it asynchronously (the SDK
/// exposes no release callback). Instead, each keepalive tick:
///
/// 1. Snapshot the newest capture-resolution planes from Ring A.
/// 2. Scale them to the current `SCALE_TARGET` into the next ring slot.
/// 3. Submit that slot as a `VideoSample` with the source's timestamp.
///
/// The ring rotates `KEEPALIVE_RING_CAPACITY` distinct buffer instances, so
/// no instance is re-packed/re-submitted within that many ticks — well beyond
/// m144's encoder retention window (`kMaxFramesInPreparation` = 10). The ring
/// is owned by `run_capture` (single-threaded access), so no lock is needed.
///
/// On Linux this ring is unnecessary: every keepalive buffer is a fresh
/// allocation or an `I420_FREELIST` pop, both released by `GStreamer`, so the
/// encoder can never see the same buffer instance twice (`keepalive_sample`).
#[cfg(not(target_os = "linux"))]
struct KeepaliveRing {
    slots: Vec<VideoSample>,
    /// Next slot to fill; wraps every `KEEPALIVE_RING_CAPACITY` submissions.
    next: usize,
    /// Reusable snapshot buffer: `keepalive_frame` copies the newest Ring-A
    /// planes here before packing them into a slot. Reused across ticks, so a
    /// static screen never allocates a fresh full-frame Vec (up to ~12 MB at
    /// 4K) per keepalive submission.
    scratch: Vec<u8>,
}

#[cfg(not(target_os = "linux"))]
impl KeepaliveRing {
    fn new() -> Self {
        Self {
            slots: Vec::with_capacity(KEEPALIVE_RING_CAPACITY),
            next: 0,
            scratch: Vec::new(),
        }
    }

    /// Returns the index of the slot to pack this tick, growing the ring
    /// lazily (the first `KEEPALIVE_RING_CAPACITY` ticks allocate one buffer
    /// each). Exposed as an index rather than a reference so the caller can
    /// copy from `scratch` (a disjoint field) without fighting the borrow
    /// checker.
    fn next_slot(&mut self, target: (u32, u32)) -> usize {
        if self.slots.len() < KEEPALIVE_RING_CAPACITY {
            self.slots.push(VideoSample {
                sequence: 0,
                width: target.0,
                height: target.1,
                pts_us: 0,
                buffer: I420Buffer::new(target.0, target.1),
            });
        }
        let slot = &mut self.slots[self.next];
        if (slot.buffer.width(), slot.buffer.height()) != target {
            *slot = VideoSample {
                sequence: 0,
                width: target.0,
                height: target.1,
                pts_us: 0,
                buffer: I420Buffer::new(target.0, target.1),
            };
        }
        let idx = self.next;
        self.next = (self.next + 1) % KEEPALIVE_RING_CAPACITY;
        idx
    }
}

/// The capture-source frame callback: `(width, height, bgra, pts_us)`.
pub(crate) type FrameCallback = Box<dyn FnMut(u32, u32, &[u8], i64) + Send>;

/// Converts a linear BGRA frame into a fresh `VideoSample` whose plane
/// buffer is sized to the capture resolution.
///
/// Every frame gets its own buffer: the encoder consumes frames
/// asynchronously on its own queue (the cadence adapter posts them by
/// value, `VideoStreamEncoder` encodes on the encoder thread), so a
/// reused buffer would be torn or duplicated mid-read — blocky smearing
/// during motion, independent of codec. On Linux the plane buffer becomes
/// the `GStreamer` input buffer itself (zero extra copy); on other platforms
/// it is a libwebrtc `I420Buffer`, wrapped in a `VideoFrame` at publish.
#[cfg(target_os = "linux")]
fn convert_frame(width: u32, height: u32, bgra: &[u8], pts_us: i64) -> VideoSample {
    let mut out = OwnedI420::new(width, height);
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
    VideoSample {
        sequence: NEXT_FRAME_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        width,
        height,
        // Preserve the capture timestamp the callback supplied (measured at
        // callback entry, before the stride re-pack) rather than re-measuring
        // now: variable BGRA→I420 conversion time would otherwise leak into
        // the source-timestamp jitter the encoder's `TimestampAligner` sees.
        pts_us,
        buffer: out,
    }
}

/// Non-Linux twin of `convert_frame`: the same `VideoSample` shape, with a
/// libwebrtc `I420Buffer` as the plane buffer.
#[cfg(not(target_os = "linux"))]
fn convert_frame(width: u32, height: u32, bgra: &[u8], pts_us: i64) -> VideoSample {
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
    VideoSample {
        sequence: NEXT_FRAME_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        width,
        height,
        // Preserve the capture timestamp the callback supplied (measured at
        // callback entry, before the stride re-pack) rather than re-measuring
        // now: variable BGRA→I420 conversion time would otherwise leak into
        // the source-timestamp jitter the encoder's `TimestampAligner` sees.
        pts_us,
        buffer: out,
    }
}

/// Builds the frame callback shared by the portal engine, the WGC engine
/// and the synthetic generator: counts the frame, converts it to I420 and
/// queues it for the paced delivery loop.
fn make_on_frame(pacer: Arc<FramePacer>) -> FrameCallback {
    Box::new(move |width, height, bgra, pts_us| {
        STATS_DEQUEUED.fetch_add(1, Ordering::Relaxed);
        if width == 0 || height == 0 {
            STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let frame = convert_frame(width, height, bgra, pts_us);
        trace_frame("capture", frame.sequence, frame.pts_us, 0, None, None);
        // Stash the newest frame's I420 planes for the preview thread,
        // skipped when neither consumer exists — the preview emitter/poll
        // only reads Ring A while `PREVIEW_CALLBACK` is registered, and the
        // keepalive arm only reads it while a video track is active. 1080p
        // planes are ~2.25 MB, a third of the old BGRA copy; paying that on
        // every frame with no consumer is pure waste on the capture thread's
        // 16.67 ms budget. The stash write is a brief memcpy under the lock;
        // the emitter copies out the same way, so neither side ever holds
        // the lock for a scale or convert. Always stash at the *capture
        // resolution*: Ring A feeds the keepalive delivery arm too, which
        // scales to the current `SCALE_TARGET` at delivery time, so a
        // client-side resolution change is honored without a capture restart.
        let ring_has_consumers =
            crate::is_video_track_active() || PREVIEW_CALLBACK.load_full().is_some();
        if ring_has_consumers && let Ok(mut slot) = PREVIEW_I420.lock() {
            let ring = slot.get_or_insert_with(|| HistoryRing::new(HISTORY_RING_CAPACITY));
            let (plane_y, plane_u, plane_v) = frame.buffer.data();
            // `push_planes` packs the planes straight into the ring slot —
            // no intermediate Vec, so the stash costs one copy (not an
            // allocation + a copy) on the capture thread's hot path. The
            // ring stores the *capture* timestamp, so the keepalive arm can
            // stamp re-deliveries with the source frame's clock.
            ring.push_planes(width, height, pts_us, plane_y, plane_u, plane_v);
        }
        pacer.push(frame);
    })
}

/// Builds one keepalive `VideoSample` from the newest Ring-A snapshot,
/// honoring the current `SCALE_TARGET` at delivery time (Ring A stores the
/// original capture resolution, so a client-side resolution change is
/// applied here, not baked in at capture). Packs the snapshot into the next
/// rotating Ring-B slot — each submission gets a fresh `I420Buffer` instance,
/// so the encoder's asynchronous retention window never sees the same buffer
/// twice. The sample carries the source frame's *capture* timestamp (not
/// delivery time), keeping the `TimestampAligner`'s RTP timestamp chain
/// monotonic.
///
/// Returns `None` when nothing has been captured yet (e.g. the portal picker
/// is still open, or the gap before the first frame).
#[cfg(not(target_os = "linux"))]
fn keepalive_frame(ring: &mut KeepaliveRing) -> Option<VideoSample> {
    // Scale to the configured encoder target, or fall back to the source
    // resolution when no track is live (the keepalive arm only runs with a
    // live `VIDEO_SOURCE`, so `SCALE_TARGET` is normally set).
    let target = SCALE_TARGET.load_full().map(|t| (t.width, t.height));
    let (src_w, src_h, y_len, u_len, v_len, pts_us) = {
        let Ok(guard) = PREVIEW_I420.lock() else {
            return None;
        };
        let history = guard.as_ref()?;
        let entry = history.newest()?;
        // Copy the newest snapshot's planes into the reusable scratch buffer
        // under the brief lock; the scale and pack below run outside it.
        ring.scratch.clear();
        ring.scratch.extend_from_slice(&entry.planes);
        (
            entry.width,
            entry.height,
            entry.y_len,
            entry.u_len,
            entry.v_len,
            entry.pts_us,
        )
    };
    let (out_w, out_h) = target.unwrap_or((src_w, src_h));

    // Pack the snapshot's planes into the next ring slot's buffer **at the
    // source dimensions** — the raw plane bytes are contiguous rows of
    // `src_w × src_h` (stride == width), so they're only stride-correct when
    // copied into a buffer of those exact dimensions. Packing into a
    // target-sized buffer (as the earlier code did) would copy capture-stride
    // rows into a different-stride buffer — silently truncating/reinterpreting
    // when src ≠ out, and the subsequent `.scale()` would then scale garbage
    // that already claims the target size. The preview path does the same
    // (allocate at source, then scale) for the same reason.
    let slot_idx = ring.next_slot((src_w, src_h));
    {
        let slot = &mut ring.slots[slot_idx];
        let (plane_y, plane_u, plane_v) = slot.buffer.data_mut();
        let n = plane_y.len().min(y_len);
        plane_y[..n].copy_from_slice(&ring.scratch[..n]);
        let n = plane_u.len().min(u_len);
        plane_u[..n].copy_from_slice(&ring.scratch[y_len..y_len + n]);
        let n = plane_v.len().min(v_len);
        plane_v[..n].copy_from_slice(&ring.scratch[y_len + u_len..y_len + u_len + n]);
    }
    if (src_w, src_h) != (out_w, out_h) {
        ring.slots[slot_idx].buffer = scale_buffer(
            std::mem::replace(
                &mut ring.slots[slot_idx].buffer,
                I420Buffer::new(src_w, src_h),
            ),
            out_w,
            out_h,
        )
        .ok()?;
    }
    // Stamp the keepalive with the *source frame's* capture timestamp, not
    // wall-clock now: the real path (`convert_frame`) uses capture time, and
    // `TimestampAligner::TranslateTimestamp` derives RTP timestamps from
    // `timestamp_us`. Mixing domains (capture time for real frames, delivery
    // time for keepalives) makes the aligner's offset estimate jump and the
    // translated RTP timestamps jitter forward/backward — the receiver's
    // jitter buffer then alternates between two frame buffers (the observed
    // H.264 ping-pong). Re-sending the same content with its own capture
    // timestamp keeps the capturer clock stable; the aligner clips to
    // prev+1ms and the encoder compresses the duplicate to near-nothing.
    ring.slots[slot_idx].pts_us = pts_us;
    // Hand the packed slot out; the ring slot is re-packed on its next
    // rotation, so the submitted sample's buffer instance stays unique within
    // the retention window. The fresh buffer is at the *source* size — that's
    // what `next_slot` packs into on the next rotation.
    Some(std::mem::replace(
        &mut ring.slots[slot_idx],
        VideoSample {
            sequence: 0,
            width: src_w,
            height: src_h,
            pts_us: 0,
            buffer: I420Buffer::new(src_w, src_h),
        },
    ))
}

/// Linux twin of `keepalive_frame`: builds one keepalive `VideoSample` from
/// the newest Ring-A snapshot, honoring the current `SCALE_TARGET` at
/// delivery time. No rotation ring is needed — each submission packs the
/// snapshot planes into a fresh `OwnedI420` allocation that `GStreamer`
/// releases back to `I420_FREELIST` after the encoder is done with it, so
/// the encoder can never see the same buffer instance twice (`GStreamer` is
/// the release callback Windows lacks). The allocation doubles as the
/// snapshot scratch, so a static screen's steady-state keepalives reuse the
/// freelist allocation instead of allocating per tick.
///
/// Returns `None` when nothing has been captured yet (e.g. the portal picker
/// is still open, or the gap before the first frame).
#[cfg(target_os = "linux")]
fn keepalive_sample() -> Option<VideoSample> {
    // Scale to the configured encoder target, or fall back to the source
    // resolution when no track is live (the keepalive arm only runs with a
    // live video track, so `SCALE_TARGET` is normally set).
    let target = SCALE_TARGET.load_full().map(|t| (t.width, t.height));
    let (src_w, src_h, pts_us, mut buffer) = {
        let Ok(guard) = PREVIEW_I420.lock() else {
            return None;
        };
        let history = guard.as_ref()?;
        let entry = history.newest()?;
        // Copy the newest snapshot's planes into the fresh allocation under
        // the brief lock; the scale below runs outside it. The ring stores
        // the *capture* timestamp, so re-deliveries are stamped with the
        // source frame's clock (see the Windows twin's timestamp note).
        // The allocation is a freelist pop in steady state: `set_scale_target`
        // warmed the freelist with target-sized buffers, and each keepalive
        // the pipeline releases lands back in it at capture resolution.
        let mut fresh = OwnedI420::new(entry.width, entry.height);
        let (dst_y, dst_u, dst_v) = fresh.data_mut();
        let n = dst_y.len().min(entry.y_len);
        dst_y[..n].copy_from_slice(&entry.planes[..n]);
        let n = dst_u.len().min(entry.u_len);
        dst_u[..n].copy_from_slice(&entry.planes[entry.y_len..entry.y_len + n]);
        let n = dst_v.len().min(entry.v_len);
        dst_v[..n].copy_from_slice(
            &entry.planes[entry.y_len + entry.u_len..entry.y_len + entry.u_len + n],
        );
        (entry.width, entry.height, entry.pts_us, fresh)
    };
    let (out_w, out_h) = target.unwrap_or((src_w, src_h));

    if (src_w, src_h) != (out_w, out_h) {
        buffer = match buffer.scale(out_w, out_h) {
            Ok(scaled) => scaled,
            Err(error) => {
                log::warn!("[desktop-capture] keepalive scale failed: {error}");
                return None;
            }
        };
    }
    Some(VideoSample {
        sequence: NEXT_FRAME_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        width: out_w,
        height: out_h,
        pts_us,
        buffer,
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
    // pacing even when the source delivers in bursts.
    let pacer = Arc::new(FramePacer::new(PACER_CAPACITY));
    let on_frame = make_on_frame(Arc::clone(&pacer));

    #[cfg(target_os = "linux")]
    let mut desktop_poller: Option<thread::JoinHandle<()>> = None;
    #[cfg(target_os = "windows")]
    let mut wgc: Option<crate::wgc_capture::WgcCapture> = None;
    let mut generator = None;

    match source {
        #[cfg(target_os = "linux")]
        CaptureSource::Desktop => {
            // The Linux `PipeWire` engine's `capture_frame()` runs BGRA→I420
            // conversion + pacer push *synchronously* inside its poll. Running
            // that poll on the delivery thread serializes capture → convert →
            // scale → GStreamer push on one timing-critical thread, so a slow
            // encoder stalls the very poll that feeds it (and vice versa). The
            // portal engine owns its own poller thread here: capture + convert
            // run there, and the delivery loop below only pops the pacer and
            // publishes — real separation instead of the "source thread"
            // fiction. The engine's `poll()` self-paces, so the extra sleep
            // only bounds stop-flag latency.
            let poller_stop = Arc::clone(stop);
            let poller_ready = ready_tx.clone();
            let handle = thread::Builder::new()
                .name("linux-capture-poll".into())
                .spawn(
                    move || match crate::linux_capture::LinuxDesktopCapture::start(on_frame) {
                        Ok(mut engine) => {
                            CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
                            let _ = poller_ready.send(Ok(()));
                            while !poller_stop.load(Ordering::Relaxed) {
                                engine.poll();
                                thread::sleep(Duration::from_millis(1));
                            }
                        }
                        Err(e) => {
                            let _ = poller_ready.send(Err(e));
                        }
                    },
                );
            match handle {
                Ok(handle) => desktop_poller = Some(handle),
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("Thread spawn: {e}")));
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
    // queued frame and scales/copies it at its own cadence, so the
    // (expensive) scale/pack work can never delay a paced delivery
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
                while !preview_stop.load(Ordering::Relaxed) {
                    emit_preview_frame(
                        &mut preview_bufs,
                        &mut last_preview_generation,
                        &mut next_preview_at,
                    );
                    // Sleep until the next preview deadline (capped so the stop
                    // flag stays responsive).
                    let wake = next_preview_at.min(Instant::now() + Duration::from_millis(100));
                    thread::sleep(wake.saturating_duration_since(Instant::now()));
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
    // When the pacer is empty on a delivery tick (the portal capture is
    // event-driven — KWin records only on compositor render or cursor move),
    // the newest captured frame is re-submitted with its own capture
    // timestamp so the RTP cadence stays even instead of stalling; the
    // encoder compresses the duplicates to near-nothing. Mirrors the WGC
    // engine, which re-delivers the last frame on static content.
    //
    // The re-submission snapshot is the newest Ring-A capture-resolution
    // plane set, scaled to the current `SCALE_TARGET`. On Windows it is
    // packed into the next rotating Ring-B slot so the encoder never sees
    // the same buffer instance twice within its async retention window; on
    // Linux each submission is a fresh `OwnedI420` (freelist pop or new
    // allocation) that GStreamer releases, so no rotation is needed.
    #[cfg(not(target_os = "linux"))]
    let mut keepalive_ring = KeepaliveRing::new();
    // Rate-limits the slow-scale log below (at most one line per 3 s).
    // Initialized so the first slow scale always logs.
    let mut last_slow_scale = Instant::now()
        .checked_sub(Duration::from_secs(3))
        .unwrap_or(Instant::now());

    while !stop.load(Ordering::Relaxed) {
        // Delivery tick: one queued frame per encoder-target interval, so the
        // receiver's RTP cadence stays even. Both engines are polled on the
        // same tick — they self-pace inside, so extra polls are no-ops.
        let now = Instant::now();
        if now >= next_delivery_at {
            // Compute the interval up front: it belongs to *this* tick's
            // cadence, not one re-read after the serial capture→convert→
            // scale→push work below has already consumed an unknown slice of
            // the window.
            let fps = SCALE_TARGET
                .load_full()
                .map_or(PREVIEW_FALLBACK_FPS, |target| {
                    target.fps.clamp(1, PREVIEW_MAX_FPS)
                });
            let interval = Duration::from_micros(1_000_000 / u64::from(fps));
            #[cfg(target_os = "windows")]
            if let Some(engine) = wgc.as_mut() {
                engine.poll();
            }
            match pacer.pop() {
                Some(sample) => {
                    if crate::is_video_track_active() {
                        // Fast path: a real captured frame. Scale to the
                        // configured target, up or down (OBS-style canvas
                        // scaling): libwebrtc's encoder has no explicit size,
                        // so a 4K source would otherwise stream at 4K with
                        // the user's 1080p bitrate (see `ScaleTarget`).
                        match scale_to_target(
                            sample,
                            SCALE_TARGET.load_full().as_deref(),
                            &mut last_slow_scale,
                        ) {
                            Ok(sample) => {
                                if publish_video_frame(sample) {
                                    STATS_PUSHED.fetch_add(1, Ordering::Relaxed);
                                } else {
                                    STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            Err(error) => {
                                log::warn!("[desktop-capture] target scale failed: {error}");
                                STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    } else {
                        // No live track: drop the frame, but the Ring-A stash
                        // still holds the newest capture-resolution planes, so
                        // Go Live can immediately keepalive on it (the pacer
                        // is usually empty right after publish — KWin records
                        // only on damage).
                        STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
                    }
                }
                None => {
                    // Keepalive: an empty pacer usually means the compositor
                    // had nothing to record, not that delivery should stop —
                    // but a dropped corrupt buffer or the gap between picker
                    // answer and first frame can still leave a tick empty.
                    // Re-submit the newest captured frame (fresh planes +
                    // capture timestamp) so arrival pacing stays even and the
                    // encoder never sees the same buffer instance twice
                    // within its async retention window.
                    if crate::is_video_track_active() {
                        #[cfg(target_os = "linux")]
                        let frame = keepalive_sample();
                        #[cfg(not(target_os = "linux"))]
                        let frame = keepalive_frame(&mut keepalive_ring);
                        if let Some(frame) = frame {
                            STATS_KEEPALIVE_ATTEMPTED.fetch_add(1, Ordering::Relaxed);
                            if publish_video_frame(frame) {
                                STATS_KEEPALIVE_PUSHED.fetch_add(1, Ordering::Relaxed);
                            } else {
                                STATS_KEEPALIVE_DROPPED.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
            }
            // Advance a *fixed* deadline and skip any intervals already
            // missed. Pinning `now + interval` here would measure from after
            // the serial capture→convert→scale→push work, so a slow frame
            // (> interval) would schedule the next deadline in the past and
            // the loop would immediately deliver again — a catch-up/tight-loop
            // that degrades pacing instead of backing off. Advancing the fixed
            // deadline both keeps the cadence even in steady state and
            // explicitly skips (rather than re-triggers) a missed interval.
            next_delivery_at += interval;
            let after = Instant::now();
            if next_delivery_at <= after {
                next_delivery_at = after + interval;
            }
        }
        // Sleep until the next delivery deadline (capped so the portal
        // session check and the stop flag stay responsive).
        let wake = next_delivery_at.min(now + Duration::from_millis(100));
        thread::sleep(wake.saturating_duration_since(Instant::now()));
    }
    #[cfg(target_os = "linux")]
    if let Some(handle) = desktop_poller {
        let _ = handle.join();
    }
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
    let thread_stop = Arc::clone(&stop);
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
            // Release the state lock before reaping: `stop()` and
            // `disconnect_livekit_room` block on this lock, so a hung engine
            // would otherwise freeze them (and, via `LIVEKIT`, the audio
            // callback) permanently. The worker is detached rather than
            // joined for the same reason — a hung engine start must not block
            // the caller indefinitely.
            drop(guard);
            crate::reap_detached(handle, "desktop-capture-reaper");
            Err(reason)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            drop(guard);
            crate::reap_detached(handle, "desktop-capture-reaper");
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
/// `start_desktop_capture`.
pub(crate) fn stats() -> crate::DesktopCaptureStats {
    crate::DesktopCaptureStats {
        frames_dequeued: STATS_DEQUEUED.load(Ordering::Relaxed).cast_signed(),
        frames_pushed: STATS_PUSHED.load(Ordering::Relaxed).cast_signed(),
        frames_dropped: STATS_DROPPED.load(Ordering::Relaxed).cast_signed(),
        capture_errors: STATS_ERRORS.load(Ordering::Relaxed).cast_signed(),
        preview_frames_sent: STATS_PREVIEW_SENT.load(Ordering::Relaxed).cast_signed(),
        keepalive_attempted: STATS_KEEPALIVE_ATTEMPTED
            .load(Ordering::Relaxed)
            .cast_signed(),
        keepalive_pushed: STATS_KEEPALIVE_PUSHED.load(Ordering::Relaxed).cast_signed(),
        keepalive_dropped: STATS_KEEPALIVE_DROPPED
            .load(Ordering::Relaxed)
            .cast_signed(),
        last_width: i64::from(STATS_LAST_WIDTH.load(Ordering::Relaxed)),
        last_height: i64::from(STATS_LAST_HEIGHT.load(Ordering::Relaxed)),
        pacer_pushes: STATS_PACER_PUSHES.load(Ordering::Relaxed).cast_signed(),
        pacer_pops: STATS_PACER_POPS.load(Ordering::Relaxed).cast_signed(),
        pacer_drops: STATS_PACER_DROPS.load(Ordering::Relaxed).cast_signed(),
        pacer_depth: STATS_PACER_DEPTH.load(Ordering::Relaxed).cast_signed(),
        pacer_max_depth: STATS_PACER_MAX_DEPTH.load(Ordering::Relaxed).cast_signed(),
    }
}

#[cfg(test)]
mod probe {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    /// The `OwnedI420` layout derives from `gst_video::VideoInfo`, whose
    /// builder asserts `gst::init` has run. `gst::init` is idempotent, so a
    /// one-time helper is safe across tests. Linux-only: `gst` is not
    /// imported on other platforms.
    #[cfg(target_os = "linux")]
    fn init_gst() {
        static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        INIT.get_or_init(|| {
            if let Err(error) = gst::init() {
                panic!("gst::init failed: {error}");
            }
        });
    }

    /// Serializes tests that touch the process-global capture statics
    /// (`SCALE_TARGET`, `PREVIEW_I420`, `I420_FREELIST`): cargo runs tests in
    /// parallel threads, and two tests mutating the shared target/ring state
    /// interleaved corrupt each other's fixtures (a keepalive scaling to a
    /// neighboring test's target, a freelist assertion counting another
    /// test's warmed buffers). Call this guard at the top of every test that
    /// reads or writes those statics.
    #[cfg(target_os = "linux")]
    fn capture_statics_guard() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        GUARD
            .lock()
            .unwrap_or_else(|_| panic!("capture statics guard poisoned"))
    }

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

    /// The Linux encode-target scale path uses the `GStreamer` scaler on an
    /// owned `OwnedI420` allocation: a uniform-color 8×8 frame scaled 2:1
    /// must keep its dimensions and its dominant color (a broken scaler
    /// would either panic, resize wrong or shift the color).
    #[cfg(target_os = "linux")]
    #[test]
    fn owned_i420_scale_preserves_dimensions_and_mean_color() {
        init_gst();
        let mut src = OwnedI420::new(8, 8);
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
        let scaled = src.scale(4, 4).unwrap_or_else(|error| {
            panic!("OwnedI420::scale must not fail for valid dims: {error}")
        });
        assert_eq!((scaled.width, scaled.height), (4, 4));
        let (plane_y, plane_u, plane_v) = scaled.data();
        assert!(
            plane_y.iter().all(|&v| v == 100),
            "luma must survive the scale"
        );
        // GStreamer's default I420 layout aligns the chroma row stride, so
        // the plane slices include pad bytes after each row's real pixels:
        // assert on the actual 2×2 chroma region (stride-aware), not the
        // full slice.
        let (_, stride_u, stride_v) = scaled.strides();
        let real_u: Vec<u8> = plane_u
            .chunks_exact(stride_u as usize)
            .flat_map(|row| row[..scaled.width as usize / 2].iter().copied())
            .collect();
        let real_v: Vec<u8> = plane_v
            .chunks_exact(stride_v as usize)
            .flat_map(|row| row[..scaled.width as usize / 2].iter().copied())
            .collect();
        assert!(real_u.iter().all(|&v| v == 90));
        assert!(real_v.iter().all(|&v| v == 240));
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
        // Source smaller than the viewport: no downscale needed, so the
        // source resolution is returned unchanged (the renderer upscales).
        assert_eq!(fit_preview_size(1280, 720, 1920, 1080), Some((1280, 720)));
        assert_eq!(fit_preview_size(1280, 720, 4000, 3000), Some((1280, 720)));
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
        init_gst();
        let config = CaptureConfig {
            width: 1280,
            height: 720,
            fps: 30,
            video_codec: None,
            max_bitrate: None,
            auto_bitrate: false,
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

    /// Manual control probe: runs the whole Linux capture engine (libwebrtc
    /// `PipeWire` capturer → conversion → paced delivery) through the public
    /// capture API in a plain Rust process. Run with:
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
            eprintln!("[probe] WARNING: no frames pushed — portal capture needs a Wayland session");
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

    /// Ring A (history): the newest slot is always the last pushed, at the
    /// *capture* resolution, and drop-oldest keeps the ring bounded.
    #[test]
    fn history_ring_keeps_newest_at_capture_resolution() {
        let mut ring = HistoryRing::new(2);
        ring.push_planes(1920, 1080, 1, &[1u8; 2], &[1u8; 1], &[1u8; 1]);
        ring.push_planes(1280, 720, 2, &[2u8; 2], &[2u8; 1], &[2u8; 1]);
        // Overwrite the oldest (drop-oldest): the ring stays bounded at 2.
        ring.push_planes(2560, 1440, 3, &[3u8; 2], &[3u8; 1], &[3u8; 1]);

        let newest = ring
            .newest()
            .unwrap_or_else(|| panic!("newest must exist after a push"));
        assert_eq!(
            (newest.width, newest.height, newest.pts_us),
            (2560, 1440, 3)
        );
        assert_eq!(newest.planes, vec![3u8; 4]);
        assert_eq!(ring.slots.len(), 2, "ring must stay bounded at capacity");
    }

    /// Ring A: nothing to read before the first push.
    #[test]
    fn history_ring_is_empty_before_first_push() {
        let ring = HistoryRing::new(2);
        assert!(ring.newest().is_none());
    }

    /// Ring B (keepalive, Windows): consecutive submissions never reuse the
    /// same slot instance within `KEEPALIVE_RING_CAPACITY` ticks (each tick
    /// packs the slot's buffer in place, so a reused slot would mutate a
    /// buffer the encoder may still hold).
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn keepalive_ring_rotates_distinct_buffers() {
        let mut ring = KeepaliveRing::new();
        let mut seen = std::collections::HashSet::new();
        for i in 0..(KEEPALIVE_RING_CAPACITY + 2) {
            // The slot instance (address) is what gets packed and submitted;
            // it must not repeat within the retention window.
            let idx = ring.next_slot((1280, 720));
            let frame = &ring.slots[idx];
            let ptr = std::ptr::addr_of!(*frame) as usize;
            if i < KEEPALIVE_RING_CAPACITY {
                // The first full rotation: every slot instance is distinct.
                assert!(
                    !seen.contains(&ptr),
                    "slot instance must not be reused within {KEEPALIVE_RING_CAPACITY} ticks"
                );
            }
            seen.insert(ptr);
            assert_eq!(
                (frame.buffer.width(), frame.buffer.height()),
                (1280, 720),
                "slot must be created at the delivery target size"
            );
        }
    }

    /// `keepalive_frame` (Windows) honors a dynamic `SCALE_TARGET` change:
    /// Ring A holds capture resolution, the delivered frame is at the current
    /// target.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn keepalive_frame_scales_to_current_target() {
        set_preview_callback(Box::new(|_bytes, _pts_us| {}));
        set_scale_target(640, 360, 30);
        // Seed Ring A with a capture-resolution snapshot.
        {
            let mut ring = HistoryRing::new(2);
            let planes = vec![0u8; 1280 * 720 + 640 * 360 + 640 * 360];
            ring.push_planes(
                1280,
                720,
                42,
                &planes[..1280 * 720],
                &planes[1280 * 720..1280 * 720 + 640 * 360],
                &planes[1280 * 720 + 640 * 360..],
            );
            let Ok(mut slot) = PREVIEW_I420.lock() else {
                panic!("PREVIEW_I420 lock poisoned");
            };
            *slot = Some(ring);
        }
        let mut keepalive = KeepaliveRing::new();
        let frame = keepalive_frame(&mut keepalive)
            .unwrap_or_else(|| panic!("keepalive must produce a frame"));
        assert_eq!(
            (frame.buffer.width(), frame.buffer.height()),
            (640, 360),
            "keepalive must scale the capture-resolution snapshot to SCALE_TARGET"
        );
        assert_eq!(
            frame.pts_us, 42,
            "keepalive must carry the source frame's capture timestamp (not delivery time)"
        );
        clear_scale_target();
        clear_preview_callback();
        let Ok(mut slot) = PREVIEW_I420.lock() else {
            panic!("PREVIEW_I420 lock poisoned");
        };
        *slot = None;
    }

    /// Regression: the keepalive packs capture-resolution planes into a slot
    /// buffer allocated at the *source* dimensions, then scales. Earlier code
    /// allocated the slot at the *target* size and copied capture-stride rows
    /// into it — silently corrupting content whenever src ≠ out. A horizontal
    /// gradient (each row = its own unique value) makes any stride
    /// misalignment or truncation produce wrong pixels, not just a scale
    /// artifact.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn keepalive_packs_source_planes_then_scales() {
        set_preview_callback(Box::new(|_bytes, _pts_us| {}));
        set_scale_target(640, 360, 30);
        // Seed Ring A with a 1280x720 capture-resolution snapshot whose luma
        // rows each carry a distinct value: row y has byte value (y & 0xFF).
        {
            let mut ring = HistoryRing::new(2);
            let mut planes = vec![0u8; 1280 * 720 + 640 * 360 + 640 * 360];
            for y in 0..720u32 {
                let row = &mut planes[y as usize * 1280..(y as usize + 1) * 1280];
                row.fill((y & 0xFF) as u8);
            }
            // Chroma planes: flat, distinct from any luma row value.
            planes[1280 * 720..1280 * 720 + 640 * 360].fill(128);
            planes[1280 * 720 + 640 * 360..].fill(128);
            ring.push_planes(
                1280,
                720,
                7,
                &planes[..1280 * 720],
                &planes[1280 * 720..1280 * 720 + 640 * 360],
                &planes[1280 * 720 + 640 * 360..],
            );
            let Ok(mut slot) = PREVIEW_I420.lock() else {
                panic!("PREVIEW_I420 lock poisoned");
            };
            *slot = Some(ring);
        }
        let mut keepalive = KeepaliveRing::new();
        let frame = keepalive_frame(&mut keepalive)
            .unwrap_or_else(|| panic!("keepalive must produce a frame"));
        assert_eq!((frame.buffer.width(), frame.buffer.height()), (640, 360));
        // The output must be a genuine downscale of the gradient: the top
        // output row ≈ top source rows (0..2), the bottom output row ≈ the
        // bottom source row (row 719 = 0xCF = 207, sampled with edge clamp).
        // A stride bug (copying 1280-wide rows into a 640-wide buffer) would
        // put wrong row values in the output.
        let (plane_y, _, _) = frame.buffer.data();
        let top = plane_y[0];
        let bottom = plane_y[(360 - 1) * 640];
        assert!(
            top <= 3,
            "top output row must average the top source rows (got {top})"
        );
        assert!(
            (200..=214).contains(&bottom),
            "bottom output row must sample the bottom source row 719 = 0xCF (got {bottom})"
        );
        clear_scale_target();
        clear_preview_callback();
        let Ok(mut slot) = PREVIEW_I420.lock() else {
            panic!("PREVIEW_I420 lock poisoned");
        };
        *slot = None;
    }

    /// Linux twin of `keepalive_frame_scales_to_current_target`: Ring A holds
    /// capture resolution, the delivered sample is at the current target.
    #[cfg(target_os = "linux")]
    #[test]
    fn keepalive_sample_scales_to_current_target() {
        let _guard = capture_statics_guard();
        init_gst();
        set_preview_callback(Box::new(|_bytes, _pts_us| {}));
        set_scale_target(640, 360, 30);
        // Seed Ring A with a capture-resolution snapshot.
        {
            let mut ring = HistoryRing::new(2);
            let planes = vec![0u8; 1280 * 720 + 640 * 360 + 640 * 360];
            ring.push_planes(
                1280,
                720,
                42,
                &planes[..1280 * 720],
                &planes[1280 * 720..1280 * 720 + 640 * 360],
                &planes[1280 * 720 + 640 * 360..],
            );
            let Ok(mut slot) = PREVIEW_I420.lock() else {
                panic!("PREVIEW_I420 lock poisoned");
            };
            *slot = Some(ring);
        }
        let frame = keepalive_sample().unwrap_or_else(|| panic!("keepalive must produce a frame"));
        assert_eq!(
            (frame.width, frame.height),
            (640, 360),
            "keepalive must scale the capture-resolution snapshot to SCALE_TARGET"
        );
        assert_eq!(
            (frame.buffer.width, frame.buffer.height),
            (640, 360),
            "the sample's plane buffer must match the scaled dimensions"
        );
        assert_eq!(
            frame.pts_us, 42,
            "keepalive must carry the source frame's capture timestamp (not delivery time)"
        );
        clear_scale_target();
        clear_preview_callback();
        let Ok(mut slot) = PREVIEW_I420.lock() else {
            panic!("PREVIEW_I420 lock poisoned");
        };
        *slot = None;
    }

    /// Linux twin of `keepalive_packs_source_planes_then_scales`: the
    /// keepalive packs capture-resolution planes into a fresh allocation at
    /// the *source* dimensions, then scales (see the Windows twin's
    /// regression note).
    #[cfg(target_os = "linux")]
    #[test]
    fn keepalive_sample_packs_source_planes_then_scales() {
        let _guard = capture_statics_guard();
        init_gst();
        set_preview_callback(Box::new(|_bytes, _pts_us| {}));
        set_scale_target(640, 360, 30);
        // Seed Ring A with a 1280x720 capture-resolution snapshot whose luma
        // rows each carry a distinct value: row y has byte value (y & 0xFF).
        {
            let mut ring = HistoryRing::new(2);
            let mut planes = vec![0u8; 1280 * 720 + 640 * 360 + 640 * 360];
            for y in 0..720u32 {
                let row = &mut planes[y as usize * 1280..(y as usize + 1) * 1280];
                row.fill((y & 0xFF) as u8);
            }
            // Chroma planes: flat, distinct from any luma row value.
            planes[1280 * 720..1280 * 720 + 640 * 360].fill(128);
            planes[1280 * 720 + 640 * 360..].fill(128);
            ring.push_planes(
                1280,
                720,
                7,
                &planes[..1280 * 720],
                &planes[1280 * 720..1280 * 720 + 640 * 360],
                &planes[1280 * 720 + 640 * 360..],
            );
            let Ok(mut slot) = PREVIEW_I420.lock() else {
                panic!("PREVIEW_I420 lock poisoned");
            };
            *slot = Some(ring);
        }
        let frame = keepalive_sample().unwrap_or_else(|| panic!("keepalive must produce a frame"));
        assert_eq!((frame.width, frame.height), (640, 360));
        // The output must be a genuine downscale of the gradient: the top
        // output row ≈ top source rows (0..2), the bottom output row ≈ the
        // bottom source row (row 719 = 0xCF = 207, sampled with edge clamp).
        // A stride bug (copying 1280-wide rows into a 640-wide buffer) would
        // put wrong row values in the output.
        let (plane_y, _, _) = frame.buffer.data();
        let top = plane_y[0];
        let bottom = plane_y[(360 - 1) * 640];
        assert!(
            top <= 3,
            "top output row must average the top source rows (got {top})"
        );
        assert!(
            (200..=214).contains(&bottom),
            "bottom output row must sample the bottom source row 719 = 0xCF (got {bottom})"
        );
        clear_scale_target();
        clear_preview_callback();
        let Ok(mut slot) = PREVIEW_I420.lock() else {
            panic!("PREVIEW_I420 lock poisoned");
        };
        *slot = None;
    }

    /// `set_scale_target` warms the freelist with two target-sized buffers,
    /// so the first keepalive after going live never allocates on the
    /// delivery thread (the stillness-transition hitch). The warmed buffers
    /// must already carry the target-sized plane layout.
    #[cfg(target_os = "linux")]
    #[test]
    fn scale_target_warmup_primes_keepalive_freelist() {
        let _guard = capture_statics_guard();
        init_gst();
        clear_i420_freelist();
        set_scale_target(1920, 1080, 60);

        let freelist_len = I420_FREELIST.lock().map_or_else(
            |_| panic!("I420 freelist lock poisoned"),
            |freelist| freelist.len(),
        );
        assert!(
            freelist_len >= 2,
            "set_scale_target must warm at least two keepalive buffers (got {freelist_len})"
        );
        {
            let freelist = I420_FREELIST
                .lock()
                .unwrap_or_else(|_| panic!("I420 freelist lock poisoned"));
            let layout = OwnedI420::layout(1920, 1080).unwrap_or_else(|error| {
                panic!("target layout cannot fail: {error}");
            });
            for planes in freelist.iter() {
                assert_eq!(
                    planes.len(),
                    layout.size,
                    "warmed buffer must be target-sized"
                );
            }
        }

        clear_scale_target();
        clear_i420_freelist();
    }

    /// Warm-up on a zero-sized target is a no-op (the keepalive arm falls
    /// back to the source resolution in that state, so there is nothing
    /// sensible to pre-allocate).
    #[cfg(target_os = "linux")]
    #[test]
    fn scale_target_warmup_ignores_zero_dimensions() {
        let _guard = capture_statics_guard();
        init_gst();
        clear_i420_freelist();
        set_scale_target(0, 0, 60);

        let freelist_len = I420_FREELIST.lock().map_or_else(
            |_| panic!("I420 freelist lock poisoned"),
            |freelist| freelist.len(),
        );
        assert_eq!(freelist_len, 0);

        clear_scale_target();
        clear_i420_freelist();
    }
}
