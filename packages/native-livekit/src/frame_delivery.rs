use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use gstreamer as gst;
#[cfg(target_os = "linux")]
use gstreamer_video as gst_video;
use livekit::webrtc::native::yuv_helper;
#[cfg(target_os = "linux")]
use livekit::webrtc::prelude::{I420Buffer, VideoBuffer};
#[cfg(not(target_os = "linux"))]
use livekit::webrtc::prelude::{I420Buffer, VideoBuffer, VideoFrame, VideoRotation};

static FRAME_TRACE_ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("SLOPCAST_FRAME_TRACE").is_some());

#[cfg(target_os = "linux")]
const I420_FREELIST_CAP: usize = 24;

#[cfg(target_os = "linux")]
static I420_FREELIST: LazyLock<Mutex<Vec<Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(Vec::with_capacity(I420_FREELIST_CAP)));

#[cfg(target_os = "linux")]
pub(crate) fn clear_i420_freelist() {
    if let Ok(mut freelist) = I420_FREELIST.lock() {
        freelist.clear();
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
struct I420Layout {
    offsets: [usize; 3],
    strides: [i32; 3],
    size: usize,
}

#[cfg(target_os = "linux")]
pub(crate) struct OwnedI420 {
    width: u32,
    height: u32,
    layout: I420Layout,
    planes: Vec<u8>,
}

#[cfg(target_os = "linux")]
impl OwnedI420 {
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

    fn scale(self, width: u32, height: u32) -> Result<Self, String> {
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
        #[allow(
            clippy::type_complexity,
            reason = "single cache entry keyed on four small dimensions; extracted type would obscure the tuple shape"
        )]
        static CACHE: std::sync::Mutex<
            Option<(u32, u32, u32, u32, Arc<gst_video::VideoConverter>)>,
        > = std::sync::Mutex::new(None);
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
    fn into_planes(mut self) -> Vec<u8> {
        std::mem::take(&mut self.planes)
    }
}

#[cfg(target_os = "linux")]
impl Drop for OwnedI420 {
    fn drop(&mut self) {
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

pub(crate) struct VideoSample {
    pub(crate) sequence: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pts_us: i64,
    pub(crate) buffer: SampleBuffer,
}

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

#[cfg(target_os = "linux")]
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

pub(crate) type PreviewOutput = Arc<dyn Fn(Vec<u8>, i64) + Send + Sync>;

const PREVIEW_FALLBACK_FPS: u32 = 30;
const PREVIEW_MAX_FPS: u32 = 60;
const PACER_CAPACITY: usize = 4;
const HISTORY_CAPACITY: usize = 2;
#[cfg(not(target_os = "linux"))]
const PUBLICATION_RETENTION_CAPACITY: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeliveryTarget {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) fps: u32,
}

#[derive(Clone)]
enum PublicationOutput {
    #[cfg(target_os = "linux")]
    Gstreamer(crate::gstreamer_publisher::VideoOutput),
    #[cfg(not(target_os = "linux"))]
    WebRtc(Arc<crate::NativeVideoSource>),
    #[cfg(test)]
    Test(Arc<dyn Fn(VideoSample) -> bool + Send + Sync>),
}

struct QueuedSample {
    sample: VideoSample,
    output: Option<PublicationOutput>,
}

#[derive(Default)]
struct PublicationState {
    #[cfg(not(target_os = "linux"))]
    retained: VecDeque<VideoFrame<I420Buffer>>,
}

#[cfg(not(target_os = "linux"))]
impl PublicationState {
    fn retain(&mut self, frame: VideoFrame<I420Buffer>) {
        self.retained.push_back(frame);
        if self.retained.len() > PUBLICATION_RETENTION_CAPACITY {
            self.retained.pop_front();
        }
    }

    fn take_reusable_buffer(&mut self, width: u32, height: u32) -> Option<I420Buffer> {
        if self.retained.len() < PUBLICATION_RETENTION_CAPACITY {
            return None;
        }
        let oldest = self.retained.front()?;
        if (oldest.buffer.width(), oldest.buffer.height()) != (width, height) {
            return None;
        }

        self.retained.pop_front().map(|frame| frame.buffer)
    }
}

impl PublicationOutput {
    fn is_same_publication(&self, other: &Self) -> bool {
        match (self, other) {
            #[cfg(target_os = "linux")]
            (Self::Gstreamer(left), Self::Gstreamer(right)) => left.is_same_publication(right),
            #[cfg(not(target_os = "linux"))]
            (Self::WebRtc(left), Self::WebRtc(right)) => Arc::ptr_eq(left, right),
            #[cfg(test)]
            (Self::Test(left), Self::Test(right)) => Arc::ptr_eq(left, right),
            #[allow(
                unreachable_patterns,
                reason = "test builds add a second output variant"
            )]
            _ => false,
        }
    }

    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            clippy::unnecessary_wraps,
            reason = "the infallible WebRTC adapter shares the fallible Linux publication interface"
        )
    )]
    fn publish(
        &self,
        sample: VideoSample,
        publication: &mut PublicationState,
    ) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        let _ = publication;

        match self {
            #[cfg(target_os = "linux")]
            Self::Gstreamer(output) => output.push(sample),
            #[cfg(not(target_os = "linux"))]
            Self::WebRtc(source) => {
                let frame = VideoFrame {
                    rotation: VideoRotation::VideoRotation0,
                    timestamp_us: sample.pts_us,
                    frame_metadata: None,
                    buffer: sample.buffer,
                };
                source.capture_frame(&frame);
                publication.retain(frame);
                Ok(())
            }
            #[cfg(test)]
            Self::Test(output) => {
                if output(sample) {
                    Ok(())
                } else {
                    Err("test publication rejected frame".into())
                }
            }
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct DeliveryBinding {
    target: Option<DeliveryTarget>,
    output: Option<PublicationOutput>,
}

impl DeliveryBinding {
    #[must_use]
    pub(crate) fn dormant() -> Self {
        Self::default()
    }

    pub(crate) fn live(target: DeliveryTarget) -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        let output = PublicationOutput::Gstreamer(crate::gstreamer_publisher::video_output()?);
        #[cfg(not(target_os = "linux"))]
        let output = PublicationOutput::WebRtc(
            crate::VIDEO_SOURCE
                .load_full()
                .ok_or_else(|| "WebRTC video publication is not active".to_string())?,
        );

        Ok(Self {
            target: Some(target),
            output: Some(output),
        })
    }

    #[must_use]
    pub(crate) fn target(&self) -> Option<DeliveryTarget> {
        self.target
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceIssue {
    DroppedFrame,
    CaptureError,
}

#[derive(Clone, Copy)]
pub(crate) struct CapturedFrame<'a> {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bgra: &'a [u8],
    pub(crate) pts_us: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubmitOutcome {
    Accepted,
    Rejected,
}

#[derive(Debug)]
pub(crate) enum DeliveryError {
    Spawn(std::io::Error),
    Panicked(&'static str),
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "Frame delivery thread spawn failed: {error}"),
            Self::Panicked(worker) => write!(formatter, "Frame delivery {worker} worker panicked"),
        }
    }
}

trait DeliveryClock: Send + Sync {
    fn now(&self) -> Duration;
    fn wait_until(&self, deadline: Duration, stop: &AtomicBool);
    fn wake(&self);
}

struct SystemClock(Instant);

impl SystemClock {
    fn new() -> Self {
        Self(Instant::now())
    }
}

impl DeliveryClock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }

    fn wait_until(&self, deadline: Duration, stop: &AtomicBool) {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(
            deadline
                .saturating_sub(self.now())
                .min(Duration::from_millis(100)),
        );
    }

    fn wake(&self) {}
}

#[derive(Default)]
struct Stats {
    dequeued: AtomicU64,
    pushed: AtomicU64,
    dropped: AtomicU64,
    errors: AtomicU64,
    preview: AtomicU64,
    keepalive_attempted: AtomicU64,
    keepalive_pushed: AtomicU64,
    keepalive_dropped: AtomicU64,
    width: AtomicU32,
    height: AtomicU32,
    pacer_pushes: AtomicU64,
    pacer_pops: AtomicU64,
    pacer_drops: AtomicU64,
    pacer_depth: AtomicU64,
    pacer_max_depth: AtomicU64,
}

impl Stats {
    fn snapshot(&self) -> crate::DesktopCaptureStats {
        crate::DesktopCaptureStats {
            frames_dequeued: self.dequeued.load(Ordering::Relaxed).cast_signed(),
            frames_pushed: self.pushed.load(Ordering::Relaxed).cast_signed(),
            frames_dropped: self.dropped.load(Ordering::Relaxed).cast_signed(),
            capture_errors: self.errors.load(Ordering::Relaxed).cast_signed(),
            preview_frames_sent: self.preview.load(Ordering::Relaxed).cast_signed(),
            keepalive_attempted: self
                .keepalive_attempted
                .load(Ordering::Relaxed)
                .cast_signed(),
            keepalive_pushed: self.keepalive_pushed.load(Ordering::Relaxed).cast_signed(),
            keepalive_dropped: self.keepalive_dropped.load(Ordering::Relaxed).cast_signed(),
            last_width: i64::from(self.width.load(Ordering::Relaxed)),
            last_height: i64::from(self.height.load(Ordering::Relaxed)),
            pacer_pushes: self.pacer_pushes.load(Ordering::Relaxed).cast_signed(),
            pacer_pops: self.pacer_pops.load(Ordering::Relaxed).cast_signed(),
            pacer_drops: self.pacer_drops.load(Ordering::Relaxed).cast_signed(),
            pacer_depth: self.pacer_depth.load(Ordering::Relaxed).cast_signed(),
            pacer_max_depth: self.pacer_max_depth.load(Ordering::Relaxed).cast_signed(),
            cursor_frames: 0,
            cursor_missing: false,
        }
    }
}

struct HistoryFrame {
    width: u32,
    height: u32,
    pts_us: i64,
    planes: Vec<u8>,
}

struct History {
    frames: Vec<HistoryFrame>,
    next: usize,
    count: u64,
}

impl History {
    fn new() -> Self {
        Self {
            frames: (0..HISTORY_CAPACITY)
                .map(|_| HistoryFrame {
                    width: 0,
                    height: 0,
                    pts_us: 0,
                    planes: Vec::new(),
                })
                .collect(),
            next: 0,
            count: 0,
        }
    }

    fn push(&mut self, sample: &VideoSample) {
        let strides = sample.buffer.strides();
        let (y, u, v) = sample.buffer.data();
        let frame = &mut self.frames[self.next];
        frame.width = sample.width;
        frame.height = sample.height;
        frame.pts_us = sample.pts_us;
        frame.planes.clear();
        append_active_rows(&mut frame.planes, y, strides.0, sample.width, sample.height);
        let chroma_width = sample.width.div_ceil(2);
        let chroma_height = sample.height.div_ceil(2);
        append_active_rows(&mut frame.planes, u, strides.1, chroma_width, chroma_height);
        append_active_rows(&mut frame.planes, v, strides.2, chroma_width, chroma_height);
        self.next = (self.next + 1) % HISTORY_CAPACITY;
        self.count += 1;
    }

    fn newest(&self) -> Option<&HistoryFrame> {
        if self.count == 0 {
            return None;
        }
        Some(&self.frames[(self.next + HISTORY_CAPACITY - 1) % HISTORY_CAPACITY])
    }
}

fn packed_plane_lengths(width: u32, height: u32) -> (usize, usize) {
    let y_len = width as usize * height as usize;
    let chroma_len = width.div_ceil(2) as usize * height.div_ceil(2) as usize;
    (y_len, chroma_len)
}

fn append_active_rows(
    packed: &mut Vec<u8>,
    source: &[u8],
    source_stride: u32,
    row_width: u32,
    rows: u32,
) {
    let source_stride = source_stride as usize;
    let row_width = row_width as usize;
    for row in source.chunks(source_stride).take(rows as usize) {
        if let Some(active) = row.get(..row_width) {
            packed.extend_from_slice(active);
        }
    }
}

struct State {
    accepting: AtomicBool,
    stop: AtomicBool,
    ingress_gate: RwLock<()>,
    binding: Mutex<DeliveryBinding>,
    preview_output: Mutex<Option<PreviewOutput>>,
    viewport: Mutex<Option<(u32, u32)>>,
    history: Mutex<History>,
    generation: AtomicU64,
    sequence: AtomicU64,
    pacer: Mutex<VecDeque<QueuedSample>>,
    stats: Stats,
    last_warning_at: Mutex<Option<Duration>>,
    clock: Arc<dyn DeliveryClock>,
}

impl State {
    fn binding(&self) -> DeliveryBinding {
        self.binding
            .lock()
            .map_or_else(|_| DeliveryBinding::dormant(), |value| value.clone())
    }

    fn push(&self, sample: VideoSample, output: Option<PublicationOutput>) {
        let Ok(mut queue) = self.pacer.lock() else {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if queue.len() >= PACER_CAPACITY && queue.pop_front().is_some() {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            self.stats.pacer_drops.fetch_add(1, Ordering::Relaxed);
        }
        let sequence = sample.sequence;
        let pts_us = sample.pts_us;
        queue.push_back(QueuedSample { sample, output });
        let depth = queue.len() as u64;
        self.stats.pacer_pushes.fetch_add(1, Ordering::Relaxed);
        self.stats.pacer_depth.store(depth, Ordering::Relaxed);
        self.stats
            .pacer_max_depth
            .fetch_max(depth, Ordering::Relaxed);
        trace_frame("pacer-push", sequence, pts_us, queue.len(), None, None);
    }

    fn pop(&self) -> Option<QueuedSample> {
        let mut queue = self.pacer.lock().ok()?;
        let queued = queue.pop_front()?;
        self.stats.pacer_pops.fetch_add(1, Ordering::Relaxed);
        self.stats
            .pacer_depth
            .store(queue.len() as u64, Ordering::Relaxed);
        trace_frame(
            "pacer-pop",
            queued.sample.sequence,
            queued.sample.pts_us,
            queue.len(),
            None,
            None,
        );
        Some(queued)
    }

    fn report_delivery_error(&self, operation: &str, error: &str) {
        let now = self.clock.now();
        let Ok(mut last_warning_at) = self.last_warning_at.lock() else {
            return;
        };
        if last_warning_at.is_some_and(|last| now.saturating_sub(last) < Duration::from_secs(5)) {
            return;
        }

        *last_warning_at = Some(now);
        log::warn!("[frame-delivery] {operation}: {error}");
    }
}

pub(crate) struct FrameDelivery {
    state: Arc<State>,
    delivery_join: Option<thread::JoinHandle<()>>,
    preview_join: Option<thread::JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) struct FrameIngress {
    state: Arc<State>,
}

impl FrameDelivery {
    pub(crate) fn start(
        binding: DeliveryBinding,
        preview_output: Option<PreviewOutput>,
        viewport: Option<(u32, u32)>,
    ) -> Result<(Self, FrameIngress), DeliveryError> {
        Self::start_with_clock(
            binding,
            preview_output,
            viewport,
            Arc::new(SystemClock::new()),
        )
    }

    fn start_with_clock(
        binding: DeliveryBinding,
        preview_output: Option<PreviewOutput>,
        viewport: Option<(u32, u32)>,
        clock: Arc<dyn DeliveryClock>,
    ) -> Result<(Self, FrameIngress), DeliveryError> {
        #[cfg(target_os = "linux")]
        if let Some(target) = binding.target() {
            warm_freelist(target.width, target.height);
        }
        let state = Arc::new(State {
            accepting: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            ingress_gate: RwLock::new(()),
            binding: Mutex::new(binding),
            preview_output: Mutex::new(preview_output),
            viewport: Mutex::new(viewport),
            history: Mutex::new(History::new()),
            generation: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            pacer: Mutex::new(VecDeque::with_capacity(PACER_CAPACITY)),
            stats: Stats::default(),
            last_warning_at: Mutex::new(None),
            clock,
        });
        let delivery_state = Arc::clone(&state);
        let delivery_join = thread::Builder::new()
            .name("frame-delivery".into())
            .spawn(move || run_delivery(&delivery_state))
            .map_err(DeliveryError::Spawn)?;
        let preview_state = Arc::clone(&state);
        let preview_join = match thread::Builder::new()
            .name("preview-emitter".into())
            .spawn(move || run_preview(&preview_state))
        {
            Ok(join) => join,
            Err(error) => {
                state.accepting.store(false, Ordering::SeqCst);
                state.stop.store(true, Ordering::SeqCst);
                state.clock.wake();
                let _ = delivery_join.join();
                return Err(DeliveryError::Spawn(error));
            }
        };
        let ingress = FrameIngress {
            state: Arc::clone(&state),
        };
        Ok((
            Self {
                state,
                delivery_join: Some(delivery_join),
                preview_join: Some(preview_join),
            },
            ingress,
        ))
    }

    pub(crate) fn set_binding(&self, binding: DeliveryBinding) {
        #[cfg(target_os = "linux")]
        if let Some(target) = binding.target() {
            warm_freelist(target.width, target.height);
        }
        if let Ok(mut current) = self.state.binding.lock() {
            *current = binding;
        }
        self.state.clock.wake();
    }

    pub(crate) fn set_preview_output(&self, output: Option<PreviewOutput>) {
        if let Ok(mut current) = self.state.preview_output.lock() {
            *current = output;
        }
        self.state.clock.wake();
    }

    pub(crate) fn set_viewport(&self, viewport: Option<(u32, u32)>) {
        if let Ok(mut current) = self.state.viewport.lock() {
            *current = viewport;
        }
        self.state.clock.wake();
    }

    #[must_use]
    pub(crate) fn stats(&self) -> crate::DesktopCaptureStats {
        self.state.stats.snapshot()
    }

    pub(crate) fn stop(mut self) -> Result<crate::DesktopCaptureStats, DeliveryError> {
        self.state.accepting.store(false, Ordering::SeqCst);
        let gate = self.state.ingress_gate.write();
        drop(gate);
        self.state.stop.store(true, Ordering::SeqCst);
        self.state.clock.wake();
        let delivery = self.delivery_join.take().map(thread::JoinHandle::join);
        let preview = self.preview_join.take().map(thread::JoinHandle::join);
        if delivery.is_some_and(|result| result.is_err()) {
            return Err(DeliveryError::Panicked("publication"));
        }
        if preview.is_some_and(|result| result.is_err()) {
            return Err(DeliveryError::Panicked("preview"));
        }
        if let Ok(mut queue) = self.state.pacer.lock() {
            queue.clear();
            self.state.stats.pacer_depth.store(0, Ordering::Relaxed);
        }
        if let Ok(mut history) = self.state.history.lock() {
            *history = History::new();
        }
        Ok(self.state.stats.snapshot())
    }
}

impl FrameIngress {
    pub(crate) fn submit(&self, frame: CapturedFrame<'_>) -> SubmitOutcome {
        if !self.state.accepting.load(Ordering::SeqCst) {
            return SubmitOutcome::Rejected;
        }
        let Ok(_gate) = self.state.ingress_gate.read() else {
            return SubmitOutcome::Rejected;
        };
        if !self.state.accepting.load(Ordering::SeqCst) {
            return SubmitOutcome::Rejected;
        }
        self.state.stats.dequeued.fetch_add(1, Ordering::Relaxed);
        let required = usize::try_from(frame.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .and_then(|row| {
                usize::try_from(frame.height)
                    .ok()
                    .and_then(|height| row.checked_mul(height))
            });
        let Some(required) = required else {
            self.state.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return SubmitOutcome::Accepted;
        };
        if frame.width == 0 || frame.height == 0 || frame.bgra.len() < required {
            self.state.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return SubmitOutcome::Accepted;
        }
        let sample = convert_frame(
            &self.state,
            frame.width,
            frame.height,
            &frame.bgra[..required],
            frame.pts_us,
        );
        let binding = self.state.binding();
        let should_stash = binding.target().is_some()
            || self
                .state
                .preview_output
                .lock()
                .is_ok_and(|output| output.is_some());
        if should_stash && let Ok(mut history) = self.state.history.lock() {
            history.push(&sample);
            self.state.generation.fetch_add(1, Ordering::Relaxed);
        }
        self.state.push(sample, binding.output);
        self.state.clock.wake();
        SubmitOutcome::Accepted
    }

    pub(crate) fn record_issue(&self, issue: SourceIssue) {
        if !self.state.accepting.load(Ordering::SeqCst) {
            return;
        }
        match issue {
            SourceIssue::DroppedFrame => {
                self.state.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
            SourceIssue::CaptureError => {
                self.state.stats.errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn convert_frame(state: &State, width: u32, height: u32, bgra: &[u8], pts_us: i64) -> VideoSample {
    let mut output = OwnedI420::new(width, height);
    let (stride_y, stride_u, stride_v) = output.strides();
    let (y, u, v) = output.data_mut();
    yuv_helper::argb_to_i420(
        bgra,
        width * 4,
        y,
        stride_y,
        u,
        stride_u,
        v,
        stride_v,
        i32::try_from(width).unwrap_or(0),
        i32::try_from(height).unwrap_or(0),
    );
    state.stats.width.store(width, Ordering::Relaxed);
    state.stats.height.store(height, Ordering::Relaxed);
    VideoSample {
        sequence: state.sequence.fetch_add(1, Ordering::Relaxed),
        width,
        height,
        pts_us,
        buffer: output,
    }
}

#[cfg(not(target_os = "linux"))]
fn convert_frame(state: &State, width: u32, height: u32, bgra: &[u8], pts_us: i64) -> VideoSample {
    let mut output = I420Buffer::new(width, height);
    let (stride_y, stride_u, stride_v) = output.strides();
    let (y, u, v) = output.data_mut();
    yuv_helper::argb_to_i420(
        bgra,
        width * 4,
        y,
        stride_y,
        u,
        stride_u,
        v,
        stride_v,
        i32::try_from(width).unwrap_or(0),
        i32::try_from(height).unwrap_or(0),
    );
    state.stats.width.store(width, Ordering::Relaxed);
    state.stats.height.store(height, Ordering::Relaxed);
    VideoSample {
        sequence: state.sequence.fetch_add(1, Ordering::Relaxed),
        width,
        height,
        pts_us,
        buffer: output,
    }
}

fn scale_sample(mut sample: VideoSample, target: DeliveryTarget) -> Result<VideoSample, String> {
    if sample.width == target.width && sample.height == target.height {
        return Ok(sample);
    }
    sample.buffer = scale_buffer(sample.buffer, target.width, target.height)?;
    sample.width = target.width;
    sample.height = target.height;
    Ok(sample)
}

#[cfg(target_os = "linux")]
fn scale_buffer(buffer: OwnedI420, width: u32, height: u32) -> Result<OwnedI420, String> {
    buffer.scale(width, height)
}

#[cfg(not(target_os = "linux"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "I420Buffer scaling cannot fail; Result keeps the platform paths uniform"
)]
fn scale_buffer(mut buffer: I420Buffer, width: u32, height: u32) -> Result<I420Buffer, String> {
    Ok(buffer.scale(
        i32::try_from(width).unwrap_or(0),
        i32::try_from(height).unwrap_or(0),
    ))
}

fn run_delivery(state: &State) {
    let mut publication = PublicationState::default();
    let mut next = state.clock.now();
    while !state.stop.load(Ordering::Relaxed) {
        let now = state.clock.now();
        if now >= next {
            let binding = state.binding();
            let fps = binding.target().map_or(PREVIEW_FALLBACK_FPS, |target| {
                target.fps.clamp(1, PREVIEW_MAX_FPS)
            });
            let interval = frame_interval(fps);
            match state.pop() {
                Some(sample) => deliver_sample(state, &binding, &mut publication, sample),
                None => deliver_keepalive(state, &binding, &mut publication),
            }
            next += interval;
            let after = state.clock.now();
            if next <= after {
                next = after + interval;
            }
        }
        state.clock.wait_until(next, &state.stop);
    }
}

fn deliver_sample(
    state: &State,
    binding: &DeliveryBinding,
    publication: &mut PublicationState,
    queued: QueuedSample,
) {
    let (Some(target), Some(current_output)) = (binding.target(), binding.output.as_ref()) else {
        state.stats.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let output = match queued.output.as_ref() {
        Some(output) if output.is_same_publication(current_output) => output,
        Some(_) => {
            state.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        None => current_output,
    };
    let sample = match scale_sample(queued.sample, target) {
        Ok(sample) => sample,
        Err(error) => {
            state.stats.dropped.fetch_add(1, Ordering::Relaxed);
            state.report_delivery_error("scale frame", &error);
            return;
        }
    };
    match output.publish(sample, publication) {
        Ok(()) => {
            state.stats.pushed.fetch_add(1, Ordering::Relaxed);
        }
        Err(error) => {
            state.stats.dropped.fetch_add(1, Ordering::Relaxed);
            state.report_delivery_error("publish frame", &error);
        }
    }
}

#[cfg(target_os = "linux")]
fn deliver_keepalive(state: &State, binding: &DeliveryBinding, publication: &mut PublicationState) {
    let (Some(target), Some(output)) = (binding.target(), binding.output.as_ref()) else {
        return;
    };
    let Some(sample) = keepalive_sample(state, target) else {
        return;
    };
    state
        .stats
        .keepalive_attempted
        .fetch_add(1, Ordering::Relaxed);
    match output.publish(sample, publication) {
        Ok(()) => {
            state.stats.keepalive_pushed.fetch_add(1, Ordering::Relaxed);
        }
        Err(error) => {
            state
                .stats
                .keepalive_dropped
                .fetch_add(1, Ordering::Relaxed);
            state.report_delivery_error("publish keepalive", &error);
        }
    }
}

fn copy_history(
    frame: &HistoryFrame,
    strides: (u32, u32, u32),
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
) {
    let (y_len, chroma_len) = packed_plane_lengths(frame.width, frame.height);
    let Some((packed_y, chroma)) = frame.planes.split_at_checked(y_len) else {
        return;
    };
    let Some((packed_u, packed_v)) = chroma.split_at_checked(chroma_len) else {
        return;
    };
    let chroma_width = frame.width.div_ceil(2);
    let chroma_height = frame.height.div_ceil(2);
    copy_active_rows(packed_y, frame.width, y, strides.0, frame.height);
    copy_active_rows(packed_u, chroma_width, u, strides.1, chroma_height);
    copy_active_rows(packed_v, chroma_width, v, strides.2, chroma_height);
}

fn copy_active_rows(
    packed: &[u8],
    row_width: u32,
    destination: &mut [u8],
    destination_stride: u32,
    rows: u32,
) {
    let row_width = row_width as usize;
    let destination_stride = destination_stride as usize;
    for (source_row, destination_row) in packed
        .chunks(row_width)
        .zip(destination.chunks_mut(destination_stride))
        .take(rows as usize)
    {
        if let Some(active) = destination_row.get_mut(..row_width) {
            active.copy_from_slice(source_row);
        }
    }
}

#[cfg(target_os = "linux")]
fn keepalive_sample(state: &State, target: DeliveryTarget) -> Option<VideoSample> {
    let (source_width, source_height, pts_us, mut buffer) = {
        let history = state.history.lock().ok()?;
        let frame = history.newest()?;
        let mut buffer = OwnedI420::new(frame.width, frame.height);
        let strides = buffer.strides();
        let (y, u, v) = buffer.data_mut();
        copy_history(frame, strides, y, u, v);
        (frame.width, frame.height, frame.pts_us, buffer)
    };
    if (source_width, source_height) != (target.width, target.height) {
        buffer = buffer.scale(target.width, target.height).ok()?;
    }
    Some(VideoSample {
        sequence: state.sequence.fetch_add(1, Ordering::Relaxed),
        width: target.width,
        height: target.height,
        pts_us,
        buffer,
    })
}

#[cfg(not(target_os = "linux"))]
fn deliver_keepalive(state: &State, binding: &DeliveryBinding, publication: &mut PublicationState) {
    let (Some(target), Some(output)) = (binding.target(), binding.output.as_ref()) else {
        return;
    };
    let Some(sample) = keepalive_frame(state, target, publication) else {
        return;
    };
    state
        .stats
        .keepalive_attempted
        .fetch_add(1, Ordering::Relaxed);
    match output.publish(sample, publication) {
        Ok(()) => {
            state.stats.keepalive_pushed.fetch_add(1, Ordering::Relaxed);
        }
        Err(error) => {
            state
                .stats
                .keepalive_dropped
                .fetch_add(1, Ordering::Relaxed);
            state.report_delivery_error("publish keepalive", &error);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn keepalive_frame(
    state: &State,
    target: DeliveryTarget,
    publication: &mut PublicationState,
) -> Option<VideoSample> {
    let (source_width, source_height, pts_us, mut buffer) = {
        let history = state.history.lock().ok()?;
        let frame = history.newest()?;
        let mut buffer = publication
            .take_reusable_buffer(frame.width, frame.height)
            .unwrap_or_else(|| I420Buffer::new(frame.width, frame.height));
        let strides = buffer.strides();
        let (y, u, v) = buffer.data_mut();
        copy_history(frame, strides, y, u, v);
        (frame.width, frame.height, frame.pts_us, buffer)
    };
    if (source_width, source_height) != (target.width, target.height) {
        buffer = scale_buffer(buffer, target.width, target.height).ok()?;
    }
    Some(VideoSample {
        sequence: state.sequence.fetch_add(1, Ordering::Relaxed),
        width: target.width,
        height: target.height,
        pts_us,
        buffer,
    })
}

struct PreviewBuffers {
    planes: Vec<u8>,
    payload: Vec<u8>,
    source: Option<I420Buffer>,
}

fn run_preview(state: &State) {
    let mut buffers = PreviewBuffers {
        planes: Vec::new(),
        payload: Vec::new(),
        source: None,
    };
    let mut generation = 0;
    let mut next = state.clock.now();
    while !state.stop.load(Ordering::Relaxed) {
        let now = state.clock.now();
        if now >= next {
            emit_preview(state, &mut buffers, &mut generation);
            let fps = state
                .binding()
                .target()
                .map_or(PREVIEW_FALLBACK_FPS, |target| {
                    target.fps.clamp(1, PREVIEW_MAX_FPS)
                });
            next = now + frame_interval(fps);
        }
        state.clock.wait_until(next, &state.stop);
    }
}

fn emit_preview(state: &State, buffers: &mut PreviewBuffers, generation: &mut u64) -> bool {
    let current = state.generation.load(Ordering::Relaxed);
    if current == *generation {
        return false;
    }
    let output = state
        .preview_output
        .lock()
        .ok()
        .and_then(|value| value.clone());
    let viewport = state.viewport.lock().ok().and_then(|value| *value);
    let (Some(output), Some(viewport)) = (output, viewport) else {
        return false;
    };
    let (width, height, pts_us) = {
        let Ok(history) = state.history.lock() else {
            return false;
        };
        let Some(frame) = history.newest() else {
            return false;
        };
        buffers.planes.clear();
        buffers.planes.extend_from_slice(&frame.planes);
        (frame.width, frame.height, frame.pts_us)
    };
    let Some((output_width, output_height)) =
        fit_preview_size(width, height, viewport.0, viewport.1)
    else {
        return false;
    };
    if buffers
        .source
        .as_ref()
        .is_none_or(|source| source.width() != width || source.height() != height)
    {
        buffers.source = Some(I420Buffer::new(width, height));
    }
    let Some(source) = buffers.source.as_mut() else {
        return false;
    };
    {
        let strides = source.strides();
        let (y, u, v) = source.data_mut();
        let frame = HistoryFrame {
            width,
            height,
            pts_us,
            planes: std::mem::take(&mut buffers.planes),
        };
        copy_history(&frame, strides, y, u, v);
        buffers.planes = frame.planes;
    }
    let scaled = source.scale(
        i32::try_from(output_width).unwrap_or(0),
        i32::try_from(output_height).unwrap_or(0),
    );
    buffers.payload.clear();
    buffers
        .payload
        .resize(16 + output_width as usize * output_height as usize * 4, 0);
    buffers.payload[..8].copy_from_slice(&pts_us.to_le_bytes());
    buffers.payload[8..12].copy_from_slice(&output_width.to_le_bytes());
    buffers.payload[12..16].copy_from_slice(&output_height.to_le_bytes());
    let (y, u, v) = scaled.data();
    let (stride_y, stride_u, stride_v) = scaled.strides();
    yuv_helper::i420_to_argb(
        y,
        stride_y,
        u,
        stride_u,
        v,
        stride_v,
        &mut buffers.payload[16..],
        output_width * 4,
        i32::try_from(output_width).unwrap_or(0),
        i32::try_from(output_height).unwrap_or(0),
    );
    state.stats.preview.fetch_add(1, Ordering::Relaxed);
    output(std::mem::take(&mut buffers.payload), pts_us);
    *generation = current;
    true
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "fitted dimensions are non-negative and bounded by the viewport"
)]
fn fit_preview_size(
    source_width: u32,
    source_height: u32,
    viewport_width: u32,
    viewport_height: u32,
) -> Option<(u32, u32)> {
    if source_width == 0 || source_height == 0 || viewport_width == 0 || viewport_height == 0 {
        return None;
    }
    let viewport_aspect = f64::from(viewport_width) / f64::from(viewport_height);
    let source_aspect = f64::from(source_width) / f64::from(source_height);
    let (width, height) = if viewport_aspect > source_aspect {
        (
            (f64::from(viewport_height) * source_aspect) as u32,
            viewport_height,
        )
    } else {
        (
            viewport_width,
            (f64::from(viewport_width) / source_aspect) as u32,
        )
    };
    if width >= source_width && height >= source_height {
        return Some((source_width, source_height));
    }
    Some((width.max(1), height.max(1)))
}

fn frame_interval(fps: u32) -> Duration {
    Duration::from_micros(1_000_000 / u64::from(fps.max(1)))
}

#[cfg(target_os = "linux")]
fn warm_freelist(width: u32, height: u32) {
    if width == 0 || height == 0 {
        return;
    }
    let warmed = [
        OwnedI420::new(width, height).into_planes(),
        OwnedI420::new(width, height).into_planes(),
    ];
    if let Ok(mut freelist) = I420_FREELIST.lock() {
        for planes in warmed {
            if freelist.len() >= I420_FREELIST_CAP {
                break;
            }
            freelist.push(planes);
        }
    }
}

pub(crate) fn monotonic_us() -> i64 {
    static ANCHOR: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let anchor = ANCHOR.get_or_init(Instant::now);
    i64::try_from(anchor.elapsed().as_micros()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Condvar;

    #[derive(Default)]
    struct ManualClock {
        now: Mutex<Duration>,
        changed: Condvar,
        waiters: AtomicU64,
        delivery_deadline: Mutex<Duration>,
    }

    impl ManualClock {
        fn advance(&self, duration: Duration) {
            if let Ok(mut now) = self.now.lock() {
                *now += duration;
                self.changed.notify_all();
            }
            thread::yield_now();
        }

        fn wait_for_workers(&self) {
            let deadline = Instant::now() + Duration::from_secs(1);
            while self.waiters.load(Ordering::Relaxed) < 2 && Instant::now() < deadline {
                thread::yield_now();
            }
            assert!(self.waiters.load(Ordering::Relaxed) >= 2);
        }

        fn delivery_deadline(&self) -> Duration {
            self.delivery_deadline
                .lock()
                .map_or(Duration::ZERO, |deadline| *deadline)
        }

        fn wait_for_next_delivery_deadline(&self, previous_deadline: Duration) {
            let timeout = Instant::now() + Duration::from_secs(1);
            while self.delivery_deadline() <= previous_deadline && Instant::now() < timeout {
                thread::yield_now();
            }
            assert!(self.delivery_deadline() > previous_deadline);
        }
    }

    impl DeliveryClock for ManualClock {
        fn now(&self) -> Duration {
            self.now.lock().map_or(Duration::ZERO, |now| *now)
        }

        fn wait_until(&self, deadline: Duration, stop: &AtomicBool) {
            if thread::current().name() == Some("frame-delivery")
                && let Ok(mut delivery_deadline) = self.delivery_deadline.lock()
            {
                *delivery_deadline = deadline;
            }
            self.waiters.fetch_add(1, Ordering::Relaxed);
            self.changed.notify_all();
            let Ok(mut now) = self.now.lock() else {
                return;
            };
            while *now < deadline && !stop.load(Ordering::Relaxed) {
                let Ok(next) = self.changed.wait(now) else {
                    return;
                };
                now = next;
            }
        }

        fn wake(&self) {
            self.changed.notify_all();
        }
    }

    #[test]
    fn delayed_tick_does_not_catch_up_multiple_frames() {
        initialize_platform();
        let clock = Arc::new(ManualClock::default());
        let output = RecordingOutput::new(true);
        let binding = output.binding(DeliveryTarget {
            width: 8,
            height: 8,
            fps: 60,
        });
        let (delivery, ingress) =
            FrameDelivery::start_with_clock(binding, None, None, Arc::<ManualClock>::clone(&clock))
                .unwrap_or_else(|error| panic!("delivery start failed: {error}"));
        clock.wait_for_workers();
        for pts_us in 1..=5 {
            let color = u8::try_from(pts_us * 32).unwrap_or(0);
            let bgra = [color; 8 * 8 * 4];
            assert_eq!(
                ingress.submit(CapturedFrame {
                    width: 8,
                    height: 8,
                    bgra: &bgra,
                    pts_us,
                }),
                SubmitOutcome::Accepted
            );
        }
        clock.advance(Duration::from_millis(100));
        output.wait_for(1);
        thread::yield_now();
        let frames = output
            .frames
            .lock()
            .unwrap_or_else(|_| panic!("recording output lock poisoned"));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].2, 2, "drop-oldest must retain the freshest queue");
        drop(frames);
        assert_eq!(delivery.stats().pacer_drops, 1);
        delivery
            .stop()
            .unwrap_or_else(|error| panic!("delivery stop failed: {error}"));
    }

    struct RecordingOutput {
        frames: Mutex<Vec<(u32, u32, i64, bool)>>,
        changed: Condvar,
        accepts: AtomicBool,
    }

    impl RecordingOutput {
        fn new(accepts: bool) -> Arc<Self> {
            Arc::new(Self {
                frames: Mutex::new(Vec::new()),
                changed: Condvar::new(),
                accepts: AtomicBool::new(accepts),
            })
        }

        fn binding(self: &Arc<Self>, target: DeliveryTarget) -> DeliveryBinding {
            let recorder = Arc::clone(self);
            DeliveryBinding {
                target: Some(target),
                output: Some(PublicationOutput::Test(Arc::new(move |sample| {
                    let accepts = recorder.accepts.load(Ordering::SeqCst);
                    if let Ok(mut frames) = recorder.frames.lock() {
                        frames.push((sample.width, sample.height, sample.pts_us, accepts));
                        recorder.changed.notify_all();
                    }
                    accepts
                }))),
            }
        }

        fn wait_for(&self, count: usize) {
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut frames = self
                .frames
                .lock()
                .unwrap_or_else(|_| panic!("recording output lock poisoned"));
            while frames.len() < count && Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let waited = self.changed.wait_timeout(frames, remaining);
                let Ok((next, _)) = waited else {
                    panic!("recording output wait poisoned");
                };
                frames = next;
            }
            assert!(frames.len() >= count, "expected {count} output frames");
        }
    }

    fn initialize_platform() {
        #[cfg(target_os = "linux")]
        if let Err(error) = gst::init() {
            panic!("GStreamer init failed: {error}");
        }
    }

    fn submit_color_frame(ingress: &FrameIngress, pts_us: i64) {
        let bgra = [127_u8; 8 * 8 * 4];
        assert_eq!(
            ingress.submit(CapturedFrame {
                width: 8,
                height: 8,
                bgra: &bgra,
                pts_us,
            }),
            SubmitOutcome::Accepted
        );
    }

    type PreviewFrames = Arc<(Mutex<Vec<Vec<u8>>>, Condvar)>;

    fn preview_recorder() -> (PreviewOutput, PreviewFrames) {
        let received = Arc::new((Mutex::new(Vec::<Vec<u8>>::new()), Condvar::new()));
        let callback_state = Arc::clone(&received);
        let callback: PreviewOutput = Arc::new(move |payload, _| {
            if let Ok(mut frames) = callback_state.0.lock() {
                frames.push(payload);
                callback_state.1.notify_all();
            }
        });
        (callback, received)
    }

    fn wait_for_preview(received: &PreviewFrames) {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut frames = received
            .0
            .lock()
            .unwrap_or_else(|_| panic!("preview output lock poisoned"));
        while frames.is_empty() && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let waited = received.1.wait_timeout(frames, remaining);
            let Ok((next, _)) = waited else {
                panic!("preview output wait poisoned");
            };
            frames = next;
        }
        assert!(!frames.is_empty(), "preview frame was not emitted");
    }

    #[test]
    fn target_update_scales_the_next_delivery_atomically() {
        initialize_platform();
        let clock = Arc::new(ManualClock::default());
        let output = RecordingOutput::new(true);
        let (delivery, ingress) = FrameDelivery::start_with_clock(
            DeliveryBinding::dormant(),
            None,
            None,
            Arc::<ManualClock>::clone(&clock),
        )
        .unwrap_or_else(|error| panic!("delivery start failed: {error}"));
        delivery.set_binding(output.binding(DeliveryTarget {
            width: 4,
            height: 4,
            fps: 60,
        }));
        submit_color_frame(&ingress, 7);
        clock.advance(Duration::from_millis(40));
        output.wait_for(1);
        let frames = output
            .frames
            .lock()
            .unwrap_or_else(|_| panic!("recording output lock poisoned"));
        assert_eq!(frames[0], (4, 4, 7, true));
        drop(frames);
        delivery
            .stop()
            .unwrap_or_else(|error| panic!("delivery stop failed: {error}"));
    }

    #[test]
    fn publisher_replacement_rejects_frames_queued_for_the_old_output() {
        initialize_platform();
        let clock = Arc::new(ManualClock::default());
        let old_output = RecordingOutput::new(true);
        let new_output = RecordingOutput::new(true);
        let (delivery, ingress) = FrameDelivery::start_with_clock(
            old_output.binding(DeliveryTarget {
                width: 8,
                height: 8,
                fps: 60,
            }),
            None,
            None,
            Arc::<ManualClock>::clone(&clock),
        )
        .unwrap_or_else(|error| panic!("delivery start failed: {error}"));
        clock.wait_for_workers();
        submit_color_frame(&ingress, 7);
        let delivery_deadline = clock.delivery_deadline();
        clock.advance(Duration::from_millis(20));
        old_output.wait_for(1);
        clock.wait_for_next_delivery_deadline(delivery_deadline);
        let old_bgra = [126_u8; 8 * 8 * 4];
        assert_eq!(
            ingress.submit(CapturedFrame {
                width: 8,
                height: 8,
                bgra: &old_bgra,
                pts_us: 8,
            }),
            SubmitOutcome::Accepted
        );
        delivery.set_binding(new_output.binding(DeliveryTarget {
            width: 4,
            height: 4,
            fps: 60,
        }));
        let delivery_deadline = clock.delivery_deadline();
        clock.advance(Duration::from_millis(20));
        clock.wait_for_next_delivery_deadline(delivery_deadline);
        assert!(
            new_output
                .frames
                .lock()
                .is_ok_and(|frames| frames.is_empty()),
            "replacement output received a frame queued for its predecessor"
        );
        let new_bgra = [125_u8; 8 * 8 * 4];
        assert_eq!(
            ingress.submit(CapturedFrame {
                width: 8,
                height: 8,
                bgra: &new_bgra,
                pts_us: 9,
            }),
            SubmitOutcome::Accepted
        );
        clock.advance(Duration::from_millis(20));
        new_output.wait_for(1);
        let old_frames = old_output
            .frames
            .lock()
            .unwrap_or_else(|_| panic!("old output lock poisoned"));
        let new_frames = new_output
            .frames
            .lock()
            .unwrap_or_else(|_| panic!("new output lock poisoned"));
        assert_eq!(old_frames.as_slice(), &[(8, 8, 7, true)]);
        assert_eq!(new_frames.as_slice(), &[(4, 4, 9, true)]);
        drop(old_frames);
        drop(new_frames);
        assert_eq!(delivery.stats().frames_dropped, 1);
        delivery
            .stop()
            .unwrap_or_else(|error| panic!("delivery stop failed: {error}"));
    }

    #[test]
    fn rejected_publication_drops_and_counts_without_stopping() {
        initialize_platform();
        let clock = Arc::new(ManualClock::default());
        let output = RecordingOutput::new(false);
        let binding = output.binding(DeliveryTarget {
            width: 8,
            height: 8,
            fps: 60,
        });
        let (delivery, ingress) =
            FrameDelivery::start_with_clock(binding, None, None, Arc::<ManualClock>::clone(&clock))
                .unwrap_or_else(|error| panic!("delivery start failed: {error}"));
        clock.wait_for_workers();
        submit_color_frame(&ingress, 9);
        clock.advance(Duration::from_millis(20));
        output.wait_for(1);
        output.accepts.store(true, Ordering::SeqCst);
        let bgra = [126_u8; 8 * 8 * 4];
        assert_eq!(
            ingress.submit(CapturedFrame {
                width: 8,
                height: 8,
                bgra: &bgra,
                pts_us: 10,
            }),
            SubmitOutcome::Accepted
        );
        clock.advance(Duration::from_millis(20));
        output.wait_for(2);
        let stats = delivery
            .stop()
            .unwrap_or_else(|error| panic!("delivery stop failed: {error}"));
        let decisions: Vec<_> = output
            .frames
            .lock()
            .unwrap_or_else(|_| panic!("recording output lock poisoned"))
            .iter()
            .map(|frame| frame.3)
            .collect();
        assert_eq!(decisions, [false, true]);
        assert_eq!(stats.frames_dropped, 1);
        assert_eq!(stats.frames_pushed, 1);
    }

    #[test]
    fn static_content_uses_keepalive_with_the_capture_timestamp() {
        initialize_platform();
        let clock = Arc::new(ManualClock::default());
        let output = RecordingOutput::new(true);
        let binding = output.binding(DeliveryTarget {
            width: 4,
            height: 4,
            fps: 60,
        });
        let (delivery, ingress) =
            FrameDelivery::start_with_clock(binding, None, None, Arc::<ManualClock>::clone(&clock))
                .unwrap_or_else(|error| panic!("delivery start failed: {error}"));
        clock.wait_for_workers();
        submit_color_frame(&ingress, 11);
        let delivery_deadline = clock.delivery_deadline();
        clock.advance(Duration::from_millis(20));
        output.wait_for(1);
        clock.wait_for_next_delivery_deadline(delivery_deadline);
        clock.advance(Duration::from_millis(20));
        output.wait_for(2);
        let frames = output
            .frames
            .lock()
            .unwrap_or_else(|_| panic!("recording output lock poisoned"));
        assert_eq!(frames[0], (4, 4, 11, true));
        assert_eq!(frames[1], (4, 4, 11, true));
        drop(frames);
        let stats = delivery
            .stop()
            .unwrap_or_else(|error| panic!("delivery stop failed: {error}"));
        assert_eq!(stats.frames_dequeued, 1);
        assert_eq!(stats.frames_pushed, 1);
        assert_eq!(stats.keepalive_pushed, 1);
    }

    #[test]
    fn preview_interface_preserves_header_and_fitted_dimensions() {
        initialize_platform();
        let clock = Arc::new(ManualClock::default());
        let (callback, received) = preview_recorder();
        let (delivery, ingress) = FrameDelivery::start_with_clock(
            DeliveryBinding::dormant(),
            Some(callback),
            Some((7, 5)),
            Arc::<ManualClock>::clone(&clock),
        )
        .unwrap_or_else(|error| panic!("delivery start failed: {error}"));
        clock.wait_for_workers();
        let mut bgra = [0_u8; 7 * 5 * 4];
        for (row_index, row) in bgra.as_chunks_mut::<{ 7 * 4 }>().0.iter_mut().enumerate() {
            let value = u8::try_from(20 + row_index * 40).unwrap_or(0);
            for pixel in row.as_chunks_mut::<4>().0 {
                pixel.copy_from_slice(&[value, value, value, 255]);
            }
        }
        assert_eq!(
            ingress.submit(CapturedFrame {
                width: 7,
                height: 5,
                bgra: &bgra,
                pts_us: 13,
            }),
            SubmitOutcome::Accepted
        );
        clock.advance(Duration::from_millis(40));
        wait_for_preview(&received);
        let frames = received
            .0
            .lock()
            .unwrap_or_else(|_| panic!("preview output lock poisoned"));
        let payload = frames
            .first()
            .unwrap_or_else(|| panic!("preview frame was not emitted"));
        assert_eq!(payload.len(), 16 + 7 * 5 * 4);
        assert_eq!(
            i64::from_le_bytes(payload[..8].try_into().unwrap_or([0; 8])),
            13
        );
        assert_eq!(
            u32::from_le_bytes(payload[8..12].try_into().unwrap_or([0; 4])),
            7
        );
        assert_eq!(
            u32::from_le_bytes(payload[12..16].try_into().unwrap_or([0; 4])),
            5
        );
        let row_values: Vec<_> = payload[16..]
            .as_chunks::<{ 7 * 4 }>()
            .0
            .iter()
            .map(|row| row[0])
            .collect();
        assert!(row_values.windows(2).all(|rows| rows[0] < rows[1]));
        drop(frames);
        delivery
            .stop()
            .unwrap_or_else(|error| panic!("delivery stop failed: {error}"));
    }

    #[test]
    fn preview_retries_the_current_frame_after_viewport_registration() {
        initialize_platform();
        let clock = Arc::new(ManualClock::default());
        let (callback, received) = preview_recorder();
        let (delivery, ingress) = FrameDelivery::start_with_clock(
            DeliveryBinding::dormant(),
            Some(callback),
            None,
            Arc::<ManualClock>::clone(&clock),
        )
        .unwrap_or_else(|error| panic!("delivery start failed: {error}"));
        clock.wait_for_workers();
        submit_color_frame(&ingress, 21);
        clock.advance(Duration::from_millis(40));
        assert!(
            received.0.lock().is_ok_and(|frames| frames.is_empty()),
            "preview emitted without a viewport"
        );

        delivery.set_viewport(Some((8, 8)));
        clock.advance(Duration::from_millis(40));
        wait_for_preview(&received);
        delivery
            .stop()
            .unwrap_or_else(|error| panic!("delivery stop failed: {error}"));
    }

    #[test]
    fn preview_retries_the_current_live_frame_after_callback_registration() {
        initialize_platform();
        let clock = Arc::new(ManualClock::default());
        let publication = RecordingOutput::new(true);
        let (delivery, ingress) = FrameDelivery::start_with_clock(
            publication.binding(DeliveryTarget {
                width: 8,
                height: 8,
                fps: 60,
            }),
            None,
            Some((8, 8)),
            Arc::<ManualClock>::clone(&clock),
        )
        .unwrap_or_else(|error| panic!("delivery start failed: {error}"));
        clock.wait_for_workers();
        submit_color_frame(&ingress, 22);
        clock.advance(Duration::from_millis(40));
        let (callback, received) = preview_recorder();
        delivery.set_preview_output(Some(callback));
        clock.advance(Duration::from_millis(40));
        wait_for_preview(&received);
        delivery
            .stop()
            .unwrap_or_else(|error| panic!("delivery stop failed: {error}"));
    }

    #[test]
    fn invalid_frames_and_source_issues_are_counted_through_ingress() {
        initialize_platform();
        let clock = Arc::new(ManualClock::default());
        let (delivery, ingress) = FrameDelivery::start_with_clock(
            DeliveryBinding::dormant(),
            None,
            None,
            Arc::<ManualClock>::clone(&clock),
        )
        .unwrap_or_else(|error| panic!("delivery start failed: {error}"));
        clock.wait_for_workers();
        assert_eq!(
            ingress.submit(CapturedFrame {
                width: 2,
                height: 2,
                bgra: &[0; 15],
                pts_us: 1,
            }),
            SubmitOutcome::Accepted
        );
        ingress.record_issue(SourceIssue::DroppedFrame);
        ingress.record_issue(SourceIssue::CaptureError);

        let stats = delivery
            .stop()
            .unwrap_or_else(|error| panic!("delivery stop failed: {error}"));
        assert_eq!(stats.frames_dequeued, 1);
        assert_eq!(stats.frames_dropped, 2);
        assert_eq!(stats.capture_errors, 1);
    }

    #[test]
    fn stop_freezes_stats_and_rejects_late_ingress() {
        initialize_platform();
        let clock = Arc::new(ManualClock::default());
        let (delivery, ingress) = FrameDelivery::start_with_clock(
            DeliveryBinding::dormant(),
            None,
            None,
            Arc::<ManualClock>::clone(&clock),
        )
        .unwrap_or_else(|error| panic!("delivery start failed: {error}"));
        clock.wait_for_workers();
        submit_color_frame(&ingress, 1);
        let stats = delivery
            .stop()
            .unwrap_or_else(|error| panic!("delivery stop failed: {error}"));
        let bgra = [0_u8; 16];
        assert_eq!(
            ingress.submit(CapturedFrame {
                width: 2,
                height: 2,
                bgra: &bgra,
                pts_us: 1,
            }),
            SubmitOutcome::Rejected
        );
        assert_eq!(stats.frames_dequeued, 1);
        assert_eq!(stats.frames_dropped, 0);
        assert_eq!(stats.pacer_depth, 0);
    }

    #[test]
    fn preview_fit_contains_without_upscaling() {
        assert_eq!(fit_preview_size(1920, 1080, 1600, 1000), Some((1600, 900)));
        assert_eq!(fit_preview_size(1280, 720, 1920, 1080), Some((1280, 720)));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn owned_i420_keeps_zero_copy_layout_through_scale() {
        if let Err(error) = gst::init() {
            panic!("GStreamer init failed: {error}");
        }
        let mut source = OwnedI420::new(8, 8);
        let (y, u, v) = source.data_mut();
        y.fill(100);
        u.fill(90);
        v.fill(240);
        let scaled = source
            .scale(4, 4)
            .unwrap_or_else(|error| panic!("scale failed: {error}"));
        assert_eq!((scaled.width, scaled.height), (4, 4));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn submitted_buffers_remain_owned_for_the_retention_window() {
        let mut publication = PublicationState::default();
        let capacity = i64::try_from(PUBLICATION_RETENTION_CAPACITY).unwrap_or(0);
        for timestamp_us in 0..=capacity {
            publication.retain(VideoFrame {
                rotation: VideoRotation::VideoRotation0,
                timestamp_us,
                frame_metadata: None,
                buffer: I420Buffer::new(2, 2),
            });
        }
        assert_eq!(publication.retained.len(), PUBLICATION_RETENTION_CAPACITY);
        assert_eq!(
            publication.retained.front().map(|frame| frame.timestamp_us),
            Some(1)
        );
        assert_eq!(
            publication.retained.back().map(|frame| frame.timestamp_us),
            Some(capacity)
        );
    }
}
