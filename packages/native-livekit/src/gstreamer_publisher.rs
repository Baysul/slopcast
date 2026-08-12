//! Linux `LiveKit` publication through the stock `livekitwebrtcsink` plugin.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{LazyLock, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use livekit::webrtc::prelude::{I420Buffer, VideoFrame};

use crate::gstreamer_encoder::{
    GstreamerEncoder, VideoInput, encoded_frames, reset_encoded_frames,
};
use crate::{CHANNELS, CaptureConfig, NativeTelemetry, SAMPLE_RATE};

const AUDIO_APPSRC_MAX_BUFFERS: u64 = 50;
const AUDIO_DISCOVERY_SAMPLES: usize = 960;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const INITIAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const RECONNECT_DELAY: Duration = Duration::from_secs(1);
const OPUS_BITRATE: i32 = 128_000;
const SINK_MAX_BITRATE: u32 = 200_000_000;

static PUBLISHER: LazyLock<Mutex<Option<PublisherHandle>>> = LazyLock::new(|| Mutex::new(None));
static AUDIO_INPUT: LazyLock<Mutex<Option<gst_app::AppSrc>>> = LazyLock::new(|| Mutex::new(None));
static VIDEO_INPUT: LazyLock<Mutex<Option<VideoInput>>> = LazyLock::new(|| Mutex::new(None));
static ROOM_CONNECTED: AtomicBool = AtomicBool::new(false);
static VIDEO_ACTIVE: AtomicBool = AtomicBool::new(false);
static VIDEO_FRAMES_SUBMITTED: AtomicU64 = AtomicU64::new(0);
// Sample clock for the audio appsrc: PTS is derived from a monotonically
// increasing PCM frame count (see push_pcm), never from the pipeline clock.
static NEXT_AUDIO_FRAME: AtomicU64 = AtomicU64::new(0);

struct PublisherHandle {
    command_sender: SyncSender<PublisherCommand>,
    join: JoinHandle<()>,
}

#[derive(Clone)]
struct ConnectionConfig {
    url: String,
    token: String,
    room_name: String,
    identity: String,
}

enum PublisherCommand {
    StartVideo {
        config: CaptureConfig,
        reply: SyncSender<Result<(), String>>,
    },
    StopVideo {
        reply: SyncSender<Result<(), String>>,
    },
    GetTelemetry(SyncSender<Option<NativeTelemetry>>),
    Shutdown,
}

enum ConnectedOutcome {
    Idle,
    Reconnect,
    Shutdown,
}

struct PublisherPipeline {
    pipeline: gst::Pipeline,
    sink: gst::Element,
    video_config: Option<CaptureConfig>,
    is_shutdown: bool,
}

pub(crate) fn load_plugins(plugin_dir: &Path) -> Result<(), String> {
    gst::init().map_err(|error| format!("Failed to initialize GStreamer: {error}"))?;
    if !plugin_dir.is_dir() {
        return Err(format!(
            "Bundled GStreamer plugin directory is missing: {}",
            plugin_dir.display()
        ));
    }

    // `gst_registry_scan_path` returns TRUE only when the registry *changed*
    // (i.e. new plugins were discovered). A cache hit — plugins already
    // registered from a previous run — returns FALSE, which is not an error:
    // the required elements are still present. Treat only a genuinely missing
    // directory as fatal.
    gst::Registry::get().scan_path(plugin_dir);

    verify_required_elements()
}

pub(crate) fn connect(
    url: String,
    token: String,
    room_name: String,
    identity: String,
) -> Result<(), String> {
    if url.is_empty() || token.is_empty() || room_name.is_empty() || identity.is_empty() {
        return Err("LiveKit URL, token, room name, and identity are required".into());
    }

    disconnect();
    gst::init().map_err(|error| format!("Failed to initialize GStreamer: {error}"))?;
    verify_required_elements()?;
    let connection = ConnectionConfig {
        url,
        token,
        room_name,
        identity,
    };
    let (command_sender, command_receiver) = mpsc::sync_channel(32);
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    let join = thread::Builder::new()
        .name("slopcast-gstreamer-livekit".into())
        .spawn(move || run_worker(&connection, &command_receiver, &ready_sender))
        .map_err(|error| format!("Failed to spawn GStreamer publisher worker: {error}"))?;
    match ready_receiver.recv_timeout(INITIAL_CONNECT_TIMEOUT) {
        Ok(Ok(())) => {
            let mut publisher = PUBLISHER
                .lock()
                .map_err(|_| "GStreamer publisher lock poisoned")?;
            *publisher = Some(PublisherHandle {
                command_sender,
                join,
            });
        }
        Ok(Err(error)) => {
            let _ = join.join();
            return Err(error);
        }
        Err(error) => {
            let _ = command_sender.send(PublisherCommand::Shutdown);
            let _ = join.join();
            return Err(format!("GStreamer publisher startup timed out: {error}"));
        }
    }

    Ok(())
}

pub(crate) fn disconnect() {
    let handle = PUBLISHER
        .lock()
        .ok()
        .and_then(|mut publisher| publisher.take());
    if let Some(handle) = handle {
        let _ = handle.command_sender.send(PublisherCommand::Shutdown);
        let _ = handle.join.join();
    }
    clear_inputs();
    ROOM_CONNECTED.store(false, Ordering::Relaxed);
    VIDEO_ACTIVE.store(false, Ordering::Relaxed);
}

pub(crate) fn is_connected() -> bool {
    ROOM_CONNECTED.load(Ordering::Relaxed)
}

pub(crate) fn start_video(config: CaptureConfig) -> Result<(), String> {
    let command_sender = command_sender()?;
    let (reply_sender, reply_receiver) = mpsc::sync_channel(1);

    command_sender
        .send(PublisherCommand::StartVideo {
            config,
            reply: reply_sender,
        })
        .map_err(|_| "GStreamer publisher worker stopped")?;

    reply_receiver
        .recv_timeout(COMMAND_TIMEOUT)
        .map_err(|error| format!("GStreamer video start timed out: {error}"))?
}

pub(crate) fn stop_video() -> Result<(), String> {
    let command_sender = command_sender()?;
    let (reply_sender, reply_receiver) = mpsc::sync_channel(1);

    command_sender
        .send(PublisherCommand::StopVideo {
            reply: reply_sender,
        })
        .map_err(|_| "GStreamer publisher worker stopped")?;

    reply_receiver
        .recv_timeout(COMMAND_TIMEOUT)
        .map_err(|error| format!("GStreamer video stop timed out: {error}"))?
}

pub(crate) fn is_video_active() -> bool {
    VIDEO_ACTIVE.load(Ordering::Relaxed)
}

pub(crate) fn push_video_frame(frame: &VideoFrame<I420Buffer>) -> Result<(), String> {
    let input = VIDEO_INPUT
        .lock()
        .map_err(|_| "GStreamer video input lock poisoned")?
        .clone()
        .ok_or_else(|| "GStreamer video publication is not active".to_string())?;

    input.push_frame(frame)?;
    VIDEO_FRAMES_SUBMITTED.fetch_add(1, Ordering::Relaxed);

    Ok(())
}

pub(crate) fn feed_pcm(samples: &[i16]) {
    let input = AUDIO_INPUT.lock().ok().and_then(|input| input.clone());
    let Some(input) = input else {
        return;
    };
    if samples.is_empty() {
        return;
    }

    if let Err(error) = push_pcm(&input, samples) {
        log::warn!("GStreamer audio input dropped PCM: {error}");
    }
}

pub(crate) fn telemetry() -> Option<NativeTelemetry> {
    let command_sender = command_sender().ok()?;
    let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
    command_sender
        .send(PublisherCommand::GetTelemetry(reply_sender))
        .ok()?;

    reply_receiver
        .recv_timeout(Duration::from_millis(500))
        .ok()?
}

fn command_sender() -> Result<SyncSender<PublisherCommand>, String> {
    PUBLISHER
        .lock()
        .map_err(|_| "GStreamer publisher lock poisoned")?
        .as_ref()
        .map(|publisher| publisher.command_sender.clone())
        .ok_or_else(|| "GStreamer publisher is not connected".to_string())
}

fn run_worker(
    connection: &ConnectionConfig,
    command_receiver: &Receiver<PublisherCommand>,
    ready_sender: &SyncSender<Result<(), String>>,
) {
    let mut pipeline = None;
    let mut video_config = None;
    // The room worker is deliberately dormant until Go Live. Constructing an
    // audio-only sink and putting it in PLAYING publishes audio before the
    // video pad exists; livekitwebrtcsink then keeps the negotiated topology
    // audio-only. The first pipeline is therefore created with both pads.
    let _ = ready_sender.send(Ok(()));

    loop {
        let Some(active_pipeline) = pipeline.as_mut() else {
            match command_receiver.recv_timeout(POLL_INTERVAL) {
                Ok(PublisherCommand::StartVideo { config, reply }) => {
                    match PublisherPipeline::new(connection, Some(&config)) {
                        Ok(new_pipeline) => {
                            video_config = Some(config);
                            pipeline = Some(new_pipeline);
                            let _ = reply.send(Ok(()));
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                Ok(PublisherCommand::StopVideo { reply }) => {
                    let _ = reply.send(Ok(()));
                }
                Ok(PublisherCommand::GetTelemetry(reply)) => {
                    let _ = reply.send(None);
                }
                Ok(PublisherCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            continue;
        };

        match run_connected(
            active_pipeline,
            connection,
            command_receiver,
            &mut video_config,
        ) {
            ConnectedOutcome::Idle => {
                pipeline = None;
                clear_inputs();
                ROOM_CONNECTED.store(false, Ordering::Relaxed);
            }
            ConnectedOutcome::Shutdown => break,
            ConnectedOutcome::Reconnect => {
                ROOM_CONNECTED.store(false, Ordering::Relaxed);
                clear_inputs();
                drop(pipeline.take());
                match reconnect(connection, command_receiver, &mut video_config) {
                    Some(reconnected) => {
                        pipeline = Some(reconnected);
                    }
                    None => break,
                }
            }
        }
    }

    clear_inputs();
    ROOM_CONNECTED.store(false, Ordering::Relaxed);
    VIDEO_ACTIVE.store(false, Ordering::Relaxed);
}

fn run_connected(
    pipeline: &mut PublisherPipeline,
    connection: &ConnectionConfig,
    command_receiver: &Receiver<PublisherCommand>,
    video_config: &mut Option<CaptureConfig>,
) -> ConnectedOutcome {
    loop {
        match command_receiver.recv_timeout(POLL_INTERVAL) {
            Ok(PublisherCommand::StartVideo { config, reply }) => {
                let result = pipeline.rebuild(connection, Some(&config));
                let should_reconnect = result.is_err();
                if result.is_ok() {
                    *video_config = Some(config);
                }
                let _ = reply.send(result);
                if should_reconnect {
                    return ConnectedOutcome::Reconnect;
                }
            }
            Ok(PublisherCommand::StopVideo { reply }) => {
                *video_config = None;
                pipeline.shutdown();
                VIDEO_ACTIVE.store(false, Ordering::Relaxed);
                let _ = reply.send(Ok(()));
                return ConnectedOutcome::Idle;
            }
            Ok(PublisherCommand::GetTelemetry(reply)) => {
                let _ = reply.send(Some(pipeline.telemetry()));
            }
            Ok(PublisherCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                return ConnectedOutcome::Shutdown;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        if let Some(error) = pipeline.poll_error() {
            log::warn!("GStreamer LiveKit publisher will reconnect after error: {error}");
            return ConnectedOutcome::Reconnect;
        }
    }
}

fn reconnect(
    connection: &ConnectionConfig,
    command_receiver: &Receiver<PublisherCommand>,
    video_config: &mut Option<CaptureConfig>,
) -> Option<PublisherPipeline> {
    loop {
        match command_receiver.recv_timeout(RECONNECT_DELAY) {
            Ok(PublisherCommand::StartVideo { config, reply }) => {
                *video_config = Some(config);
                let _ = reply.send(Ok(()));
            }
            Ok(PublisherCommand::StopVideo { reply }) => {
                *video_config = None;
                let _ = reply.send(Ok(()));
            }
            Ok(PublisherCommand::GetTelemetry(reply)) => {
                let _ = reply.send(None);
            }
            Ok(PublisherCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => return None,
            Err(RecvTimeoutError::Timeout) => {}
        }

        match PublisherPipeline::new(connection, video_config.as_ref()) {
            Ok(pipeline) => {
                log::info!("GStreamer LiveKit publisher reconnected");
                return Some(pipeline);
            }
            Err(error) => log::warn!("GStreamer LiveKit reconnect failed: {error}"),
        }
    }
}

impl PublisherPipeline {
    fn new(
        connection: &ConnectionConfig,
        video_config: Option<&CaptureConfig>,
    ) -> Result<Self, String> {
        let audio_caps = gst::Caps::builder("audio/x-opus")
            .field(
                "channels",
                i32::try_from(CHANNELS).map_err(|_| "Audio channels exceed i32")?,
            )
            .field(
                "rate",
                i32::try_from(SAMPLE_RATE).map_err(|_| "Audio rate exceeds i32")?,
            )
            .build();
        let video_caps = gst::Caps::builder("video/x-h264")
            .field("stream-format", "avc")
            .field("alignment", "au")
            .build();
        let sink = gst::ElementFactory::make("livekitwebrtcsink")
            .property("audio-caps", &audio_caps)
            .property("video-caps", &video_caps)
            .property("max-bitrate", SINK_MAX_BITRATE)
            .build()
            .map_err(|error| format!("Failed to create livekitwebrtcsink: {error}"))?;
        configure_signaller(&sink, connection);
        let pipeline = gst::Pipeline::new();
        pipeline
            .add(&sink)
            .map_err(|error| format!("Failed to add livekitwebrtcsink: {error}"))?;
        let audio_input = attach_audio(&pipeline, &sink)?;
        let mut publisher = Self {
            pipeline,
            sink,
            video_config: None,
            is_shutdown: false,
        };
        if let Some(config) = video_config {
            publisher.attach_video(config.clone())?;
        }
        publisher
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|error| format!("Failed to start GStreamer LiveKit pipeline: {error}"))?;
        set_audio_input(Some(audio_input.clone()))?;
        push_pcm(&audio_input, &[0; AUDIO_DISCOVERY_SAMPLES])?;

        Ok(publisher)
    }

    fn rebuild(
        &mut self,
        connection: &ConnectionConfig,
        video_config: Option<&CaptureConfig>,
    ) -> Result<(), String> {
        self.shutdown();
        let replacement = Self::new(connection, video_config)?;
        *self = replacement;

        Ok(())
    }

    fn attach_video(&mut self, config: CaptureConfig) -> Result<(), String> {
        let encoder = GstreamerEncoder::attach(&self.pipeline, &self.sink, &config)?;
        set_video_input(Some(encoder.input()))?;
        self.video_config = Some(config);
        VIDEO_FRAMES_SUBMITTED.store(0, Ordering::Relaxed);
        reset_encoded_frames();
        VIDEO_ACTIVE.store(true, Ordering::Relaxed);

        Ok(())
    }

    fn poll_error(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        for message in bus.iter_timed(gst::ClockTime::ZERO) {
            match message.view() {
                gst::MessageView::Error(error) => {
                    let source = error.src().map_or_else(
                        || "unknown".to_string(),
                        |source| source.path_string().to_string(),
                    );
                    return Some(format!("{source}: {} ({:?})", error.error(), error.debug()));
                }
                gst::MessageView::Eos(_) => return Some("pipeline reached EOS".into()),
                _ => {}
            }
        }

        None
    }

    fn telemetry(&self) -> NativeTelemetry {
        let stats = self.sink.property::<gst::Structure>("stats");
        fold_telemetry(&stats, self.video_config.as_ref())
    }
}

impl Drop for PublisherPipeline {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl PublisherPipeline {
    fn shutdown(&mut self) {
        if self.is_shutdown {
            return;
        }

        let _ = set_audio_input(None);
        let _ = set_video_input(None);
        VIDEO_ACTIVE.store(false, Ordering::Relaxed);
        let _ = self.pipeline.set_state(gst::State::Null);
        self.is_shutdown = true;
    }
}

fn attach_audio(pipeline: &gst::Pipeline, sink: &gst::Element) -> Result<gst_app::AppSrc, String> {
    let raw_caps = gst::Caps::builder("audio/x-raw")
        .field("format", "S16LE")
        .field("layout", "interleaved")
        .field(
            "rate",
            i32::try_from(SAMPLE_RATE).map_err(|_| "Audio rate exceeds i32")?,
        )
        .field(
            "channels",
            i32::try_from(CHANNELS).map_err(|_| "Audio channels exceed i32")?,
        )
        .build();
    let appsrc = gst_app::AppSrc::builder()
        .caps(&raw_caps)
        .format(gst::Format::Time)
        .is_live(true)
        .block(false)
        .max_buffers(AUDIO_APPSRC_MAX_BUFFERS)
        // TEMPORARY (debug): non-leaky so dropped PCM surfaces as visible
        // backpressure/pipeline stalls instead of silently vanishing; revert
        // to leaky downstream once the pops are gone.
        .build();
    // PTS is sample-clocked in push_pcm, so the pipeline clock must not
    // stamp buffers (do-timestamp would drift against the sample count and
    // the resampler would hear it as rate wobble). Reset the sample clock
    // per pipeline instance — rebuild() constructs a fresh audio chain.
    NEXT_AUDIO_FRAME.store(0, Ordering::Relaxed);
    let queue = gst::ElementFactory::make("queue")
        .property("max-size-buffers", 10_u32)
        .property("max-size-bytes", 0_u32)
        .property("max-size-time", 100_000_000_u64)
        .build()
        .map_err(|error| format!("Failed to create GStreamer audio queue: {error}"))?;
    let convert = make_element("audioconvert")?;
    let resample = make_element("audioresample")?;
    let encoder = gst::ElementFactory::make("opusenc")
        .property("bitrate", OPUS_BITRATE)
        .property("inband-fec", true)
        .build()
        .map_err(|error| format!("Failed to create GStreamer Opus encoder: {error}"))?;
    let parser = make_element("opusparse")?;
    let output_queue = gst::ElementFactory::make("queue")
        .property("max-size-buffers", 4_u32)
        .property("max-size-bytes", 0_u32)
        .property("max-size-time", 0_u64)
        .build()
        .map_err(|error| format!("Failed to create GStreamer encoded-audio queue: {error}"))?;
    let elements = [
        appsrc.clone().upcast::<gst::Element>(),
        queue,
        convert,
        resample,
        encoder,
        parser,
        output_queue.clone(),
    ];

    pipeline
        .add_many(elements.iter())
        .map_err(|error| format!("Failed to add GStreamer audio elements: {error}"))?;
    gst::Element::link_many(elements.iter())
        .map_err(|error| format!("Failed to link GStreamer audio pipeline: {error}"))?;
    let sink_pad = sink
        .request_pad_simple("audio_%u")
        .ok_or_else(|| "livekitwebrtcsink refused an audio pad".to_string())?;
    output_queue
        .static_pad("src")
        .ok_or_else(|| "GStreamer encoded-audio queue has no src pad".to_string())?
        .link(&sink_pad)
        .map_err(|error| format!("Failed to link Opus into livekitwebrtcsink: {error}"))?;

    Ok(appsrc)
}

fn configure_signaller(sink: &gst::Element, config: &ConnectionConfig) {
    let signaller = sink.property::<gst::glib::Object>("signaller");
    signaller.set_property("ws-url", &config.url);
    signaller.set_property("auth-token", &config.token);
    signaller.set_property("room-name", &config.room_name);
    signaller.set_property("identity", &config.identity);
    // LiveKit join state lives on the signaller's `connection-state`
    // property (`server-connected` and beyond once the join completes);
    // mirror it so is_native_room_connected reflects a real connection,
    // not merely a pipeline that was built.
    signaller.connect_notify(Some("connection-state"), move |obj, _| {
        let connected = obj
            .property_value("connection-state")
            .transform::<String>()
            .ok()
            .and_then(|value| value.get::<String>().ok())
            .is_some_and(|state| {
                matches!(
                    state.as_str(),
                    "server-connected" | "publishing" | "published" | "subscribed"
                )
            });
        ROOM_CONNECTED.store(connected, Ordering::Relaxed);
    });
}

fn push_pcm(appsrc: &gst_app::AppSrc, samples: &[i16]) -> Result<(), String> {
    let channel_count = usize::try_from(CHANNELS).map_err(|_| "Audio channels exceed usize")?;
    if !samples.len().is_multiple_of(channel_count) {
        return Err(format!(
            "PCM buffer has {} samples, not divisible by {} channels",
            samples.len(),
            channel_count
        ));
    }
    let frames = samples.len() / channel_count;

    let mut buffer = gst::Buffer::with_size(samples.len().saturating_mul(size_of::<i16>()))
        .map_err(|error| format!("Failed to allocate GStreamer audio buffer: {error}"))?;
    {
        let buffer_ref = buffer
            .get_mut()
            .ok_or_else(|| "GStreamer audio buffer is unexpectedly shared".to_string())?;
        let mut mapped = buffer_ref
            .map_writable()
            .map_err(|_| "Failed to map GStreamer audio buffer writable".to_string())?;
        for (sample, destination) in samples.iter().zip(mapped.chunks_exact_mut(2)) {
            destination.copy_from_slice(&sample.to_le_bytes());
        }
        drop(mapped);

        // Sample-clocked PTS: timestamps must advance exactly with the
        // sample count, or the resampler → opusenc → RTP chain hears rate
        // drift as pops and out-of-tune audio. The counter is reset per
        // pipeline instance in attach_audio.
        let start_frame = NEXT_AUDIO_FRAME.fetch_add(frames as u64, Ordering::Relaxed);
        let pts_ns = start_frame * gst::ClockTime::SECOND.nseconds() / u64::from(SAMPLE_RATE);
        let end_ns = (start_frame + frames as u64) * gst::ClockTime::SECOND.nseconds()
            / u64::from(SAMPLE_RATE);
        buffer_ref.set_pts(gst::ClockTime::from_nseconds(pts_ns));
        buffer_ref.set_duration(gst::ClockTime::from_nseconds(end_ns - pts_ns));
    }
    appsrc
        .push_buffer(buffer)
        .map_err(|error| format!("GStreamer appsrc rejected PCM: {error}"))?;

    Ok(())
}

fn set_audio_input(input: Option<gst_app::AppSrc>) -> Result<(), String> {
    *AUDIO_INPUT
        .lock()
        .map_err(|_| "GStreamer audio input lock poisoned")? = input;

    Ok(())
}

fn set_video_input(input: Option<VideoInput>) -> Result<(), String> {
    *VIDEO_INPUT
        .lock()
        .map_err(|_| "GStreamer video input lock poisoned")? = input;

    Ok(())
}

fn clear_inputs() {
    let _ = set_audio_input(None);
    let _ = set_video_input(None);
}

fn make_element(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|error| format!("Failed to create GStreamer element {name}: {error}"))
}

fn verify_required_elements() -> Result<(), String> {
    const REQUIRED: &[&str] = &[
        "appsrc",
        "queue",
        "videoconvert",
        "vah264enc",
        "h264parse",
        "audioconvert",
        "audioresample",
        "opusenc",
        "opusparse",
        "webrtcbin",
        "nicesrc",
        "nicesink",
        "rtpbin",
        "rtph264pay",
        "rtpgccbwe",
        "livekitwebrtcsink",
    ];

    let missing = REQUIRED
        .iter()
        .copied()
        .filter(|name| gst::ElementFactory::find(name).is_none())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }

    Err(format!(
        "Required GStreamer elements are unavailable: {}",
        missing.join(", ")
    ))
}

fn fold_telemetry(stats: &gst::Structure, config: Option<&CaptureConfig>) -> NativeTelemetry {
    let structures = flatten_structures(stats);
    let mut telemetry = NativeTelemetry {
        timestamp_ms: Some(epoch_timestamp_ms()),
        ..NativeTelemetry::default()
    };
    // gst-webrtc-bin stats carry no reliable `kind` split across
    // versions: the bundled 1.28.6 emits `kind` on the RTP stream
    // structures but master removed it again ("To be added: kind" in
    // gstwebrtcstats.c), so the video/audio split is recovered from the
    // codec each RTP stream references: codec structures expose their
    // `clock-rate` (video is always 90 kHz, Opus 48 kHz) under their
    // `id`, which the RTP stream structures reference via `codec-id`. The
    // pipeline fixes the codecs (vah264enc + opusenc), so the clock rate
    // is unambiguous. Structure *names* also differ by version — 1.28.6
    // still uses `outbound-rtp` / `remote-inbound-rtp` / `codec`, while
    // master renamed them to `rtp-outbound-stream-stats_<ssrc>` /
    // `rtp-remote-inbound-stream-stats_<ssrc>` / `codec-stats-<pad>` —
    // so both variants are matched.
    let codec_clock_rates = structures
        .iter()
        .filter(|structure| is_codec_stats(structure.name().as_str()))
        .filter_map(|structure| {
            Some((
                structure.get::<String>("id").ok()?,
                structure.get::<u32>("clock-rate").ok()?,
            ))
        })
        .collect::<HashMap<_, _>>();
    for structure in structures {
        let name = structure.name().as_str();
        if is_outbound_stats(name) {
            fold_outbound(&structure, &codec_clock_rates, &mut telemetry);
        } else if is_remote_inbound_stats(name) {
            fold_remote_inbound(&structure, &codec_clock_rates, &mut telemetry);
        }
    }
    if let Some(config) = config {
        // Encoded count is measured after h264parse (the real encoder
        // throughput); the submitted count is frames pushed into the appsrc.
        // The gap between them is backpressure drops.
        telemetry.video_frames_encoded = Some(stat_as_f64(encoded_frames()));
        telemetry.video_frames_submitted = Some(VIDEO_FRAMES_SUBMITTED.load(Ordering::Relaxed));
        telemetry.video_width = Some(config.width);
        telemetry.video_height = Some(config.height);
        telemetry.encoder_implementation = Some("GStreamer vah264enc".into());
        telemetry
            .video_codec
            .get_or_insert_with(|| "video/H264".into());
        // Live appsrc statistics — `dropped` is the stutter diagnostic
        // (buffers the leaky appsrc discarded when the queue was full).
        if let Ok(input) = VIDEO_INPUT.lock()
            && let Some(input) = input.as_ref()
        {
            let stats = input.appsrc_stats();
            telemetry.video_appsrc_input = stats.input;
            telemetry.video_appsrc_output = stats.output;
            telemetry.video_appsrc_dropped = stats.dropped;
            telemetry.video_appsrc_level_buffers = stats.level_buffers;
            telemetry.video_appsrc_level_bytes = stats.level_bytes;
            telemetry.video_appsrc_level_time = stats.level_time;
        }
    }

    telemetry
}

fn flatten_structures(root: &gst::Structure) -> Vec<gst::Structure> {
    let mut flattened = Vec::new();
    flatten_into(root, &mut flattened);
    flattened
}

fn flatten_into(structure: &gst::Structure, flattened: &mut Vec<gst::Structure>) {
    flattened.push(structure.to_owned());
    for (_, value) in structure.iter() {
        if let Ok(nested) = value.get::<gst::Structure>() {
            flatten_into(&nested, flattened);
        }
    }
}

/// Stats structure-name variants seen across gst-webrtc-bin versions
/// (classic names in the bundled 1.28.6, W3C-style names on master).
fn is_outbound_stats(name: &str) -> bool {
    name == "outbound-rtp" || name.starts_with("rtp-outbound-stream-stats_")
}

fn is_remote_inbound_stats(name: &str) -> bool {
    name == "remote-inbound-rtp" || name.starts_with("rtp-remote-inbound-stream-stats_")
}

fn is_codec_stats(name: &str) -> bool {
    name == "codec" || name.starts_with("codec-stats-")
}

/// The RTP stream's media kind, recovered through the codec it references
/// (gst-webrtc-bin's `kind` field is present in 1.28.6 but was removed
/// again on master). The pipeline fixes the codecs — vah264enc at 90 kHz,
/// opusenc at 48 kHz — so the clock rate is unambiguous. `None` when the
/// codec cannot be resolved; such streams are ignored rather than
/// guessed.
fn stream_is_video(
    structure: &gst::Structure,
    codec_clock_rates: &HashMap<String, u32>,
) -> Option<bool> {
    let clock_rate = structure
        .get::<String>("codec-id")
        .ok()
        .and_then(|codec_id| codec_clock_rates.get(&codec_id).copied())?;
    match clock_rate {
        90_000 => Some(true),
        48_000 => Some(false),
        _ => None,
    }
}

fn fold_outbound(
    structure: &gst::Structure,
    codec_clock_rates: &HashMap<String, u32>,
    telemetry: &mut NativeTelemetry,
) {
    let Some(is_video) = stream_is_video(structure, codec_clock_rates) else {
        return;
    };
    let bytes = structure.get::<u64>("bytes-sent").ok().map(stat_as_f64);
    let packets = structure.get::<u64>("packets-sent").ok().map(stat_as_f64);
    if is_video {
        telemetry.video_bytes_sent = bytes;
        telemetry.video_packets_sent = packets;
    } else {
        telemetry.audio_bytes_sent = bytes;
        telemetry.audio_packets_sent = packets;
        // The codec structure carries no usable format marker across
        // versions; the pipeline's only audio codec is Opus at 48 kHz.
        telemetry
            .audio_codec
            .get_or_insert_with(|| "audio/OPUS".into());
    }
}

fn fold_remote_inbound(
    structure: &gst::Structure,
    codec_clock_rates: &HashMap<String, u32>,
    telemetry: &mut NativeTelemetry,
) {
    if stream_is_video(structure, codec_clock_rates) != Some(true) {
        return;
    }

    telemetry.video_packets_lost = structure
        .get::<i32>("packets-lost")
        .ok()
        .map(|packets| stat_as_f64(u64::try_from(packets.max(0)).unwrap_or(0)));
    telemetry.rtt_ms = structure
        .get::<f64>("round-trip-time")
        .ok()
        .filter(|seconds| *seconds > 0.0)
        .map(|seconds| seconds * 1000.0);
}

#[allow(
    clippy::cast_precision_loss,
    reason = "WebRTC byte/frame counters remain well below f64's exact integer range during a session"
)]
fn stat_as_f64(value: u64) -> f64 {
    value as f64
}

fn epoch_timestamp_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;

    /// `Structure` creation asserts a `gst::init`; `gst_init` is not
    /// thread-safe to race, so every test initializes through this once.
    fn init_gst() {
        static INIT: OnceLock<()> = OnceLock::new();
        INIT.get_or_init(|| {
            if let Err(error) = gst::init() {
                panic!("GStreamer must initialize in tests: {error}");
            }
        });
    }

    fn codec(id: &str, clock_rate: u32) -> gst::Structure {
        // Bundled 1.28.6 names the codec structure `codec` (master:
        // `codec-stats-<pad>`) and exposes the W3C-style id, which the
        // RTP stream structures reference via `codec-id`.
        gst::Structure::builder("codec")
            .field("id", id)
            .field("clock-rate", clock_rate)
            .build()
    }

    fn outbound(codec_id: &str, ssrc: u32, bytes: u64, packets: u64) -> gst::Structure {
        // Bundled 1.28.6 names it `outbound-rtp` (master:
        // `rtp-outbound-stream-stats_<ssrc>`).
        gst::Structure::builder("outbound-rtp")
            .field("id", format!("rtp-outbound-stream-stats_{ssrc}"))
            .field("codec-id", codec_id)
            .field("bytes-sent", bytes)
            .field("packets-sent", packets)
            .build()
    }

    // The fixtures mirror what gst-webrtc-bin stats actually emit: classic
    // structure names with W3C-style `id`/`codec-id` fields. `packets-lost`
    // stays an i32.
    #[test]
    fn telemetry_folds_gstreamer_outbound_stats() {
        init_gst();
        let stats = gst::Structure::builder("application/x-webrtcsink-stats")
            .field("video-codec", codec("codec-stats-src_0", 90_000))
            .field("audio-codec", codec("codec-stats-src_1", 48_000))
            .field("video", outbound("codec-stats-src_0", 1000, 42, 7))
            .field("audio", outbound("codec-stats-src_1", 2000, 13, 3))
            .build();

        let telemetry = fold_telemetry(&stats, None);

        assert_eq!(telemetry.video_bytes_sent, Some(42.0));
        assert_eq!(telemetry.video_packets_sent, Some(7.0));
        assert_eq!(telemetry.audio_bytes_sent, Some(13.0));
        assert_eq!(telemetry.audio_packets_sent, Some(3.0));
        assert_eq!(telemetry.audio_codec.as_deref(), Some("audio/OPUS"));
    }

    #[test]
    fn telemetry_folds_video_remote_inbound_loss_and_rtt() {
        init_gst();
        let stats = gst::Structure::builder("application/x-webrtcsink-stats")
            .field("video-codec", codec("codec-stats-src_0", 90_000))
            .field(
                "remote-video",
                gst::Structure::builder("remote-inbound-rtp")
                    .field("id", "rtp-remote-inbound-stream-stats_1000")
                    .field("codec-id", "codec-stats-src_0")
                    .field("packets-lost", -3_i32)
                    .field("round-trip-time", 0.05_f64)
                    .build(),
            )
            .build();

        let telemetry = fold_telemetry(&stats, None);

        assert_eq!(telemetry.video_packets_lost, Some(0.0));
        assert_eq!(telemetry.rtt_ms, Some(50.0));
    }

    #[test]
    fn telemetry_ignores_audio_remote_inbound() {
        init_gst();
        let stats = gst::Structure::builder("application/x-webrtcsink-stats")
            .field("audio-codec", codec("codec-stats-src_1", 48_000))
            .field(
                "remote-audio",
                gst::Structure::builder("remote-inbound-rtp")
                    .field("id", "rtp-remote-inbound-stream-stats_2000")
                    .field("codec-id", "codec-stats-src_1")
                    .field("packets-lost", 7_i32)
                    .build(),
            )
            .build();

        let telemetry = fold_telemetry(&stats, None);

        assert!(telemetry.video_packets_lost.is_none());
        assert!(telemetry.rtt_ms.is_none());
    }

    #[test]
    fn telemetry_ignores_streams_without_a_resolvable_codec() {
        init_gst();
        let stats = gst::Structure::builder("application/x-webrtcsink-stats")
            .field(
                "unknown",
                gst::Structure::builder("rtp-outbound-stream-stats_999")
                    .field("codec-id", "no-such-codec")
                    .field("bytes-sent", 99_u64)
                    .build(),
            )
            .build();

        let telemetry = fold_telemetry(&stats, None);

        assert!(telemetry.video_bytes_sent.is_none());
        assert!(telemetry.audio_bytes_sent.is_none());
    }
}
