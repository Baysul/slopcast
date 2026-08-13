//! Linux desktop capture engine: libwebrtc's `DesktopCapturer` with the
//! `PipeWire` backend (XDG Desktop Portal `ScreenCast`), driven through the
//! livekit crate's bundled bindings — the same `desktop_capturer` module the
//! Windows arm uses for WGC, no forks, no patches. It replaces the former
//! in-house portal + `pw_stream` + EGL engine (see SCREEN-CAPTURE-INHOUSE.md):
//! the m144 prebuilt ships the whole `PipeWire` capturer including the three
//! `KWin` corrupt-buffer checks and cursor handling, so ~3,000 lines of
//! hand-rolled zbus/EGL code and portal-protocol maintenance move upstream.
//!
//! Capture semantics (verified against `webrtc-sdk/webrtc@m144_release`):
//! - `DesktopCaptureSourceType::Generic` → `kAnyScreenContent` (Screen |
//!   Window bits) — the KDE picker shows monitors *and* windows.
//! - `get_source_list()` returns only a placeholder on Wayland; the real
//!   selection happens in the portal picker opened by `start_capture(None)`.
//! - `capture_frame()` is a non-blocking poll; the callback fires
//!   synchronously on the polling thread. Once the first `PipeWire` buffer
//!   arrived, the capturer re-shares the last frame on every poll (static
//!   content keeps flowing, like WGC); before the first frame it returns
//!   `ERROR_TEMPORARY`.
//! - `ERROR_PERMANENT` = portal session closed (captured window gone) or
//!   portal failure — mapped once to the `capture-ended` event, mirroring
//!   the old `Session::Closed` semantics.
//! - The portal handshake is async `GDBus`; callbacks dispatch only when the
//!   thread-default `GMainContext` is iterated. `start` creates the capturer
//!   under `MainContext::with_thread_default` (so the proxies bind to *our*
//!   context, never the process-global one the `GTK` main thread runs), and
//!   `poll` drains that context — with the context kept pushed as the
//!   thread-default for every iteration, because `GDBus` method-call replies
//!   and signal subscriptions dispatch on the *calling* thread's
//!   thread-default context (`GTask` captures it at call time), not on the
//!   proxy's context. Without the push, the whole post-creation handshake
//!   (`CreateSession` / `SelectSources` / `Start` / `OpenPipeWireRemote`)
//!   lands on the `GTK` main thread, and the last reply handler runs the
//!   blocking `PipeWire` setup on the `UI` thread — the app freezes when a
//!   source is picked. With it, the whole portal flow stays on the capture
//!   thread and can never race `CaptureFrame`.
//!
//! Feeds the shared packed-BGRA `FrameCallback` contract (see
//! `desktop_capture`), reusing `convert_frame` and the paced delivery loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use glib::MainContext;
use livekit::webrtc::desktop_capturer::{
    CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions,
};

use crate::desktop_capture::{
    FrameCallback, STATS_DROPPED, STATS_ERRORS, capture_poll_fps, fire_capture_ended_once,
    monotonic_us,
};

/// The `PipeWire` desktop capture engine: owns the libwebrtc capturer (created,
/// started and polled on the capture thread) and maps its callback into the
/// shared packed-BGRA `FrameCallback` contract. Created inside `run_capture`'s
/// Linux arm; `poll` is driven by the shared capture loop at the encoder
/// target fps (or the fallback preview cadence before a track is live).
pub(crate) struct LinuxDesktopCapture {
    capturer: DesktopCapturer,
    /// Set on the first `ERROR_PERMANENT`; polling stops until `stop()`.
    ended: Arc<AtomicBool>,
    /// Next `capture_frame` tick; paced so the pipeline never sees more
    /// polls than the encoder target.
    next_poll_at: Instant,
    /// The thread-default context the portal's `GDBus` handshake dispatches
    /// on; drained in `poll` so callbacks run on this thread.
    glib_ctx: MainContext,
}

impl LinuxDesktopCapture {
    /// Creates the capturer (Generic → portal picker with monitors *and*
    /// windows), starts the capture session and returns. The portal picker
    /// opens inside `start_capture` — the session is reported running before
    /// the user answers, matching the old engine's UX. Runs on the capture
    /// thread.
    ///
    /// # Errors
    ///
    /// Returns an error when the `GLib` thread-default context cannot be set
    /// or no `PipeWire` capturer can be created (e.g. X11, where
    /// `IsSupported` = Wayland && `InitializePipeWire` fails).
    pub(crate) fn start(on_frame: FrameCallback) -> Result<Self, String> {
        let glib_ctx = MainContext::new();
        let ended = Arc::new(AtomicBool::new(false));
        let ended_cb = Arc::clone(&ended);
        let mut options = DesktopCapturerOptions::new(DesktopCaptureSourceType::Generic);
        options.set_include_cursor(true);
        let capturer = glib_ctx
            .with_thread_default(move || {
                let mut capturer = DesktopCapturer::new(options)
                    .ok_or_else(|| "Desktop capturer unavailable (PipeWire portal)".to_string())?;
                // Scratch buffer to re-pack stride-padded rows into the packed-BGRA contract.
                let mut packed: Vec<u8> = Vec::new();
                let mut on_frame = on_frame;
                capturer.start_capture(None, move |result| match result {
                    Ok(frame) => {
                        let width = u32::try_from(frame.width()).unwrap_or(0);
                        let height = u32::try_from(frame.height()).unwrap_or(0);
                        if width == 0 || height == 0 {
                            STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
                            return;
                        }
                        let row_bytes = usize::try_from(width * 4).unwrap_or(0);
                        let frame_rows = usize::try_from(height).unwrap_or(0);
                        let stride = usize::try_from(frame.stride()).unwrap_or(0);
                        let data = frame.data();
                        let bgra = if stride == row_bytes {
                            // Bound the checked slice: a corrupt/short FFI
                            // buffer must surface as a dropped capture frame,
                            // never as a panic inside the PipeWire process
                            // callback (which cannot unwind).
                            let required = row_bytes.saturating_mul(frame_rows);
                            if data.len() < required {
                                STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
                                return;
                            }
                            &data[..required]
                        } else if stride >= row_bytes {
                            // Each source row must hold a full destination
                            // row, and there must be one full row per frame
                            // row, else the copy below would read out of
                            // bounds.
                            let required = stride.saturating_mul(frame_rows);
                            if data.len() < required {
                                STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
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
                            // stride < row_bytes cannot be packed safely.
                            STATS_DROPPED.fetch_add(1, Ordering::Relaxed);
                            return;
                        };
                        on_frame(width, height, bgra, monotonic_us());
                    }
                    Err(CaptureError::Temporary) => {
                        // No new buffer since the last poll — expected while
                        // the portal picker is open and when the compositor
                        // has nothing new to record. NOT an error: counting
                        // it in `capture_errors` made every normal session
                        // start (picker open, pre-first-frame) read as
                        // faulted in the telemetry and e2e diagnostics.
                    }
                    Err(CaptureError::Permanent) => {
                        STATS_ERRORS.fetch_add(1, Ordering::Relaxed);
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

    /// Drains pending portal callbacks, then paces and issues one
    /// `capture_frame` poll when due. The callback runs synchronously inside
    /// the call; `ERROR_PERMANENT` flips `ended` so later polls are no-ops
    /// until the session is stopped.
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
        // Dispatch pending portal GDBus callbacks before polling — the
        // capturer's portal state machine advances only while its context is
        // iterated. The context must stay pushed as thread-default while
        // callbacks run (GDBus replies dispatch on the caller's
        // thread-default context — see the module doc for the UI-thread
        // freeze this prevents).
        let _ = glib_ctx.with_thread_default(|| {
            while glib_ctx.pending() {
                glib_ctx.iteration(false);
            }
            capturer.capture_frame();
        });
        let interval_ms = u64::from(1000 / capture_poll_fps().max(1));
        self.next_poll_at = now + Duration::from_millis(interval_ms);
    }
}
