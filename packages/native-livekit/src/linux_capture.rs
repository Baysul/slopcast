use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use glib::MainContext;
use livekit::webrtc::desktop_capturer::{
    CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions,
};

use crate::desktop_capture::{capture_poll_fps, fire_capture_ended_once};
use crate::frame_delivery::{CapturedFrame, FrameIngress, SourceIssue, monotonic_us};

pub(crate) struct LinuxDesktopCapture {
    capturer: DesktopCapturer,
    ended: Arc<AtomicBool>,
    next_poll_at: Instant,
    glib_ctx: MainContext,
}

impl LinuxDesktopCapture {
    pub(crate) fn start(ingress: FrameIngress) -> Result<Self, String> {
        // Portal callbacks use the capture worker's thread-default GLib context.
        let glib_ctx = MainContext::new();
        let ended = Arc::new(AtomicBool::new(false));
        let ended_cb = Arc::clone(&ended);
        let mut options = DesktopCapturerOptions::new(DesktopCaptureSourceType::Generic);
        options.set_include_cursor(true);
        let capturer = glib_ctx
            .with_thread_default(move || {
                let mut capturer = DesktopCapturer::new(options)
                    .ok_or_else(|| "Desktop capturer unavailable (PipeWire portal)".to_string())?;
                let mut packed: Vec<u8> = Vec::new();
                capturer.start_capture(None, move |result| match result {
                    Ok(frame) => {
                        let width = u32::try_from(frame.width()).unwrap_or(0);
                        let height = u32::try_from(frame.height()).unwrap_or(0);
                        if width == 0 || height == 0 {
                            ingress.record_issue(SourceIssue::DroppedFrame);
                            return;
                        }
                        let row_bytes = usize::try_from(width * 4).unwrap_or(0);
                        let frame_rows = usize::try_from(height).unwrap_or(0);
                        let stride = usize::try_from(frame.stride()).unwrap_or(0);
                        let data = frame.data();
                        let bgra = if stride == row_bytes {
                            let required = row_bytes.saturating_mul(frame_rows);
                            if data.len() < required {
                                ingress.record_issue(SourceIssue::DroppedFrame);
                                return;
                            }
                            &data[..required]
                        } else if stride >= row_bytes {
                            let required = stride.saturating_mul(frame_rows);
                            if data.len() < required {
                                ingress.record_issue(SourceIssue::DroppedFrame);
                                return;
                            }
                            packed.clear();
                            packed.resize(row_bytes * frame_rows, 0);
                            for (dst, src) in packed
                                .chunks_exact_mut(row_bytes)
                                .zip(data.chunks_exact(stride))
                            {
                                dst.copy_from_slice(&src[..row_bytes]);
                            }
                            &packed
                        } else {
                            ingress.record_issue(SourceIssue::DroppedFrame);
                            return;
                        };
                        let _ = ingress.submit(CapturedFrame {
                            width,
                            height,
                            bgra,
                            pts_us: monotonic_us(),
                        });
                    }
                    Err(CaptureError::Temporary) => {}
                    Err(CaptureError::Permanent) => {
                        ingress.record_issue(SourceIssue::CaptureError);
                        if !ended_cb.swap(true, Ordering::Relaxed) {
                            log::info!(
                                "[desktop-capture] portal session closed — captured source is gone"
                            );
                            fire_capture_ended_once();
                        }
                    }
                });
                Ok::<DesktopCapturer, String>(capturer)
            })
            .map_err(|e| format!("GLib thread-default context: {e}"))??;
        Ok(Self {
            capturer,
            ended,
            next_poll_at: Instant::now(),
            glib_ctx,
        })
    }

    pub(crate) fn poll(&mut self) {
        if self.ended.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        if now < self.next_poll_at {
            return;
        }
        let glib_ctx = &self.glib_ctx;
        let capturer = &mut self.capturer;
        let _ = glib_ctx.with_thread_default(|| {
            while glib_ctx.pending() {
                glib_ctx.iteration(false);
            }
            capturer.capture_frame();
        });
        let interval = Duration::from_micros(1_000_000 / u64::from(capture_poll_fps().max(1)));
        self.next_poll_at += interval;
        let after = Instant::now();
        if self.next_poll_at <= after {
            self.next_poll_at = after + interval;
        }
    }
}
