//! Linux `LiveKit` publication through the stock `livekitwebrtcsink` plugin.

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

use crate::gstreamer_encoder::{GstreamerEncoder, VideoInput};
use crate::{CHANNELS, CaptureConfig, NativeTelemetry, SAMPLE_RATE};

const AUDIO_APPSRC_MAX_BUFFERS: u64 = 10;
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
static VIDEO_FRAMES_ENCODED: AtomicU64 = AtomicU64::new(0);

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
    VIDEO_FRAMES_ENCODED.fetch_add(1, Ordering::Relaxed);

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
    let mut video_config = None;
    let mut pipeline = match PublisherPipeline::new(connection, video_config.as_ref()) {
        Ok(pipeline) => pipeline,
        Err(error) => {
            let _ = ready_sender.send(Err(error));
            return;
        }
    };
    ROOM_CONNECTED.store(true, Ordering::Relaxed);
    let _ = ready_sender.send(Ok(()));

    loop {
        match run_connected(
            &mut pipeline,
            connection,
            command_receiver,
            &mut video_config,
        ) {
            ConnectedOutcome::Shutdown => break,
            ConnectedOutcome::Reconnect => {
                ROOM_CONNECTED.store(false, Ordering::Relaxed);
                clear_inputs();
                drop(pipeline);
                match reconnect(connection, command_receiver, &mut video_config) {
                    Some(reconnected) => {
                        pipeline = reconnected;
                        ROOM_CONNECTED.store(true, Ordering::Relaxed);
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
                let result = pipeline.rebuild(connection, None);
                let should_reconnect = result.is_err();
                if result.is_ok() {
                    VIDEO_ACTIVE.store(false, Ordering::Relaxed);
                }
                let _ = reply.send(result);
                if should_reconnect {
                    return ConnectedOutcome::Reconnect;
                }
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
            .field("stream-format", "byte-stream")
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
        VIDEO_FRAMES_ENCODED.store(0, Ordering::Relaxed);
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
        .leaky_type(gst_app::AppLeakyType::Downstream)
        .build();
    appsrc.set_property("do-timestamp", true);
    let queue = gst::ElementFactory::make("queue")
        .property("max-size-buffers", 10_u32)
        .property("max-size-bytes", 0_u32)
        .property("max-size-time", 100_000_000_u64)
        .property_from_str("leaky", "downstream")
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
        .property_from_str("leaky", "downstream")
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
}

fn push_pcm(appsrc: &gst_app::AppSrc, samples: &[i16]) -> Result<(), String> {
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
        let channel_count = usize::try_from(CHANNELS).map_err(|_| "Audio channels exceed usize")?;
        let frames = samples.len() / channel_count;
        let duration_ns = u64::try_from(frames)
            .unwrap_or(u64::MAX)
            .saturating_mul(gst::ClockTime::SECOND.nseconds())
            / u64::from(SAMPLE_RATE);
        drop(mapped);
        buffer_ref.set_duration(gst::ClockTime::from_nseconds(duration_ns));
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
    for structure in structures {
        match structure.name().as_str() {
            "outbound-rtp" => fold_outbound(&structure, &mut telemetry),
            "remote-inbound-rtp" => fold_remote_inbound(&structure, &mut telemetry),
            "codec" => fold_codec(&structure, &mut telemetry),
            _ => {}
        }
    }
    if let Some(config) = config {
        telemetry.video_frames_encoded =
            Some(stat_as_f64(VIDEO_FRAMES_ENCODED.load(Ordering::Relaxed)));
        telemetry.video_width = Some(config.width);
        telemetry.video_height = Some(config.height);
        telemetry.encoder_implementation = Some("GStreamer vah264enc".into());
        telemetry
            .video_codec
            .get_or_insert_with(|| "video/H264".into());
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

fn fold_outbound(structure: &gst::Structure, telemetry: &mut NativeTelemetry) {
    let kind = structure.get::<String>("kind").ok();
    let bytes = structure.get::<u64>("bytes-sent").ok().map(stat_as_f64);
    let packets = structure.get::<u64>("packets-sent").ok().map(stat_as_f64);
    match kind.as_deref() {
        Some("video") => {
            telemetry.video_bytes_sent = bytes;
            telemetry.video_packets_sent = packets;
        }
        Some("audio") => {
            telemetry.audio_bytes_sent = bytes;
            telemetry.audio_packets_sent = packets;
        }
        _ => {}
    }
}

fn fold_remote_inbound(structure: &gst::Structure, telemetry: &mut NativeTelemetry) {
    if structure.get::<String>("kind").ok().as_deref() != Some("video") {
        return;
    }

    telemetry.video_packets_lost = structure
        .get::<i64>("packets-lost")
        .ok()
        .map(|packets| stat_as_f64(u64::try_from(packets.max(0)).unwrap_or(0)));
    telemetry.rtt_ms = structure
        .get::<f64>("round-trip-time")
        .ok()
        .filter(|seconds| *seconds > 0.0)
        .map(|seconds| seconds * 1000.0);
}

fn fold_codec(structure: &gst::Structure, telemetry: &mut NativeTelemetry) {
    let mime_type = structure.get::<String>("mime-type").ok();
    if mime_type
        .as_deref()
        .is_some_and(|mime_type| mime_type.starts_with("video/"))
    {
        telemetry.video_codec = mime_type;
    }
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
    use super::*;

    #[test]
    fn telemetry_folds_gstreamer_outbound_stats() {
        let video = gst::Structure::builder("outbound-rtp")
            .field("kind", "video")
            .field("bytes-sent", 42_u64)
            .field("packets-sent", 7_u64)
            .build();
        let audio = gst::Structure::builder("outbound-rtp")
            .field("kind", "audio")
            .field("bytes-sent", 13_u64)
            .field("packets-sent", 3_u64)
            .build();
        let stats = gst::Structure::builder("application/x-webrtcsink-stats")
            .field("video", video)
            .field("audio", audio)
            .build();

        let telemetry = fold_telemetry(&stats, None);

        assert_eq!(telemetry.video_bytes_sent, Some(42.0));
        assert_eq!(telemetry.video_packets_sent, Some(7.0));
        assert_eq!(telemetry.audio_bytes_sent, Some(13.0));
        assert_eq!(telemetry.audio_packets_sent, Some(3.0));
    }
}
