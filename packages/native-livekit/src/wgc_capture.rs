use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use livekit::webrtc::desktop_capturer::{
    CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::HRESULT;

use crate::desktop_capture::{capture_poll_fps, fire_capture_ended_once};
use crate::frame_delivery::{CapturedFrame, FrameIngress, SourceIssue, monotonic_us};

const RPC_E_CHANGED_MODE: i32 = -2_147_417_850;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WgcSourceKind {
    Screen,
    Window,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSourceInfo {
    pub id: u64,
    pub title: String,
    pub display_id: i64,
    pub kind: WgcSourceKind,
}

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

impl From<WgcSourceKind> for DesktopCaptureSourceType {
    fn from(kind: WgcSourceKind) -> Self {
        match kind {
            WgcSourceKind::Screen => DesktopCaptureSourceType::Screen,
            WgcSourceKind::Window => DesktopCaptureSourceType::Window,
        }
    }
}

pub(crate) fn get_windows_capture_sources() -> Result<Vec<CaptureSourceInfo>, String> {
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

pub(crate) struct WgcCapture {
    // `_com` must remain last so the capturer drops before COM uninitializes.
    capturer: DesktopCapturer,
    ended: Arc<AtomicBool>,
    next_poll_at: Instant,
    _com: ComApartment,
}

impl WgcCapture {
    pub(crate) fn start(
        kind: WgcSourceKind,
        id: u64,
        ingress: FrameIngress,
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
        let mut packed: Vec<u8> = Vec::new();
        capturer.start_capture(Some(source), move |result| match result {
            Ok(frame) => {
                let width = u32::try_from(frame.width()).unwrap_or(0);
                let height = u32::try_from(frame.height()).unwrap_or(0);
                if width == 0 || height == 0 {
                    ingress.record_issue(SourceIssue::DroppedFrame);
                    return;
                }
                let Some(row_bytes) = usize::try_from(width)
                    .ok()
                    .and_then(|frame_width| frame_width.checked_mul(4))
                else {
                    ingress.record_issue(SourceIssue::DroppedFrame);
                    return;
                };
                let frame_rows = usize::try_from(height).unwrap_or(0);
                let stride = usize::try_from(frame.stride()).unwrap_or(0);
                let Some(packed_len) = row_bytes.checked_mul(frame_rows) else {
                    ingress.record_issue(SourceIssue::DroppedFrame);
                    return;
                };
                let Some(source_len) = stride.checked_mul(frame_rows) else {
                    ingress.record_issue(SourceIssue::DroppedFrame);
                    return;
                };
                let data = frame.data();
                if stride < row_bytes || data.len() < source_len {
                    ingress.record_issue(SourceIssue::DroppedFrame);
                    return;
                }

                let bgra = if stride == row_bytes {
                    &data[..packed_len]
                } else {
                    packed.clear();
                    packed.resize(packed_len, 0);
                    for (dst, src) in packed
                        .chunks_exact_mut(row_bytes)
                        .zip(data[..source_len].chunks_exact(stride))
                    {
                        dst.copy_from_slice(&src[..row_bytes]);
                    }
                    &packed
                };
                let _ = ingress.submit(CapturedFrame {
                    width,
                    height,
                    bgra,
                    pts_us: monotonic_us(),
                });
            }
            Err(CaptureError::Temporary) => {
                ingress.record_issue(SourceIssue::CaptureError);
            }
            Err(CaptureError::Permanent) => {
                ingress.record_issue(SourceIssue::CaptureError);
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
