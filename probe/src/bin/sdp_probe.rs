//! Temporary diagnostic probe (2026-08-06): publish a synthetic video track
//! through the same livekit SDK path as the desktop app — but drive the
//! source DIRECTLY at 60 fps (no app pacer, no portal) — and sample
//! outbound-rtp `framesEncoded` twice to measure the encoder's real
//! sustained framerate. 30 fps here => the cap lives in the
//! SDK/C++/SFU-negotiation path; 60 fps => the app's delivery is the cap.
//! Run: livekit-server --dev on :7880, then
//! `cargo run -p pw-conflict-probe --bin sdp_probe`.

use std::time::Duration;

use livekit::options::{TrackPublishOptions, VideoCodec, VideoEncoding};
use livekit::prelude::*;
use livekit::track::{LocalTrack, LocalVideoTrack, TrackSource};
use livekit::webrtc::prelude::{
    I420Buffer, RtcVideoSource, VideoFrame, VideoResolution, VideoRotation,
};
use livekit::webrtc::stats::RtcStats;
use livekit::webrtc::video_source::native::NativeVideoSource;

fn main() {
    let codec = match std::env::var("PROBE_CODEC").as_deref() {
        Ok("h264") => VideoCodec::H264,
        Ok("vp9") => VideoCodec::VP9,
        Ok("av1") => VideoCodec::AV1,
        _ => VideoCodec::VP8,
    };
    let width: u32 = std::env::var("PROBE_WIDTH").ok().and_then(|v| v.parse().ok()).unwrap_or(1280);
    let height: u32 = std::env::var("PROBE_HEIGHT").ok().and_then(|v| v.parse().ok()).unwrap_or(720);
    let fps: u32 = std::env::var("PROBE_FPS").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    let bitrate: u64 = std::env::var("PROBE_BITRATE").ok().and_then(|v| v.parse().ok()).unwrap_or(80_000_000);

    let token = livekit_api::access_token::AccessToken::with_api_key("devkey", "secret")
        .with_identity("sdp-probe")
        .with_grants(livekit_api::access_token::VideoGrants {
            room_join: true,
            room: "probe-room".into(),
            can_publish: true,
            ..Default::default()
        })
        .to_jwt()
        .unwrap();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (room, _events) =
            Room::connect("ws://127.0.0.1:7880", &token, RoomOptions::default())
                .await
                .expect("connect");
        println!("[probe] room connected");

        let source = NativeVideoSource::new(
            VideoResolution {
                width,
                height,
            },
            true,
        );
        let track =
            LocalVideoTrack::create_video_track("screen_share", RtcVideoSource::Native(source.clone()));

        room.local_participant()
            .publish_track(
                LocalTrack::Video(track),
                TrackPublishOptions {
                    source: TrackSource::Screenshare,
                    video_codec: codec,
                    video_encoding: Some(VideoEncoding {
                        max_bitrate: bitrate,
                        max_framerate: f64::from(fps),
                    }),
                    simulcast: false,
                    ..Default::default()
                },
            )
            .await
            .expect("publish");
        println!(
            "[probe] track published ({width}x{height}, {codec:?}, {} Mbps, {fps} fps requested)",
            bitrate / 1_000_000
        );

        let started = std::time::Instant::now();
        let mut first_sample = None;
        let mut pushed = 0u64;
        let push_interval = Duration::from_micros(1_000_000 / u64::from(fps.max(1)));
        loop {
            let t = started.elapsed();
            if t >= Duration::from_secs(6) {
                break;
            }
            if t.as_secs() == 1 && first_sample.is_none() {
                first_sample = Some(sample(&room).await);
            }
            pushed += 1;
            let mut frame_buf = I420Buffer::new(width, height);
            {
                let (y, u, v) = frame_buf.data_mut();
                y.fill(128);
                u.fill(128);
                v.fill(128);
            }
            let frame = VideoFrame {
                rotation: VideoRotation::VideoRotation0,
                timestamp_us: t.as_micros() as i64,
                frame_metadata: None,
                buffer: frame_buf,
            };
            source.capture_frame(&frame);
            tokio::time::sleep(push_interval).await;
        }
        let second = sample(&room).await;
        let first = first_sample.unwrap();
        println!(
            "[probe] framesEncoded at 1s: {first}; at 6s: {second}; sustained = {:.1} fps over 5s; input pushed = {pushed}",
            (second - first) as f64 / 5.0
        );
        println!("[probe] videoBytes at 6s: {}", sample_bytes(&room).await);

        room.close().await.ok();
    });
}

/// Frames encoded by the published video track (outbound-rtp framesEncoded).
async fn sample(room: &Room) -> u64 {
    for (_sid, publication) in room.local_participant().track_publications() {
        let Some(track) = publication.track() else {
            continue;
        };
        if track.kind() != TrackKind::Video {
            continue;
        }
        let Ok(stats) = track.get_stats().await else {
            continue;
        };
        for stat in stats {
            if let RtcStats::OutboundRtp(outbound) = stat {
                return u64::from(outbound.outbound.frames_encoded);
            }
        }
    }
    0
}

/// Bytes sent by the published video track.
async fn sample_bytes(room: &Room) -> u64 {
    for (_sid, publication) in room.local_participant().track_publications() {
        let Some(track) = publication.track() else {
            continue;
        };
        if track.kind() != TrackKind::Video {
            continue;
        }
        let Ok(stats) = track.get_stats().await else {
            continue;
        };
        for stat in stats {
            if let RtcStats::OutboundRtp(outbound) = stat {
                return outbound.sent.bytes_sent;
            }
        }
    }
    0
}
