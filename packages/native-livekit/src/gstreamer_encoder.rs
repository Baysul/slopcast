//! Linux H.264 branch attached to `livekitwebrtcsink`.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use livekit::webrtc::prelude::{I420Buffer, VideoBuffer, VideoFrame};

use crate::CaptureConfig;

const APPSRC_MAX_BUFFERS: u64 = 2;
const GOP_SECONDS: u32 = 5;

#[derive(Clone)]
pub(crate) struct VideoInput {
    appsrc: gst_app::AppSrc,
    input_info: gst_video::VideoInfo,
    width: u32,
    height: u32,
    fps: u32,
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
        let timestamp_us = u64::try_from(frame.timestamp_us)
            .map_err(|_| "GStreamer input timestamp must be non-negative")?;
        let buffer_ref = buffer
            .get_mut()
            .ok_or_else(|| "GStreamer input buffer is unexpectedly shared".to_string())?;
        buffer_ref.set_pts(gst::ClockTime::from_useconds(timestamp_us));
        buffer_ref.set_duration(frame_duration(self.fps));

        self.appsrc
            .push_buffer(buffer)
            .map_err(|error| format!("GStreamer appsrc rejected an I420 frame: {error}"))?;

        Ok(())
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
            .field("stream-format", "byte-stream")
            .field("alignment", "au")
            .field("profile", "constrained-baseline")
            .build();
        let appsrc = gst_app::AppSrc::builder()
            .caps(&input_caps)
            .format(gst::Format::Time)
            .is_live(true)
            .block(false)
            .max_buffers(APPSRC_MAX_BUFFERS)
            .leaky_type(gst_app::AppLeakyType::Downstream)
            .build();
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
            .property("max-size-buffers", 2_u32)
            .property("max-size-bytes", 0_u32)
            .property("max-size-time", 0_u64)
            .property_from_str("leaky", "downstream")
            .build()
            .map_err(|error| format!("Failed to create GStreamer video queue: {error}"))?;
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
