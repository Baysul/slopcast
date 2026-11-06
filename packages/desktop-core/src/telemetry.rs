//! Live stream telemetry: per-second deltas and smoothing of native sender statistics.

use std::time::Instant;

use native_livekit::{DesktopCaptureStats, NativeTelemetry};

use crate::codecs::codec_label;

const HISTORY_MAX: usize = 48;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StreamTelemetry {
    pub live: bool,
    pub video_codec: Option<String>,
    pub video_encoder: Option<String>,
    pub audio_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<f64>,
    pub capture_fps: Option<f64>,
    pub target_frame_rate: Option<f64>,
    pub video_bitrate: Option<f64>,
    pub audio_bitrate: Option<f64>,
    pub packet_loss_pct: Option<f64>,
    pub rtt_ms: Option<f64>,
    pub spectator_count: u32,
    pub bitrate_history: Vec<f64>,
    pub elapsed_ms: u64,
    pub has_audio: bool,
    pub cursor_frames: i64,
    pub cursor_missing: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct TickInputs {
    pub width: u32,
    pub height: u32,
    pub target_frame_rate: f64,
    pub has_audio: bool,
    pub spectator_count: u32,
}

#[derive(Default)]
struct Previous {
    video: Option<(f64, f64, f64)>,
    audio: Option<(f64, f64)>,
    capture: Option<(Instant, i64)>,
}

fn push_smooth(buffer: &mut Vec<f64>, value: f64) -> f64 {
    buffer.push(value);
    if buffer.len() > 3 {
        buffer.remove(0);
    }
    #[allow(clippy::cast_precision_loss, reason = "at most three samples")]
    let len = buffer.len() as f64;
    buffer.iter().sum::<f64>() / len
}

/// Folds successive native samples into the displayed telemetry.
pub struct TelemetryTracker {
    started: Instant,
    previous: Previous,
    fps: Vec<f64>,
    bitrate: Vec<f64>,
    current: StreamTelemetry,
    ticks: u64,
}

impl TelemetryTracker {
    #[must_use]
    pub fn start(inputs: TickInputs) -> Self {
        Self::start_at(inputs, Instant::now())
    }

    /// Starts tracking from an explicit instant, so elapsed time is reproducible in tests.
    #[must_use]
    pub fn start_at(inputs: TickInputs, started: Instant) -> Self {
        Self {
            started,
            previous: Previous::default(),
            fps: Vec::new(),
            bitrate: Vec::new(),
            current: StreamTelemetry {
                live: true,
                width: Some(inputs.width),
                height: Some(inputs.height),
                target_frame_rate: Some(inputs.target_frame_rate),
                has_audio: inputs.has_audio,
                spectator_count: inputs.spectator_count,
                ..StreamTelemetry::default()
            },
            ticks: 0,
        }
    }

    /// Forgets sample baselines after an encoder reconfiguration.
    pub fn reset_baselines(&mut self) {
        self.previous = Previous::default();
    }

    #[must_use]
    pub fn current(&self) -> &StreamTelemetry {
        &self.current
    }

    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "telemetry counters and durations stay well inside f64/u64 precision"
    )]
    pub fn tick(
        &mut self,
        sample: &NativeTelemetry,
        capture: &DesktopCaptureStats,
        inputs: TickInputs,
        now: Instant,
    ) -> &StreamTelemetry {
        self.ticks += 1;
        let elapsed_ms = now.duration_since(self.started).as_millis() as u64;
        let c = &mut self.current;
        c.live = true;
        c.elapsed_ms = elapsed_ms;
        c.has_audio = inputs.has_audio;
        c.spectator_count = inputs.spectator_count;
        c.target_frame_rate = Some(inputs.target_frame_rate);

        if sample.video_bytes_sent.is_none() && sample.audio_bytes_sent.is_none() {
            c.audio_bitrate = None;
            c.capture_fps = None;
            c.cursor_frames = 0;
            c.cursor_missing = false;
            c.width = Some(inputs.width);
            c.height = Some(inputs.height);
            c.packet_loss_pct = None;
            c.video_bitrate = None;
            c.frame_rate = None;
            return &self.current;
        }

        let mut video_bps = None;
        let mut fps = None;
        if let (Some(ts), Some(bytes)) = (sample.timestamp_ms, sample.video_bytes_sent) {
            if let Some((prev_bytes, prev_frames, prev_ts)) = self.previous.video
                && ts > prev_ts
            {
                let dt = (ts - prev_ts) / 1000.0;
                let db = bytes - prev_bytes;
                if db > 0.0 {
                    video_bps = Some(db * 8.0 / dt);
                }
                if let Some(frames) = sample.video_frames_encoded {
                    let df = frames - prev_frames;
                    if df > 0.0 {
                        fps = Some(df / dt);
                    }
                }
            }
            let frames = sample
                .video_frames_encoded
                .or(self.previous.video.map(|v| v.1))
                .unwrap_or(0.0);
            self.previous.video = Some((bytes, frames, ts));
        }
        let mut audio_bps = None;
        if let (Some(ts), Some(bytes)) = (sample.timestamp_ms, sample.audio_bytes_sent) {
            if let Some((prev_bytes, prev_ts)) = self.previous.audio
                && ts > prev_ts
            {
                let db = bytes - prev_bytes;
                if db > 0.0 {
                    audio_bps = Some(db * 8.0 / ((ts - prev_ts) / 1000.0));
                }
            }
            self.previous.audio = Some((bytes, ts));
        }
        let mut capture_fps = None;
        if let Some((at, dequeued)) = self.previous.capture {
            let dt = now.duration_since(at).as_secs_f64();
            let delta = capture.frames_dequeued - dequeued;
            if delta >= 0 && dt > 0.0 {
                capture_fps = Some(delta as f64 / dt);
            }
        }
        self.previous.capture = Some((now, capture.frames_dequeued));

        if let Some(fps) = fps {
            c.frame_rate = Some(push_smooth(&mut self.fps, fps));
        }
        if let Some(bps) = video_bps {
            let mbps = bps / 1_000_000.0;
            c.video_bitrate = Some(push_smooth(&mut self.bitrate, mbps) * 1_000_000.0);
            c.bitrate_history.push(mbps);
            if c.bitrate_history.len() > HISTORY_MAX {
                let excess = c.bitrate_history.len() - HISTORY_MAX;
                c.bitrate_history.drain(..excess);
            }
        }
        let sent =
            sample.video_packets_sent.unwrap_or(0.0) + sample.audio_packets_sent.unwrap_or(0.0);
        let lost =
            sample.video_packets_lost.unwrap_or(0.0) + sample.audio_packets_lost.unwrap_or(0.0);
        c.packet_loss_pct = Some(if sent > 0.0 {
            lost / (sent + lost) * 100.0
        } else {
            0.0
        });
        if audio_bps.is_some() {
            c.audio_bitrate = audio_bps;
        }
        if let Some(mime) = sample.audio_codec.as_deref().filter(|m| !m.is_empty()) {
            c.audio_codec = Some(codec_label(mime));
        }
        if let Some(mime) = sample.video_codec.as_deref().filter(|m| !m.is_empty()) {
            c.video_codec = Some(codec_label(mime));
        }
        if sample.encoder_implementation.is_some() {
            c.video_encoder.clone_from(&sample.encoder_implementation);
        }
        if capture_fps.is_some() {
            c.capture_fps = capture_fps;
        }
        c.cursor_frames = capture.cursor_frames;
        c.cursor_missing = capture.cursor_missing;
        c.rtt_ms = sample.rtt_ms.or(c.rtt_ms);
        c.width = sample.video_width.or(Some(inputs.width));
        c.height = sample.video_height.or(Some(inputs.height));

        if self.ticks.is_multiple_of(5) {
            log::info!(
                "[Telemetry] {} {}x{} {:.0}/{:.0}fps cap {:.0}fps {:.1}Mbps loss {:.2}% rtt {:.0}ms · {} spectator(s)",
                c.video_codec.as_deref().unwrap_or("–"),
                c.width.unwrap_or(0),
                c.height.unwrap_or(0),
                c.frame_rate.unwrap_or(0.0),
                inputs.target_frame_rate,
                c.capture_fps.unwrap_or(0.0),
                c.video_bitrate.unwrap_or(0.0) / 1e6,
                c.packet_loss_pct.unwrap_or(0.0),
                c.rtt_ms.unwrap_or(0.0),
                c.spectator_count,
            );
        }
        &self.current
    }
}

/// `MM:SS` for the live duration.
#[must_use]
pub fn format_duration(ms: u64) -> String {
    let total = ms / 1000;
    format!("{:02}:{:02}", total / 60, total % 60)
}

#[must_use]
pub fn format_bitrate(bps: Option<f64>) -> String {
    match bps {
        Some(bps) if bps >= 1_000_000.0 => format!("{:.1} Mbps", bps / 1_000_000.0),
        Some(bps) => format!("{:.0} kbps", bps / 1000.0),
        None => "–".into(),
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "derived from exact sample deltas")]
mod tests {
    use super::*;
    use std::time::Duration;

    fn inputs() -> TickInputs {
        TickInputs {
            width: 1920,
            height: 1080,
            target_frame_rate: 60.0,
            has_audio: true,
            spectator_count: 2,
        }
    }

    fn sample(ts: f64, bytes: f64, frames: f64) -> NativeTelemetry {
        NativeTelemetry {
            video_codec: Some("video/VP8".into()),
            video_bytes_sent: Some(bytes),
            video_frames_encoded: Some(frames),
            video_packets_sent: Some(99.0),
            video_packets_lost: Some(1.0),
            timestamp_ms: Some(ts),
            ..NativeTelemetry::default()
        }
    }

    #[test]
    fn derives_rates_from_successive_samples() {
        let start = Instant::now();
        let mut tracker = TelemetryTracker::start_at(inputs(), start);
        let stats = DesktopCaptureStats::default();
        tracker.tick(&sample(1000.0, 0.0, 0.0), &stats, inputs(), start);
        let t = tracker.tick(
            &sample(2000.0, 1_000_000.0, 60.0),
            &stats,
            inputs(),
            start + Duration::from_secs(1),
        );
        assert_eq!(t.video_bitrate, Some(8_000_000.0));
        assert_eq!(t.frame_rate, Some(60.0));
        assert_eq!(t.video_codec.as_deref(), Some("VP8"));
        assert_eq!(t.packet_loss_pct, Some(1.0));
        assert_eq!(t.bitrate_history, vec![8.0]);
        assert_eq!(t.elapsed_ms, 1000);
    }

    #[test]
    fn missing_sender_keeps_targets() {
        let mut tracker = TelemetryTracker::start(inputs());
        let t = tracker.tick(
            &NativeTelemetry::default(),
            &DesktopCaptureStats::default(),
            inputs(),
            Instant::now(),
        );
        assert!(t.live);
        assert_eq!(t.width, Some(1920));
        assert_eq!(t.video_bitrate, None);
    }

    #[test]
    fn formats() {
        assert_eq!(format_duration(61_500), "01:01");
        assert_eq!(format_bitrate(Some(2_500_000.0)), "2.5 Mbps");
        assert_eq!(format_bitrate(Some(64_000.0)), "64 kbps");
        assert_eq!(format_bitrate(None), "–");
    }
}
