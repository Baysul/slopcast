use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

mod desktop_capture;

use arc_swap::ArcSwapOption;
use livekit::options::{TrackPublishOptions, VideoCodec, VideoEncoding};
use livekit::prelude::*;
use livekit::track::{LocalTrack, LocalVideoTrack, TrackSource};
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::AudioSourceOptions;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::prelude::{RtcAudioSource, RtcVideoSource, VideoResolution};
use livekit::webrtc::stats::RtcStats;
use livekit::webrtc::video_source::native::NativeVideoSource;
use std::collections::HashMap;
use std::time::Duration;

pub const SAMPLE_RATE: u32 = 48000;
pub const CHANNELS: u32 = 2;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_codec: Option<String>,
    pub max_bitrate: Option<f64>,
}

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeTelemetry {
    pub video_codec: Option<String>,
    pub video_bytes_sent: Option<f64>,
    pub video_packets_sent: Option<f64>,
    pub video_packets_lost: Option<f64>,
    pub video_frames_sent: Option<f64>,
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    pub audio_codec: Option<String>,
    pub audio_bytes_sent: Option<f64>,
    pub audio_packets_sent: Option<f64>,
    pub audio_packets_lost: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub timestamp_ms: Option<f64>,
}

/// Per-stage counters for the desktop capture pipeline, reset on every
/// `startDesktopCapture`. `framesDequeued` counts frames received from the
/// capturer; `framesPushed` counts those converted to I420 and delivered to
/// the video track; `framesDropped` counts frames skipped while no track was
/// active; `previewFramesSent` counts base64 JPEG preview frames emitted via
/// the preview callback (640×360 @ ~15 fps, MIGRATION §9.1); `captureErrors`
/// counts capturer-reported failures.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopCaptureStats {
    pub frames_dequeued: i64,
    pub frames_pushed: i64,
    pub frames_dropped: i64,
    pub capture_errors: i64,
    pub preview_frames_sent: i64,
    pub last_width: i64,
    pub last_height: i64,
}

enum WorkerCmd {
    StartVideo {
        config: CaptureConfig,
    },
    StopVideo,
    GetTelemetry {
        reply: std::sync::mpsc::Sender<NativeTelemetry>,
    },
    Shutdown,
}

struct NativeLiveKit {
    pcm_tx: tokio::sync::mpsc::Sender<Vec<i16>>,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<WorkerCmd>,
    stop: tokio::sync::oneshot::Sender<()>,
    _join: std::thread::JoinHandle<()>,
}

/// Bounded PCM queue: ~1.28 s of 10 ms audio chunks. A full channel means
/// WebRTC audio encoding is stalled; the newest chunk is dropped rather than
/// letting memory grow without bound (same drop-newest policy as `audio_ring`).
const PCM_CHANNEL_CAPACITY: usize = 128;

static LIVEKIT: Mutex<Option<NativeLiveKit>> = Mutex::new(None);

static ROOM_CONNECTED: AtomicBool = AtomicBool::new(false);
static SPECTATOR_COUNT: AtomicU32 = AtomicU32::new(0);
static VIDEO_ACTIVE: AtomicBool = AtomicBool::new(false);
/// The published video track's source; the desktop capturer feeds it frames.
/// `None` while no track is active — capture frames are dropped then.
pub(crate) static VIDEO_SOURCE: ArcSwapOption<NativeVideoSource> = ArcSwapOption::const_empty();

// The bundled libwebrtc statically links hidden-weak `pw_*` dlopen shims
// (`modules::portal::*`, from `pipewire_stubs.cc`) that capture pipewire-rs's
// direct `pw_init` reference in the same binary at link time. The shims
// tail-jump through static pointers that stay NULL until `InitializePipewire`
// dlopens `libpipewire` and arms them — any earlier `pw_init` call SIGSEGVs.
#[cfg(target_os = "linux")]
unsafe extern "C" {
    #[link_name = "_ZN14modules_portal18InitializePipewireEPv"]
    fn webrtc_initialize_pipewire(module: *mut std::ffi::c_void);
}

/// Arms libwebrtc's bundled `PipeWire` dlopen shims so `pipewire-rs` calls
/// reach the real libpipewire. Must run before any native-rust `PipeWire`
/// usage (see the Tauri setup wiring).
#[cfg(target_os = "linux")]
pub fn arm_pipewire_shims() {
    // SAFETY: `InitializePipewire` only dlopens libpipewire and stores
    // dlsym results into static pointers; the module argument is forwarded
    // to the stub machinery and never dereferenced.
    unsafe { webrtc_initialize_pipewire(std::ptr::null_mut()) };
}

/// No-op on platforms without the libwebrtc PipeWire shims.
#[cfg(not(target_os = "linux"))]
pub fn arm_pipewire_shims() {}

fn with_guard<F, R>(f: F) -> Result<R, String>
where
    F: FnOnce(&NativeLiveKit) -> Result<R, String>,
{
    let guard = LIVEKIT.lock().map_err(|e| format!("Lock poisoned: {e}"))?;
    let Some(state) = guard.as_ref() else {
        return Err("Room not connected".into());
    };
    f(state)
}

/// Connects to a `LiveKit` room and publishes a screenshare audio track. The
/// worker thread runs its own tokio runtime; video tracks are published
/// through `start_video_track` once the room is live.
///
/// # Errors
///
/// Returns an error if a room is already connected or the worker thread cannot
/// be spawned.
pub fn connect_livekit_room(url: String, token: String) -> Result<(), String> {
    let mut guard = LIVEKIT.lock().map_err(|e| format!("Lock poisoned: {e}"))?;
    if guard.is_some() {
        return Err("Native LiveKit room is already connected".into());
    }

    ROOM_CONNECTED.store(false, Ordering::Relaxed);
    SPECTATOR_COUNT.store(0, Ordering::Relaxed);
    VIDEO_ACTIVE.store(false, Ordering::Relaxed);
    VIDEO_SOURCE.store(None);

    let (pcm_tx, pcm_rx) = tokio::sync::mpsc::channel::<Vec<i16>>(PCM_CHANNEL_CAPACITY);
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<WorkerCmd>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();

    let handle = std::thread::Builder::new()
        .name("lk-worker".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                eprintln!("[livekit] Failed to build tokio runtime");
                return;
            };

            rt.block_on(async {
                if let Err(e) = run_worker(url, token, pcm_rx, cmd_rx, stop_rx).await {
                    eprintln!("[livekit] worker error: {e}");
                }
            });
        })
        .map_err(|e| format!("Failed to spawn: {e}"))?;

    *guard = Some(NativeLiveKit {
        pcm_tx,
        cmd_tx,
        stop: stop_tx,
        _join: handle,
    });
    Ok(())
}

/// Disconnects from the live room, stopping the video track and desktop
/// capture, and tearing down the worker thread.
///
/// # Errors
///
/// Returns an error if the room state lock is poisoned.
pub fn disconnect_livekit_room() -> Result<(), String> {
    let mut guard = LIVEKIT.lock().map_err(|e| format!("Lock poisoned: {e}"))?;
    if let Some(state) = guard.take() {
        let _ = state.cmd_tx.send(WorkerCmd::StopVideo);
        let _ = state.cmd_tx.send(WorkerCmd::Shutdown);
        let _ = state.stop.send(());
    }
    *guard = None;
    desktop_capture::stop();
    ROOM_CONNECTED.store(false, Ordering::SeqCst);
    SPECTATOR_COUNT.store(0, Ordering::Relaxed);
    VIDEO_ACTIVE.store(false, Ordering::SeqCst);
    VIDEO_SOURCE.store(None);
    Ok(())
}

pub fn is_livekit_room_connected() -> bool {
    ROOM_CONNECTED.load(Ordering::Relaxed)
}

/// Resolves a codec string to a `VideoCodec`, defaulting to VP9 for anything
/// unrecognized (including `None`).
fn parse_video_codec(codec: Option<&str>) -> VideoCodec {
    match codec {
        Some("vp8") => VideoCodec::VP8,
        Some("h264") => VideoCodec::H264,
        Some("av1") => VideoCodec::AV1,
        _ => VideoCodec::VP9,
    }
}

/// Drains full `samples_per_chunk`-sized chunks from the worker's buffer,
/// leaving a partial tail queued for the next push. Off-by-one safe: a buffer
/// with exactly `n * samples_per_chunk` samples produces exactly `n` chunks.
fn drain_pcm_chunks(buffer: &mut VecDeque<i16>, samples_per_chunk: usize, out: &mut Vec<Vec<i16>>) {
    while buffer.len() >= samples_per_chunk {
        out.push(buffer.drain(..samples_per_chunk).collect());
    }
}

/// Feeds one PCM chunk (48 kHz stereo `i16` samples) into the room's audio
/// track. When the channel is full, WebRTC encoding is stalled and the newest
/// chunk is dropped rather than queued.
///
/// # Errors
///
/// Returns an error if no room is connected or the worker's PCM channel is
/// closed.
pub fn feed_pcm(pcm: Vec<i16>) -> Result<(), String> {
    with_guard(|state| {
        // Channel full: WebRTC encoding is stalled, drop the newest chunk.
        match state.pcm_tx.try_send(pcm) {
            Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err("PCM channel closed".into())
            }
        }
    })
}

/// Publishes (or re-publishes) the screenshare video track with the given
/// encoder settings, restarting the track without restarting the capture.
///
/// # Errors
///
/// Returns an error if no room is connected or the worker channel is closed.
pub fn start_video_track(config: CaptureConfig) -> Result<(), String> {
    with_guard(|state| {
        state
            .cmd_tx
            .send(WorkerCmd::StartVideo { config })
            .map_err(|_| "Worker channel closed".into())
    })
}

/// Unpublishes the screenshare video track.
///
/// # Errors
///
/// Returns an error if the room state lock is poisoned.
pub fn stop_video_track() -> Result<(), String> {
    let guard = LIVEKIT.lock().map_err(|e| format!("Lock poisoned: {e}"))?;
    let Some(state) = guard.as_ref() else {
        return Ok(());
    };
    let _ = state.cmd_tx.send(WorkerCmd::StopVideo);
    Ok(())
}

pub fn is_video_track_active() -> bool {
    VIDEO_ACTIVE.load(Ordering::Relaxed)
}

/// Starts the desktop capturer on its own thread. Returns once the capturer is
/// running; on Wayland the native portal picker then appears and frames flow
/// after the user selects a source.
///
/// # Errors
///
/// Returns an error if a capture session is already active, the thread cannot
/// be spawned, or the capturer fails to initialize within five seconds.
pub fn start_desktop_capture() -> Result<bool, String> {
    desktop_capture::start()
}

/// Stops the active desktop capture session (closing its portal stream).
#[must_use]
pub fn stop_desktop_capture() -> bool {
    desktop_capture::stop()
}

/// Returns `true` while the capturer is running (the portal picker may still
/// be awaiting a selection).
#[must_use]
pub fn is_desktop_capture_active() -> bool {
    desktop_capture::is_active()
}

/// Returns the current desktop capture stage counters.
#[must_use]
pub fn get_desktop_capture_stats() -> DesktopCaptureStats {
    desktop_capture::stats()
}

/// Registers the preview-frame callback: the capture thread invokes it with
/// base64 JPEG frames `(width, height, data, pts_us)` at ~15 fps while a
/// capture session is active (MIGRATION §9.1). The engine stays
/// Tauri-unaware — the Tauri backend wires this callback to its
/// `preview-frame` event. Replaces any previously registered callback.
pub fn set_preview_callback(callback: Box<dyn Fn(u32, u32, String, i64) + Send + Sync>) {
    desktop_capture::set_preview_callback(callback);
}

/// Clears the registered preview-frame callback.
pub fn clear_preview_callback() {
    desktop_capture::clear_preview_callback();
}

pub fn get_spectator_count() -> u32 {
    SPECTATOR_COUNT.load(Ordering::Relaxed)
}

/// Collects the latest libwebrtc stats for the local published tracks on the
/// worker thread (stats queries must run on the tokio runtime) and returns the
/// result over a bounded blocking channel. Falls back to an empty snapshot if
/// the worker cannot answer within 500 ms.
pub fn get_native_telemetry() -> NativeTelemetry {
    let Ok(guard) = LIVEKIT.lock() else {
        return NativeTelemetry::default();
    };
    let Some(state) = guard.as_ref() else {
        return NativeTelemetry::default();
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if state
        .cmd_tx
        .send(WorkerCmd::GetTelemetry { reply: reply_tx })
        .is_err()
    {
        return NativeTelemetry::default();
    }
    drop(guard);
    reply_rx
        .recv_timeout(Duration::from_millis(500))
        .unwrap_or_default()
}

async fn collect_telemetry(room: &Room) -> NativeTelemetry {
    let mut telemetry = NativeTelemetry::default();
    for (_sid, publication) in room.local_participant().track_publications() {
        let Some(track) = publication.track() else {
            continue;
        };
        let Ok(stats) = track.get_stats().await else {
            continue;
        };
        fold_stats(&mut telemetry, &stats);
    }
    telemetry
}

// Byte counters and epoch-millisecond timestamps stay well below 2^53, so the
// f64 conversion loses no precision in practice (JSON cannot carry u64).
#[allow(
    clippy::cast_precision_loss,
    reason = "Byte counters and epoch-millisecond timestamps stay well below 2^53; f64 is exact in this range"
)]
fn fold_stats(telemetry: &mut NativeTelemetry, stats: &[RtcStats]) {
    let mut video_ssrc = None;
    let mut audio_ssrc = None;
    let mut codecs = HashMap::<String, String>::new();

    for stat in stats {
        match stat {
            RtcStats::Codec(codec) => {
                codecs.insert(codec.rtc.id.clone(), codec.codec.mime_type.clone());
            }
            RtcStats::OutboundRtp(outbound) => {
                let mime = codecs.get(&outbound.stream.codec_id);
                match outbound.stream.kind.as_str() {
                    "video" => {
                        video_ssrc = Some(outbound.stream.ssrc);
                        telemetry.video_codec = mime.cloned();
                        telemetry.video_bytes_sent = Some(outbound.sent.bytes_sent as f64);
                        telemetry.video_packets_sent = Some(outbound.sent.packets_sent as f64);
                        telemetry.video_frames_sent =
                            Some(f64::from(outbound.outbound.frames_sent));
                        telemetry.video_width = Some(outbound.outbound.frame_width);
                        telemetry.video_height = Some(outbound.outbound.frame_height);
                        telemetry.timestamp_ms = Some(outbound.rtc.timestamp as f64);
                    }
                    "audio" => {
                        audio_ssrc = Some(outbound.stream.ssrc);
                        telemetry.audio_codec = mime.cloned();
                        telemetry.audio_bytes_sent = Some(outbound.sent.bytes_sent as f64);
                        telemetry.audio_packets_sent = Some(outbound.sent.packets_sent as f64);
                    }
                    _ => {}
                }
            }
            RtcStats::RemoteInboundRtp(inbound) => {
                if inbound.remote_inbound.round_trip_time > 0.0 {
                    telemetry.rtt_ms = Some(inbound.remote_inbound.round_trip_time * 1000.0);
                }
                if let Ok(packets_lost) = u64::try_from(inbound.received.packets_lost.max(0)) {
                    if Some(inbound.stream.ssrc) == video_ssrc {
                        telemetry.video_packets_lost = Some(packets_lost as f64);
                    } else if Some(inbound.stream.ssrc) == audio_ssrc {
                        telemetry.audio_packets_lost = Some(packets_lost as f64);
                    }
                }
            }
            _ => {}
        }
    }
}

async fn run_worker(
    url: String,
    token: String,
    mut pcm_rx: tokio::sync::mpsc::Receiver<Vec<i16>>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerCmd>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let audio = NativeAudioSource::new(AudioSourceOptions::default(), SAMPLE_RATE, CHANNELS, 0);

    let (room, mut room_events) = Room::connect(&url, &token, RoomOptions::default()).await?;

    let audio_track = livekit::track::LocalAudioTrack::create_audio_track(
        "screen_audio",
        RtcAudioSource::Native(audio.clone()),
    );

    room.local_participant()
        .publish_track(
            livekit::track::LocalTrack::Audio(audio_track),
            TrackPublishOptions {
                source: TrackSource::ScreenshareAudio,
                ..Default::default()
            },
        )
        .await?;

    eprintln!("[livekit] audio track published");
    ROOM_CONNECTED.store(true, Ordering::SeqCst);

    let samples_per_10ms = (SAMPLE_RATE / 100 * CHANNELS) as usize;
    let mut buffer = VecDeque::new();

    loop {
        tokio::select! {
            biased;
            _ = &mut stop_rx => {
                break;
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    break;
                };
                match cmd {
                    WorkerCmd::StartVideo { config } => {
                        handle_start_video(&room, &config).await;
                    }
                    WorkerCmd::StopVideo => {
                        handle_stop_video(&room).await;
                    }
                    WorkerCmd::GetTelemetry { reply } => {
                        let telemetry = collect_telemetry(&room).await;
                        let _ = reply.send(telemetry);
                    }
                    WorkerCmd::Shutdown => {
                        break;
                    }
                }
            }
            event = room_events.recv() => {
                if let Some(
                    RoomEvent::ParticipantConnected(_)
                    | RoomEvent::ParticipantDisconnected(_)
                    | RoomEvent::ParticipantsUpdated { .. },
                ) = event
                {
                    SPECTATOR_COUNT.store(
                        u32::try_from(room.remote_participants().len()).unwrap_or(u32::MAX),
                        Ordering::Relaxed,
                    );
                }
            }
            pcm = pcm_rx.recv() => {
                let Some(pcm_chunk) = pcm else {
                    break;
                };
                buffer.extend(pcm_chunk);
                let mut chunks = Vec::new();
                drain_pcm_chunks(&mut buffer, samples_per_10ms, &mut chunks);
                for chunk in chunks {
                    let frame = AudioFrame {
                        data: chunk.into(),
                        sample_rate: SAMPLE_RATE,
                        num_channels: CHANNELS,
                        samples_per_channel: SAMPLE_RATE / 100,
                    };
                    if let Err(e) = audio.capture_frame(&frame).await {
                        eprintln!("[livekit] audio capture_frame error: {e}");
                    }
                }
            }
        }
    }

    handle_stop_video(&room).await;

    ROOM_CONNECTED.store(false, Ordering::SeqCst);
    SPECTATOR_COUNT.store(0, Ordering::Relaxed);
    room.close().await?;
    Ok(())
}

async fn handle_start_video(room: &Room, config: &CaptureConfig) {
    handle_stop_video(room).await;

    let resolution = VideoResolution {
        width: config.width,
        height: config.height,
    };
    let source = NativeVideoSource::new(resolution, true);

    let track =
        LocalVideoTrack::create_video_track("screen_share", RtcVideoSource::Native(source.clone()));

    let codec = parse_video_codec(config.video_codec.as_deref());

    let encoding = config.max_bitrate.map(|bitrate| VideoEncoding {
        // The renderer sends the bitrate limit from its settings UI as a whole
        // number; rounding the f64 to u64 is exact for all sane values.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "Bitrate limits from the settings UI are whole numbers; rounding is exact"
        )]
        max_bitrate: bitrate.round() as u64,
        max_framerate: f64::from(config.fps),
    });

    match room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                video_codec: codec,
                video_encoding: encoding,
                ..Default::default()
            },
        )
        .await
    {
        Ok(_) => eprintln!("[livekit] video track published"),
        Err(e) => {
            eprintln!("[livekit] video track publish failed: {e}");
            return;
        }
    }

    VIDEO_SOURCE.store(Some(Arc::new(source)));
    VIDEO_ACTIVE.store(true, Ordering::SeqCst);
}

async fn handle_stop_video(room: &Room) {
    VIDEO_SOURCE.store(None);
    VIDEO_ACTIVE.store(false, Ordering::SeqCst);

    let publications: Vec<_> = room
        .local_participant()
        .track_publications()
        .into_iter()
        .filter(|(_sid, p)| p.kind() == livekit::track::TrackKind::Video)
        .collect();
    for (_sid, pub_info) in publications {
        let _ = room
            .local_participant()
            .unpublish_track(&pub_info.sid())
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use livekit::webrtc::stats::dictionaries as d;
    use livekit::webrtc::stats::{CodecStats, OutboundRtpStats, RemoteInboundRtpStats};

    fn codec_stats(id: &str, mime: &str) -> RtcStats {
        RtcStats::Codec(CodecStats {
            rtc: d::RtcStats {
                id: id.into(),
                timestamp: 0,
            },
            codec: d::CodecStats {
                mime_type: mime.into(),
                ..Default::default()
            },
        })
    }

    fn outbound_stats(kind: &str, ssrc: u32, codec_id: &str, bytes: u64, frames: u32) -> RtcStats {
        RtcStats::OutboundRtp(OutboundRtpStats {
            rtc: d::RtcStats {
                id: format!("out-{kind}"),
                timestamp: 1_750_000_000_000,
            },
            stream: d::RtpStreamStats {
                ssrc,
                kind: kind.into(),
                codec_id: codec_id.into(),
                ..Default::default()
            },
            sent: d::SentRtpStreamStats {
                packets_sent: 100,
                bytes_sent: bytes,
            },
            outbound: d::OutboundRtpStreamStats {
                frames_sent: frames,
                frame_width: 1920,
                frame_height: 1080,
                ..Default::default()
            },
        })
    }

    fn remote_inbound_stats(ssrc: u32, packets_lost: i64, rtt_s: f64) -> RtcStats {
        RtcStats::RemoteInboundRtp(RemoteInboundRtpStats {
            rtc: d::RtcStats {
                id: format!("inb-{ssrc}"),
                timestamp: 0,
            },
            stream: d::RtpStreamStats {
                ssrc,
                ..Default::default()
            },
            received: d::ReceivedRtpStreamStats {
                packets_lost,
                ..Default::default()
            },
            remote_inbound: d::RemoteInboundRtpStreamStats {
                round_trip_time: rtt_s,
                ..Default::default()
            },
        })
    }

    #[test]
    fn fold_stats_extracts_outbound_video_and_audio() {
        let stats = [
            codec_stats("codec-v", "video/VP8"),
            codec_stats("codec-a", "audio/OPUS"),
            outbound_stats("video", 11, "codec-v", 2_000_000, 1200),
            outbound_stats("audio", 22, "codec-a", 400_000, 0),
        ];
        let mut t = NativeTelemetry::default();
        fold_stats(&mut t, &stats);
        assert_eq!(t.video_codec.as_deref(), Some("video/VP8"));
        assert_eq!(t.video_bytes_sent, Some(2_000_000.0));
        assert_eq!(t.video_packets_sent, Some(100.0));
        assert_eq!(t.video_frames_sent, Some(1200.0));
        assert_eq!(t.video_width, Some(1920));
        assert_eq!(t.video_height, Some(1080));
        assert_eq!(t.timestamp_ms, Some(1_750_000_000_000.0));
        assert_eq!(t.audio_codec.as_deref(), Some("audio/OPUS"));
        assert_eq!(t.audio_bytes_sent, Some(400_000.0));
        assert_eq!(t.audio_packets_sent, Some(100.0));
    }

    #[test]
    fn fold_stats_attributes_packets_lost_by_ssrc_and_reports_rtt() {
        let stats = [
            outbound_stats("video", 11, "codec-v", 0, 0),
            outbound_stats("audio", 22, "codec-a", 0, 0),
            remote_inbound_stats(11, -3, 0.05),
            remote_inbound_stats(22, 7, 0.0),
        ];
        let mut t = NativeTelemetry::default();
        fold_stats(&mut t, &stats);
        // Negative lost values clamp to 0; RTT of 0.0 (unmeasured) is ignored.
        assert_eq!(t.video_packets_lost, Some(0.0));
        assert_eq!(t.audio_packets_lost, Some(7.0));
        assert_eq!(t.rtt_ms, Some(50.0));
    }

    #[test]
    fn fold_stats_ignores_unrelated_stat_types() {
        let mut t = NativeTelemetry::default();
        fold_stats(&mut t, &[]);
        assert!(t.video_bytes_sent.is_none());
        assert!(t.audio_codec.is_none());
        assert!(t.rtt_ms.is_none());
    }

    #[test]
    #[allow(
        clippy::cast_possible_wrap,
        reason = "test fixture: 0xABCD_EF01 deliberately reinterpreted as a negative i32"
    )]
    fn parse_video_codec_maps_known_strings() {
        assert_eq!(parse_video_codec(Some("vp8")), VideoCodec::VP8);
        assert_eq!(parse_video_codec(Some("vp9")), VideoCodec::VP9);
        assert_eq!(parse_video_codec(Some("h264")), VideoCodec::H264);
        assert_eq!(parse_video_codec(Some("av1")), VideoCodec::AV1);
    }

    #[test]
    fn parse_video_codec_falls_back_to_vp9() {
        assert_eq!(parse_video_codec(Some("theora")), VideoCodec::VP9);
        assert_eq!(parse_video_codec(Some("VP9")), VideoCodec::VP9);
        assert_eq!(parse_video_codec(Some("")), VideoCodec::VP9);
        assert_eq!(parse_video_codec(None), VideoCodec::VP9);
    }

    #[test]
    fn drain_chunks_emits_nothing_below_chunk_size() {
        let mut buffer = VecDeque::from(vec![1i16, 2, 3]);
        let mut chunks = Vec::new();
        drain_pcm_chunks(&mut buffer, 960, &mut chunks);
        assert!(chunks.is_empty());
        assert_eq!(buffer.len(), 3);
    }

    #[test]
    fn drain_chunks_splits_exact_multiples_without_remainder() {
        let mut buffer: VecDeque<i16> = (0..1920).collect();
        let mut chunks = Vec::new();
        drain_pcm_chunks(&mut buffer, 960, &mut chunks);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], (0..960).collect::<Vec<i16>>());
        assert_eq!(chunks[1], (960..1920).collect::<Vec<i16>>());
        assert!(buffer.is_empty());
    }

    #[test]
    fn drain_chunks_carries_partial_tail_between_pushes() {
        let mut buffer: VecDeque<i16> = (0..1000).collect();
        let mut chunks = Vec::new();
        drain_pcm_chunks(&mut buffer, 960, &mut chunks);
        assert_eq!(chunks.len(), 1);
        assert_eq!(buffer.len(), 40);
        // The next push fills the tail to a full chunk.
        buffer.extend(1000..1920);
        drain_pcm_chunks(&mut buffer, 960, &mut chunks);
        assert_eq!(chunks.len(), 2);
        assert!(buffer.is_empty());
    }

    #[test]
    fn with_guard_errors_when_no_room_is_connected() {
        // The singleton is empty in a fresh test binary; the guard must fail
        // with the "Room not connected" reason, not poison or hang.
        let Err(err) = with_guard(|_| Ok(())) else {
            panic!("expected an error without a connected room");
        };
        assert!(err.contains("Room not connected"));
    }
}
