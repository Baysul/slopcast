//! Linux video encoder branch attached to `livekitwebrtcsink`.
//!
//! `vah264enc` provides a hard VBR ceiling through `target-percentage`.
//! Software VPx/AV1 encoders expose only target bitrate controls, so their
//! output can vary around the requested rate. The publisher's congestion
//! controller re-targets each encoder at runtime through
//! `GstreamerEncoder::set_ceiling_kbps`.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use livekit::webrtc::prelude::{I420Buffer, VideoBuffer, VideoFrame};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::CaptureConfig;

/// Video appsrc queue depth in buffers. Two was too tight: any transient
/// encoder hiccup (VA-API pipeline stall, driver swap) saturated it
/// instantly and dropped frames continuously. Six buffers (~100 ms at
/// 60 fps) absorbs short encoder jitter while keeping the queue's
/// freshness bound well under a typical receiver jitter budget.
const APPSRC_MAX_BUFFERS: u64 = 6;
/// Size of the I420 input buffer pool (buffers in flight: the appsrc queue
/// plus the synchronous convert/encode stages). Pool buffers are returned
/// automatically when the pipeline releases them, so the steady state does
/// zero per-frame input-buffer allocation — the previous
/// `gst::Buffer::with_size` per frame was a 2.25 MB allocate+page-fault
/// at 1080p (4× at 4K) on the hot delivery path.
///
/// Sized well above `APPSRC_MAX_BUFFERS` (6) so the leaky-appsrc queue can
/// fill while the encoder's 120 ms output window retains frames without
/// exhausting the pool — pool acquisition fails *before* `push_buffer`, so a
/// too-small pool shows up as silent frame drops that `appsrc.dropped` never
/// counts (see `VIDEO_POOL_EXHAUSTED`). 24 buffers is a deliberate, generous
/// headroom for the diagnostic: it is not a cure for encoder latency, but it
/// disambiguates pool exhaustion from genuine downstream backpressure.
const VIDEO_BUFFER_POOL_SIZE: u32 = 24;
const GOP_SECONDS: u32 = 1;

/// VBR target as a percentage of the presenter's bitrate ceiling. With
/// `rate-control = vbr`, `vah264enc` computes the encoder's **maximum**
/// bitrate as `bitrate × 100 / target-percentage` (there is no max-bitrate
/// property on the `va` plugin's encoder), so target 80% of the ceiling
/// with percentage 80 pins the maximum exactly onto the ceiling while the
/// average stays below it: static screens encode well under the ceiling and
/// busy scenes may burst up to it — never past. A percentage of 100 would
/// degenerate to CBR (the driver sets minimum = maximum), so it is never
/// used here.
const VBR_TARGET_PERCENTAGE: u32 = 80;
/// SVT-AV1 VBR target as a percentage of the presenter's bitrate ceiling.
/// SVT-AV1 4.x rejects `max-bitrate` in VBR mode (it is CRF-only), so the
/// target sits below the ceiling to leave headroom instead of relying on a
/// hard cap. 75% matches the validated AV1 screenshare sweet spots (6 Mbps
/// target on an 8 Mbps ceiling at 1080p60).
const AV1_TARGET_PERCENTAGE: u32 = 75;
/// SVT-AV1 quality floor for rate control (QP never exceeds this, so
/// high-entropy frames stay legible instead of decaying to mush). 52 is a
/// balance between the encoder's 63 default and over-soft screenshare text.
const AV1_MAX_QP_ALLOWED: u32 = 52;

/// Number of encoded H.264 access units that emerged from `h264parse`
/// (i.e. actually encoded and pushed downstream), as opposed to
/// `VIDEO_FRAMES_SUBMITTED` which counts frames pushed into the appsrc.
/// This is the true encoder-throughput counter; any gap vs. the submitted
/// count is frames dropped by the backpressure path (leaky appsrc or the
/// 200 ms time-bounded queue) before they could be encoded.
static VIDEO_FRAMES_ENCODED: AtomicU64 = AtomicU64::new(0);

/// Frames dropped because the input buffer pool was exhausted when
/// `push_frame` acquired with `DONTWAIT`. This happens *before* the frame
/// reaches the appsrc, so it is invisible to `appsrc.dropped` — a dedicated
/// counter is the only way to distinguish "encoder retained every buffer"
/// from "the leaky appsrc discarded a buffered frame".
static VIDEO_POOL_EXHAUSTED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn reset_encoded_frames() {
    VIDEO_FRAMES_ENCODED.store(0, Ordering::Relaxed);
    VIDEO_POOL_EXHAUSTED.store(0, Ordering::Relaxed);
}

pub(crate) fn encoded_frames() -> u64 {
    VIDEO_FRAMES_ENCODED.load(Ordering::Relaxed)
}

pub(crate) fn pool_exhausted_frames() -> u64 {
    VIDEO_POOL_EXHAUSTED.load(Ordering::Relaxed)
}

/// Live GstBaseSrc/AppSrc statistics (all present on the appsrc element
/// itself; `dropped` is the number of buffers discarded by appsrc, i.e.
/// the leaky-downstream drops that surface as video stutter).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AppSrcStats {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub dropped: Option<u64>,
    pub level_buffers: Option<u32>,
    pub level_bytes: Option<u32>,
    pub level_time: Option<u64>,
}

fn stat_u64(element: &gst::Element, name: &str) -> Option<u64> {
    element.property_value(name).get::<u64>().ok()
}

fn stat_u32(element: &gst::Element, name: &str) -> Option<u32> {
    element.property_value(name).get::<u32>().ok()
}

/// Pipeline running time in nanoseconds at the instant of the call.
fn running_time_ns(appsrc: &gst_app::AppSrc) -> Result<u64, String> {
    let clock = appsrc
        .clock()
        .ok_or_else(|| "video appsrc has no pipeline clock".to_string())?;
    let base_time = appsrc
        .base_time()
        .ok_or_else(|| "video appsrc has no base time".to_string())?;
    Ok((clock.time() - base_time).nseconds())
}

#[derive(Clone)]
pub(crate) struct VideoInput {
    appsrc: gst_app::AppSrc,
    input_info: gst_video::VideoInfo,
    width: u32,
    height: u32,
    fps: u32,
    /// Capture-clock anchor `(C0_us, P0_ns)`: the first pushed frame's
    /// capture timestamp and the pipeline running time at that instant.
    /// Every frame's PTS is then `P0 + (capture_timestamp - C0)`, which
    /// advances at the capture engine's true frame cadence instead of the
    /// jittery push time (do-timestamp). Shared via `Arc` so clones (the
    /// `VIDEO_INPUT` static and per-call clones) anchor exactly once.
    pts_anchor: Arc<Mutex<Option<(i64, u64)>>>,
    /// Last emitted buffer PTS in nanoseconds. Keepalives re-deliver a static
    /// screen's newest frame with its own capture timestamp, so consecutive
    /// pushes can carry identical PTS; clamping each new PTS to
    /// `max(capture_pts, last + frame_duration)` keeps the sink's RTP
    /// timestamp chain strictly monotonic (see `push_frame`).
    last_pts_ns: Arc<Mutex<Option<u64>>>,
    /// Preallocated I420 input buffers (see `VIDEO_BUFFER_POOL_SIZE`):
    /// `push_frame` acquires from the pool instead of allocating a fresh
    /// full-frame buffer per frame. Pool buffers are returned to the pool
    /// automatically when the pipeline releases them.
    buffer_pool: gst::BufferPool,
}

impl VideoInput {
    pub(crate) fn push_frame(&self, frame: &VideoFrame<I420Buffer>) -> Result<(), String> {
        if frame.buffer.width() != self.width || frame.buffer.height() != self.height {
            return Err(format!(
                "GStreamer input frame is {}x{}, expected {}x{}",
                frame.buffer.width(),
                frame.buffer.height(),
                self.width,
                self.height
            ));
        }

        // Acquire from the preallocated input pool rather than allocating a
        // fresh 2.25 MB (1080p) / 6.2 MB (4K) buffer + page faults per
        // frame. DONTWAIT: when the encoder is stalled and every pool buffer
        // is in flight, fail fast (the publish path counts the drop) instead
        // of blocking the delivery loop.
        let acquire_params =
            gst::BufferPoolAcquireParams::with_flags(gst::BufferPoolAcquireFlags::DONTWAIT);
        let mut buffer = self
            .buffer_pool
            .acquire_buffer(Some(&acquire_params))
            .map_err(|_| {
                VIDEO_POOL_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
                "GStreamer video input buffer pool exhausted (encoder stalled; dropping frame)"
                    .to_string()
            })?;
        {
            let buffer_ref = buffer
                .get_mut()
                .ok_or_else(|| "GStreamer input buffer is unexpectedly shared".to_string())?;
            let mut mapped = buffer_ref
                .map_writable()
                .map_err(|_| "Failed to map GStreamer input buffer writable".to_string())?;
            let (plane_y, plane_u, plane_v) = frame.buffer.data();
            let (stride_y, stride_u, stride_v) = frame.buffer.strides();
            let offsets = self.input_info.offset();
            let strides = self.input_info.stride();
            let chroma_width = self.width.div_ceil(2);
            let chroma_height = self.height.div_ceil(2);

            copy_plane(
                plane_y,
                stride_y,
                &mut mapped,
                offsets[0],
                strides[0],
                self.width,
                self.height,
            )?;
            copy_plane(
                plane_u,
                stride_u,
                &mut mapped,
                offsets[1],
                strides[1],
                chroma_width,
                chroma_height,
            )?;
            copy_plane(
                plane_v,
                stride_v,
                &mut mapped,
                offsets[2],
                strides[2],
                chroma_width,
                chroma_height,
            )?;
        }
        // PTS comes from the capture clock, anchored onto the pipeline's
        // running time at the first frame: PTS = P0 + (C - C0). Capture
        // timestamps advance at the true frame cadence, so the sink's RTP
        // timestamp derivation stays even regardless of push jitter. On a
        // capture-clock reset (new capture session) the anchor re-bases so
        // PTS stays monotonic. Only the duration is set explicitly, from
        // the declared frame cadence.
        let mut anchor = self
            .pts_anchor
            .lock()
            .map_err(|_| "video PTS anchor lock poisoned".to_string())?;
        let (c0_us, p0_ns) = match *anchor {
            Some((c0, p0)) if frame.timestamp_us >= c0 => (c0, p0),
            _ => {
                let anchored = (frame.timestamp_us, running_time_ns(&self.appsrc)?);
                *anchor = Some(anchored);
                anchored
            }
        };
        // Capture anchors are i64; the anchor branch guarantees a
        // non-negative difference, so cast_unsigned is lossless.
        let pts_ns = p0_ns + (frame.timestamp_us - c0_us).cast_unsigned() * 1000;
        // Keepalives re-deliver a static screen's newest frame with its own
        // capture timestamp, so back-to-back pushes can carry an identical
        // PTS. Clamp to at least `last + frame_duration` so the sink's RTP
        // timestamp chain stays strictly monotonic — a duplicated PTS would
        // make the receiver's jitter buffer treat the two frames as one.
        let mut last_pts = self
            .last_pts_ns
            .lock()
            .map_err(|_| "video PTS monotonicity lock poisoned".to_string())?;
        let pts_ns = match *last_pts {
            Some(previous) if pts_ns <= previous => previous + frame_duration(self.fps).nseconds(),
            _ => pts_ns,
        };
        *last_pts = Some(pts_ns);
        drop(last_pts);
        let buffer_ref = buffer
            .get_mut()
            .ok_or_else(|| "GStreamer input buffer is unexpectedly shared".to_string())?;
        buffer_ref.set_pts(gst::ClockTime::from_nseconds(pts_ns));
        buffer_ref.set_duration(frame_duration(self.fps));

        self.appsrc
            .push_buffer(buffer)
            .map_err(|error| format!("GStreamer appsrc rejected an I420 frame: {error}"))?;

        Ok(())
    }

    /// Live appsrc statistics — `dropped` counts buffers the appsrc discarded
    /// (leaky-downstream on a full 6-buffer queue). Frames that never reached
    /// the appsrc because the input buffer pool was exhausted are counted
    /// separately in `VIDEO_POOL_EXHAUSTED` (see `push_frame`).
    pub(crate) fn appsrc_stats(&self) -> AppSrcStats {
        let element = self.appsrc.upcast_ref::<gst::Element>();
        AppSrcStats {
            input: stat_u64(element, "in"),
            output: stat_u64(element, "out"),
            dropped: stat_u64(element, "dropped"),
            level_buffers: stat_u32(element, "current-level-buffers"),
            level_bytes: stat_u32(element, "current-level-bytes"),
            level_time: stat_u64(element, "current-level-time"),
        }
    }
}

pub(crate) struct GstreamerEncoder {
    input: VideoInput,
    /// The active codec encoder; the congestion controller re-targets its
    /// bitrate at runtime through this handle without rebuilding the pipeline.
    encoder: gst::Element,
    /// The currently applied bitrate ceiling in kbps — starts at the value
    /// derived from the stream settings and is stepped by the publisher's
    /// `RateController` on congestion signals.
    ceiling_kbps: u32,
    elements: Vec<gst::Element>,
    sink_pad: gst::Pad,
}

impl GstreamerEncoder {
    /// Re-targets the VBR ceiling at runtime: sets `bitrate` (the VBR
    /// target, `ceiling × target-percentage / 100`) so the driver-side
    /// maximum lands on the new ceiling. The VA-API rate controller picks
    /// the new rate up on the next encoded frame — no pipeline rebuild,
    /// so this is safe to call from the publisher's ~1 s congestion
    /// control tick even mid-stream.
    pub(crate) fn set_ceiling_kbps(&mut self, ceiling_kbps: u32) {
        let ceiling_kbps = ceiling_kbps.clamp(1, u32::MAX);
        if ceiling_kbps == self.ceiling_kbps {
            return;
        }
        let target = encoder_target_kbps(&self.encoder, ceiling_kbps);
        // VA-API uses `bitrate` in kbps; software VPx/AV1 encoders use
        // `target-bitrate` — VPx in bits/sec, svtav1enc in kilobits/sec (see
        // configure_encoder). Updated in place so congestion control keeps
        // the selected codec's target effective without a pipeline rebuild.
        if self.encoder.find_property("bitrate").is_some() {
            self.encoder
                .set_property_from_str("bitrate", &target.to_string());
        } else if self.encoder.find_property("target-bitrate").is_some() {
            let value = if target_bitrate_is_kbps(&self.encoder) {
                target
            } else {
                target.saturating_mul(1000).min(i32::MAX as u32)
            };
            self.encoder
                .set_property_from_str("target-bitrate", &value.to_string());
        } else {
            log::warn!("GStreamer encoder exposes no runtime bitrate property");
        }
        log::info!(
            "[gstreamer-encoder] VBR ceiling {} kbps -> {ceiling_kbps} kbps (VBR target {target} kbps)",
            self.ceiling_kbps,
        );
        self.ceiling_kbps = ceiling_kbps;
    }
    #[allow(
        clippy::too_many_lines,
        reason = "linear GStreamer element construction and linking is clearest in one function"
    )]
    pub(crate) fn attach(
        pipeline: &gst::Pipeline,
        sink: &gst::Element,
        config: &CaptureConfig,
    ) -> Result<Self, String> {
        if config.width < 128 || config.height < 128 {
            return Err(
                "GStreamer hardware encoders require a frame size of at least 128x128".into(),
            );
        }
        if config.fps == 0 {
            return Err("GStreamer encoder fps must be greater than zero".into());
        }
        let codec = config.video_codec.as_deref().unwrap_or("vp8");
        let ceiling_kbps = bitrate_bps_to_kbps(config.max_bitrate.unwrap_or(20_000_000.0));
        crate::gstreamer_publisher::verify_codec_elements(codec)?;
        let (encoder_name, parser_name, output_caps) = codec_pipeline(codec)?;

        let fps = i32::try_from(config.fps).map_err(|_| "GStreamer encoder fps exceeds i32")?;
        let input_info = gst_video::VideoInfo::builder(
            gst_video::VideoFormat::I420,
            config.width,
            config.height,
        )
        .fps(gst::Fraction::new(fps, 1))
        .build()
        .map_err(|error| format!("Failed to build GStreamer I420 video info: {error}"))?;
        let input_caps = input_info
            .to_caps()
            .map_err(|error| format!("Failed to build GStreamer I420 caps: {error}"))?;
        let appsrc = gst_app::AppSrc::builder()
            .caps(&input_caps)
            .format(gst::Format::Time)
            .is_live(true)
            .block(false)
            .max_buffers(APPSRC_MAX_BUFFERS)
            .max_bytes(0)
            .max_time(gst::ClockTime::ZERO)
            .leaky_type(gst_app::AppLeakyType::Downstream)
            .build();
        // PTS is stamped explicitly in `push_frame` from the capture clock
        // anchored to the pipeline's running time at the first frame — no
        // do-timestamp, so push jitter never wobbles the stream clock.
        // Leaky-downstream drops the *oldest* buffered frame on a full queue:
        // for live screen share stale frames are worthless, so freshness
        // wins (leaky-upstream would instead keep the oldest frame queued
        // and reject the newest, trailing real time).
        let convert = make_element("videoconvert")?;
        // VBR, ceiling-capped: the requested rate is the *ceiling*; the
        // `bitrate` property gets the VBR target (a fraction of the ceiling,
        // see VBR_TARGET_PERCENTAGE) and `target-percentage` makes the
        // driver-side maximum land on the ceiling.
        let key_int_max = config.fps.saturating_mul(GOP_SECONDS).min(1024);
        let encoder = gst::ElementFactory::make(encoder_name)
            .build()
            .map_err(|error| {
                format!("Failed to create GStreamer element {encoder_name}: {error}")
            })?;
        let encoder_rate = encoder_target_kbps(&encoder, ceiling_kbps);
        configure_encoder(&encoder, codec, encoder_rate, config.height, key_int_max);
        let parser = gst::ElementFactory::make(parser_name)
            .build()
            .map_err(|error| {
                format!("Failed to create GStreamer element {parser_name}: {error}")
            })?;
        if codec == "h264" && parser.find_property("config-interval").is_some() {
            parser.set_property_from_str("config-interval", "-1");
        }
        let capsfilter = gst::ElementFactory::make("capsfilter")
            .property("caps", &output_caps)
            .build()
            .map_err(|error| format!("Failed to create GStreamer {codec} capsfilter: {error}"))?;
        let output_queue = gst::ElementFactory::make("queue")
            .property("max-size-buffers", 0_u32)
            .property("max-size-bytes", 0_u32)
            // 120 ms of encoded data. This must stay non-leaky: dropping
            // *encoded* access units here would create decode gaps
            // (H.264 references the dropped frames), so a stalled sink
            // propagates backpressure into the encoder instead — bounded
            // by this window, after which the leaky appsrc drops raw
            // frames. 120 ms (down from 200 ms) keeps the post-stall drain
            // burst to ~7 frames at 60 fps instead of ~12, while still
            // smoothing VBR keyframe jitter.
            .property("max-size-time", 120_000_000_u64) // 120 ms
            .build()
            .map_err(|error| format!("Failed to create GStreamer video queue: {error}"))?;
        // Count encoded H.264 access units as they leave h264parse. This is
        // the real encoder-throughput signal (`VIDEO_FRAMES_ENCODED`); it
        // only advances for frames that were actually encoded, so any shortfall
        // vs. `VIDEO_FRAMES_SUBMITTED` reveals the leaky-appsrc / queue
        // backpressure drops. The counter must be monotonic per capture
        // session, so it is reset on every `attach_video`/capture start.
        let parser_src = parser
            .static_pad("src")
            .ok_or_else(|| "GStreamer h264parse has no src pad".to_string())?;
        parser_src.add_probe(gst::PadProbeType::BUFFER, |_, _| {
            VIDEO_FRAMES_ENCODED.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
        let elements = vec![
            appsrc.clone().upcast(),
            convert,
            encoder.clone(),
            parser,
            capsfilter,
            output_queue.clone(),
        ];

        // I420 input buffer pool: preallocate `VIDEO_BUFFER_POOL_SIZE`
        // buffers of the input frame size at attach time. Steady-state
        // `push_frame` then acquires from the pool (zero allocation); the
        // pool is deactivated and freed when `VideoInput` is dropped
        // (pipeline teardown).
        let buffer_pool = gst::BufferPool::new();
        {
            let mut pool_config = buffer_pool.config();
            pool_config.set_params(
                Some(&input_caps),
                u32::try_from(input_info.size())
                    .map_err(|_| "GStreamer input buffer size exceeds u32")?,
                0,
                VIDEO_BUFFER_POOL_SIZE,
            );
            buffer_pool.set_config(pool_config).map_err(|error| {
                format!("Failed to configure GStreamer input buffer pool: {error}")
            })?;
        }
        buffer_pool
            .set_active(true)
            .map_err(|error| format!("Failed to activate GStreamer input buffer pool: {error}"))?;

        pipeline
            .add_many(elements.iter())
            .map_err(|error| format!("Failed to add GStreamer video elements: {error}"))?;
        let attach_result = (|| {
            gst::Element::link_many(elements.iter())
                .map_err(|error| format!("Failed to link GStreamer video pipeline: {error}"))?;
            let sink_pad = sink
                .request_pad_simple("video_%u")
                .ok_or_else(|| "livekitwebrtcsink refused a video pad".to_string())?;
            let output_pad = output_queue
                .static_pad("src")
                .ok_or_else(|| "GStreamer video queue has no src pad".to_string())?;
            if let Err(error) = output_pad.link(&sink_pad) {
                sink.release_request_pad(&sink_pad);
                return Err(format!(
                    "Failed to link video into livekitwebrtcsink: {error}"
                ));
            }
            for element in &elements {
                if let Err(error) = element.sync_state_with_parent() {
                    let _ = output_pad.unlink(&sink_pad);
                    sink.release_request_pad(&sink_pad);
                    return Err(format!("Failed to start GStreamer video element: {error}"));
                }
            }

            Ok(sink_pad)
        })();
        let sink_pad = match attach_result {
            Ok(sink_pad) => sink_pad,
            Err(error) => {
                for element in &elements {
                    let _ = element.set_state(gst::State::Null);
                }
                let _ = pipeline.remove_many(elements.iter());
                return Err(error);
            }
        };

        Ok(Self {
            encoder,
            ceiling_kbps,
            elements,
            sink_pad,
            input: VideoInput {
                appsrc,
                input_info,
                width: config.width,
                height: config.height,
                fps: config.fps,
                pts_anchor: Arc::new(Mutex::new(None)),
                last_pts_ns: Arc::new(Mutex::new(None)),
                buffer_pool,
            },
        })
    }

    pub(crate) fn input(&self) -> VideoInput {
        self.input.clone()
    }

    pub(crate) fn detach(
        &self,
        pipeline: &gst::Pipeline,
        sink: &gst::Element,
    ) -> Result<(), String> {
        let output_pad = self
            .elements
            .last()
            .and_then(|element| element.static_pad("src"))
            .ok_or_else(|| "GStreamer video queue has no src pad".to_string())?;
        let (blocked_sender, blocked_receiver) = std::sync::mpsc::sync_channel(1);
        let probe_id = output_pad
            .add_probe(gst::PadProbeType::IDLE, move |_, _| {
                let _ = blocked_sender.try_send(());
                gst::PadProbeReturn::Ok
            })
            .ok_or_else(|| "Failed to block GStreamer video branch".to_string())?;
        if let Err(error) = blocked_receiver.recv_timeout(std::time::Duration::from_secs(1)) {
            output_pad.remove_probe(probe_id);
            return Err(format!(
                "Timed out blocking GStreamer video branch: {error}"
            ));
        }

        let mut failure = None;
        for element in &self.elements {
            if let Err(error) = element.set_state(gst::State::Null) {
                failure.get_or_insert_with(|| {
                    format!("Failed to stop GStreamer video element: {error}")
                });
            }
        }
        if let Err(error) = output_pad.unlink(&self.sink_pad) {
            failure
                .get_or_insert_with(|| format!("Failed to unlink GStreamer video branch: {error}"));
        }
        if let Err(error) = pipeline.remove_many(self.elements.iter()) {
            failure.get_or_insert_with(|| {
                format!("Failed to remove GStreamer video elements: {error}")
            });
        }
        sink.release_request_pad(&self.sink_pad);
        output_pad.remove_probe(probe_id);

        failure.map_or(Ok(()), Err)
    }
}

fn codec_pipeline(codec: &str) -> Result<(&'static str, &'static str, gst::Caps), String> {
    let (software, hardware, parser, caps) = match codec {
        "h264" => (
            "x264enc",
            "vah264enc",
            "h264parse",
            gst::Caps::builder("video/x-h264")
                .field("alignment", "au")
                .field("profile", "constrained-baseline")
                .build(),
        ),
        "vp9" => (
            "vp9enc",
            "vavp9enc",
            "vp9parse",
            // Both VP9 RTP payloaders assume one frame per input buffer. The
            // parser otherwise negotiates super-frame alignment, which makes
            // the SFU forward packets Chromium cannot assemble into frames.
            //
            // Note the fields deliberately omitted here: `profile` and
            // `bit-depth-luma`/`bit-depth-chroma` are *not* declared on the
            // parser's src template, so a capsfilter carrying them forces a
            // `not-negotiated` (the parser cannot promise a profile/bit-depth
            // before it has parsed the first frame). `chroma-format` and
            // `codec-alpha` are the safe structural pins.
            gst::Caps::builder("video/x-vp9")
                .field("chroma-format", "4:2:0")
                .field("codec-alpha", false)
                .field("parsed", true)
                .field("alignment", "frame")
                .build(),
        ),
        "av1" => (
            "svtav1enc",
            "vaav1enc",
            "av1parse",
            // `rtpav1pay` will accept only parsed OBU temporal units; a bare
            // `video/x-av1` capsfilter still publishes RTP but Chromium sees
            // packets without decodable frame boundaries.
            gst::Caps::builder("video/x-av1")
                .field("parsed", true)
                .field("stream-format", "obu-stream")
                .field("alignment", "tu")
                .build(),
        ),
        "vp8" => (
            "vp8enc",
            "",
            "identity",
            gst::Caps::builder("video/x-vp8").build(),
        ),
        other => return Err(format!("Unsupported GStreamer video codec: {other}")),
    };
    let encoder = crate::gstreamer_publisher::selected_encoder_name(codec);
    if !crate::gstreamer_publisher::can_initialize_element(encoder) {
        return Err(format!(
            "GStreamer encoder unavailable for {codec}: {software} or {hardware}"
        ));
    }
    if gst::ElementFactory::find(parser).is_none() {
        return Err(format!(
            "GStreamer parser unavailable for {codec}: {parser}"
        ));
    }
    Ok((encoder, parser, caps))
}

fn configure_encoder(
    encoder: &gst::Element,
    codec: &str,
    bitrate: u32,
    height: u32,
    key_int_max: u32,
) {
    if encoder.find_property("bitrate").is_some() {
        encoder.set_property_from_str("bitrate", &bitrate.to_string());
    } else if encoder.find_property("target-bitrate").is_some() {
        // `target-bitrate` units differ by plugin: VPx (vp8enc/vp9enc) take
        // bits/sec, but av1enc/svtav1enc take *kilobits*/sec — scaling by
        // 1000 for those (8 Mbps ceiling -> 8,000,000 "kbps") makes the
        // encoder reject the input caps at set_format time (`not-negotiated`).
        let value = if target_bitrate_is_kbps(encoder) {
            bitrate
        } else {
            bitrate.saturating_mul(1000).min(i32::MAX as u32)
        };
        encoder.set_property_from_str("target-bitrate", &value.to_string());
    }
    if encoder.find_property("key-int-max").is_some() {
        encoder.set_property_from_str("key-int-max", &key_int_max.to_string());
    }
    if encoder.find_property("keyframe-max-dist").is_some() {
        encoder.set_property_from_str("keyframe-max-dist", &key_int_max.to_string());
    }
    if codec == "h264" {
        if encoder.find_property("target-percentage").is_some() {
            encoder.set_property_from_str("target-percentage", &VBR_TARGET_PERCENTAGE.to_string());
        }
        if encoder.find_property("target-usage").is_some() {
            encoder.set_property_from_str("target-usage", "7");
        }
        if encoder.find_property("ref-frames").is_some() {
            encoder.set_property_from_str("ref-frames", "1");
        }
        if encoder.find_property("b-frames").is_some() {
            encoder.set_property_from_str("b-frames", "0");
        }
        if encoder.find_property("cabac").is_some() {
            encoder.set_property("cabac", false);
        }
        if encoder.find_property("dct8x8").is_some() {
            encoder.set_property("dct8x8", false);
        }
        if encoder.find_property("rate-control").is_some() {
            encoder.set_property_from_str("rate-control", "vbr");
        }
        // The x264enc fallback (no VA display) disables B-frames above but
        // otherwise keeps its lookahead default, whose buffering can stall a
        // pipeline that contains queues. Pin it to zerolatency so the software
        // path behaves like the low-latency VA path instead of holding frames.
        if encoder
            .factory()
            .is_some_and(|factory| factory.name() == "x264enc")
        {
            if encoder.find_property("tune").is_some() {
                encoder.set_property_from_str("tune", "zerolatency");
            }
            if encoder.find_property("speed-preset").is_some() {
                encoder.set_property_from_str("speed-preset", "veryfast");
            }
            if encoder.find_property("rc-lookahead").is_some() {
                encoder.set_property("rc-lookahead", 0_u32);
            }
            if encoder.find_property("sync-lookahead").is_some() {
                encoder.set_property("sync-lookahead", 0_i32);
            }
        }
    } else {
        if encoder.find_property("end-usage").is_some() {
            encoder.set_property_from_str("end-usage", "vbr");
        }
        if encoder.find_property("deadline").is_some() {
            encoder.set_property_from_str("deadline", "1");
        }
        if encoder.find_property("lag-in-frames").is_some() {
            encoder.set_property_from_str("lag-in-frames", "0");
        }
        if codec == "vp9" {
            if encoder.find_property("cpu-used").is_some() {
                encoder.set_property_from_str("cpu-used", "8");
            }
            if encoder.find_property("row-mt").is_some() {
                encoder.set_property("row-mt", true);
            }
            if encoder.find_property("tile-columns").is_some() {
                encoder.set_property_from_str("tile-columns", "2");
            }
            if encoder.find_property("static-threshold").is_some() {
                encoder.set_property_from_str("static-threshold", "100");
            }
        } else if codec == "av1" {
            // SVT-AV1 (svtav1enc): the shared path above sets `target-bitrate`.
            // Here we pin the quality floor and the speed/quality preset, and
            // set the keyframe interval in frames (SVT uses
            // `intra-period-length` instead of `keyframe-max-dist`).
            if encoder.find_property("max-qp-allowed").is_some() {
                encoder.set_property_from_str("max-qp-allowed", &AV1_MAX_QP_ALLOWED.to_string());
            }
            if encoder.find_property("preset").is_some() {
                // Preset scales with resolution: higher resolutions need more
                // encode parallelism to stay realtime (10 at 1080p, 11 at
                // 1440p, 12 at 4K).
                encoder.set_property_from_str("preset", &svt_preset_for(height).to_string());
            }
            if encoder.find_property("intra-period-length").is_some() {
                encoder.set_property_from_str("intra-period-length", &key_int_max.to_string());
            }
        }
    }
}

/// SVT-AV1 speed/quality preset for a frame height. Real-time screenshare
/// sits in the live band (10–12): preset 10 sustains 1080p60 on modest
/// hardware, while larger frames need the extra throughput of 11 (1440p) and
/// 12 (4K). Below 1080p reuses preset 10 — it is comfortably real-time.
fn svt_preset_for(height: u32) -> u32 {
    if height >= 2160 {
        return 12;
    }
    if height >= 1440 {
        return 11;
    }

    10
}

fn make_element(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|error| format!("Failed to create GStreamer element {name}: {error}"))
}

/// Whether the encoder's `target-bitrate` property is in kilobits/sec.
/// `av1enc` (libaom) and `svtav1enc` (SVT-AV1) document kilobits/sec; `VPx`
/// encoders document bits/sec. Reading the *property blurb* is fragile across
/// versions, so discriminate on the element's factory name — the pipeline only
/// ever instantiates `svtav1enc`, `vp8enc`, or `vp9enc` with this property.
fn target_bitrate_is_kbps(encoder: &gst::Element) -> bool {
    encoder
        .factory()
        .is_some_and(|factory| factory.name() == "svtav1enc")
}

fn encoder_target_kbps(encoder: &gst::Element, ceiling_kbps: u32) -> u32 {
    let percentage = if target_bitrate_is_kbps(encoder) {
        AV1_TARGET_PERCENTAGE
    } else {
        VBR_TARGET_PERCENTAGE
    };

    vbr_target_kbps(ceiling_kbps, percentage)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "validated UI bitrate values are finite, positive, and far below u32::MAX kbps"
)]
pub(crate) fn bitrate_bps_to_kbps(bitrate_bps: f64) -> u32 {
    if !bitrate_bps.is_finite() || bitrate_bps <= 0.0 {
        return 1;
    }

    (bitrate_bps / 1000.0)
        .round()
        .clamp(1.0, f64::from(u32::MAX)) as u32
}

/// VBR target bitrate in kbps for a given ceiling: `ceiling × percentage /
/// 100`, floored to whole kbps. Because `vah264enc` derives the maximum as
/// `target × 100 / percentage`, the floor keeps the driver-side maximum at
/// or below the ceiling (never above) for every ceiling value.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the u64 product of two u32s is clamped to u32::MAX before the lossless narrowing"
)]
fn vbr_target_kbps(ceiling_kbps: u32, percentage: u32) -> u32 {
    (u64::from(ceiling_kbps) * u64::from(percentage) / 100).clamp(1, u64::from(u32::MAX)) as u32
}

fn frame_duration(fps: u32) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(gst::ClockTime::SECOND.nseconds() / u64::from(fps.max(1)))
}

#[allow(
    clippy::too_many_arguments,
    reason = "plane copy requires explicit source and destination layout metadata"
)]
fn copy_plane(
    source: &[u8],
    source_stride: u32,
    destination: &mut [u8],
    destination_offset: usize,
    destination_stride: i32,
    row_width: u32,
    row_count: u32,
) -> Result<(), String> {
    let destination_stride = usize::try_from(destination_stride)
        .map_err(|_| "GStreamer I420 plane has a negative stride")?;
    let source_stride =
        usize::try_from(source_stride).map_err(|_| "I420 source stride overflow")?;
    let row_width = usize::try_from(row_width).map_err(|_| "I420 plane width overflow")?;
    let row_count = usize::try_from(row_count).map_err(|_| "I420 plane height overflow")?;
    if row_width > source_stride || row_width > destination_stride {
        return Err("I420 plane row width exceeds its stride".into());
    }

    for row in 0..row_count {
        let source_start = row
            .checked_mul(source_stride)
            .ok_or_else(|| "I420 source row offset overflow".to_string())?;
        let destination_start = destination_offset
            .checked_add(
                row.checked_mul(destination_stride)
                    .ok_or_else(|| "GStreamer destination row offset overflow".to_string())?,
            )
            .ok_or_else(|| "GStreamer destination plane offset overflow".to_string())?;
        let source_row = source
            .get(source_start..source_start + row_width)
            .ok_or_else(|| "I420 source plane is shorter than its declared layout".to_string())?;
        let destination_row = destination
            .get_mut(destination_start..destination_start + row_width)
            .ok_or_else(|| "GStreamer buffer is shorter than its VideoInfo layout".to_string())?;

        destination_row.copy_from_slice(source_row);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_conversion_uses_kilobits_per_second() {
        assert_eq!(bitrate_bps_to_kbps(20_000_000.0), 20_000);
        assert_eq!(bitrate_bps_to_kbps(8_000_499.0), 8_000);
        assert_eq!(bitrate_bps_to_kbps(8_000_500.0), 8_001);
        assert_eq!(bitrate_bps_to_kbps(f64::NAN), 1);
    }

    #[test]
    fn vbr_target_is_a_fraction_of_the_ceiling() {
        assert_eq!(vbr_target_kbps(20_000, 80), 16_000);
        assert_eq!(vbr_target_kbps(8_000, 80), 6_400);
        assert_eq!(vbr_target_kbps(1_000, 80), 800);
    }

    #[test]
    fn vbr_driver_max_never_exceeds_the_ceiling() {
        // vah264enc derives the encoder maximum as
        // `target × 100 / target-percentage`; the floor in
        // `vbr_target_kbps` must keep that maximum ≤ the ceiling.
        for ceiling in [1_u32, 500, 1_000, 2_400, 8_000, 20_000, 50_000, u32::MAX] {
            let target = vbr_target_kbps(ceiling, 80);
            let driver_max = u64::from(target) * 100u64 / u64::from(VBR_TARGET_PERCENTAGE);
            assert!(driver_max <= u64::from(ceiling), "ceiling {ceiling}");
        }
    }

    #[test]
    fn vbr_target_never_drops_below_one_kbps() {
        assert_eq!(vbr_target_kbps(1, 80), 1);
        assert_eq!(vbr_target_kbps(0, 80), 1);
    }

    #[test]
    fn vp9_and_av1_use_software_encoders() {
        assert_eq!(
            crate::gstreamer_publisher::selected_encoder_name("vp9"),
            "vp9enc"
        );
        assert_eq!(
            crate::gstreamer_publisher::selected_encoder_name("av1"),
            "svtav1enc"
        );
    }

    #[test]
    fn vp9_output_is_parsed_and_frame_aligned() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let (_encoder, parser, caps) = codec_pipeline("vp9")?;
        let structure = caps
            .structure(0)
            .ok_or_else(|| "VP9 output caps have no structure".to_string())?;

        assert_eq!(parser, "vp9parse");
        assert_eq!(structure.get::<&str>("chroma-format"), Ok("4:2:0"));
        assert_eq!(structure.get::<bool>("parsed"), Ok(true));
        assert_eq!(structure.get::<&str>("alignment"), Ok("frame"));
        // `profile` and the bit-depth fields must be absent: the parser cannot
        // promise them before the first frame, so pinning them on the output
        // capsfilter causes `not-negotiated`.
        assert!(structure.get::<&str>("profile").is_err());
        assert!(structure.get::<u32>("bit-depth-luma").is_err());
        assert!(structure.get::<u32>("bit-depth-chroma").is_err());

        Ok(())
    }

    #[test]
    fn av1_target_is_a_fraction_of_the_ceiling() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("svtav1enc")
            .build()
            .map_err(|error| error.to_string())?;
        let ceiling_kbps = 8_000;
        let target_kbps = encoder_target_kbps(&encoder, ceiling_kbps);
        configure_encoder(&encoder, "av1", target_kbps, 1080, 60);

        assert_eq!(target_kbps, 6_000);
        assert_eq!(encoder.property::<u32>("target-bitrate"), 6_000);
        assert_eq!(
            encoder.property::<u32>("max-qp-allowed"),
            AV1_MAX_QP_ALLOWED
        );
        assert_eq!(encoder.property::<u32>("preset"), 10);
        assert_eq!(encoder.property::<i32>("intra-period-length"), 60);

        Ok(())
    }

    #[test]
    fn av1_preset_scales_with_resolution() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("svtav1enc")
            .build()
            .map_err(|error| error.to_string())?;
        let target_kbps = encoder_target_kbps(&encoder, 12_000);
        configure_encoder(&encoder, "av1", target_kbps, 1440, 60);
        assert_eq!(encoder.property::<u32>("preset"), 11);

        let encoder = gst::ElementFactory::make("svtav1enc")
            .build()
            .map_err(|error| error.to_string())?;
        let target_kbps = encoder_target_kbps(&encoder, 20_000);
        configure_encoder(&encoder, "av1", target_kbps, 2160, 60);
        assert_eq!(encoder.property::<u32>("preset"), 12);

        Ok(())
    }
}
