use super::{CAPTURE_NODE_NAME, pw_init};
use crate::AudioAppWave;
use arc_swap::ArcSwapOption;
use crossbeam_queue::ArrayQueue;
use pipewire::properties::properties;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pipewire::spa::param::format::MediaType;
use pipewire::spa::param::{ParamType, format_utils};
use pipewire::spa::pod::{Object, Pod, Value};
use pipewire::spa::utils::SpaTypes;
use pipewire::stream::{StreamFlags, StreamListener, StreamRc};
use pipewire::types::ObjectType;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const METER_WAVE_COLUMNS: usize = 96;
const METER_WAVE_WINDOW: usize = 4096;
const METER_WAVE_INTERVAL_MS: u64 = 33;
const METER_DEFAULT_RATE: u32 = 48_000;
const METER_DEFAULT_CHANNELS: u16 = 2;
// Ring silent this long → paused: publish silence instead of re-decimating
// stale audio.
const METER_STALE_MS: u64 = 150;
// Mono queue between the process callback and the wave pass. Over two worst-case
// pass gaps, so a stalled pass only drops the newest samples (invisible after
// decimation).
const METER_RING_CAPACITY: usize = 4096;

/// Per-app meter state shared between the worker thread and the JS thread.
struct MeterLevel {
    samples: ArrayQueue<f32>,
    rate: AtomicU32,
    channels: AtomicU16,
    /// 96 interleaved (min, max) pairs; amplitudes in [-1, 1].
    wave: Mutex<Vec<f32>>,
}

impl MeterLevel {
    fn new() -> Self {
        Self {
            samples: ArrayQueue::new(METER_RING_CAPACITY),
            rate: AtomicU32::new(METER_DEFAULT_RATE),
            channels: AtomicU16::new(METER_DEFAULT_CHANNELS),
            wave: Mutex::new(vec![0.0; METER_WAVE_COLUMNS * 2]),
        }
    }
}

struct MeterStream {
    _stream: StreamRc,
    _listener: StreamListener<Arc<MeterLevel>>,
    level: Arc<MeterLevel>,
    /// Rolling mono window drained from `level.samples`, capped at
    /// `METER_WAVE_WINDOW`. Worker-thread only, hence a plain `Vec`.
    window: Vec<f32>,
    last_feed: Instant,
}

struct MeterSession {
    stop: Arc<AtomicBool>,
    join: thread::JoinHandle<()>,
}

static METER_STATE: Mutex<Option<MeterSession>> = Mutex::new(None);

/// The meter worker pushes each waveform snapshot here; the renderer reads
/// them via `get_audio_wave`.
static AUDIO_WAVE_CALLBACK: ArcSwapOption<Box<dyn Fn(Vec<AudioAppWave>) + Send + Sync>> =
    ArcSwapOption::const_empty();

pub(crate) fn set_wave_callback(callback: Box<dyn Fn(Vec<AudioAppWave>) + Send + Sync>) {
    AUDIO_WAVE_CALLBACK.store(Some(Arc::new(callback)));
}

pub(crate) fn clear_wave_callback() {
    AUDIO_WAVE_CALLBACK.store(None);
}

/// Non-destructive wave snapshot; dropped if the caller is busy — the next
/// 33 ms tick supersedes it.
fn invoke_wave_callback(waves: Vec<AudioAppWave>) {
    let guard = AUDIO_WAVE_CALLBACK.load();
    let Some(callback) = guard.as_ref() else {
        return;
    };
    callback(waves);
}

fn wave_snapshot(meters: &HashMap<u32, MeterStream>) -> Vec<AudioAppWave> {
    let mut out = Vec::with_capacity(meters.len());
    for (&id, meter) in meters {
        let Ok(wave) = meter.level.wave.lock() else {
            continue;
        };
        out.push(AudioAppWave {
            id: id.cast_signed(),
            columns: wave.iter().map(|&v| f64::from(v)).collect(),
        });
    }
    out
}

fn meter_format_param() -> Option<Vec<u8>> {
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let serialized = pipewire::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .ok()?;
    Some(serialized.0.into_inner())
}

/// Downmix this quantum to mono and queue it (drop-newest when full; the
/// decimated envelope hides a few-ms gap).
fn meter_process_quantum(stream: &pipewire::stream::Stream, level: &Arc<MeterLevel>) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let channels = usize::from(level.channels.load(Ordering::Relaxed).max(1));
    let inv_channels = 1.0 / f32::from(level.channels.load(Ordering::Relaxed).max(1));
    // Negotiated F32LE buffers are interleaved in a single data; the
    // multi-data branch is a fallback that treats each data as its own mono
    // stream.
    let interleaved = buffer.datas_mut().len() <= 1;
    for data in buffer.datas_mut() {
        let start = data.chunk().offset() as usize;
        let size = data.chunk().size() as usize;
        let Some(bytes) = data.data() else { continue };
        let end = start.saturating_add(size).min(bytes.len());
        let Some(slice) = bytes.get(start..end) else {
            continue;
        };
        if interleaved && channels > 1 {
            for frame in slice.chunks_exact(channels * 4) {
                let mut sum = 0.0f32;
                for ch in 0..channels {
                    let off = ch * 4;
                    sum += f32::from_le_bytes([
                        frame[off],
                        frame[off + 1],
                        frame[off + 2],
                        frame[off + 3],
                    ]);
                }
                if level.samples.push(sum * inv_channels).is_err() {
                    return;
                }
            }
        } else {
            for sample in slice.chunks_exact(4) {
                let s = f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
                if level.samples.push(s).is_err() {
                    return;
                }
            }
        }
    }
}

/// Tap the app's output with a capture stream. `AUTOCONNECT` links additively —
/// the app's existing links to the speaker are never touched.
fn meter_stream(
    core: &pipewire::core::CoreRc,
    node_id: u32,
    level: Arc<MeterLevel>,
) -> Option<MeterStream> {
    let stream = StreamRc::new(
        core.clone(),
        "slopcast-audio-meter",
        properties! {
            "media.class" => "Stream/Input/Audio",
            "node.name" => format!("slopcast-meter-{node_id}"),
            "node.description" => "Slopcast Audio Meter",
            "node.dont-move" => "true",
            "node.dont-reconnect" => "true",
            "node.dont-fallback" => "true",
        },
    )
    .ok()?;

    let listener = stream
        .add_local_listener_with_user_data(Arc::clone(&level))
        .param_changed(|_stream, level, _id, param| {
            let Some(pod) = param else { return };
            let Ok((media_type, _)) = format_utils::parse_format(pod) else {
                return;
            };
            if media_type != MediaType::Audio {
                return;
            }
            let mut info = AudioInfoRaw::new();
            if info.parse(pod).is_err() {
                return;
            }
            let rate = info.rate();
            if rate > 0 {
                level.rate.store(rate, Ordering::Relaxed);
            }
            let channels = u16::try_from(info.channels()).unwrap_or(METER_DEFAULT_CHANNELS);
            if channels > 0 {
                level.channels.store(channels, Ordering::Relaxed);
            }
        })
        .process(|stream, level| meter_process_quantum(stream, level))
        .register()
        .ok()?;

    let values = meter_format_param()?;
    let pod = Pod::from_bytes(&values)?;
    let mut params = [pod];
    stream
        .connect(
            pipewire::spa::utils::Direction::Input,
            Some(node_id),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .ok()?;

    Some(MeterStream {
        _stream: stream,
        _listener: listener,
        level,
        window: Vec::with_capacity(METER_WAVE_WINDOW),
        last_feed: Instant::now(),
    })
}

/// Decimate a mono window into 96 interleaved (min, max) pairs. NaN is
/// treated as 0.0 — a NaN would poison both min and max, which the JS side
/// renders directly.
fn decimate_wave(window: &[f32], wave: &mut [f32]) {
    let len = window.len();
    let bucket = len.div_ceil(METER_WAVE_COLUMNS);
    for c in 0..METER_WAVE_COLUMNS {
        let start = c * bucket;
        let end = ((c + 1) * bucket).min(len);
        if start >= end {
            wave[c * 2] = 0.0;
            wave[c * 2 + 1] = 0.0;
            continue;
        }
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for sample in &window[start..end] {
            let sample = if sample.is_nan() { 0.0 } else { *sample };
            min = min.min(sample);
            max = max.max(sample);
        }
        wave[c * 2] = min;
        wave[c * 2 + 1] = max;
    }
}

/// Decimate every meter's rolling window into 96 (min, max) pairs and publish
/// them. Meters silent for `METER_STALE_MS` publish zeros — their window still
/// holds pre-pause audio, so re-decimating would pin the bars instead of
/// letting them flatline.
fn run_wave_pass(meters: &mut HashMap<u32, MeterStream>) {
    for meter in meters.values_mut() {
        let level = &meter.level;
        let mut fed = false;
        while let Some(sample) = level.samples.pop() {
            meter.window.push(sample);
            fed = true;
        }
        let overflow = meter.window.len().saturating_sub(METER_WAVE_WINDOW);
        if overflow > 0 {
            meter.window.drain(0..overflow);
        }
        if meter.window.is_empty() {
            continue;
        }

        let now = Instant::now();
        if fed {
            meter.last_feed = now;
        }
        let stale = now.duration_since(meter.last_feed) > Duration::from_millis(METER_STALE_MS);

        let Ok(mut wave) = level.wave.lock() else {
            continue;
        };
        if stale {
            wave.fill(0.0);
            continue;
        }
        decimate_wave(&meter.window, &mut wave);
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "run_meter_session is spawned in a thread::spawn(move || ...) closure that must own all Arc values"
)]
fn run_meter_session(stop: Arc<AtomicBool>, ready_tx: mpsc::Sender<Result<(), String>>) {
    pipewire::init();

    let Ok(pw) = pw_init() else {
        let _ = ready_tx.send(Err("PipeWire init failed".into()));
        return;
    };
    let registry = match pw.core.get_registry() {
        Ok(r) => r,
        Err(e) => {
            let _ = ready_tx.send(Err(e.to_string()));
            return;
        }
    };

    let meters: Rc<RefCell<HashMap<u32, MeterStream>>> = Rc::new(RefCell::new(HashMap::new()));
    let our_pid = std::process::id();

    let _reg_listener = registry
        .add_listener_local()
        .global({
            let meters = Rc::clone(&meters);
            let core = pw.core.clone();
            move |global| {
                let Some(props) = global.props else { return };
                if global.type_ != ObjectType::Node {
                    return;
                }
                if props.get("media.class") != Some("Stream/Output/Audio") {
                    return;
                }
                let name = props
                    .get("application.name")
                    .or_else(|| props.get("node.name"))
                    .or_else(|| props.get("media.name"))
                    .unwrap_or("");
                if name.is_empty()
                    || name.contains(CAPTURE_NODE_NAME)
                    || name.to_lowercase().contains("slopcast")
                {
                    return;
                }
                if props
                    .get("application.process.id")
                    .and_then(|v| v.parse::<u32>().ok())
                    == Some(our_pid)
                {
                    return;
                }

                let mut map = meters.borrow_mut();
                if map.contains_key(&global.id) {
                    return;
                }
                let level = Arc::new(MeterLevel::new());
                let Some(meter) = meter_stream(&core, global.id, level) else {
                    return;
                };
                map.insert(global.id, meter);
            }
        })
        .global_remove({
            let meters = Rc::clone(&meters);
            move |id| {
                meters.borrow_mut().remove(&id);
            }
        })
        .register();

    let mut last_wave = Instant::now();

    let _ = ready_tx.send(Ok(()));

    while !stop.load(Ordering::SeqCst) {
        // 50 ms idle bound: with live audio the loop wakes on buffer events, so
        // metering latency is unaffected and a tighter timeout would just spin.
        pw.main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(50)));

        let now = Instant::now();
        if now.duration_since(last_wave) >= Duration::from_millis(METER_WAVE_INTERVAL_MS) {
            last_wave = now;
            let mut meter_map = meters.borrow_mut();
            run_wave_pass(&mut meter_map);
            invoke_wave_callback(wave_snapshot(&meter_map));
        }
    }

    meters.borrow_mut().clear();
}

pub(crate) fn start_audio_metering() -> Result<bool, String> {
    let mut guard = METER_STATE
        .lock()
        .map_err(|e| format!("Audio metering state lock poisoned: {e}"))?;
    if guard.is_some() {
        return Ok(true);
    }

    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

    let join = {
        let stop = Arc::clone(&stop);
        thread::Builder::new()
            .name("pw-audio-metering".into())
            .spawn(move || run_meter_session(stop, ready_tx))
            .map_err(|e| format!("Failed to spawn metering worker: {e}"))?
    };

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            stop.store(true, Ordering::SeqCst);
            crate::reap_detached(join, "pw-meter-reaper");
            return Err(reason);
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            crate::reap_detached(join, "pw-meter-reaper");
            return Err("Timed out starting audio metering".into());
        }
    }

    *guard = Some(MeterSession { stop, join });
    Ok(true)
}

pub(crate) fn stop_audio_metering() -> bool {
    let Ok(mut guard) = METER_STATE.lock() else {
        eprintln!("[meter] state lock poisoned; nothing to stop");
        return true;
    };
    if let Some(session) = guard.take() {
        session.stop.store(true, Ordering::SeqCst);
        // Reap off-thread: the meter thread only checks the stop flag between
        // 50 ms loop iterations, so a join here would stall the main process.
        // The streams are dropped with the session.
        let _ = thread::Builder::new()
            .name("pw-meter-reaper".into())
            .spawn(move || {
                let _ = session.join.join();
            });
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_wave() -> Vec<f32> {
        vec![f32::MAX; METER_WAVE_COLUMNS * 2]
    }

    #[test]
    fn decimate_wave_empty_window_writes_zeros() {
        let mut wave = fresh_wave();
        decimate_wave(&[], &mut wave);
        assert!(wave.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn decimate_wave_single_sample_fills_first_column_only() {
        let mut wave = fresh_wave();
        decimate_wave(&[0.25], &mut wave);
        // bucket = ceil(1/96) = 1: only column 0 holds the sample.
        assert!((wave[0] - 0.25).abs() < f32::EPSILON);
        assert!((wave[1] - 0.25).abs() < f32::EPSILON);
        assert!(wave[2..].iter().all(|v| *v == 0.0));
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        reason = "test fixtures: usize indices converted to f32 samples"
    )]
    fn decimate_wave_captures_extrema_per_column() {
        let mut wave = fresh_wave();
        // 96 samples over 96 columns → exactly one sample per column.
        let samples: Vec<f32> = (0..METER_WAVE_COLUMNS).map(|i| i as f32 / 10.0).collect();
        decimate_wave(&samples, &mut wave);
        for c in 0..METER_WAVE_COLUMNS {
            let expected = c as f32 / 10.0;
            assert!((wave[c * 2] - expected).abs() < f32::EPSILON);
            assert!((wave[c * 2 + 1] - expected).abs() < f32::EPSILON);
        }
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        reason = "test fixtures: i32 indices converted to f32 samples"
    )]
    fn decimate_wave_buckets_oversized_windows_with_div_ceil() {
        // 191 samples: bucket = ceil(191/96) = 2, so columns 0..94 hold two
        // samples each and the last column holds a single trailing sample.
        let mut wave = fresh_wave();
        let samples: Vec<f32> = (0..191).map(|i| i as f32 / 191.0).collect();
        decimate_wave(&samples, &mut wave);
        // Column 0 bucket: samples 0 and 1.
        assert!((wave[0] - 0.0).abs() < f32::EPSILON);
        assert!((wave[1] - 1.0 / 191.0).abs() < f32::EPSILON);
        // Last column holds the final sample only.
        let last_min = wave[95 * 2];
        let last_max = wave[95 * 2 + 1];
        assert!((last_min - 190.0 / 191.0).abs() < f32::EPSILON);
        assert!((last_max - 190.0 / 191.0).abs() < f32::EPSILON);
    }

    #[test]
    fn decimate_wave_shorter_windows_zero_pad_tail_columns() {
        let mut wave = fresh_wave();
        // 10 samples → bucket 1; columns 10..96 must be zeroed, not retain
        // stale data from a previous pass.
        let samples = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0];
        decimate_wave(&samples, &mut wave);
        assert!((wave[9 * 2] - 1.0).abs() < f32::EPSILON);
        assert!(wave[10 * 2].abs() < f32::EPSILON);
        assert!(wave[10 * 2 + 1].abs() < f32::EPSILON);
        assert!(wave[95 * 2].abs() < f32::EPSILON);
        assert!(wave[95 * 2 + 1].abs() < f32::EPSILON);
    }

    #[test]
    fn decimate_wave_treats_nan_samples_as_silence() {
        let mut wave = fresh_wave();
        decimate_wave(&[f32::NAN, 0.5], &mut wave);
        // Column 0 holds only the NaN (→ 0.0); column 1 holds the 0.5 sample.
        assert!(wave[0].abs() < f32::EPSILON);
        assert!(wave[1].abs() < f32::EPSILON);
        assert!((wave[2] - 0.5).abs() < f32::EPSILON);
        assert!((wave[3] - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn decimate_wave_mixed_sign_keeps_raw_min_max() {
        let mut wave = fresh_wave();
        // 192 samples → bucket 2: column 0 = [-0.9, 0.4], column 1 = [-0.2, 0.8].
        let mut samples = vec![0.0; 192];
        samples[0] = -0.9;
        samples[1] = 0.4;
        samples[2] = -0.2;
        samples[3] = 0.8;
        decimate_wave(&samples, &mut wave);
        assert!((wave[0] - (-0.9)).abs() < f32::EPSILON);
        assert!((wave[1] - 0.4).abs() < f32::EPSILON);
        assert!((wave[2] - (-0.2)).abs() < f32::EPSILON);
        assert!((wave[3] - 0.8).abs() < f32::EPSILON);
    }
}
