//! Temporary diagnostic probe (2026-08-06): publish a synthetic video track
//! through the same livekit SDK path as the desktop app — but drive the
//! source DIRECTLY at 60 fps (no app pacer, no portal) — and sample
//! outbound-rtp `framesEncoded` twice to measure the encoder's real
//! sustained framerate. 30 fps here => the cap lives in the
//! SDK/C++/SFU-negotiation path; 60 fps => the app's delivery is the cap.
//! Run: livekit-server --dev on :7880, then
//! `cargo run -p pw-conflict-probe --bin sdp_probe`.
//! 2026-08-08: PROBE_DURATION env (s), and the test pattern is a moving
//! white box on gray (was solid gray) so a spectator can detect
//! frame alternation (old/new jumping) vs even motion.

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
    let width: u32 = std::env::var("PROBE_WIDTH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1280);
    let height: u32 = std::env::var("PROBE_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(720);
    let fps: u32 = std::env::var("PROBE_FPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let bitrate: u64 = std::env::var("PROBE_BITRATE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(80_000_000);
    let duration: u64 = std::env::var("PROBE_DURATION")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);

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
        // Moving white box (1/6 width, 1/6 height) travelling left->right,
        // wrapping, so the content differs every frame and a spectator can
        // detect frame alternation (box jumping back) vs even motion.
        let box_w = (width / 6).max(1);
        let box_h = (height / 6).max(1);
        let travel = width - box_w;
        loop {
            let t = started.elapsed();
            if t >= Duration::from_secs(duration) {
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
                // White box (Y=235, U=V=128) at x0; U/V stay neutral so the
                // box is visible as luminance only.
                let x0 = ((u64::from(travel) * (pushed % 128)) / 128) as usize;
                let y0 = (height / 2 - box_h / 2) as usize;
                let stride_y = width as usize;
                let row_end = (y0 + box_h as usize).min(height as usize);
                for row in y0..row_end {
                    let x_end = (x0 + box_w as usize).min(width as usize);
                    y[row * stride_y + x0..row * stride_y + x_end].fill(235);
                }
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
            "[probe] framesEncoded at 1s: {first}; at {duration}s: {second}; sustained = {:.1} fps over {}s; input pushed = {pushed}",
            (second - first) as f64 / ((duration - 1).max(1)) as f64,
            duration - 1
        );
        println!(
            "[probe] live codec: {}",
            report_live_codec(&room).await.unwrap_or_else(|| "?".into())
        );
        println!("[probe] videoBytes at {duration}s: {}", sample_bytes(&room).await);

        room.close().await.ok();
    });
}

/// Reports the negotiated video codec mime from the local track's stats.
async fn report_live_codec(room: &Room) -> Option<String> {
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
        for stat in &stats {
            if let RtcStats::OutboundRtp(outbound) = stat {
                let codec_id = &outbound.stream.codec_id;
                for stat2 in stats.iter().filter(|s| matches!(s, RtcStats::Codec(_))) {
                    if let RtcStats::Codec(codec) = stat2
                        && codec.rtc.id == *codec_id
                    {
                        return Some(codec.codec.mime_type.clone());
                    }
                }
            }
        }
    }
    None
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
