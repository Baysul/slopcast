//! Windows desktop capture engine: libwebrtc's `DesktopCapturer` with the
//! WGC backend, driven through the livekit crate's bundled bindings
//! (`livekit::webrtc::desktop_capturer` — libwebrtc 0.3.44 over
//! webrtc-sys 0.3.41, already in the dependency tree; no forks, no patches).
//!
//! Why WGC via libwebrtc instead of the `windows` crate's `GraphicsCapture`
//! APIs directly: the shim (`webrtc-sys` `desktop_capturer.cpp`, `_WIN64`)
//! already wires the options — `allow_wgc_screen_capturer` /
//! `allow_wgc_window_capturer`, `allow_directx_capturer` as the pre-2004
//! fallback, `set_enumerate_current_process_windows(false)` so our own
//! windows never appear in the picker, and `prefer_cursor_embedded` so the
//! cursor is painted into the frames — and the prebuilt libwebrtc ships the
//! whole `modules/desktop_capture` module. The engine only has to poll.
//!
//! Capture semantics (verified against the webrtc-sdk fork's m144 line —
//! `webrtc-sdk/webrtc@m144_release`, tags `libwebrtc.m144.7559.xx` —
//! `wgc_capturer_win.cc` / `wgc_capture_session.cc`):
//! - `capture_frame()` is a *non-blocking poll*: the WGC session keeps the
//!   last `Direct3D11CaptureFramePool` frame in an internal queue and
//!   re-delivers it when the screen is static, so polling at the encoder
//!   target fps yields a steady stream even for static content (no
//!   keepalive hack needed).
//! - The callback fires synchronously on the polling thread; `DesktopFrame`
//!   must be consumed inside it (`data()` is `stride × height` bytes of
//!   BGRA with a lifetime tied to the frame).
//! - `ERROR_PERMANENT` means the source is gone (window closed, D3D device
//!   lost, no frame within ~200 ms at startup) — mapped to the existing
//!   `capture-ended` event once per session, mirroring the portal
//!   `session_closed` semantics.
//! - The capture thread must be COM-initialized (MTA) before the first
//!   `capture_frame()` — WGC is WinRT-based and the shim expects the thread
//!   to already own an apartment.
//!
//! The engine feeds the exact same conversion → preview → publish pipeline
//! as the Linux portal engine: packed BGRA frames into the shared
//! `FrameCallback` contract, so `convert_frame` (I420) + the paced
//! delivery loop (encoder-target scaling, `NativeVideoSource`) and the
//! JPEG preview emitter are reused unchanged.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use livekit::webrtc::desktop_capturer::{
    CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::HRESULT;

use crate::desktop_capture::{
    FrameCallback, STATS_DEQUEUED, STATS_DROPPED, STATS_ERRORS, capture_poll_fps,
    fire_capture_ended_once, monotonic_us,
};

/// The thread was already initialized in a different COM apartment; the WGC
/// session may still work, so it is tolerated (same policy as the WASAPI
/// capture thread in native-rust).
const RPC_E_CHANGED_MODE: i32 = -2_147_417_850;

/// What kind of source a WGC capture session targets. Serialized camelCase
/// for the renderer's source picker (`"screen"` / `"window"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WgcSourceKind {
    Screen,
    Window,
}

/// One capturable source as reported by libwebrtc's `GetSourceList`:
/// screens (monitors) and windows. `id` is the monitor/source id (Windows)
/// or HWND-derived id; `display_id` is the monitor's `DISPLAY_DEVICE`
/// id for screens (0 for windows).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSourceInfo {
    pub id: u64,
    pub title: String,
    pub display_id: i64,
    pub kind: WgcSourceKind,
}

/// RAII guard for the calling thread's COM apartment (MTA). WGC is
/// WinRT-based and libwebrtc's capturer requires the thread to own an
/// apartment before the first `capture_frame`. `CoUninitialize` runs only
/// when this guard actually initialized the apartment (`S_OK`/`S_FALSE`); an
/// already-initialized thread (`RPC_E_CHANGED_MODE`) is left untouched.
struct ComApartment {
    uninit: bool,
}

impl ComApartment {
    fn init() -> Result<Self, String> {
        // SAFETY: initializes the current thread's COM apartment (MTA) for
        // the lifetime of the returned guard, which balances it exactly once
        // in Drop when `uninit` is set.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_err() && hr != HRESULT(RPC_E_CHANGED_MODE) {
            return Err(format!("CoInitializeEx failed: {hr:?}"));
        }
        Ok(Self { uninit: hr.is_ok() })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninit {
            // SAFETY: `init` returned S_OK/S_FALSE, so this thread's
            // apartment was initialized by this guard and is released
            // exactly once here.
            unsafe { CoUninitialize() };
        }
    }
}

/// Maps the public source kind to libwebrtc's capturer type.
impl From<WgcSourceKind> for DesktopCaptureSourceType {
    fn from(kind: WgcSourceKind) -> Self {
        match kind {
            WgcSourceKind::Screen => DesktopCaptureSourceType::Screen,
            WgcSourceKind::Window => DesktopCaptureSourceType::Window,
        }
    }
}

/// Enumerates every screen (monitor) and window capturable through WGC on a
/// short-lived COM-initialized thread. The renderer's picker consumes the
/// result; the chosen `(kind, id)` is passed back into
/// [`WgcCapture::start`].
///
/// # Errors
///
/// Returns an error when the worker thread cannot be spawned, COM fails to
/// initialize, or no capturer of either kind can be created.
pub fn get_windows_capture_sources() -> Result<Vec<CaptureSourceInfo>, String> {
    let (tx, rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("wgc-enumerate".into())
        .spawn(move || {
            let com = match ComApartment::init() {
                Ok(com) => com,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            let result = (|| {
                let mut screens =
                    enumerate_kind(DesktopCaptureSourceType::Screen, WgcSourceKind::Screen)?;
                screens.extend(enumerate_kind(
                    DesktopCaptureSourceType::Window,
                    WgcSourceKind::Window,
                )?);
                Ok(screens)
            })();
            drop(com);
            let _ = tx.send(result);
        })
        .map_err(|e| format!("Thread spawn: {e}"))?;
    let result = rx
        .recv()
        .map_err(|_| "Enumerate channel closed".to_string())?;
    let _ = handle.join();
    result
}

fn enumerate_kind(
    kind: DesktopCaptureSourceType,
    wgc_kind: WgcSourceKind,
) -> Result<Vec<CaptureSourceInfo>, String> {
    let mut options = DesktopCapturerOptions::new(kind);
    options.set_include_cursor(true);
    let capturer =
        DesktopCapturer::new(options).ok_or_else(|| "Desktop capturer unavailable".to_string())?;
    Ok(capturer
        .get_source_list()
        .into_iter()
        .map(|source| CaptureSourceInfo {
            id: source.id(),
            title: source.title(),
            display_id: source.display_id(),
            kind: wgc_kind,
        })
        .collect())
}

/// The WGC capture engine: owns the libwebrtc capturer (created, started and
/// polled on the capture thread) and maps its callback into the shared
/// packed-BGRA `FrameCallback` contract. Created inside `run_capture`'s WGC
/// arm; `poll` is driven by the shared capture loop at the encoder target
/// fps (or the fallback preview cadence before a track is live).
pub(crate) struct WgcCapture {
    /// Declared before `_com` so it drops first (fields drop in declaration
    /// order): the libwebrtc capturer must be destroyed while the COM
    /// apartment is still alive.
    capturer: DesktopCapturer,
    /// Set on the first `ERROR_PERMANENT`; polling stops so the shared loop
    /// keeps running (preview stays idle) until the renderer calls `stop()`
    /// — the same lifecycle as a closed portal session.
    ended: Arc<AtomicBool>,
    /// Next `capture_frame` tick; paced so the pipeline never sees more
    /// polls than the encoder target.
    next_poll_at: Instant,
    _com: ComApartment,
}

impl WgcCapture {
    /// Creates the capturer, resolves `id` against the live source list (the
    /// chosen window may have closed between the picker and this call) and
    /// starts the capture session. Runs on the capture thread.
    ///
    /// # Errors
    ///
    /// Returns an error when COM cannot be initialized, no capturer can be
    /// created, or the chosen source no longer exists.
    pub(crate) fn start(
        kind: WgcSourceKind,
        id: u64,
        on_frame: FrameCallback,
    ) -> Result<Self, String> {
        let com = ComApartment::init()?;
        let mut options = DesktopCapturerOptions::new(kind.into());
        options.set_include_cursor(true);
        let mut capturer = DesktopCapturer::new(options)
            .ok_or_else(|| "Desktop capturer unavailable".to_string())?;
        let source = capturer
            .get_source_list()
            .into_iter()
            .find(|source| source.id() == id)
            .ok_or_else(|| "Capture source no longer exists".to_string())?;

        let ended = Arc::new(AtomicBool::new(false));
        let ended_cb = Arc::clone(&ended);
        // Scratch buffer for stride-padded frames: WGC rows can be padded to
        // a D3D11 row pitch, while the shared pipeline contract is tightly
        // packed (stride == width * 4).
        let mut packed: Vec<u8> = Vec::new();
        let mut on_frame = on_frame;
        capturer.start_capture(Some(source), move |result| match result {
            Ok(frame) => {
                STATS_DEQUEUED.fetch_add(1, Ordering::Relaxed);
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
                    &data[..row_bytes * frame_rows]
                } else {
                    packed.clear();
                    packed.resize(row_bytes * frame_rows, 0);
                    for (dst, src) in packed
                        .chunks_exact_mut(row_bytes)
                        .zip(data.chunks_exact(stride))
                    {
                        dst.copy_from_slice(&src[..row_bytes]);
                    }
                    &packed
                };
                on_frame(width, height, bgra, monotonic_us());
            }
            Err(CaptureError::Temporary) => {
                // Transient WGC drop (frame pool raced the poll); keep going.
                STATS_ERRORS.fetch_add(1, Ordering::Relaxed);
            }
            Err(CaptureError::Permanent) => {
                STATS_ERRORS.fetch_add(1, Ordering::Relaxed);
                if !ended_cb.swap(true, Ordering::Relaxed) {
                    log::info!("[desktop-capture] WGC source is gone — captured source closed");
                    fire_capture_ended_once();
                }
            }
        });
        Ok(Self {
            capturer,
            ended,
            next_poll_at: Instant::now(),
            _com: com,
        })
    }

    /// Paces and issues one `capture_frame` poll when due. The callback runs
    /// synchronously inside the call; `ERROR_PERMANENT` flips `ended` so
    /// later polls are no-ops until the session is stopped.
    pub(crate) fn poll(&mut self) {
        if self.ended.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        if now < self.next_poll_at {
            return;
        }
        let interval_ms = u64::from(1000 / capture_poll_fps().max(1));
        self.next_poll_at = now + Duration::from_millis(interval_ms);
        self.capturer.capture_frame();
    }
}
