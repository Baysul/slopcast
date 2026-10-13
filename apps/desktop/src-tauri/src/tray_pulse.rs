use crate::AppHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;
use tauri::image::Image;

pub(crate) struct TrayPulse {
    is_active: Arc<AtomicBool>,
    _stop: mpsc::Sender<()>,
}

impl Drop for TrayPulse {
    fn drop(&mut self) {
        // Invalidate any frame already queued on the UI thread before restoring the idle icon.
        self.is_active.store(false, Ordering::Release);
    }
}

impl TrayPulse {
    pub(crate) fn start(app: &AppHandle) -> Result<Self, String> {
        let icon = app
            .default_window_icon()
            .ok_or("Application icon unavailable")?;
        let frames = [180, 190, 210, 235, 255, 235, 210, 190]
            .into_iter()
            .map(|brightness| recording_frame(icon, brightness))
            .collect::<Result<Vec<_>, _>>()?;
        let tray = app.tray_by_id("slopcast").ok_or("Tray icon unavailable")?;
        let (stop, receiver) = mpsc::channel();
        let is_active = Arc::new(AtomicBool::new(true));
        let worker_active = Arc::clone(&is_active);
        let app = app.clone();

        tray.set_icon(Some(frames[0].clone()))
            .map_err(|error| error.to_string())?;
        tray.set_tooltip(Some("Slopcast — Live"))
            .map_err(|error| error.to_string())?;
        std::thread::Builder::new()
            .name("tray-recording-pulse".into())
            .spawn(move || {
                let mut frame_index = 0;
                while let Err(mpsc::RecvTimeoutError::Timeout) =
                    receiver.recv_timeout(Duration::from_millis(125))
                {
                    frame_index = (frame_index + 1) % frames.len();
                    let frame = frames[frame_index].clone();
                    let frame_active = Arc::clone(&worker_active);
                    let frame_app = app.clone();
                    let update = move || {
                        if !frame_active.load(Ordering::Acquire) {
                            return;
                        }
                        if let Some(tray) = frame_app.tray_by_id("slopcast")
                            && let Err(error) = tray.set_icon(Some(frame))
                        {
                            log::error!("Cannot animate tray icon: {error}");
                        }
                    };
                    if let Err(error) = app.run_on_main_thread(update) {
                        log::error!("Cannot schedule tray animation: {error}");
                        break;
                    }
                }
            })
            .map_err(|error| format!("Cannot start tray animation: {error}"))?;

        Ok(Self {
            is_active,
            _stop: stop,
        })
    }
}

fn recording_frame(icon: &Image<'_>, brightness: u8) -> Result<Image<'static>, String> {
    let mut pixels = icon.rgba().to_vec();
    let width = i64::from(icon.width());
    let height = i64::from(icon.height());
    let radius = (width.min(height) / 5).max(2);
    let center_x = width - radius - 1;
    let center_y = height - radius - 1;
    let inner_radius = radius * 3 / 4;

    for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let index = i64::try_from(index).map_err(|error| error.to_string())?;
        let x = index % width - center_x;
        let y = index / width - center_y;
        let distance = x * x + y * y;
        if distance <= inner_radius * inner_radius {
            pixel.copy_from_slice(&[brightness, 45, 55, 255]);
        } else if distance <= radius * radius {
            pixel.copy_from_slice(&[255, 255, 255, 255]);
        }
    }

    Ok(Image::new_owned(pixels, icon.width(), icon.height()))
}
