//! Opt-in native test driver. Compiled out of ordinary desktop builds.
//!
//! Uses typed presenter commands and the existing file handoff to the browser spectator.
//! There is no debugging port or remote command listener.

use crate::app::PresenterApp;
use desktop_core::presenter::{CaptureStage, Command, PresenterClient, Snapshot};
use gpui_kit::{Context, GpuSpecs};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

static FRAMES: AtomicU64 = AtomicU64::new(0);
static PAINTS: AtomicU64 = AtomicU64::new(0);
static DIALOG: AtomicBool = AtomicBool::new(false);
static COLORS: AtomicBool = AtomicBool::new(false);
static GPU: OnceLock<Value> = OnceLock::new();
static VIEWPORT: std::sync::Mutex<Option<(u32, u32)>> = std::sync::Mutex::new(None);

pub fn record_gpu(gpu: Option<GpuSpecs>) {
    if let Some(gpu) = gpu {
        let backend = if cfg!(target_os = "windows") {
            "DirectX"
        } else {
            "Vulkan"
        };
        let _ = GPU.set(json!({
            "backend": backend,
            "deviceName": gpu.device_name,
            "driverName": gpu.driver_name,
            "driverInfo": gpu.driver_info,
            "softwareRasterizer": gpu.is_software_emulated,
        }));
    }
}
pub fn record_dialog(open: bool) {
    DIALOG.store(open, Ordering::Relaxed);
}
pub fn record_paint() {
    PAINTS.fetch_add(1, Ordering::Relaxed);
}
pub fn record_viewport(viewport: Option<(u32, u32)>) {
    if let Ok(mut current) = VIEWPORT.lock() {
        *current = viewport;
    }
}
pub fn record_frame(frame: &desktop_core::media::PreviewFrame) {
    FRAMES.fetch_add(1, Ordering::Relaxed);
    let expected = [
        [255i16, 255, 255],
        [255, 255, 0],
        [0, 255, 255],
        [0, 255, 0],
        [255, 0, 255],
        [255, 0, 0],
        [0, 0, 255],
        [0, 0, 0],
    ];
    let valid = expected.iter().enumerate().all(|(bar, rgb)| {
        let x = ((bar as u32 * 2 + 1) * frame.width / 16).min(frame.width.saturating_sub(1));
        let y = frame.height * 3 / 4;
        let offset = ((y * frame.width + x) * 4) as usize;
        frame.bgra.get(offset..offset + 3).is_some_and(|bgra| {
            (i16::from(bgra[2]) - rgb[0]).abs() <= 6
                && (i16::from(bgra[1]) - rgb[1]).abs() <= 6
                && (i16::from(bgra[0]) - rgb[2]).abs() <= 6
        })
    });
    COLORS.store(valid, Ordering::Relaxed);
}

fn preview_only() -> bool {
    std::env::var_os("SLOPCAST_E2E_PREVIEW_ONLY").is_some()
}
pub fn snapshot(mut snapshot: Snapshot) -> Snapshot {
    if preview_only() && native_livekit::is_desktop_capture_active() {
        snapshot.capture = CaptureStage::Previewing;
    }
    snapshot
}

pub fn start(app: &mut PresenterApp, _cx: &mut Context<PresenterApp>) {
    if std::env::var_os("SLOPCAST_E2E_DIALOG").is_some() {
        app.fixture_confirmation();
    }
    let Some(presenter) = app.presenter.as_ref() else {
        return;
    };
    let runtime = presenter.runtime();
    let client = presenter.client();
    if preview_only() {
        runtime.spawn(async move {
            let settings = desktop_core::settings::StreamSettings {
                resolution: "720p".into(),
                fps: 30.,
                ..Default::default()
            };
            if let Err(error) = tokio::task::spawn_blocking(move || {
                desktop_core::media::start_preview(None, &settings)
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()))
            {
                log::error!("Native preview test failed: {error}");
                return;
            }
            client.dispatch(Command::SetMainVisible(true));
            loop {
                if let Some(output) = std::env::var_os("SLOPCAST_E2E_METRICS") {
                    let viewport = *VIEWPORT
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let snapshot = client.snapshot();
                    let metrics = json!({
                        "frames": FRAMES.load(Ordering::Relaxed),
                        "paints": PAINTS.load(Ordering::Relaxed),
                        "colorsCorrect": COLORS.load(Ordering::Relaxed),
                        "viewport": viewport,
                        "captureActive": native_livekit::is_desktop_capture_active(),
                        "gpu": GPU.get(),
                        "muted": snapshot.muted,
                        "dialogOpen": DIALOG.load(Ordering::Relaxed),
                        "preferences": snapshot.preferences,
                        "shortcuts": {
                            "status": snapshot.shortcuts.status,
                            "bindings": snapshot.shortcuts.bindings,
                        },
                    });
                    let _ = write_json(&PathBuf::from(output), &metrics);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
    } else if let Some(path) = std::env::var_os("E2E_PHASE_JSON") {
        runtime.spawn(async move {
            let path = PathBuf::from(path);
            let mut phase = json!({
                "ok": true,
                "handoffReady": false,
                "roomCode": "",
                "shareUrl": "",
                "isWayland": client.snapshot().platform.is_wayland,
                "captureMode": std::env::var("E2E_CAPTURE").unwrap_or_else(|_| "synthetic".into()),
                "codec": std::env::var("E2E_CODEC").unwrap_or_else(|_| "vp8".into()),
                "gpuReport": null,
                "previewFramesSent": 0,
                "videoFramesEncoded": 0,
                "videoBytesSent": 0,
                "videoCodecReported": null,
                "encoderImplementation": null,
                "captureFramesPushed": 0,
                "telemetryFlowing": false,
                "telemetryFps": 0,
                "senderBitrateBps": 0,
                "senderBitrateSampleMs": 0,
                "postSubscriptionTelemetryReady": false,
                "errors": [],
            });
            if let Err(error) = run(&client, &path, &mut phase).await {
                phase["ok"] = false.into();
                phase["errors"] = json!([error]);
                let _ = write_json(&path, &phase);
            }
            client.dispatch(Command::Quit { anyway: false });
        });
    }
}

fn write_json(path: &std::path::Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, value.to_string()).map_err(|e| e.to_string())?;
    #[cfg(target_os = "windows")]
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    std::fs::rename(temporary, path).map_err(|e| e.to_string())
}

async fn wait(
    client: &PresenterClient,
    description: &str,
    seconds: u64,
    predicate: impl Fn(&Snapshot) -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        if predicate(&client.snapshot()) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!("Timed out waiting for {description}"))
}

async fn run(
    client: &PresenterClient,
    path: &std::path::Path,
    phase: &mut Value,
) -> Result<(), String> {
    let codec = std::env::var("E2E_CODEC").unwrap_or_else(|_| "vp8".into());
    let fps = std::env::var("E2E_EXPECTED_FPS")
        .ok()
        .and_then(|n| n.parse::<f64>().ok())
        .unwrap_or(60.);
    let bitrate = std::env::var("E2E_EXPECTED_BITRATE")
        .ok()
        .and_then(|n| n.parse::<f64>().ok())
        .unwrap_or(20_000_000.);
    wait(client, "hydrated settings", 10, |s| s.hydrated).await?;
    let changes = json!({"apiEndpoint":std::env::var("API_ENDPOINT").unwrap_or_else(|_|client.snapshot().settings.api_endpoint),"videoCodec":codec,"fps":fps,"bitrateLimit":bitrate,"autoBitrate":false,"resolution":std::env::var("E2E_RESOLUTION").unwrap_or_else(|_|"1080p".into())});
    client.dispatch(Command::UpdateStreamSettings(
        changes
            .as_object()
            .cloned()
            .ok_or("Invalid test settings")?,
    ));
    wait(client, "applied test settings", 10, |s| {
        s.settings.video_codec == codec
            && s.settings.fps == fps
            && s.settings.bitrate_limit == bitrate
    })
    .await?;
    wait(client, "healthy endpoint", 10, |s| s.can_create_room()).await?;
    client.dispatch(Command::CreateRoom);
    wait(client, "prepared room", 30, |s| s.room.is_some()).await?;
    let room = client.snapshot().room.ok_or("Room disappeared")?;
    phase["roomCode"] = room.code.into();
    phase["shareUrl"] = room.share_url.into();
    write_json(path, phase)?;
    client.dispatch(Command::StartShare);
    wait(client, "native preview", 90, |s| {
        s.capture == CaptureStage::Previewing
            && FRAMES.load(Ordering::Relaxed) > 0
            && PAINTS.load(Ordering::Relaxed) > 0
    })
    .await?;
    if std::env::var("E2E_CAPTURE").as_deref() != Ok("portal") && !COLORS.load(Ordering::Relaxed) {
        return Err("Synthetic preview colors are incorrect".into());
    }
    client.dispatch(Command::GoLive);
    wait(client, "live publication", 30, |s| {
        s.capture == CaptureStage::Live
    })
    .await?;
    let initial = native_livekit::get_native_telemetry();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let latest = native_livekit::get_native_telemetry();
    let delta =
        latest.video_frames_encoded.unwrap_or(0.) - initial.video_frames_encoded.unwrap_or(0.);
    let stats = native_livekit::get_desktop_capture_stats();
    phase["videoFramesEncoded"] = latest.video_frames_encoded.unwrap_or(0.).into();
    phase["videoBytesSent"] = latest.video_bytes_sent.unwrap_or(0.).into();
    phase["videoCodecReported"] = json!(latest.video_codec);
    phase["encoderImplementation"] = json!(latest.encoder_implementation);
    phase["captureFramesPushed"] = stats.frames_pushed.into();
    phase["previewFramesSent"] = stats.preview_frames_sent.into();
    phase["telemetryFps"] = (delta / 2.).into();
    phase["telemetryFlowing"] = (delta > 0. && stats.frames_pushed > 0).into();
    phase["gpuReport"] = GPU.get().cloned().unwrap_or(Value::Null);
    if delta < fps * 0.8 * 2. {
        return Err(format!(
            "Encoder ran at {} fps, expected at least {}",
            delta / 2.,
            fps * 0.8
        ));
    }
    if !latest.video_codec.as_deref().is_some_and(|actual| {
        actual.eq_ignore_ascii_case(&format!("video/{codec}"))
            || actual.eq_ignore_ascii_case(&codec)
    }) {
        return Err("Published codec does not match the requested codec".into());
    }
    phase["handoffReady"] = true.into();
    write_json(path, phase)?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut subscription_checked = false;
    let mut stopped = false;
    while Instant::now() < deadline {
        if flag("E2E_RELEASE_FLAG") {
            return Ok(());
        }
        if flag("E2E_STOP_FLAG") && !stopped {
            client.dispatch(Command::StopShare);
            wait(client, "stopped capture", 20, |s| {
                s.capture == CaptureStage::Idle
            })
            .await?;
            if let Some(path) = std::env::var_os("E2E_STOPPED_FLAG") {
                std::fs::write(path, "stopped").map_err(|e| e.to_string())?;
            }
            stopped = true;
        }
        if flag("E2E_SPECTATOR_READY_FLAG") && !subscription_checked {
            let before = native_livekit::get_native_telemetry();
            tokio::time::sleep(Duration::from_secs(5)).await;
            let after = native_livekit::get_native_telemetry();
            let delta =
                after.video_bytes_sent.unwrap_or(0.) - before.video_bytes_sent.unwrap_or(0.);
            let ms = after.timestamp_ms.unwrap_or(0.) - before.timestamp_ms.unwrap_or(0.);
            if delta <= 0. || ms <= 0. {
                return Err("Sender RTP bytes did not advance after subscription".into());
            }
            let rate = delta * 8000. / ms;
            if rate > bitrate * 1.25 {
                return Err(
                    "Sender exceeded the configured bitrate limit plus 25% VBR tolerance".into(),
                );
            }
            wait(client, "displayed bitrate", 10, |s| {
                s.telemetry.video_bitrate.is_some_and(|rate| rate > 0.)
            })
            .await?;
            phase["videoBytesSent"] = after.video_bytes_sent.unwrap_or(0.).into();
            phase["senderBitrateBps"] = rate.into();
            phase["senderBitrateSampleMs"] = ms.into();
            phase["postSubscriptionTelemetryReady"] = true.into();
            write_json(path, phase)?;
            subscription_checked = true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err("Spectator did not release the presenter before the test deadline".into())
}
fn flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|path| std::path::Path::new(&path).exists())
}
