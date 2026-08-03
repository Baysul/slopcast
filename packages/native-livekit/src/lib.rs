#![allow(
    clippy::needless_pass_by_value,
    reason = "napi-rs #[napi] function signatures must take ownership of String params for JS type conversion"
)]
#![allow(
    clippy::used_underscore_binding,
    reason = "clippy warns on room_events even though it must be bound for the receiver"
)]
#![allow(
    clippy::missing_errors_doc,
    reason = "NAPI exported functions return NapiResult without explicit rustdoc error sections"
)]

use napi::Result as NapiResult;
use napi_derive::napi;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(test)]
mod test_stubs;

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

#[napi(object)]
#[derive(Clone)]
pub struct CaptureConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_codec: Option<String>,
    pub max_bitrate: Option<f64>,
}

#[napi(object)]
#[derive(Default, Clone)]
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
    _stop: tokio::sync::oneshot::Sender<()>,
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
static VIDEO_SOURCE: ArcSwapOption<NativeVideoSource> = ArcSwapOption::const_empty();

fn with_guard<F, R>(f: F) -> NapiResult<R>
where
    F: FnOnce(&NativeLiveKit) -> NapiResult<R>,
{
    let guard = LIVEKIT
        .lock()
        .map_err(|e| napi::Error::from_reason(format!("Lock poisoned: {e}")))?;
    let Some(state) = guard.as_ref() else {
        return Err(napi::Error::from_reason("Room not connected"));
    };
    f(state)
}

#[napi]
pub fn connect_livekit_room(url: String, token: String) -> NapiResult<()> {
    let mut guard = LIVEKIT
        .lock()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    if guard.is_some() {
        return Err(napi::Error::from_reason(
            "Native LiveKit room is already connected",
        ));
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
        .map_err(|e| napi::Error::from_reason(format!("Failed to spawn: {e}")))?;

    *guard = Some(NativeLiveKit {
        pcm_tx,
        cmd_tx,
        _stop: stop_tx,
        _join: handle,
    });
    Ok(())
}

#[napi]
pub fn disconnect_livekit_room() -> NapiResult<()> {
    let mut guard = LIVEKIT
        .lock()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    if let Some(state) = guard.take() {
        let _ = state.cmd_tx.send(WorkerCmd::StopVideo);
        let _ = state.cmd_tx.send(WorkerCmd::Shutdown);
        let _ = state._stop.send(());
    }
    *guard = None;
    ROOM_CONNECTED.store(false, Ordering::SeqCst);
    SPECTATOR_COUNT.store(0, Ordering::Relaxed);
    VIDEO_ACTIVE.store(false, Ordering::SeqCst);
    VIDEO_SOURCE.store(None);
    Ok(())
}

#[napi]
pub fn is_livekit_room_connected() -> bool {
    ROOM_CONNECTED.load(Ordering::Relaxed)
}

/// Decodes raw i16 LE PCM bytes into samples, independent of the N-API
/// `Buffer` type so the conversion is unit-testable. On little-endian hosts
/// the fast path casts the slice directly; odd byte counts (and big-endian
/// hosts) fall back to per-pair decoding, dropping a trailing half sample.
fn decode_pcm_bytes(bytes: &[u8]) -> Vec<i16> {
    if cfg!(target_endian = "little")
        && let Ok(slice) = bytemuck::try_cast_slice::<u8, i16>(bytes)
    {
        return slice.to_vec();
    }
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Recombines the high and low 32-bit timestamp integers (as passed through
/// N-API) into a 64-bit microsecond integer. Bit-faithful: negative halves
/// are re-interpreted as their unsigned bit patterns.
#[cfg(any(target_os = "linux", test))]
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    reason = "Recombining high and low 32-bit timestamp integers into a 64-bit microsecond integer"
)]
fn recombine_dmabuf_timestamp(timestamp_lo: i32, timestamp_hi: i32) -> i64 {
    ((u64::from(timestamp_lo as u32)) | (u64::from(timestamp_hi as u32) << 32)) as i64
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

#[napi]
pub fn feed_pcm(pcm: napi::bindgen_prelude::Buffer) -> NapiResult<()> {
    with_guard(|state| {
        // PCM arrives packed as i16 LE bytes from the capture native module.
        let samples = decode_pcm_bytes(pcm.as_ref());
        // Channel full: WebRTC encoding is stalled, drop the newest chunk.
        match state.pcm_tx.try_send(samples) {
            Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(napi::Error::from_reason("PCM channel closed"))
            }
        }
    })
}

#[napi]
pub fn start_video_track(config: CaptureConfig) -> NapiResult<()> {
    with_guard(|state| {
        state
            .cmd_tx
            .send(WorkerCmd::StartVideo {
                config: config.clone(),
            })
            .map_err(|_| napi::Error::from_reason("Worker channel closed"))
    })
}

#[napi]
pub fn stop_video_track() -> NapiResult<()> {
    let guard = LIVEKIT
        .lock()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let Some(state) = guard.as_ref() else {
        return Ok(());
    };
    let _ = state.cmd_tx.send(WorkerCmd::StopVideo);
    Ok(())
}

#[napi]
pub fn is_video_track_active() -> bool {
    VIDEO_ACTIVE.load(Ordering::Relaxed)
}

#[napi]
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    reason = "Recombining high and low 32-bit timestamp integers into a 64-bit microsecond integer"
)]
pub fn capture_dmabuf_frame(
    dmabuf_fd: i32,
    width: u32,
    height: u32,
    pixel_format: i32,
    timestamp_lo: i32,
    timestamp_hi: i32,
) -> NapiResult<()> {
    #[cfg(target_os = "linux")]
    {
        let Some(source) = VIDEO_SOURCE.load_full() else {
            return Err(napi::Error::from_reason("Video track not active"));
        };
        let timestamp_us = recombine_dmabuf_timestamp(timestamp_lo, timestamp_hi);
        source.capture_dmabuf_frame(dmabuf_fd, width, height, pixel_format, timestamp_us);
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (
            dmabuf_fd,
            width,
            height,
            pixel_format,
            timestamp_lo,
            timestamp_hi,
        );
        Err(napi::Error::from_reason(
            "DMA-BUF frame capture is only supported on Linux",
        ))
    }
}

#[napi]
pub fn get_spectator_count() -> u32 {
    SPECTATOR_COUNT.load(Ordering::Relaxed)
}

/// Collects the latest libwebrtc stats for the local published tracks on the
/// worker thread (stats queries must run on the tokio runtime) and returns the
/// result over a bounded blocking channel. Falls back to an empty snapshot if
/// the worker cannot answer within 500 ms.
#[napi]
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
// f64 conversion loses no precision in practice (NAPI objects cannot carry u64).
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
    fn decode_pcm_decodes_little_endian_i16() {
        let bytes = 42i16.to_le_bytes();
        assert_eq!(decode_pcm_bytes(&bytes), vec![42]);
    }

    #[test]
    fn decode_pcm_handles_multiple_samples() {
        let samples: Vec<i16> = vec![-32768, 0, 32767];
        let mut bytes = Vec::new();
        for s in &samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        assert_eq!(decode_pcm_bytes(&bytes), samples);
    }

    #[test]
    fn decode_pcm_drops_trailing_odd_byte() {
        // 5 bytes = 2 full samples + 1 orphan byte that must be discarded.
        let bytes = [1u8, 0, 2, 0, 0xAB];
        let decoded = decode_pcm_bytes(&bytes);
        assert_eq!(decoded, vec![1, 2]);
    }

    #[test]
    fn decode_pcm_empty_input_is_empty() {
        assert!(decode_pcm_bytes(&[]).is_empty());
    }

    #[test]
    #[allow(
        clippy::cast_possible_wrap,
        reason = "test fixture: 0xABCD_EF01 deliberately reinterpreted as a negative i32"
    )]
    fn recombine_timestamp_roundtrips_positive_values() {
        assert_eq!(recombine_dmabuf_timestamp(0, 0), 0);
        assert_eq!(recombine_dmabuf_timestamp(0x1234_5678, 0), 0x1234_5678);
        assert_eq!(
            recombine_dmabuf_timestamp(0xABCD_EF01u32 as i32, 0x0000_0001),
            0x0000_0001_ABCD_EF01
        );
    }

    #[test]
    fn recombine_timestamp_handles_negative_halves_bit_faithfully() {
        // Both halves negative: lo = 0xFFFF_FFFF, hi = 0xFFFF_FFFF → -1.
        assert_eq!(recombine_dmabuf_timestamp(-1, -1), -1);
        // Negative high half with zero low half → 0x8000_0000_0000_0000.
        assert_eq!(recombine_dmabuf_timestamp(0, i32::MIN), i64::MIN);
        // Positive hi, negative lo: lo's top bit must extend into the result.
        assert_eq!(recombine_dmabuf_timestamp(-1, 1), 0x0000_0001_FFFF_FFFF);
    }

    #[test]
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
        assert!(err.to_string().contains("Room not connected"));
    }
}
