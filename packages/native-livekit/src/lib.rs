#![allow(
    clippy::needless_pass_by_value,
    reason = "napi-rs #[napi] function signatures must take ownership of String params for JS type conversion"
)]
#![allow(
    clippy::used_underscore_binding,
    reason = "clippy warns on room_events even though it must be bound for the receiver"
)]

use napi::Result as NapiResult;
use napi_derive::napi;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use livekit::options::{TrackPublishOptions, VideoCodec};
use livekit::prelude::*;
use livekit::track::{LocalTrack, LocalVideoTrack, TrackSource};
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::AudioSourceOptions;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::prelude::{RtcAudioSource, RtcVideoSource, VideoResolution};
use livekit::webrtc::video_source::native::NativeVideoSource;

pub const SAMPLE_RATE: u32 = 48000;
pub const CHANNELS: u32 = 2;

// ── Shared Types ─────────────────────────────────────────────────────────

#[napi(object)]
#[derive(Clone)]
pub struct CaptureConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_codec: Option<String>,
}

// ── Worker Commands ──────────────────────────────────────────────────────

enum WorkerCmd {
    StartVideo { config: CaptureConfig },
    StopVideo,
    Shutdown,
}

// ── Singleton ────────────────────────────────────────────────────────────

struct NativeLiveKit {
    pcm_tx: tokio::sync::mpsc::UnboundedSender<Vec<i16>>,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<WorkerCmd>,
    room_connected: Arc<AtomicBool>,
    spectator_count: Arc<AtomicU32>,
    #[allow(dead_code)]
    video_source: Arc<Mutex<Option<NativeVideoSource>>>,
    video_active: Arc<AtomicBool>,
    _stop: tokio::sync::oneshot::Sender<()>,
    _join: std::thread::JoinHandle<()>,
}

static LIVEKIT: Mutex<Option<NativeLiveKit>> = Mutex::new(None);

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

// ── NAPI: Room Connection ────────────────────────────────────────────────

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

    let (pcm_tx, pcm_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<WorkerCmd>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let video_source = Arc::new(Mutex::new(None));
    let video_active = Arc::new(AtomicBool::new(false));
    let room_connected = Arc::new(AtomicBool::new(false));
    let spectator_count = Arc::new(AtomicU32::new(0));

    let vs = video_source.clone();
    let va = video_active.clone();
    let rc = room_connected.clone();
    let sc = spectator_count.clone();

    let handle = std::thread::Builder::new()
        .name("lk-worker".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Tokio runtime");

            rt.block_on(async {
                if let Err(e) =
                    run_worker(url, token, pcm_rx, cmd_rx, stop_rx, vs, va, rc, sc).await
                {
                    eprintln!("[livekit] worker error: {e}");
                }
            });
        })
        .map_err(|e| napi::Error::from_reason(format!("Failed to spawn: {e}")))?;

    *guard = Some(NativeLiveKit {
        pcm_tx,
        cmd_tx,
        room_connected,
        spectator_count,
        video_source,
        video_active,
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
    Ok(())
}

#[napi]
pub fn is_livekit_room_connected() -> bool {
    LIVEKIT.lock().ok().is_some_and(|g| {
        g.as_ref()
            .is_some_and(|s| s.room_connected.load(Ordering::Relaxed))
    })
}

// ── NAPI: Audio PCM Feed ─────────────────────────────────────────────────

#[napi]
pub fn feed_pcm(pcm: Vec<i32>) -> NapiResult<()> {
    with_guard(|state| {
        let samples: Vec<i16> = pcm
            .into_iter()
            .map(|s| s.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
            .collect();
        state
            .pcm_tx
            .send(samples)
            .map_err(|e| napi::Error::from_reason(format!("Failed to send PCM: {e}")))?;
        Ok(())
    })
}

// ── NAPI: Video Track Control ────────────────────────────────────────────

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
    LIVEKIT.lock().ok().is_some_and(|g| {
        g.as_ref()
            .is_some_and(|s| s.video_active.load(Ordering::Relaxed))
    })
}

#[napi]
pub fn capture_dmabuf_frame(
    dmabuf_fd: i32,
    width: u32,
    height: u32,
    pixel_format: i32,
    timestamp_lo: i32,
    timestamp_hi: i32,
) -> NapiResult<()> {
    let guard = LIVEKIT
        .lock()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let Some(state) = guard.as_ref() else {
        return Err(napi::Error::from_reason("Room not connected"));
    };
    let vs = state
        .video_source
        .lock()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let Some(ref source) = *vs else {
        return Err(napi::Error::from_reason("Video track not active"));
    };
    let timestamp_us = ((timestamp_lo as u32 as u64) | ((timestamp_hi as u32 as u64) << 32)) as i64;
    source.capture_dmabuf_frame(dmabuf_fd, width, height, pixel_format, timestamp_us);
    Ok(())
}

#[napi]
pub fn get_spectator_count() -> u32 {
    LIVEKIT
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref()
                .map(|s| s.spectator_count.load(Ordering::Relaxed))
        })
        .unwrap_or(0)
}

// ── Worker Thread ────────────────────────────────────────────────────────

async fn run_worker(
    url: String,
    token: String,
    mut pcm_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<i16>>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<WorkerCmd>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
    video_source: Arc<Mutex<Option<NativeVideoSource>>>,
    video_active: Arc<AtomicBool>,
    room_connected: Arc<AtomicBool>,
    spectator_count: Arc<AtomicU32>,
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
    room_connected.store(true, Ordering::SeqCst);

    let samples_per_10ms = (SAMPLE_RATE / 100 * CHANNELS) as usize;
    let mut buffer = Vec::new();

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
                        handle_start_video(&room, &config, &video_source, &video_active).await;
                    }
                    WorkerCmd::StopVideo => {
                        handle_stop_video(&room, &video_source, &video_active).await;
                    }
                    WorkerCmd::Shutdown => {
                        break;
                    }
                }
            }
            event = room_events.recv() => {
                if let Some(event) = event {
                    match event {
                        RoomEvent::ParticipantConnected(_)
                        | RoomEvent::ParticipantDisconnected(_)
                        | RoomEvent::ParticipantsUpdated { .. } => {
                            spectator_count.store(
                                room.remote_participants().len() as u32,
                                Ordering::Relaxed,
                            );
                        }
                        _ => {}
                    }
                }
            }
            pcm = pcm_rx.recv() => {
                let Some(pcm_chunk) = pcm else {
                    break;
                };
                buffer.extend_from_slice(&pcm_chunk);
                while buffer.len() >= samples_per_10ms {
                    let chunk: Vec<i16> = buffer.drain(..samples_per_10ms).collect();
                    let frame = AudioFrame {
                        data: chunk.into(),
                        sample_rate: SAMPLE_RATE,
                        num_channels: CHANNELS,
                        samples_per_channel: (SAMPLE_RATE / 100) as u32,
                    };
                    if let Err(e) = audio.capture_frame(&frame).await {
                        eprintln!("[livekit] audio capture_frame error: {e}");
                    }
                }
            }
        }
    }

    handle_stop_video(&room, &video_source, &video_active).await;

    room_connected.store(false, Ordering::SeqCst);
    room.close().await?;
    Ok(())
}

async fn handle_start_video(
    room: &Room,
    config: &CaptureConfig,
    video_source: &Arc<Mutex<Option<NativeVideoSource>>>,
    video_active: &Arc<AtomicBool>,
) {
    handle_stop_video(room, video_source, video_active).await;

    let resolution = VideoResolution {
        width: config.width,
        height: config.height,
    };
    let source = NativeVideoSource::new(resolution, true);

    let track =
        LocalVideoTrack::create_video_track("screen_share", RtcVideoSource::Native(source.clone()));

    let codec = config
        .video_codec
        .as_deref()
        .and_then(|s| match s {
            "vp8" => Some(VideoCodec::VP8),
            "vp9" => Some(VideoCodec::VP9),
            "h264" => Some(VideoCodec::H264),
            "av1" => Some(VideoCodec::AV1),
            "h265" => Some(VideoCodec::H265),
            _ => None,
        })
        .unwrap_or(VideoCodec::VP9);

    match room
        .local_participant()
        .publish_track(
            LocalTrack::Video(track),
            TrackPublishOptions {
                source: TrackSource::Screenshare,
                video_codec: codec,
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

    *video_source.lock().unwrap() = Some(source.clone());
    video_active.store(true, Ordering::SeqCst);
}

async fn handle_stop_video(
    room: &Room,
    video_source: &Arc<Mutex<Option<NativeVideoSource>>>,
    video_active: &Arc<AtomicBool>,
) {
    video_source.lock().unwrap().take();
    video_active.store(false, Ordering::SeqCst);

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
