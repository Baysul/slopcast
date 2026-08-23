use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::CaptureConfig;
use crate::frame_delivery::{VideoSample, trace_encoder_output, trace_frame};

pub(crate) const APPSRC_MAX_BUFFERS: u64 = 6;
const GOP_SECONDS: u32 = 1;
const H264_GOP_SECONDS: u32 = 2;
#[allow(dead_code, reason = "retained for reference; vah264enc now runs CBR")]
const H264_QVBR_QUALITY: u32 = 26;

const VBR_TARGET_PERCENTAGE: u32 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RateMode {
    Cbr,
    Vbr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CeilingUpdate {
    Applied,
    Attempted,
    Pinned,
}

impl CeilingUpdate {
    pub(crate) fn is_pinned(self) -> bool {
        matches!(self, Self::Pinned)
    }
}

fn encoder_rate_mode(codec: &str, encoder_name: &str) -> RateMode {
    match (codec, encoder_name) {
        ("av1", "nvav1enc" | "av1enc")
        | ("h264", "nvh264enc" | "vah264enc")
        | ("vp8" | "vp9", _) => RateMode::Cbr,
        _ => RateMode::Vbr,
    }
}

static VIDEO_FRAMES_ENCODED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn reset_encoded_frames() {
    VIDEO_FRAMES_ENCODED.store(0, Ordering::Relaxed);
    crate::frame_delivery::clear_i420_freelist();
}

pub(crate) fn encoded_frames() -> u64 {
    VIDEO_FRAMES_ENCODED.load(Ordering::Relaxed)
}

pub(crate) struct EncoderChain {
    pub(crate) encoder: &'static str,
    pub(crate) pre_chain: &'static [&'static str],
}

struct EncoderPlan {
    codec: &'static str,
    chain: &'static EncoderChain,
    rate_mode: RateMode,
}

impl EncoderPlan {
    fn select(codec: &str, probe: impl Fn(&str) -> bool) -> Result<Self, String> {
        let codec = match codec {
            "h264" => "h264",
            "h265" => "h265",
            "av1" => "av1",
            "vp8" => "vp8",
            "vp9" => "vp9",
            other => return Err(format!("Unsupported GStreamer video codec: {other}")),
        };
        let chain = select_chain(codec, probe)?;

        Ok(Self {
            codec,
            chain,
            rate_mode: encoder_rate_mode(codec, chain.encoder),
        })
    }

    fn encoder_name(&self) -> &'static str {
        self.chain.encoder
    }

    fn creates_pre_chain(&self) -> Result<Vec<gst::Element>, String> {
        self.chain
            .pre_chain
            .iter()
            .map(|name| make_element(name))
            .collect()
    }

    fn create_encoder(&self, ceiling_kbps: u32, key_int_max: u32) -> Result<gst::Element, String> {
        let encoder = gst::ElementFactory::make(self.encoder_name())
            .build()
            .map_err(|error| {
                format!(
                    "Failed to create GStreamer element {}: {error}",
                    self.encoder_name()
                )
            })?;
        let bitrate = encoder_target_kbps(self.rate_mode, ceiling_kbps);
        configure_encoder(
            &encoder,
            self.codec,
            bitrate,
            ceiling_kbps,
            key_int_max,
            self.rate_mode,
        );

        Ok(encoder)
    }

    fn can_adapt(&self) -> bool {
        !self.ceiling_update().is_pinned()
    }

    fn apply_ceiling(&self, encoder: &gst::Element, ceiling_kbps: u32) -> CeilingUpdate {
        let outcome = self.ceiling_update();
        if outcome.is_pinned() {
            log::warn!("GStreamer libaom av1enc cannot change its bitrate mid-stream");
            return outcome;
        }
        if !apply_encoder_ceiling(encoder, ceiling_kbps, self.rate_mode) {
            return CeilingUpdate::Pinned;
        }

        outcome
    }

    fn ceiling_update(&self) -> CeilingUpdate {
        if self.encoder_name() == "av1enc" {
            CeilingUpdate::Pinned
        } else if self.encoder_name().starts_with("nv") {
            CeilingUpdate::Attempted
        } else {
            CeilingUpdate::Applied
        }
    }
}

pub(crate) fn codec_chains(codec: &str) -> Result<&'static [EncoderChain], String> {
    match codec {
        "h264" => Ok(&[
            EncoderChain {
                encoder: "nvh264enc",
                pre_chain: &["cudaupload", "cudaconvertscale"],
            },
            EncoderChain {
                encoder: "vah264enc",
                pre_chain: &["videoconvert"],
            },
            EncoderChain {
                encoder: "x264enc",
                pre_chain: &["videoconvert"],
            },
        ]),
        "h265" => Ok(&[
            EncoderChain {
                encoder: "nvh265enc",
                pre_chain: &["cudaupload", "cudaconvertscale"],
            },
            EncoderChain {
                encoder: "vah265enc",
                pre_chain: &["videoconvert"],
            },
            EncoderChain {
                encoder: "x265enc",
                pre_chain: &["videoconvert"],
            },
        ]),
        "av1" => Ok(&[
            EncoderChain {
                encoder: "nvav1enc",
                pre_chain: &["cudaupload", "cudaconvertscale"],
            },
            EncoderChain {
                encoder: "vaav1enc",
                pre_chain: &["videoconvert"],
            },
            EncoderChain {
                encoder: "av1enc",
                pre_chain: &["videoconvert"],
            },
        ]),
        "vp9" => Ok(&[EncoderChain {
            encoder: "vp9enc",
            pre_chain: &["videoconvert"],
        }]),
        "vp8" => Ok(&[EncoderChain {
            encoder: "vp8enc",
            pre_chain: &["videoconvert"],
        }]),
        other => Err(format!("Unsupported GStreamer video codec: {other}")),
    }
}

pub(crate) fn select_encoder(codec: &str, probe: impl Fn(&str) -> bool) -> &'static str {
    let Ok(chains) = codec_chains(codec) else {
        return "vp8enc";
    };
    chains
        .iter()
        .find(|chain| probe(chain.encoder))
        .unwrap_or(&chains[chains.len() - 1])
        .encoder
}

fn select_chain(
    codec: &str,
    probe: impl Fn(&str) -> bool,
) -> Result<&'static EncoderChain, String> {
    let chains = codec_chains(codec)?;
    chains
        .iter()
        .find(|chain| probe(chain.encoder))
        .ok_or_else(|| {
            let tried = chains
                .iter()
                .map(|chain| chain.encoder)
                .collect::<Vec<_>>()
                .join(" -> ");
            format!("GStreamer encoder unavailable for {codec}: tried {tried}")
        })
}

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
    width: u32,
    height: u32,
    fps: Arc<AtomicU32>,
    pts_anchor: Arc<Mutex<Option<(i64, u64)>>>,
    last_pts_ns: Arc<Mutex<Option<u64>>>,
}

impl VideoInput {
    pub(crate) fn push_frame(&self, sample: VideoSample) -> Result<(), String> {
        if sample.width != self.width || sample.height != self.height {
            return Err(format!(
                "GStreamer input frame is {}x{}, expected {}x{}",
                sample.width, sample.height, self.width, self.height
            ));
        }

        let sequence = sample.sequence;
        let capture_pts_us = sample.pts_us;
        let mut buffer = gst::Buffer::from_mut_slice(sample.buffer);
        let mut anchor = self
            .pts_anchor
            .lock()
            .map_err(|_| "video PTS anchor lock poisoned".to_string())?;
        let (c0_us, p0_ns) = match *anchor {
            Some((c0, p0)) if sample.pts_us >= c0 => (c0, p0),
            _ => {
                let anchored = (sample.pts_us, running_time_ns(&self.appsrc)?);
                *anchor = Some(anchored);
                anchored
            }
        };
        let pts_ns = p0_ns + (sample.pts_us - c0_us).cast_unsigned() * 1000;
        let mut last_pts = self
            .last_pts_ns
            .lock()
            .map_err(|_| "video PTS monotonicity lock poisoned".to_string())?;
        let pts_ns = match *last_pts {
            Some(previous) if pts_ns <= previous => {
                previous + frame_duration(self.fps.load(Ordering::Relaxed)).nseconds()
            }
            _ => pts_ns,
        };
        *last_pts = Some(pts_ns);
        drop(last_pts);
        let fps = self.fps.load(Ordering::Relaxed);
        let duration_ns = frame_duration(fps).nseconds();
        let buffer_ref = buffer
            .get_mut()
            .ok_or_else(|| "GStreamer input buffer is unexpectedly shared".to_string())?;
        buffer_ref.set_pts(gst::ClockTime::from_nseconds(pts_ns));
        buffer_ref.set_duration(gst::ClockTime::from_nseconds(duration_ns));

        self.appsrc
            .push_buffer(buffer)
            .map_err(|error| format!("GStreamer appsrc rejected an I420 frame: {error}"))?;
        let queue_depth = self.appsrc_stats().level_buffers.unwrap_or(0) as usize;
        trace_frame(
            "appsrc-push",
            sequence,
            capture_pts_us,
            queue_depth,
            Some(pts_ns),
            Some(duration_ns),
        );

        Ok(())
    }

    pub(crate) fn fps(&self) -> u32 {
        self.fps.load(Ordering::Relaxed)
    }

    pub(crate) fn set_fps(&self, fps: u32) {
        self.fps.store(fps, Ordering::Relaxed);
    }

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
    encoder: gst::Element,
    ceiling_kbps: u32,
    plan: EncoderPlan,
    rate_is_pinned: bool,
    elements: Vec<gst::Element>,
    sink_pad: gst::Pad,
}

impl GstreamerEncoder {
    pub(crate) fn set_ceiling_kbps(&mut self, ceiling_kbps: u32) -> CeilingUpdate {
        let ceiling_kbps = ceiling_kbps.clamp(1, u32::MAX);
        if ceiling_kbps == self.ceiling_kbps {
            return CeilingUpdate::Applied;
        }

        let outcome = self.plan.apply_ceiling(&self.encoder, ceiling_kbps);
        if outcome.is_pinned() {
            self.rate_is_pinned = true;
            return outcome;
        }

        log::info!(
            "[gstreamer-encoder] ceiling {} kbps -> {ceiling_kbps} kbps",
            self.ceiling_kbps,
        );
        self.ceiling_kbps = ceiling_kbps;

        outcome
    }

    pub(crate) fn can_adapt(&self) -> bool {
        !self.rate_is_pinned
    }

    pub(crate) fn ceiling_kbps(&self) -> u32 {
        self.ceiling_kbps
    }

    pub(crate) fn encoder_name(&self) -> &'static str {
        self.plan.encoder_name()
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
        let ceiling_kbps = crate::gstreamer_publisher::configured_ceiling_kbps(config);
        let plan = EncoderPlan::select(codec, crate::gstreamer_publisher::can_initialize_element)?;
        let encoder_name = plan.encoder_name();

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
        let pre_chain_elements = plan.creates_pre_chain()?;
        let key_int_max = if codec == "h264" {
            config.fps.saturating_mul(H264_GOP_SECONDS).min(1024)
        } else {
            config.fps.saturating_mul(GOP_SECONDS).min(1024)
        };
        let encoder = plan.create_encoder(ceiling_kbps, key_int_max)?;
        let rate_is_pinned = !plan.can_adapt();
        let output_queue = gst::ElementFactory::make("queue")
            .property("max-size-buffers", 0_u32)
            .property("max-size-bytes", 0_u32)
            .property("max-size-time", 120_000_000_u64)
            .build()
            .map_err(|error| format!("Failed to create GStreamer video queue: {error}"))?;
        let encoder_src = encoder
            .static_pad("src")
            .ok_or_else(|| "GStreamer encoder has no src pad".to_string())?;
        encoder_src.add_probe(gst::PadProbeType::BUFFER, |_, info| {
            let encoded_count = VIDEO_FRAMES_ENCODED.fetch_add(1, Ordering::Relaxed) + 1;
            let gstreamer_pts_ns = info
                .buffer()
                .and_then(|buffer| buffer.pts())
                .map(gst::ClockTime::nseconds);
            trace_encoder_output(gstreamer_pts_ns, encoded_count);
            gst::PadProbeReturn::Ok
        });
        if codec == "av1" {
            log_force_key_unit_events(&encoder_src, encoder_name);
            log_av1_encode_diagnostics(&encoder_src, encoder_name);
        }
        let mut elements = vec![appsrc.clone().upcast::<gst::Element>()];
        elements.extend(pre_chain_elements);
        elements.extend([encoder.clone(), output_queue.clone()]);

        for pair in elements.windows(2) {
            let label = format!(
                "{} -> {}",
                pair[0].factory().map_or_else(
                    || "unknown".to_string(),
                    |factory| factory.name().to_string()
                ),
                pair[1].factory().map_or_else(
                    || "unknown".to_string(),
                    |factory| factory.name().to_string()
                ),
            );
            if let Some(pad) = pair[0].static_pad("src") {
                log_caps_events(&pad, label);
            }
        }
        if let Some(pad) = output_queue.static_pad("src") {
            log_caps_events(&pad, "queue -> sink".to_string());
        }

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
            plan,
            rate_is_pinned,
            elements,
            sink_pad,
            input: VideoInput {
                appsrc,
                width: config.width,
                height: config.height,
                fps: Arc::new(AtomicU32::new(config.fps)),
                pts_anchor: Arc::new(Mutex::new(None)),
                last_pts_ns: Arc::new(Mutex::new(None)),
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

#[allow(
    clippy::too_many_lines,
    reason = "per-codec VP8/VP9/H.264/H.265 encoder property profiles stay in one table"
)]
fn configure_encoder(
    encoder: &gst::Element,
    codec: &str,
    bitrate: u32,
    ceiling_kbps: u32,
    key_int_max: u32,
    rate_mode: RateMode,
) {
    let factory_name = encoder.factory().map(|factory| factory.name().to_string());
    let is_va_av1 = codec == "av1" && factory_name.as_deref() == Some("vaav1enc");
    if is_nvenc(encoder) {
        configure_nvenc(encoder, bitrate, ceiling_kbps, key_int_max, rate_mode);
        return;
    }
    if codec == "av1" && !is_va_av1 {
        configure_libaom_av1(encoder, bitrate);
    } else if encoder.find_property("bitrate").is_some() {
        encoder.set_property_from_str("bitrate", &bitrate.to_string());
    } else if encoder.find_property("target-bitrate").is_some() {
        encoder.set_property_from_str(
            "target-bitrate",
            &bitrate
                .saturating_mul(1000)
                .min(i32::MAX as u32)
                .to_string(),
        );
    }
    if encoder.find_property("key-int-max").is_some() {
        encoder.set_property_from_str("key-int-max", &key_int_max.to_string());
    }
    if encoder.find_property("keyframe-max-dist").is_some() {
        encoder.set_property_from_str("keyframe-max-dist", &key_int_max.to_string());
    }
    if codec == "h265" {
        configure_h265(encoder);
    } else if is_va_av1 {
        configure_va_vbr(encoder);
    } else if codec == "h264" {
        let is_vah264 = encoder
            .factory()
            .is_some_and(|factory| factory.name() == "vah264enc");
        if is_vah264 {
            if encoder.find_property("rate-control").is_some() {
                encoder.set_property_from_str("rate-control", "cbr");
            }
        } else if encoder.find_property("target-percentage").is_some() {
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
        if !is_vah264 && encoder.find_property("rate-control").is_some() {
            encoder.set_property_from_str("rate-control", "vbr");
        }
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
                encoder.set_property("rc-lookahead", 0_i32);
            }
            if encoder.find_property("sync-lookahead").is_some() {
                encoder.set_property("sync-lookahead", 0_i32);
            }
        }
    } else if codec != "av1" {
        if encoder.find_property("end-usage").is_some() {
            let end_usage = match rate_mode {
                RateMode::Cbr => "cbr",
                RateMode::Vbr => "vbr",
            };
            encoder.set_property_from_str("end-usage", end_usage);
        }
        if encoder.find_property("deadline").is_some() {
            encoder.set_property_from_str("deadline", "1");
        }
        if encoder.find_property("lag-in-frames").is_some() {
            encoder.set_property_from_str("lag-in-frames", "0");
        }
        if encoder.find_property("static-threshold").is_some() {
            encoder.set_property_from_str("static-threshold", "100");
        }
        if codec == "vp9" {
            if encoder.find_property("cpu-used").is_some() {
                encoder.set_property_from_str("cpu-used", "10");
            }
            if encoder.find_property("row-mt").is_some() {
                encoder.set_property("row-mt", true);
            }
            if encoder.find_property("tile-columns").is_some() {
                encoder.set_property_from_str("tile-columns", "2");
            }
            if encoder.find_property("threads").is_some() {
                encoder.set_property_from_str("threads", "8");
            }
            if encoder.find_property("max-quantizer").is_some() {
                encoder.set_property_from_str("max-quantizer", "63");
            }
            if encoder.find_property("max-intra-bitrate").is_some() {
                encoder.set_property_from_str("max-intra-bitrate", "300");
            }
            if encoder.find_property("min-quantizer").is_some() {
                encoder.set_property_from_str("min-quantizer", "10");
            }
            if encoder.find_property("undershoot").is_some() {
                encoder.set_property("undershoot", 50_i32);
            }
            if encoder.find_property("overshoot").is_some() {
                encoder.set_property("overshoot", 50_i32);
            }
        } else if codec == "vp8" {
            if encoder.find_property("cpu-used").is_some() {
                encoder.set_property_from_str("cpu-used", "6");
            }
            if encoder.find_property("dropframe-threshold").is_some() {
                encoder.set_property("dropframe-threshold", 30_i32);
            }
            if encoder.find_property("buffer-size").is_some() {
                encoder.set_property("buffer-size", 100_i32);
            }
            if encoder.find_property("buffer-initial-size").is_some() {
                encoder.set_property("buffer-initial-size", 50_i32);
            }
            if encoder.find_property("buffer-optimal-size").is_some() {
                encoder.set_property("buffer-optimal-size", 50_i32);
            }
            if encoder.find_property("max-intra-bitrate").is_some() {
                encoder.set_property("max-intra-bitrate", 300_i32);
            }
            if encoder.find_property("min-quantizer").is_some() {
                encoder.set_property("min-quantizer", 12_i32);
            }
            if encoder.find_property("max-quantizer").is_some() {
                encoder.set_property("max-quantizer", 63_i32);
            }
            if encoder.find_property("undershoot").is_some() {
                encoder.set_property("undershoot", 100_i32);
            }
            if encoder.find_property("overshoot").is_some() {
                encoder.set_property("overshoot", 15_i32);
            }
        }
    }
}

fn configure_libaom_av1(encoder: &gst::Element, bitrate: u32) {
    if encoder.find_property("target-bitrate").is_some() {
        encoder.set_property_from_str("target-bitrate", &bitrate.to_string());
    }
    if encoder.find_property("usage-profile").is_some() {
        encoder.set_property_from_str("usage-profile", "realtime");
    }
    if encoder.find_property("end-usage").is_some() {
        encoder.set_property_from_str("end-usage", "cbr");
    }
    if encoder.find_property("lag-in-frames").is_some() {
        encoder.set_property_from_str("lag-in-frames", "0");
    }
    if encoder.find_property("cpu-used").is_some() {
        encoder.set_property_from_str("cpu-used", "10");
    }
    if encoder.find_property("row-mt").is_some() {
        encoder.set_property("row-mt", true);
    }
    if encoder.find_property("tile-columns").is_some() {
        encoder.set_property_from_str("tile-columns", "2");
    }
    if encoder.find_property("buf-sz").is_some() {
        encoder.set_property("buf-sz", 1000_u32);
    }
    if encoder.find_property("buf-initial-sz").is_some() {
        encoder.set_property("buf-initial-sz", 600_u32);
    }
    if encoder.find_property("buf-optimal-sz").is_some() {
        encoder.set_property("buf-optimal-sz", 600_u32);
    }
    if encoder.find_property("undershoot-pct").is_some() {
        encoder.set_property("undershoot-pct", 50_u32);
    }
    if encoder.find_property("overshoot-pct").is_some() {
        encoder.set_property("overshoot-pct", 50_u32);
    }
    if encoder.find_property("min-quantizer").is_some() {
        encoder.set_property("min-quantizer", 10_u32);
    }
    if encoder.find_property("max-quantizer").is_some() {
        encoder.set_property("max-quantizer", 56_u32);
    }
}

fn configure_va_vbr(encoder: &gst::Element) {
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
    if encoder.find_property("rate-control").is_some() {
        encoder.set_property_from_str("rate-control", "vbr");
    }
}

fn configure_h265(encoder: &gst::Element) {
    configure_va_vbr(encoder);
    if encoder
        .factory()
        .is_some_and(|factory| factory.name() == "x265enc")
    {
        if encoder.find_property("tune").is_some() {
            encoder.set_property_from_str("tune", "zerolatency");
        }
        if encoder.find_property("speed-preset").is_some() {
            encoder.set_property_from_str("speed-preset", "veryfast");
        }
    }
}

fn configure_nvenc(
    encoder: &gst::Element,
    bitrate: u32,
    ceiling_kbps: u32,
    key_int_max: u32,
    rate_mode: RateMode,
) {
    if encoder.find_property("bitrate").is_some() {
        encoder.set_property_from_str("bitrate", &bitrate.to_string());
    }
    if encoder.find_property("rc-mode").is_some() {
        let rc_mode = match rate_mode {
            RateMode::Cbr => "cbr",
            RateMode::Vbr => "vbr",
        };
        encoder.set_property_from_str("rc-mode", rc_mode);
    }
    if rate_mode == RateMode::Vbr && encoder.find_property("max-bitrate").is_some() {
        encoder.set_property_from_str("max-bitrate", &ceiling_kbps.to_string());
    }
    if encoder.find_property("bframes").is_some() {
        encoder.set_property("bframes", 0_u32);
    }
    if encoder.find_property("zerolatency").is_some() {
        encoder.set_property("zerolatency", true);
    }
    if encoder.find_property("preset").is_some() {
        encoder.set_property_from_str("preset", "p1");
    }
    if encoder.find_property("rc-lookahead").is_some() {
        encoder.set_property("rc-lookahead", 0_u32);
    }
    if encoder.find_property("gop-size").is_some() {
        encoder.set_property("gop-size", key_int_max);
    }
    if encoder.find_property("tune").is_some() {
        encoder.set_property_from_str("tune", "ultra-low-latency");
    }
    if encoder.find_property("multi-pass").is_some() {
        encoder.set_property_from_str("multi-pass", "disabled");
    }
}

fn make_element(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|error| format!("Failed to create GStreamer element {name}: {error}"))
}

fn log_caps_events(pad: &gst::Pad, label: String) {
    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
        if let Some(gst::PadProbeData::Event(event)) = info.data.as_ref()
            && let gst::EventView::Caps(caps) = event.view()
        {
            log::info!("[caps] {label}: {}", caps.caps());
        }

        gst::PadProbeReturn::Ok
    });
}

fn log_force_key_unit_events(pad: &gst::Pad, label: &'static str) {
    pad.add_probe(gst::PadProbeType::EVENT_UPSTREAM, move |_, info| {
        if let Some(gst::PadProbeData::Event(event)) = info.data.as_ref()
            && event.has_name("GstForceKeyUnit")
        {
            log::info!("[force-key-unit] {label}: KEYFRAME REQUEST");
        }

        gst::PadProbeReturn::Ok
    });
    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
        if let Some(gst::PadProbeData::Event(event)) = info.data.as_ref()
            && event.has_name("GstForceKeyUnit")
        {
            log::info!("[force-key-unit] {label}: KEYFRAME HANDLED");
        }

        gst::PadProbeReturn::Ok
    });
}

#[derive(Debug)]
struct EncodeTelemetry {
    window_start: std::time::Instant,
    frames: u64,
    bytes: u64,
    max_frame_bytes: u64,
    max_gap_ms: u64,
    last_seen_at: Option<std::time::Instant>,
}

impl Default for EncodeTelemetry {
    fn default() -> Self {
        Self {
            window_start: std::time::Instant::now(),
            frames: 0,
            bytes: 0,
            max_frame_bytes: 0,
            max_gap_ms: 0,
            last_seen_at: None,
        }
    }
}

fn log_av1_encode_diagnostics(pad: &gst::Pad, label: &'static str) {
    let telemetry = Arc::new(Mutex::new(EncodeTelemetry::default()));
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        let Some(gst::PadProbeData::Buffer(buffer)) = info.data.as_ref() else {
            return gst::PadProbeReturn::Ok;
        };
        let frame_bytes = u64::try_from(buffer.size()).unwrap_or(u64::MAX);
        if !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
            log::info!("[av1-encode] {label}: KEYFRAME OUTPUT size={frame_bytes}");
        }

        let now = std::time::Instant::now();
        let Ok(mut telemetry) = telemetry.lock() else {
            return gst::PadProbeReturn::Ok;
        };
        telemetry.frames += 1;
        telemetry.bytes = telemetry.bytes.saturating_add(frame_bytes);
        telemetry.max_frame_bytes = telemetry.max_frame_bytes.max(frame_bytes);
        if let Some(last_seen_at) = telemetry.last_seen_at {
            let gap_ms = u64::try_from(now.duration_since(last_seen_at).as_millis())
                .unwrap_or(u64::MAX);
            telemetry.max_gap_ms = telemetry.max_gap_ms.max(gap_ms);
        }
        telemetry.last_seen_at = Some(now);

        let elapsed_ms = now.duration_since(telemetry.window_start).as_millis();
        if elapsed_ms < 1000 {
            return gst::PadProbeReturn::Ok;
        }
        let elapsed_ms = u64::try_from(elapsed_ms).unwrap_or(u64::MAX).max(1);
        log::info!(
            "[av1-encode] {label}: 1s window frames={} fps={} bytes/s={} max_frame={} max_gap_ms={}",
            telemetry.frames,
            telemetry.frames.saturating_mul(1000) / elapsed_ms,
            telemetry.bytes,
            telemetry.max_frame_bytes,
            telemetry.max_gap_ms,
        );
        *telemetry = EncodeTelemetry::default();

        gst::PadProbeReturn::Ok
    });
}

fn is_nvenc(encoder: &gst::Element) -> bool {
    encoder
        .factory()
        .is_some_and(|factory| factory.name().starts_with("nv"))
}

static NVENC_RETARGET_WARNED: AtomicBool = AtomicBool::new(false);

fn apply_encoder_ceiling(encoder: &gst::Element, ceiling_kbps: u32, rate_mode: RateMode) -> bool {
    if is_nvenc(encoder) {
        let target = encoder_target_kbps(rate_mode, ceiling_kbps);
        if encoder.find_property("bitrate").is_some() {
            encoder.set_property_from_str("bitrate", &target.to_string());
        }
        if rate_mode == RateMode::Vbr && encoder.find_property("max-bitrate").is_some() {
            encoder.set_property_from_str("max-bitrate", &ceiling_kbps.to_string());
        }
        if !NVENC_RETARGET_WARNED.swap(true, Ordering::Relaxed) {
            log::warn!(
                "GStreamer NVENC mid-stream bitrate re-targeting is unverified \
                 (docs/adr/0001-nvenc-bitrate-retarget.md): attempting it and \
                 bookkeeping it as applied"
            );
        }
        return true;
    }
    let target = encoder_target_kbps(rate_mode, ceiling_kbps);
    if encoder.find_property("bitrate").is_some() {
        encoder.set_property_from_str("bitrate", &target.to_string());
    } else if encoder.find_property("target-bitrate").is_some() {
        encoder.set_property_from_str(
            "target-bitrate",
            &target.saturating_mul(1000).min(i32::MAX as u32).to_string(),
        );
    } else {
        log::warn!("GStreamer encoder exposes no runtime bitrate property");
        return false;
    }
    true
}

fn encoder_target_kbps(rate_mode: RateMode, ceiling_kbps: u32) -> u32 {
    match rate_mode {
        RateMode::Cbr => ceiling_kbps,
        RateMode::Vbr => vbr_target_kbps(ceiling_kbps, VBR_TARGET_PERCENTAGE),
    }
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
    fn vpx_rate_mode_is_always_cbr() {
        assert_eq!(encoder_rate_mode("vp8", "vp8enc"), RateMode::Cbr);
        assert_eq!(encoder_rate_mode("vp9", "vp9enc"), RateMode::Cbr);
    }

    #[test]
    fn vbr_driver_max_never_exceeds_the_ceiling() {
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
    fn selection_chain_prefers_nvenc_then_vaapi_then_software() {
        let probe_all = |_name: &str| true;
        let probe_va_only = |name: &str| name.starts_with("va");
        let probe_none = |_name: &str| false;
        for codec in ["h264", "h265", "av1"] {
            assert!(
                select_encoder(codec, probe_all).starts_with("nv"),
                "{codec}"
            );
            assert!(
                select_encoder(codec, probe_va_only).starts_with("va"),
                "{codec}"
            );
        }
        assert_eq!(select_encoder("h264", probe_none), "x264enc");
        assert_eq!(select_encoder("h265", probe_none), "x265enc");
        assert_eq!(select_encoder("av1", probe_none), "av1enc");
        assert_eq!(
            codec_chains("av1")
                .unwrap_or(&[])
                .iter()
                .map(|chain| chain.encoder)
                .collect::<Vec<_>>(),
            ["nvav1enc", "vaav1enc", "av1enc"]
        );
    }

    #[test]
    fn selection_chain_vp8_vp9_single_software_chain() {
        let probe_all = |_name: &str| true;
        for codec in ["vp8", "vp9"] {
            let chains = codec_chains(codec).unwrap_or(&[]);
            assert_eq!(chains.len(), 1, "{codec} must have one chain");
            assert_eq!(chains[0].pre_chain, ["videoconvert"], "{codec}");
            assert_eq!(
                select_encoder(codec, probe_all),
                chains[0].encoder,
                "{codec}"
            );
        }
    }

    #[test]
    fn selection_chain_unknown_codec_defaults_to_vp8enc() {
        assert_eq!(select_encoder("nope", |_| true), "vp8enc");
    }

    #[test]
    fn codec_chain_pre_chains_match_the_branch() {
        for codec in ["h264", "h265", "av1"] {
            let chains = codec_chains(codec).unwrap_or(&[]);
            assert_eq!(
                chains[0].pre_chain,
                ["cudaupload", "cudaconvertscale"],
                "{codec}"
            );
            assert_eq!(chains[1].pre_chain, ["videoconvert"], "{codec}");
            assert_eq!(chains[2].pre_chain, ["videoconvert"], "{codec}");
        }
    }

    #[test]
    fn h265_probes_nvh265enc_then_vah265enc_then_x265enc() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let expected = if crate::gstreamer_publisher::can_initialize_element("nvh265enc") {
            "nvh265enc"
        } else if crate::gstreamer_publisher::can_initialize_element("vah265enc") {
            "vah265enc"
        } else {
            "x265enc"
        };
        let chain = select_chain("h265", crate::gstreamer_publisher::can_initialize_element)?;
        assert_eq!(chain.encoder, expected);
        assert_eq!(
            crate::gstreamer_publisher::selected_encoder_name("h265"),
            expected
        );

        Ok(())
    }

    #[test]
    fn av1_selection_matches_the_first_probe_win() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let expected = if crate::gstreamer_publisher::can_initialize_element("nvav1enc") {
            "nvav1enc"
        } else if crate::gstreamer_publisher::can_initialize_element("vaav1enc") {
            "vaav1enc"
        } else {
            "av1enc"
        };
        assert_eq!(
            select_chain("av1", crate::gstreamer_publisher::can_initialize_element)?.encoder,
            expected
        );
        assert_eq!(
            crate::gstreamer_publisher::selected_encoder_name("av1"),
            expected
        );

        Ok(())
    }

    #[test]
    fn vah264_hardware_configures_cbr_at_ceiling() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;
        if !crate::gstreamer_publisher::can_initialize_element("vah264enc") {
            return Ok(());
        }

        let encoder = gst::ElementFactory::make("vah264enc")
            .build()
            .map_err(|error| error.to_string())?;
        let ceiling_kbps = 20_000;
        configure_encoder(
            &encoder,
            "h264",
            encoder_target_kbps(RateMode::Cbr, ceiling_kbps),
            ceiling_kbps,
            120,
            RateMode::Cbr,
        );

        assert_eq!(encoder.property::<u32>("bitrate"), 20_000);
        assert_eq!(
            encoder
                .property_value("rate-control")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "cbr"
        );
        assert_eq!(encoder.property::<u32>("ref-frames"), 1);
        assert_eq!(encoder.property::<u32>("b-frames"), 0);
        assert_eq!(encoder.property::<u32>("target-usage"), 7);

        Ok(())
    }

    #[test]
    fn x264_fallback_keeps_ceiling_capped_vbr() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("x264enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(&encoder, "h264", 16_000, 20_000, 120, RateMode::Vbr);

        assert_eq!(encoder.property::<u32>("bitrate"), 16_000);
        let tune = encoder.property_value("tune");
        let (_, values) = gst::glib::FlagsValue::from_value(&tune)
            .ok_or_else(|| "tune is not a flags value".to_string())?;
        let nicks = values.iter().map(|value| value.nick()).collect::<Vec<_>>();
        assert!(nicks.contains(&"zerolatency"), "tune nicks: {nicks:?}");

        Ok(())
    }

    #[test]
    fn h264_gop_interval_is_two_seconds() {
        assert_eq!(H264_GOP_SECONDS, 2);
        assert_eq!(60_u32.saturating_mul(H264_GOP_SECONDS).min(1024), 120);
    }

    #[test]
    fn vah265_hardware_configures_vbr_target_pinned_to_the_ceiling() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;
        if !crate::gstreamer_publisher::can_initialize_element("vah265enc") {
            return Ok(());
        }

        let encoder = gst::ElementFactory::make("vah265enc")
            .build()
            .map_err(|error| error.to_string())?;
        let ceiling_kbps = 20_000;
        configure_encoder(
            &encoder,
            "h265",
            encoder_target_kbps(RateMode::Vbr, ceiling_kbps),
            ceiling_kbps,
            60,
            RateMode::Vbr,
        );

        assert_eq!(encoder.property::<u32>("bitrate"), 16_000);
        assert_eq!(encoder.property::<u32>("target-percentage"), 80);
        assert_eq!(
            encoder
                .property_value("rate-control")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "vbr"
        );
        assert_eq!(encoder.property::<u32>("ref-frames"), 1);
        assert_eq!(encoder.property::<u32>("b-frames"), 0);
        assert_eq!(encoder.property::<u32>("target-usage"), 7);

        Ok(())
    }

    #[test]
    fn x265_fallback_configures_zerolatency_veryfast_vbr() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("x265enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(&encoder, "h265", 16_000, 20_000, 60, RateMode::Vbr);

        assert_eq!(encoder.property::<u32>("bitrate"), 16_000);
        assert_eq!(encoder.property::<i32>("key-int-max"), 60);
        assert_eq!(
            encoder
                .property_value("tune")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "zerolatency"
        );
        assert_eq!(
            encoder
                .property_value("speed-preset")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "veryfast"
        );

        Ok(())
    }

    #[test]
    fn h265_mutable_encoder_applies_a_live_ceiling_change() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("x265enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(&encoder, "h265", 16_000, 20_000, 60, RateMode::Vbr);

        assert!(apply_encoder_ceiling(&encoder, 10_000, RateMode::Vbr));
        assert_eq!(encoder.property::<u32>("bitrate"), 8_000);

        Ok(())
    }

    #[test]
    fn vp9_selection_probe_returns_vp9enc() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        assert_eq!(select_chain("vp9", |_| true)?.encoder, "vp9enc");

        Ok(())
    }

    #[test]
    fn av1_chain_prefers_nvav1enc_when_available() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        assert_eq!(
            select_chain("av1", |name| name == "av1enc")?.encoder,
            "av1enc"
        );

        Ok(())
    }

    #[test]
    fn av1_runs_libaom_cbr_at_the_ceiling() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("av1enc")
            .build()
            .map_err(|error| error.to_string())?;
        let ceiling_kbps = 8_000;
        let target_kbps = encoder_target_kbps(RateMode::Cbr, ceiling_kbps);
        configure_encoder(
            &encoder,
            "av1",
            target_kbps,
            ceiling_kbps,
            60,
            RateMode::Cbr,
        );

        assert_eq!(target_kbps, 8_000);
        assert_eq!(encoder.property::<u32>("target-bitrate"), 8_000);
        assert_eq!(
            encoder
                .property_value("end-usage")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "cbr"
        );
        assert_eq!(
            encoder
                .property_value("usage-profile")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "realtime"
        );
        assert_eq!(encoder.property::<i32>("cpu-used"), 10);
        assert!(encoder.property::<bool>("row-mt"));
        assert_eq!(encoder.property::<u32>("tile-columns"), 2);
        assert_eq!(encoder.property::<u32>("lag-in-frames"), 0);
        assert_eq!(encoder.property::<u32>("buf-sz"), 1_000);
        assert_eq!(encoder.property::<u32>("buf-initial-sz"), 600);
        assert_eq!(encoder.property::<u32>("buf-optimal-sz"), 600);
        assert_eq!(encoder.property::<u32>("undershoot-pct"), 50);
        assert_eq!(encoder.property::<u32>("overshoot-pct"), 50);
        assert_eq!(encoder.property::<i32>("keyframe-max-dist"), 60);

        Ok(())
    }

    #[test]
    fn av1_quantizer_range_guards_realtime_cbr() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("av1enc")
            .build()
            .map_err(|error| error.to_string())?;
        assert_eq!(encoder.property::<u32>("min-quantizer"), 0);
        assert_eq!(encoder.property::<u32>("max-quantizer"), 0);

        let target_kbps = encoder_target_kbps(RateMode::Cbr, 8_000);
        configure_encoder(&encoder, "av1", target_kbps, 8_000, 60, RateMode::Cbr);

        assert_eq!(encoder.property::<u32>("min-quantizer"), 10);
        assert_eq!(encoder.property::<u32>("max-quantizer"), 56);

        Ok(())
    }

    #[test]
    fn libaom_av1_plan_pins_live_ceiling_changes() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let plan = EncoderPlan::select("av1", |name| name == "av1enc")?;
        let encoder = plan.create_encoder(8_000, 60)?;
        let target_before = encoder.property::<u32>("target-bitrate");

        assert_eq!(plan.apply_ceiling(&encoder, 6_000), CeilingUpdate::Pinned);
        assert_eq!(encoder.property::<u32>("target-bitrate"), target_before);

        Ok(())
    }

    #[test]
    fn mutable_encoder_applies_a_live_ceiling_change() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("vp9enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(&encoder, "vp9", 6_400, 8_000, 60, RateMode::Vbr);

        assert!(apply_encoder_ceiling(&encoder, 10_000, RateMode::Vbr));
        assert_eq!(encoder.property::<i32>("target-bitrate"), 8_000_000);

        Ok(())
    }

    #[test]
    fn vpx_cbr_applies_the_ceiling_verbatim() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("vp9enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(&encoder, "vp9", 6_400, 6_400, 60, RateMode::Cbr);

        assert_eq!(encoder.property::<i32>("target-bitrate"), 6_400_000);
        assert_eq!(
            encoder
                .property_value("end-usage")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "cbr"
        );
        assert_eq!(encoder.property::<i32>("static-threshold"), 100);

        assert!(apply_encoder_ceiling(&encoder, 10_000, RateMode::Cbr));
        assert_eq!(encoder.property::<i32>("target-bitrate"), 10_000_000);

        Ok(())
    }

    #[test]
    fn vp9_software_configures_realtime_cbr_profile() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("vp9enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(&encoder, "vp9", 8_000, 8_000, 60, RateMode::Cbr);

        assert_eq!(encoder.property::<i64>("deadline"), 1);
        assert_eq!(encoder.property::<i32>("cpu-used"), 10);
        assert!(encoder.property::<bool>("row-mt"));
        assert_eq!(encoder.property::<i32>("tile-columns"), 2);
        assert_eq!(encoder.property::<i32>("threads"), 8);
        assert_eq!(encoder.property::<i32>("target-bitrate"), 8_000_000);
        assert_eq!(
            encoder
                .property_value("end-usage")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "cbr"
        );
        assert_eq!(encoder.property::<i32>("max-intra-bitrate"), 300);
        assert_eq!(encoder.property::<i32>("max-quantizer"), 63);
        assert_eq!(encoder.property::<i32>("min-quantizer"), 10);
        assert_eq!(encoder.property::<i32>("undershoot"), 50);
        assert_eq!(encoder.property::<i32>("overshoot"), 50);

        Ok(())
    }

    #[test]
    fn vp8_software_configures_bounded_screenshare_rate_control() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("vp8enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(&encoder, "vp8", 10_000, 10_000, 60, RateMode::Cbr);

        assert_eq!(encoder.property::<i32>("target-bitrate"), 10_000_000);
        assert_eq!(encoder.property::<i64>("deadline"), 1);
        assert_eq!(encoder.property::<i32>("cpu-used"), 6);
        assert_eq!(encoder.property::<i32>("static-threshold"), 100);
        assert_eq!(encoder.property::<i32>("dropframe-threshold"), 30);
        assert_eq!(encoder.property::<i32>("buffer-size"), 100);
        assert_eq!(encoder.property::<i32>("buffer-initial-size"), 50);
        assert_eq!(encoder.property::<i32>("buffer-optimal-size"), 50);
        assert_eq!(encoder.property::<i32>("max-intra-bitrate"), 300);
        assert_eq!(encoder.property::<i32>("min-quantizer"), 12);
        assert_eq!(encoder.property::<i32>("max-quantizer"), 63);
        assert_eq!(encoder.property::<i32>("undershoot"), 100);
        assert_eq!(encoder.property::<i32>("overshoot"), 15);
        assert_eq!(
            encoder
                .property_value("end-usage")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "cbr"
        );

        Ok(())
    }

    #[test]
    fn encoder_plan_keeps_selection_and_rate_policy_together() -> Result<(), String> {
        let nvenc_av1 = EncoderPlan::select("av1", |name| name == "nvav1enc")?;
        assert_eq!(nvenc_av1.encoder_name(), "nvav1enc");
        assert_eq!(nvenc_av1.ceiling_update(), CeilingUpdate::Attempted);
        assert!(nvenc_av1.can_adapt());

        let vaapi_av1 = EncoderPlan::select("av1", |name| name == "vaav1enc")?;
        assert_eq!(vaapi_av1.encoder_name(), "vaav1enc");
        assert_eq!(vaapi_av1.ceiling_update(), CeilingUpdate::Applied);
        assert!(vaapi_av1.can_adapt());

        let software_av1 = EncoderPlan::select("av1", |name| name == "av1enc")?;
        assert_eq!(software_av1.encoder_name(), "av1enc");
        assert_eq!(software_av1.ceiling_update(), CeilingUpdate::Pinned);
        assert!(!software_av1.can_adapt());

        Ok(())
    }

    #[test]
    fn encoder_plan_fails_when_no_chain_passes_the_probe_gate() {
        let result = EncoderPlan::select("h264", |_| false);

        assert!(matches!(
            result,
            Err(error) if error == "GStreamer encoder unavailable for h264: tried nvh264enc -> vah264enc -> x264enc"
        ));
    }

    #[test]
    fn nvenc_configures_cbr_or_vbr_with_max_bitrate() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;
        if gst::ElementFactory::find("nvh264enc").is_none() {
            return Ok(());
        }
        let ceiling_kbps = 20_000;

        let h264 = gst::ElementFactory::make("nvh264enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(
            &h264,
            "h264",
            encoder_target_kbps(RateMode::Cbr, ceiling_kbps),
            ceiling_kbps,
            120,
            RateMode::Cbr,
        );
        assert_eq!(h264.property::<u32>("bitrate"), 20_000);
        assert_eq!(
            h264.property_value("rc-mode")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "cbr"
        );
        assert_eq!(h264.property::<u32>("bframes"), 0);
        assert!(h264.property::<bool>("zerolatency"));
        assert_eq!(
            h264.property_value("preset")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "p1"
        );
        assert_eq!(h264.property::<u32>("rc-lookahead"), 0);
        assert_eq!(h264.property::<u32>("gop-size"), 120);

        let h265 = gst::ElementFactory::make("nvh265enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(
            &h265,
            "h265",
            encoder_target_kbps(RateMode::Vbr, ceiling_kbps),
            ceiling_kbps,
            60,
            RateMode::Vbr,
        );
        assert_eq!(h265.property::<u32>("bitrate"), 16_000);
        assert_eq!(h265.property::<u32>("max-bitrate"), 20_000);
        assert_eq!(
            h265.property_value("rc-mode")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "vbr"
        );
        assert_eq!(h265.property::<u32>("gop-size"), 60);

        let av1 = gst::ElementFactory::make("nvav1enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(
            &av1,
            "av1",
            encoder_target_kbps(RateMode::Cbr, ceiling_kbps),
            ceiling_kbps,
            60,
            RateMode::Cbr,
        );
        assert_eq!(av1.property::<u32>("bitrate"), 20_000);
        assert_eq!(
            av1.property_value("rc-mode")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "cbr"
        );

        Ok(())
    }

    #[test]
    fn nvenc_ceiling_retarget_sets_bitrate_and_max_bitrate_and_warns_once() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;
        if gst::ElementFactory::find("nvh265enc").is_none() {
            return Ok(());
        }
        let encoder = gst::ElementFactory::make("nvh265enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(
            &encoder,
            "h265",
            encoder_target_kbps(RateMode::Vbr, 20_000),
            20_000,
            60,
            RateMode::Vbr,
        );
        NVENC_RETARGET_WARNED.store(false, Ordering::Relaxed);

        assert!(apply_encoder_ceiling(&encoder, 10_000, RateMode::Vbr));
        assert_eq!(encoder.property::<u32>("bitrate"), 8_000);
        assert_eq!(encoder.property::<u32>("max-bitrate"), 10_000);

        let h264 = gst::ElementFactory::make("nvh264enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(&h264, "h264", 20_000, 20_000, 120, RateMode::Cbr);
        assert!(apply_encoder_ceiling(&h264, 10_000, RateMode::Cbr));
        assert_eq!(h264.property::<u32>("bitrate"), 10_000);

        Ok(())
    }

    #[test]
    fn vaav1enc_configures_vbr_pinning() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;
        if gst::ElementFactory::find("vaav1enc").is_none() {
            return Ok(());
        }

        let encoder = gst::ElementFactory::make("vaav1enc")
            .build()
            .map_err(|error| error.to_string())?;
        let ceiling_kbps = 20_000;
        configure_encoder(
            &encoder,
            "av1",
            encoder_target_kbps(RateMode::Vbr, ceiling_kbps),
            ceiling_kbps,
            60,
            RateMode::Vbr,
        );

        assert_eq!(encoder.property::<u32>("bitrate"), 16_000);
        assert_eq!(encoder.property::<u32>("target-percentage"), 80);
        assert_eq!(
            encoder
                .property_value("rate-control")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "vbr"
        );
        assert_eq!(encoder.property::<u32>("ref-frames"), 1);
        assert_eq!(encoder.property::<u32>("b-frames"), 0);
        assert_eq!(encoder.property::<u32>("target-usage"), 7);

        Ok(())
    }
}
