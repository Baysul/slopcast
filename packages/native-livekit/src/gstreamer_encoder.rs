//! Linux video encoder branch attached to `livekitwebrtcsink`.
//!
//! `vah264enc` runs CBR at the configured ceiling: the `bitrate` is applied
//! verbatim as the encoder's target for predictable output under the
//! real-time requirement; `vah265enc` keeps VBR with the same ceiling
//! pinning via `target-percentage`. The software
//! encoders run CBR (`VPx` in automatic mode, libaom `av1enc` always) or
//! the ceiling-capped VBR quality target (manual-mode `VPx`,
//! `x264enc`/`x265enc` always). The publisher's congestion controller
//! re-targets each encoder at runtime through
//! `GstreamerEncoder::set_ceiling_kbps`.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::CaptureConfig;
use crate::desktop_capture::VideoSample;

/// Video appsrc queue depth in buffers. Two was too tight: any transient
/// encoder hiccup (VA-API pipeline stall, driver swap) saturated it
/// instantly and dropped frames continuously. Six buffers (~100 ms at
/// 60 fps) absorbs short encoder jitter while keeping the queue's
/// freshness bound well under a typical receiver jitter budget. The
/// publisher's congestion controller reads this as the "queue is full"
/// threshold for its local backpressure signal.
pub(crate) const APPSRC_MAX_BUFFERS: u64 = 6;
const GOP_SECONDS: u32 = 1;
/// H.264 IDR interval in seconds. VBR quality intra frames are large
/// (multi-megabit), so the shared 1 s GOP bursts 2-4 Mbit into the
/// non-leaky output queue every 60 frames — at 20 Mbps ceilings that
/// overflows the queue and the receiver's jitter buffer (periodic hitch).
/// Two seconds halves the burst frequency without meaningfully slowing
/// join-time keyframe recovery.
const H264_GOP_SECONDS: u32 = 2;
/// Base quality factor for `vah264enc` QVBR (`qpi`): retained for reference;
/// the `vah264enc` path now runs CBR and does not use `qpi`.
#[allow(dead_code, reason = "retained for reference; vah264enc now runs CBR")]
const H264_QVBR_QUALITY: u32 = 26;

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

/// Rate-control mode for the software encoders. CBR pins the output to the
/// ceiling: with software encoding and a strict real-time requirement,
/// predictable output is worth more than VBR quality bursts, and the
/// congestion controller's ceiling step is directly observable as bits/s.
/// VBR keeps the ceiling-capped quality-target behavior for manual/high
/// bitrate sessions and for the `vah265enc` hardware path (`vah264enc`
/// runs QVBR, whose `bitrate` semantics match VBR's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RateMode {
    Cbr,
    Vbr,
}

/// Number of encoded access units that emerged from the codec parser
/// (i.e. actually encoded and pushed downstream), as opposed to
/// `VIDEO_FRAMES_SUBMITTED` which counts frames pushed into the appsrc.
/// This is the true encoder-throughput counter; any gap vs. the submitted
/// count is frames dropped by the backpressure path (leaky appsrc or the
/// 200 ms time-bounded queue) before they could be encoded.
static VIDEO_FRAMES_ENCODED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn reset_encoded_frames() {
    VIDEO_FRAMES_ENCODED.store(0, Ordering::Relaxed);
    crate::desktop_capture::clear_i420_freelist();
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
}

impl VideoInput {
    pub(crate) fn push_frame(&self, sample: VideoSample) -> Result<(), String> {
        if sample.width != self.width || sample.height != self.height {
            return Err(format!(
                "GStreamer input frame is {}x{}, expected {}x{}",
                sample.width, sample.height, self.width, self.height
            ));
        }

        // Wrap the capture engine's plane allocation directly (zero copy):
        // the frame was converted into this buffer, the appsrc pipeline
        // reads it in place, and `OwnedI420::drop` returns the allocation to
        // the capture engine's freelist when the pipeline releases it.
        let mut buffer = gst::Buffer::from_mut_slice(sample.buffer);
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
            Some((c0, p0)) if sample.pts_us >= c0 => (c0, p0),
            _ => {
                let anchored = (sample.pts_us, running_time_ns(&self.appsrc)?);
                *anchor = Some(anchored);
                anchored
            }
        };
        // Capture anchors are i64; the anchor branch guarantees a
        // non-negative difference, so cast_unsigned is lossless.
        let pts_ns = p0_ns + (sample.pts_us - c0_us).cast_unsigned() * 1000;
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
    /// (leaky-downstream on a full 6-buffer queue). The capture engine's
    /// I420 freelist doubles as the input-buffer headroom, so there is no
    /// separate exhaustion signal here (see `push_frame`).
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
    /// Whether the encoder runs CBR (ceiling applied verbatim) or VBR
    /// (ceiling-capped quality target, see `VBR_TARGET_PERCENTAGE`). Drives
    /// how the ceiling maps onto the encoder's rate knob.
    rate_mode: RateMode,
    elements: Vec<gst::Element>,
    sink_pad: gst::Pad,
}

impl GstreamerEncoder {
    /// Re-targets the encoder at runtime: VA-API takes `bitrate` (the VBR
    /// target, `ceiling × target-percentage / 100`) and `VPx` takes
    /// `target-bitrate` (the ceiling itself in CBR, the VBR target in VBR
    /// mode). libaom `av1enc`'s CBR rate path is not verified to accept
    /// mid-stream `target-bitrate` reconfiguration, so it reports `false`
    /// and leaves `ceiling_kbps` untouched (the encoder keeps its configured
    /// cap; a `true`/`false` return keeps the caller's state from pretending
    /// the change landed). The VA-API and `VPx` rate controllers pick the
    /// new rate up on the next encoded frame — no pipeline rebuild, so this
    /// is safe to call from the publisher's ~1 s congestion control tick
    /// even mid-stream.
    ///
    /// Returns `true` when the new ceiling was actually applied (or was
    /// already in effect), `false` when the encoder cannot change it live.
    pub(crate) fn set_ceiling_kbps(&mut self, ceiling_kbps: u32) -> bool {
        let ceiling_kbps = ceiling_kbps.clamp(1, u32::MAX);
        if ceiling_kbps == self.ceiling_kbps {
            return true;
        }
        if !apply_encoder_ceiling(&self.encoder, ceiling_kbps, self.rate_mode) {
            // The encoder cannot change its ceiling live; leave `ceiling_kbps`
            // untouched so our bookkeeping never lies about the real cap.
            return false;
        }
        log::info!(
            "[gstreamer-encoder] ceiling {} kbps -> {ceiling_kbps} kbps",
            self.ceiling_kbps,
        );
        self.ceiling_kbps = ceiling_kbps;
        true
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
        crate::gstreamer_publisher::verify_codec_elements(codec)?;
        let encoder_name = codec_pipeline(codec)?;

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
        // Rate mode: VP8/VP9 run CBR in automatic mode (predictable output
        // beats VBR bursts under a real-time requirement) and keep the
        // ceiling-capped VBR quality target in manual mode; libaom av1enc
        // always runs CBR at the ceiling; H.264 runs QVBR with the ceiling
        // as the encoder maximum and H.265 keeps VBR with the ceiling
        // pinned by `target-percentage` (see VBR_TARGET_PERCENTAGE).
        let rate_mode = match codec {
            "av1" => RateMode::Cbr,
            "h264" if encoder_name == "vah264enc" => RateMode::Cbr,
            "h264" | "h265" => RateMode::Vbr,
            _ if config.auto_bitrate => RateMode::Cbr,
            _ => RateMode::Vbr,
        };
        let key_int_max = if codec == "h264" {
            config.fps.saturating_mul(H264_GOP_SECONDS).min(1024)
        } else {
            config.fps.saturating_mul(GOP_SECONDS).min(1024)
        };
        let encoder = gst::ElementFactory::make(encoder_name)
            .build()
            .map_err(|error| {
                format!("Failed to create GStreamer element {encoder_name}: {error}")
            })?;
        let encoder_rate = encoder_target_kbps(rate_mode, ceiling_kbps);
        configure_encoder(&encoder, codec, encoder_rate, key_int_max, rate_mode);
        // On the bundled livekitwebrtcsink (gst-plugin-webrtc 0.15.3) we
        // observed the sink inserting its own codec parser (vp9parse/av1parse)
        // internally before its payloader and rejecting caps renegotiation on
        // its input pad; a second external parser caused `not-negotiated`
        // failures there (VP9/AV1 parser caps evolve after the first frame).
        // H.264 behavior on this exact build was less clear-cut, so the
        // external parsers are removed based on what we observed rather than
        // a claim that every bundled parser definitely exists and that AVCC
        // was definitely mis-parsed. Feed the encoder's raw output straight
        // to the sink and let its internal parser produce the caps the
        // payloader wants.
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
        // Count encoded access units as they leave the encoder. This is the
        // real encoder-throughput signal (`VIDEO_FRAMES_ENCODED`); it only
        // advances for frames that were actually encoded, so any shortfall
        // vs. `VIDEO_FRAMES_SUBMITTED` reveals the leaky-appsrc / queue
        // backpressure drops. The counter must be monotonic per capture
        // session, so it is reset on every `attach_video`/capture start.
        let encoder_src = encoder
            .static_pad("src")
            .ok_or_else(|| "GStreamer encoder has no src pad".to_string())?;
        encoder_src.add_probe(gst::PadProbeType::BUFFER, |_, _| {
            VIDEO_FRAMES_ENCODED.fetch_add(1, Ordering::Relaxed);
            gst::PadProbeReturn::Ok
        });
        // `keyframe-max-dist` bounds the automatic keyframe interval; verify
        // on-demand keyframe requests from `livekitwebrtcsink` also reach the
        // AV1 encoder, and audit whether each request actually produces a
        // keyframe.
        if codec == "av1" {
            log_force_key_unit_events(&encoder_src, "av1enc");
            log_av1_encode_diagnostics(&encoder_src, "av1enc");
        }
        let elements = vec![
            appsrc.clone().upcast(),
            convert,
            encoder.clone(),
            output_queue.clone(),
        ];

        // Diagnostic: attribute a `not-negotiated` to the stage where
        // downstream CAPS negotiation stopped.
        if let Some(pad) = appsrc.static_pad("src") {
            log_caps_events(&pad, "appsrc -> videoconvert");
        }
        if let Some(pad) = elements[1].static_pad("src") {
            log_caps_events(&pad, "videoconvert -> encoder");
        }
        if let Some(pad) = encoder.static_pad("src") {
            log_caps_events(&pad, "encoder -> queue");
        }
        if let Some(pad) = output_queue.static_pad("src") {
            log_caps_events(&pad, "queue -> sink");
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
            rate_mode,
            elements,
            sink_pad,
            input: VideoInput {
                appsrc,
                width: config.width,
                height: config.height,
                fps: config.fps,
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

fn codec_pipeline(codec: &str) -> Result<&'static str, String> {
    let (software, hardware) = match codec {
        "h264" => ("x264enc", "vah264enc"),
        "h265" => ("x265enc", "vah265enc"),
        "vp9" => ("vp9enc", "vavp9enc"),
        "av1" => ("av1enc", "vaav1enc"),
        "vp8" => ("vp8enc", ""),
        other => return Err(format!("Unsupported GStreamer video codec: {other}")),
    };
    let encoder = crate::gstreamer_publisher::selected_encoder_name(codec);
    if !crate::gstreamer_publisher::can_initialize_element(encoder) {
        return Err(format!(
            "GStreamer encoder unavailable for {codec}: {software} or {hardware}"
        ));
    }
    Ok(encoder)
}

#[allow(
    clippy::too_many_lines,
    reason = "per-codec VP8/VP9/H.264/H.265 encoder property profiles stay in one table"
)]
fn configure_encoder(
    encoder: &gst::Element,
    codec: &str,
    bitrate: u32,
    key_int_max: u32,
    rate_mode: RateMode,
) {
    if codec == "av1" {
        configure_libaom_av1(encoder, bitrate);
    } else if encoder.find_property("bitrate").is_some() {
        encoder.set_property_from_str("bitrate", &bitrate.to_string());
    } else if encoder.find_property("target-bitrate").is_some() {
        // `target-bitrate` units differ by plugin: VPx (vp8enc/vp9enc) take
        // bits/sec, unlike `bitrate` (kbps).
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
    } else if codec == "h264" {
        let is_vah264 = encoder
            .factory()
            .is_some_and(|factory| factory.name() == "vah264enc");
        if !is_vah264 && encoder.find_property("target-percentage").is_some() {
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
        if is_vah264 {
            // CBR at the ceiling: predictable output, no VBR/qvbr
            // window-averaged overshoot that burst multi-megabit I-frames
            // into the non-leaky output queue.
            if encoder.find_property("rate-control").is_some() {
                encoder.set_property_from_str("rate-control", "cbr");
            }
        } else if encoder.find_property("rate-control").is_some() {
            // The x264enc fallback has no QVBR mode; keep ceiling-capped VBR.
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
                encoder.set_property("rc-lookahead", 0_i32);
            }
            if encoder.find_property("sync-lookahead").is_some() {
                encoder.set_property("sync-lookahead", 0_i32);
            }
        }
    } else if codec != "av1" {
        // VP8/VP9 rate knobs; AV1 is fully configured in
        // `configure_libaom_av1`, so it must not pick these up.
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
        // Skip-frame threshold recommended for screen/window sharing: static
        // desktop content re-uses the previous frame instead of re-encoding,
        // which buys real-time headroom for the frames that actually move.
        if encoder.find_property("static-threshold").is_some() {
            encoder.set_property_from_str("static-threshold", "100");
        }
        if codec == "vp9" {
            // VP9 realtime screen-share profile: `cpu-used=10` favors
            // throughput so high-motion 1080p60 frames stay inside their
            // deadline, while row/tile parallelism spreads the work over
            // the encoder threads. The quantizer range lets CBR shed
            // quality instead of falling behind on complex scenes —
            // `max-quantizer=63` gives the controller headroom, while
            // `min-quantizer=10` (below) stops it from idling at QP 0 on
            // still content and overshooting when motion returns.
            // `max-intra-bitrate=300` (a libvpx percentage of the target)
            // bounds keyframe bursts on scene changes.
            // `error-resilient` is left at its default — WebRTC's own loss
            // recovery (PLI/keyframe requests) covers packet loss, so
            // libvpx's redundancy would only waste bandwidth.
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
            // Mirror the AV1/WebRTC treatment: `min-quantizer` (default 0)
            // is the hard quality floor — after a still stretch the CBR
            // buffer refills at the undershoot floor and QP walks down to
            // zero, so the first frames of returning motion overshoot until
            // the buffer drains (the "lazy" hitch). Pinning the floor to 10
            // stops the controller from idling in the near-lossless corner
            // it then has to recover from, while still spending freely on
            // truly static content (those frames never approach the floor).
            // `undershoot`/`overshoot` (default 25) cap the per-frame
            // target-size adjustment the CBR buffer correction may apply;
            // 50/50 matches WebRTC's libaom AV1 RTC path and keeps the
            // target closer to the optimal-buffer rate without letting a
            // single deviation swing the frame target more than 25%.
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
            // vp8enc defaults cpu-used to 0 (slowest): fast enough for the
            // average frame but a busy scene's worst-case encode time blows
            // the frame interval, and the encoder falls behind. 6 keeps the
            // worst case under the frame interval at screenshare
            // resolutions; target-bitrate/quantizer settings are the
            // quality controls, not this knob.
            if encoder.find_property("cpu-used").is_some() {
                encoder.set_property_from_str("cpu-used", "6");
            }
        }
    }
}

/// Applies the libaom `av1enc` realtime CBR configuration. The presenter's
/// ceiling is the `target-bitrate` itself — unlike `VPx` (bits/sec), av1enc's
/// is in kilobits/sec, so it is applied verbatim, never ×1000. `usage-profile`
/// picks the low-latency encoder path, `end-usage=cbr` holds the rate at the
/// target, and `lag-in-frames=0` disables lookahead so no frames buffer before
/// encode. `cpu-used` and the row/tile parallelism keep the software encoder
/// realtime at screenshare resolutions.
///
/// The rate-control buffer sizing and undershoot/overshoot targets mirror
/// WebRTC's libaom AV1 RTC path (its `rc_buf_sz`/`rc_buf_optimal_sz` of
/// 1000/600 ms and 50/50 undershoot/overshoot): av1enc's defaults are a
/// 6000 ms buffer tuned for offline encodes, which would let a burst ride
/// for seconds before CBR reacts.
///
/// The quantizer range must mirror WebRTC's wrapper too (`rc_min_quantizer`
/// 10, `rc_max_quantizer`/`qpMax` 56), because `GStreamer`'s `av1enc` defaults
/// `min-quantizer`/`max-quantizer` to **0/0** — the extreme high-quality end
/// of libaom's 0–63 Q range. With no Q headroom the rate controller cannot
/// shed quality when scene complexity spikes, so a busy scene pins the encoder
/// at ~max quality and the output blows far past the ceiling (measured ~40
/// Mbps from a 8 Mbps target during gameplay). These two properties are the
/// regression guard: `configure_libaom_av1` must never drop them, and the
/// test below asserts the exact values.
///
/// Keyframe placement follows the shared `keyframe-max-dist` interval
/// (`configure_encoder`); on-demand `GstForceKeyUnit` events from
/// `livekitwebrtcsink` still trigger intra frames early.
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
    // av1enc defaults min/max quantizer to 0 — the extreme high-quality end
    // of libaom's 0–63 Q range, giving the CBR controller no headroom to
    // shed quality when scene complexity spikes. Without a usable Q range a
    // busy scene pins the encoder at ~max quality and the output blows far
    // past the ceiling (observed ~40 Mbps on a 8 Mbps target during
    // gameplay). Mirror WebRTC's libaom AV1 wrapper: min Q 10, max Q 56.
    if encoder.find_property("min-quantizer").is_some() {
        encoder.set_property("min-quantizer", 10_u32);
    }
    if encoder.find_property("max-quantizer").is_some() {
        encoder.set_property("max-quantizer", 56_u32);
    }
}

/// Applies the H.265 low-latency VBR configuration shared by the VA-API
/// `vah265enc` hardware path and the `x265enc` software fallback. Both
/// encoders expose `bitrate` (kbps), which the congestion controller also
/// re-targets live (`apply_encoder_ceiling`), so H.265 inherits the
/// ceiling-capped VBR behavior of the H.264 branch.
///
/// `vah265enc`: `rate-control=vbr` + `target-percentage=80` pins the
/// driver-computed maximum exactly onto the presenter's ceiling (the
/// encoder maximum is `bitrate × 100 / target-percentage`), the same
/// ceiling-pinning trick as `vah264enc` (`VBR_TARGET_PERCENTAGE`).
/// `b-frames=0` and `ref-frames=1` minimize decode latency — B-frames pack
/// out of order (reordering delay) and single-reference decoding is the
/// low-latency norm for realtime WebRTC; the quality cost is small at
/// high bitrates and screenshare content. `target-usage=7` is the fastest
/// VA encode preset (range 1–7). `aud=false` is the default (unset):
/// webrtcsink's `rtph265pay` runs zero-latency with `config-interval=-1`,
/// so per-frame AUDs are unnecessary overhead.
///
/// `x265enc` (the no-VA fallback): `tune=zerolatency` zeroes lookahead,
/// B-frames and sliced-threads latency at once, `speed-preset=veryfast`
/// keeps the worst-case encode time under the frame interval, and the VBR
/// target is the 80% bitrate — mirroring the `x264enc` fallback. The
/// plugin exposes no `bframes`/`ref`/`rc-lookahead` properties (they are
/// x265 CLI options, not element properties; gst-inspect 1.28.6), so the
/// latency knobs are the tune preset, never `option-string`.
fn configure_h265(encoder: &gst::Element) {
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
    // The x265enc fallback (no VA display) pins the low-latency tune and
    // the fast preset so the software path behaves like the VA path
    // instead of holding frames (x264enc does the same).
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

fn make_element(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|error| format!("Failed to create GStreamer element {name}: {error}"))
}

/// Logs every downstream CAPS event crossing a pad so a `not-negotiated`
/// failure can be attributed to the exact stage where negotiation stopped.
fn log_caps_events(pad: &gst::Pad, label: &'static str) {
    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
        if let Some(gst::PadProbeData::Event(event)) = info.data.as_ref()
            && let gst::EventView::Caps(caps) = event.view()
        {
            log::info!("[caps] {label}: {}", caps.caps());
        }

        gst::PadProbeReturn::Ok
    });
}

/// Logs `GstForceKeyUnit` events on the encoder source pad so a libaom
/// `av1enc` keyframe-control path can be audited end to end. The upstream
/// event is the request travelling back from `livekitwebrtcsink`; the
/// downstream event is the notification the encoder pushes ahead of the
/// keyframe it actually produced — so a "HANDLED" with no following "KEYFRAME
/// OUTPUT" (see `log_av1_encode_diagnostics`) means the encoder acknowledged
/// the request but did not emit an intra frame.
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

/// Encoder telemetry accumulated between 1 s log ticks: encoded buffers, bytes
/// encoded, the largest single encoded frame, and the longest wall-clock gap
/// between consecutive buffers crossing the probe (a stall in the
/// encode/output path).
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

/// Logs AV1 keyframe output and per-second encode telemetry. A buffer without
/// `DELTA_UNIT` is the encoder's own keyframe (decodable standalone); its byte
/// size plus the `KEYFRAME REQUEST`/`KEYFRAME HANDLED` events above answer
/// whether libaom actually produces a keyframe for every PLI it receives.
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

/// Whether `encoder` is the libaom `av1enc` (software AV1), whose CBR rate
/// path is not verified to accept mid-stream `target-bitrate` reconfiguration.
fn is_libaom_av1(encoder: &gst::Element) -> bool {
    encoder
        .factory()
        .is_some_and(|factory| factory.name() == "av1enc")
}

/// Applies `ceiling_kbps` to `encoder`'s runtime rate knob. Returns `false`
/// when the encoder cannot change its ceiling live: libaom `av1enc`'s CBR
/// `target-bitrate` reconfiguration mid-stream is unverified, and a codec
/// exposing neither `bitrate` nor `target-bitrate` has no runtime knob at
/// all. The caller must treat `false` as "the encoder kept its old cap" and
/// must not record the requested value.
fn apply_encoder_ceiling(encoder: &gst::Element, ceiling_kbps: u32, rate_mode: RateMode) -> bool {
    if is_libaom_av1(encoder) {
        log::warn!("GStreamer libaom av1enc cannot change its bitrate mid-stream");
        return false;
    }
    let target = encoder_target_kbps(rate_mode, ceiling_kbps);
    if encoder.find_property("bitrate").is_some() {
        encoder.set_property_from_str("bitrate", &target.to_string());
    } else if encoder.find_property("target-bitrate").is_some() {
        // VPx `target-bitrate` is in bits/sec, unlike `bitrate` (kbps).
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
        // CBR applies the ceiling in full as the target rate.
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
            "av1enc"
        );
    }

    #[test]
    fn h265_probes_vah265enc_then_falls_back_to_x265enc() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        // The VA-API H.265 path is probe-gated exactly like H.264: when the
        // hardware element initializes (VA display + driver encode support)
        // it wins; otherwise the software x265enc fallback is selected.
        let expected = if crate::gstreamer_publisher::can_initialize_element("vah265enc") {
            "vah265enc"
        } else {
            "x265enc"
        };
        assert_eq!(codec_pipeline("h265")?, expected);
        assert_eq!(
            crate::gstreamer_publisher::selected_encoder_name("h265"),
            expected
        );

        Ok(())
    }

    #[test]
    fn vah264_hardware_configures_cbr_at_ceiling() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;
        // Skip when the machine has no VA-API H.264 encode path (software
        // x264 fallback machines run the x264 test instead).
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
            120,
            RateMode::Cbr,
        );

        // CBR at the ceiling: predictable output, no VBR/qvbr burst.
        assert_eq!(encoder.property::<u32>("bitrate"), 20_000);
        assert_eq!(
            encoder
                .property_value("rate-control")
                .get::<&gst::glib::EnumValue>()
                .map_err(|error| error.to_string())?
                .nick(),
            "cbr"
        );
        // Low-latency realtime profile: single reference, no B-frames
        // (reordering delay), fastest target-usage.
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
        configure_encoder(&encoder, "h264", 16_000, 120, RateMode::Vbr);

        // The software fallback has no QVBR mode, so it keeps VBR at the
        // 80% target.
        assert_eq!(encoder.property::<u32>("bitrate"), 16_000);
        // x264enc's `tune` is a dynamically-registered flags type with no
        // Rust type to fetch; read the set nicks through the GLib flags
        // class and assert zerolatency.
        let tune = encoder.property_value("tune");
        let (_, values) = gst::glib::FlagsValue::from_value(&tune)
            .ok_or_else(|| "tune is not a flags value".to_string())?;
        let nicks = values.iter().map(|value| value.nick()).collect::<Vec<_>>();
        assert!(nicks.contains(&"zerolatency"), "tune nicks: {nicks:?}");

        Ok(())
    }

    #[test]
    fn h264_gop_interval_is_two_seconds() {
        // The H.264 IDR interval is 2 s (not the shared 1 s GOP): VBR/qvbr
        // quality intra frames are multi-megabit, and a 1 s cadence burst
        // them into the output queue and the receiver's jitter buffer at
        // high ceilings (the periodic hitch).
        assert_eq!(H264_GOP_SECONDS, 2);
        assert_eq!(60_u32.saturating_mul(H264_GOP_SECONDS).min(1024), 120);
    }

    #[test]
    fn vah265_hardware_configures_vbr_target_pinned_to_the_ceiling() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;
        // Skip when the machine has no VA-API H.265 encode path (software
        // x265 fallback machines run the x265 test instead).
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
            60,
            RateMode::Vbr,
        );

        // VBR target at 80% of the ceiling: the driver computes the encoder
        // maximum as `bitrate × 100 / target-percentage`, so 80%/80% pins
        // the max exactly onto the ceiling (never past it).
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
        // Low-latency realtime profile: single reference, no B-frames
        // (reordering delay), fastest target-usage.
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
        configure_encoder(&encoder, "h265", 16_000, 60, RateMode::Vbr);

        // VBR target is the 80% ceiling discount (the shared
        // VBR_TARGET_PERCENTAGE), the same as the H.264 VA path. x265enc's
        // `bitrate` is an unsigned gint; `key-int-max` is signed.
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
        configure_encoder(&encoder, "h265", 16_000, 60, RateMode::Vbr);

        // Unlike libaom av1enc, x265enc's `bitrate` is mutable mid-stream,
        // so the congestion controller can step the ceiling live (the same
        // property both H.265 encoders expose).
        assert!(apply_encoder_ceiling(&encoder, 10_000, RateMode::Vbr));
        assert_eq!(encoder.property::<u32>("bitrate"), 8_000);

        Ok(())
    }

    #[test]
    fn vp9_selects_software_encoder() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        assert_eq!(codec_pipeline("vp9")?, "vp9enc");

        Ok(())
    }

    #[test]
    fn av1_selects_av1enc() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        assert_eq!(codec_pipeline("av1")?, "av1enc");

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
        configure_encoder(&encoder, "av1", target_kbps, 60, RateMode::Cbr);

        assert_eq!(target_kbps, 8_000);
        // av1enc `target-bitrate` is kilobits/sec: the ceiling is applied
        // verbatim, never ×1000.
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
        // RTC rate-control buffer and undershoot/overshoot targets matching
        // WebRTC's libaom AV1 path, not av1enc's offline 6000 ms defaults.
        assert_eq!(encoder.property::<u32>("buf-sz"), 1_000);
        assert_eq!(encoder.property::<u32>("buf-initial-sz"), 600);
        assert_eq!(encoder.property::<u32>("buf-optimal-sz"), 600);
        assert_eq!(encoder.property::<u32>("undershoot-pct"), 50);
        assert_eq!(encoder.property::<u32>("overshoot-pct"), 50);
        // Quantizer range is asserted in `av1_quantizer_range_guards_realtime_cbr`.
        // Periodic intra frames follow the shared `keyframe-max-dist`
        // interval, matching the other codecs' `key-int-max`.
        assert_eq!(encoder.property::<i32>("keyframe-max-dist"), 60);

        Ok(())
    }

    #[test]
    fn av1_quantizer_range_guards_realtime_cbr() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("av1enc")
            .build()
            .map_err(|error| error.to_string())?;
        // GStreamer's av1enc ships min/max quantizer at 0/0 — the extreme
        // high-quality end of libaom's 0–63 range. With no Q headroom the CBR
        // controller cannot shed quality on busy scenes, so the output blows
        // far past the ceiling (measured ~40 Mbps from a 8 Mbps target during
        // gameplay). Pin the unsuitable default so a future change cannot
        // silently "simplify" the override away.
        assert_eq!(encoder.property::<u32>("min-quantizer"), 0);
        assert_eq!(encoder.property::<u32>("max-quantizer"), 0);

        let target_kbps = encoder_target_kbps(RateMode::Cbr, 8_000);
        configure_encoder(&encoder, "av1", target_kbps, 60, RateMode::Cbr);

        // WebRTC's libaom AV1 wrapper: rc_min_quantizer 10, qpMax 56.
        assert_eq!(encoder.property::<u32>("min-quantizer"), 10);
        assert_eq!(encoder.property::<u32>("max-quantizer"), 56);

        Ok(())
    }

    #[test]
    fn libaom_av1_rejects_live_ceiling_changes() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("av1enc")
            .build()
            .map_err(|error| error.to_string())?;
        let target_kbps = encoder_target_kbps(RateMode::Cbr, 8_000);
        configure_encoder(&encoder, "av1", target_kbps, 60, RateMode::Cbr);
        let target_before = encoder.property::<u32>("target-bitrate");

        // The CBR rate path is not verified to accept mid-stream changes:
        // the caller must not record the requested ceiling as applied.
        assert!(!apply_encoder_ceiling(&encoder, 6_000, RateMode::Cbr));
        assert_eq!(encoder.property::<u32>("target-bitrate"), target_before);

        Ok(())
    }

    #[test]
    fn mutable_encoder_applies_a_live_ceiling_change() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("vp9enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(&encoder, "vp9", 6_400, 60, RateMode::Vbr);

        assert!(apply_encoder_ceiling(&encoder, 10_000, RateMode::Vbr));
        // VPx `target-bitrate` is bits/sec of the VBR target (80% of 10 Mbps).
        assert_eq!(encoder.property::<i32>("target-bitrate"), 8_000_000);

        Ok(())
    }

    #[test]
    fn vpx_cbr_applies_the_ceiling_verbatim() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("vp9enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(&encoder, "vp9", 6_400, 60, RateMode::Cbr);

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
        // CBR applies the ceiling in full, never the VBR 80% discount.
        assert_eq!(encoder.property::<i32>("target-bitrate"), 10_000_000);

        Ok(())
    }

    #[test]
    fn vp9_software_configures_realtime_cbr_profile() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("vp9enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(&encoder, "vp9", 8_000, 60, RateMode::Cbr);

        // Realtime libvpx knobs (screen-share profile): CBR deadline 1,
        // cpu-used 10, row/tile parallelism over 8 threads. `error-resilient`
        // stays default — WebRTC handles loss recovery itself.
        assert_eq!(encoder.property::<i64>("deadline"), 1);
        assert_eq!(encoder.property::<i32>("cpu-used"), 10);
        assert!(encoder.property::<bool>("row-mt"));
        assert_eq!(encoder.property::<i32>("tile-columns"), 2);
        assert_eq!(encoder.property::<i32>("threads"), 8);
        // Rate control: CBR target applied in bits/sec, keyframe burst
        // capped at 300% of the target, and the quantizer range gives the
        // controller headroom in both directions (min 10 stops the
        // near-lossless idle after still stretches).
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
        // WebRTC's libaom AV1 treatment mirrored on VP9: min Q 10,
        // undershoot/overshoot 50 (the libvpx default is 25).
        assert_eq!(encoder.property::<i32>("min-quantizer"), 10);
        assert_eq!(encoder.property::<i32>("undershoot"), 50);
        assert_eq!(encoder.property::<i32>("overshoot"), 50);

        Ok(())
    }

    #[test]
    fn vp8_software_configures_realtime_speed_knobs() -> Result<(), String> {
        gst::init().map_err(|error| error.to_string())?;

        let encoder = gst::ElementFactory::make("vp8enc")
            .build()
            .map_err(|error| error.to_string())?;
        configure_encoder(&encoder, "vp8", 10_000, 60, RateMode::Cbr);

        assert_eq!(encoder.property::<i32>("cpu-used"), 6);
        assert_eq!(encoder.property::<i32>("static-threshold"), 100);
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
}
