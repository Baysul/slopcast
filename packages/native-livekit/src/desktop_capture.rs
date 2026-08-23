use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;

use crate::CaptureConfig;
use crate::frame_delivery::{
    CapturedFrame, DeliveryBinding, DeliveryTarget, FrameDelivery, FrameIngress, PreviewOutput,
    SourceIssue, monotonic_us,
};

pub(crate) type CaptureEndedCallback = Box<dyn Fn() + Send + Sync>;

struct CaptureSession {
    stop: Arc<AtomicBool>,
    source_join: Option<thread::JoinHandle<()>>,
    delivery: FrameDelivery,
}

struct CaptureCoordinator {
    session: Option<CaptureSession>,
    binding: DeliveryBinding,
    preview_output: Option<PreviewOutput>,
    viewport: Option<(u32, u32)>,
    frozen_stats: crate::DesktopCaptureStats,
}

impl Default for CaptureCoordinator {
    fn default() -> Self {
        Self {
            session: None,
            binding: DeliveryBinding::dormant(),
            preview_output: None,
            viewport: None,
            frozen_stats: crate::DesktopCaptureStats::default(),
        }
    }
}

static CAPTURE: LazyLock<Mutex<CaptureCoordinator>> =
    LazyLock::new(|| Mutex::new(CaptureCoordinator::default()));
static CAPTURE_ENDED_CALLBACK: ArcSwapOption<CaptureEndedCallback> = ArcSwapOption::const_empty();
static CAPTURE_ENDED_EMITTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_preview_callback(callback: Box<dyn Fn(Vec<u8>, i64) + Send + Sync>) {
    let output: PreviewOutput = Arc::from(callback);
    if let Ok(mut coordinator) = CAPTURE.lock() {
        coordinator.preview_output = Some(Arc::clone(&output));
        if let Some(session) = coordinator.session.as_ref() {
            session.delivery.set_preview_output(Some(output));
        }
    }
}

pub(crate) fn clear_preview_callback() {
    if let Ok(mut coordinator) = CAPTURE.lock() {
        coordinator.preview_output = None;
        if let Some(session) = coordinator.session.as_ref() {
            session.delivery.set_preview_output(None);
        }
    }
}

pub(crate) fn set_capture_ended_callback(callback: CaptureEndedCallback) {
    CAPTURE_ENDED_CALLBACK.store(Some(Arc::new(callback)));
}

pub(crate) fn fire_capture_ended_once() {
    if !CAPTURE_ENDED_EMITTED.swap(true, Ordering::Relaxed)
        && let Some(callback) = CAPTURE_ENDED_CALLBACK.load_full()
    {
        callback();
    }
}

pub(crate) fn create_live_binding(
    width: u32,
    height: u32,
    fps: u32,
) -> Result<DeliveryBinding, String> {
    DeliveryBinding::live(DeliveryTarget { width, height, fps })
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn set_scale_target(width: u32, height: u32, fps: u32) -> Result<(), String> {
    let binding = create_live_binding(width, height, fps)?;

    set_delivery_binding(binding);
    Ok(())
}

pub(crate) fn clear_scale_target() {
    set_delivery_binding(DeliveryBinding::dormant());
}

pub(crate) fn set_delivery_binding(binding: DeliveryBinding) {
    if let Ok(mut coordinator) = CAPTURE.lock() {
        coordinator.binding = binding.clone();
        if let Some(session) = coordinator.session.as_ref() {
            session.delivery.set_binding(binding);
        }
    }
}

pub(crate) fn capture_poll_fps() -> u32 {
    CAPTURE
        .lock()
        .ok()
        .and_then(|coordinator| coordinator.binding.target())
        .map_or(30, |target| target.fps.clamp(1, 60))
}

pub(crate) fn set_preview_viewport(width: u32, height: u32) {
    if width == 0 || height == 0 {
        clear_preview_viewport();
        return;
    }
    if let Ok(mut coordinator) = CAPTURE.lock() {
        coordinator.viewport = Some((width, height));
        if let Some(session) = coordinator.session.as_ref() {
            session.delivery.set_viewport(coordinator.viewport);
        }
    }
}

pub(crate) fn clear_preview_viewport() {
    if let Ok(mut coordinator) = CAPTURE.lock() {
        coordinator.viewport = None;
        if let Some(session) = coordinator.session.as_ref() {
            session.delivery.set_viewport(None);
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum CaptureSource {
    #[cfg(target_os = "linux")]
    Desktop,
    #[cfg(target_os = "windows")]
    Wgc {
        kind: crate::WgcSourceKind,
        id: u64,
    },
    Synthetic {
        width: u32,
        height: u32,
        fps: u32,
    },
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "the non-Linux stub mirrors the fallible Linux capture interface"
)]
pub(crate) fn start() -> Result<bool, String> {
    #[cfg(target_os = "linux")]
    {
        start_with(CaptureSource::Desktop)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(false)
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn start_windows(kind: crate::WgcSourceKind, id: u64) -> Result<bool, String> {
    start_with(CaptureSource::Wgc { kind, id })
}

pub(crate) fn start_synthetic_capture(config: &CaptureConfig) -> Result<bool, String> {
    start_with(CaptureSource::Synthetic {
        width: config.width,
        height: config.height,
        fps: config.fps,
    })
}

fn start_with(source: CaptureSource) -> Result<bool, String> {
    let mut coordinator = CAPTURE
        .lock()
        .map_err(|error| format!("Desktop capture state lock poisoned: {error}"))?;
    if coordinator.session.is_some() {
        return Err("Desktop capture already active".into());
    }

    CAPTURE_ENDED_EMITTED.store(false, Ordering::Relaxed);
    let (delivery, ingress) = FrameDelivery::start(
        coordinator.binding.clone(),
        coordinator.preview_output.clone(),
        coordinator.viewport,
    )
    .map_err(|error| error.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let source_stop = Arc::clone(&stop);
    let (ready_sender, ready_receiver) = mpsc::channel();
    let source_join = match thread::Builder::new()
        .name("desktop-capture".into())
        .spawn(move || run_source(&source_stop, &ready_sender, source, ingress))
    {
        Ok(join) => join,
        Err(error) => {
            let _ = delivery.stop();
            return Err(format!("Thread spawn: {error}"));
        }
    };

    match ready_receiver.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {
            coordinator.frozen_stats = crate::DesktopCaptureStats::default();
            coordinator.session = Some(CaptureSession {
                stop,
                source_join: Some(source_join),
                delivery,
            });
            Ok(true)
        }
        Ok(Err(error)) => {
            stop.store(true, Ordering::Relaxed);
            drop(coordinator);
            crate::reap_detached(source_join, "desktop-capture-reaper");
            let _ = delivery.stop();
            Err(error)
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            drop(coordinator);
            crate::reap_detached(source_join, "desktop-capture-reaper");
            let _ = delivery.stop();
            Err("Timed out starting desktop capture".into())
        }
    }
}

fn run_source(
    stop: &Arc<AtomicBool>,
    ready_sender: &mpsc::Sender<Result<(), String>>,
    source: CaptureSource,
    ingress: FrameIngress,
) {
    match source {
        #[cfg(target_os = "linux")]
        CaptureSource::Desktop => match crate::linux_capture::LinuxDesktopCapture::start(ingress) {
            Ok(mut capture) => {
                let _ = ready_sender.send(Ok(()));
                while !stop.load(Ordering::Relaxed) {
                    capture.poll();
                    thread::sleep(Duration::from_millis(1));
                }
            }
            Err(error) => {
                let _ = ready_sender.send(Err(error));
            }
        },
        #[cfg(target_os = "windows")]
        CaptureSource::Wgc { kind, id } => {
            match crate::wgc_capture::WgcCapture::start(kind, id, ingress) {
                Ok(mut capture) => {
                    let _ = ready_sender.send(Ok(()));
                    while !stop.load(Ordering::Relaxed) {
                        capture.poll();
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                Err(error) => {
                    let _ = ready_sender.send(Err(error));
                }
            }
        }
        CaptureSource::Synthetic { width, height, fps } => {
            let _ = ready_sender.send(Ok(()));
            run_synthetic(stop, &ingress, width, height, fps);
        }
    }
}

fn run_synthetic(stop: &AtomicBool, ingress: &FrameIngress, width: u32, height: u32, fps: u32) {
    let Some(pixel_count) = width
        .checked_mul(height)
        .and_then(|count| count.checked_mul(4))
    else {
        ingress.record_issue(SourceIssue::CaptureError);
        return;
    };
    let mut pixels = vec![0_u8; pixel_count as usize];
    let interval = Duration::from_micros(1_000_000 / u64::from(fps.max(1)));
    let mut frame_index = 0;
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        synthetic_frame(width, height, frame_index, &mut pixels);
        let _ = ingress.submit(CapturedFrame {
            width,
            height,
            bgra: &pixels,
            pts_us: monotonic_us(),
        });
        frame_index += 1;
        thread::sleep(interval.saturating_sub(started.elapsed()));
    }
}

fn synthetic_frame(width: u32, height: u32, frame_index: u64, bgra: &mut [u8]) {
    const BARS: [[u8; 3]; 8] = [
        [255, 255, 255],
        [255, 255, 0],
        [0, 255, 255],
        [0, 255, 0],
        [255, 0, 255],
        [255, 0, 0],
        [0, 0, 255],
        [0, 0, 0],
    ];
    let width = width as usize;
    let height = height as usize;
    let bar_width = width.div_ceil(BARS.len());
    for row in bgra.chunks_exact_mut(width * 4).take(height) {
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let [red, green, blue] = BARS[(x / bar_width).min(BARS.len() - 1)];
            *pixel = [blue, green, red, 255];
        }
    }
    let box_width = (width / 8).max(1);
    let box_height = (height / 8).max(1);
    let travel = width - box_width;
    let left = u64::try_from(travel)
        .unwrap_or(0)
        .saturating_mul(frame_index % 128)
        / 128;
    let left = left as usize;
    let top = height / 8;
    for row_index in top..(top + box_height).min(height) {
        let row =
            &mut bgra[(row_index * width + left) * 4..(row_index * width + left + box_width) * 4];
        for pixel in row.as_chunks_mut::<4>().0 {
            *pixel = [255; 4];
        }
    }
}

pub(crate) fn stop() -> bool {
    let session = CAPTURE
        .lock()
        .ok()
        .and_then(|mut coordinator| coordinator.session.take());
    let Some(mut session) = session else {
        return true;
    };
    session.stop.store(true, Ordering::Relaxed);
    if let Some(join) = session.source_join.take() {
        let _ = join.join();
    }
    let stats = match session.delivery.stop() {
        Ok(stats) => stats,
        Err(error) => {
            log::error!("[desktop-capture] {error}");
            return false;
        }
    };
    if let Ok(mut coordinator) = CAPTURE.lock() {
        coordinator.frozen_stats = stats;
    }
    true
}

pub(crate) fn is_active() -> bool {
    CAPTURE
        .lock()
        .is_ok_and(|coordinator| coordinator.session.is_some())
}

pub(crate) fn stats() -> crate::DesktopCaptureStats {
    let Ok(coordinator) = CAPTURE.lock() else {
        return crate::DesktopCaptureStats::default();
    };
    coordinator
        .session
        .as_ref()
        .map_or(coordinator.frozen_stats, |session| session.delivery.stats())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_pattern_is_deterministic_and_moving() {
        let mut first = vec![0_u8; 128 * 64 * 4];
        let mut same = vec![0_u8; first.len()];
        let mut later = vec![0_u8; first.len()];
        synthetic_frame(128, 64, 0, &mut first);
        synthetic_frame(128, 64, 0, &mut same);
        synthetic_frame(128, 64, 10, &mut later);
        assert_eq!(first, same);
        assert_ne!(first, later);
        assert!(first.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255));
    }
}
