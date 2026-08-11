//! Linux H.264 branch attached to `livekitwebrtcsink`.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use livekit::webrtc::prelude::{I420Buffer, VideoBuffer, VideoFrame};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::CaptureConfig;

const APPSRC_MAX_BUFFERS: u64 = 2;
const GOP_SECONDS: u32 = 1;

/// Number of encoded H.264 access units that emerged from `h264parse`
/// (i.e. actually encoded and pushed downstream), as opposed to
/// `VIDEO_FRAMES_SUBMITTED` which counts frames pushed into the appsrc.
/// This is the true encoder-throughput counter; any gap vs. the submitted
/// count is frames dropped by the backpressure path (leaky appsrc or the
/// 200 ms time-bounded queue) before they could be encoded.
static VIDEO_FRAMES_ENCODED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn reset_encoded_frames() {
    VIDEO_FRAMES_ENCODED.store(0, Ordering::Relaxed);
}

pub(crate) fn encoded_frames() -> u64 {
    VIDEO_FRAMES_ENCODED.load(Ordering::Relaxed)
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

        let mut buffer = gst::Buffer::with_size(self.input_info.size())
            .map_err(|error| format!("Failed to allocate GStreamer input buffer: {error}"))?;
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

    /// Live appsrc statistics — `dropped` counts buffers the appsrc
    /// discarded (leaky downstream on a full 2-buffer queue), the stutter
    /// diagnostic for the encoder-side publish path.
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
}

impl GstreamerEncoder {
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
            return Err("vah264enc requires a frame size of at least 128x128".into());
        }
        if config.fps == 0 {
            return Err("GStreamer encoder fps must be greater than zero".into());
        }
        if config.video_codec.as_deref() != Some("h264") {
            return Err("Linux GStreamer publishing currently supports only H.264".into());
        }

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
        let output_caps = gst::Caps::builder("video/x-h264")
            .field("stream-format", "avc")
            .field("alignment", "au")
            .field("profile", "constrained-baseline")
            .build();
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
        let convert = make_element("videoconvert")?;
        let requested_rate = config.max_bitrate.unwrap_or(20_000_000.0);
        let encoder_rate = bitrate_bps_to_kbps(requested_rate);
        let key_int_max = config.fps.saturating_mul(GOP_SECONDS).min(1024);
        let encoder = gst::ElementFactory::make("vah264enc")
            .property("b-frames", 0_u32)
            .property("bitrate", encoder_rate)
            .property("cabac", false)
            .property("key-int-max", key_int_max)
            .property("ref-frames", 1_u32)
            .property("target-usage", 7_u32)
            .property_from_str("rate-control", "cbr")
            .build()
            .map_err(|error| format!("Failed to create GStreamer element vah264enc: {error}"))?;
        let parser = gst::ElementFactory::make("h264parse")
            .property("config-interval", -1_i32)
            .build()
            .map_err(|error| format!("Failed to create GStreamer element h264parse: {error}"))?;
        let capsfilter = gst::ElementFactory::make("capsfilter")
            .property("caps", &output_caps)
            .build()
            .map_err(|error| format!("Failed to create GStreamer H.264 capsfilter: {error}"))?;
        let output_queue = gst::ElementFactory::make("queue")
            .property("max-size-buffers", 0_u32)
            .property("max-size-bytes", 0_u32)
            .property("max-size-time", 200_000_000_u64) // 200 ms
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
            encoder,
            parser,
            capsfilter,
            output_queue.clone(),
        ];

        pipeline
            .add_many(elements.iter())
            .map_err(|error| format!("Failed to add GStreamer H.264 elements: {error}"))?;
        gst::Element::link_many(elements.iter())
            .map_err(|error| format!("Failed to link GStreamer H.264 pipeline: {error}"))?;
        let sink_pad = sink
            .request_pad_simple("video_%u")
            .ok_or_else(|| "livekitwebrtcsink refused a video pad".to_string())?;
        output_queue
            .static_pad("src")
            .ok_or_else(|| "GStreamer video queue has no src pad".to_string())?
            .link(&sink_pad)
            .map_err(|error| format!("Failed to link H.264 into livekitwebrtcsink: {error}"))?;
        for element in &elements {
            element
                .sync_state_with_parent()
                .map_err(|error| format!("Failed to start GStreamer H.264 element: {error}"))?;
        }

        Ok(Self {
            input: VideoInput {
                appsrc,
                input_info,
                width: config.width,
                height: config.height,
                fps: config.fps,
                pts_anchor: Arc::new(Mutex::new(None)),
            },
        })
    }

    pub(crate) fn input(&self) -> VideoInput {
        self.input.clone()
    }
}

fn make_element(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|error| format!("Failed to create GStreamer element {name}: {error}"))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "validated UI bitrate values are finite, positive, and far below u32::MAX kbps"
)]
fn bitrate_bps_to_kbps(bitrate_bps: f64) -> u32 {
    if !bitrate_bps.is_finite() || bitrate_bps <= 0.0 {
        return 1;
    }

    (bitrate_bps / 1000.0)
        .round()
        .clamp(1.0, f64::from(u32::MAX)) as u32
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
}
