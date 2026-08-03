use arc_swap::ArcSwapOption;
use napi::bindgen_prelude::Buffer;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Bounded lock-free ring queue for up to 64 audio frame chunks.
const AUDIO_QUEUE_CAPACITY: usize = 64;

/// PCM format parameters: 48 kHz stereo 16-bit PCM (2 channels * 2 bytes/sample = 4 bytes/frame).
const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_CHANNELS: u16 = 2;
const DEFAULT_SAMPLE_BYTES: usize = 2; // i16
pub const PCM_FRAME_SIZE: usize = (DEFAULT_CHANNELS as usize) * DEFAULT_SAMPLE_BYTES; // 4 bytes

/// Default slot capacity (16 KiB = 16,384 bytes).
/// At 48 kHz stereo 16-bit PCM (192,000 B/s), 16 KiB covers ~85.33 ms (4,096 frames).
pub const DEFAULT_SLOT_CAPACITY: usize = 16_384;

pub type AudioThreadsafeFunction =
    ThreadsafeFunction<Buffer, (), Buffer, napi::Status, true, false, AUDIO_QUEUE_CAPACITY>;

struct AudioProducer {
    data_queue: Arc<crossbeam_queue::ArrayQueue<Vec<u8>>>,
    free_slots: Arc<crossbeam_queue::ArrayQueue<Vec<u8>>>,
    slot_capacity: usize,
    frame_size: usize,
    worker_thread: thread::Thread,
}

struct AudioRingSession {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRingStats {
    pub captured_chunks: u64,
    pub captured_bytes: u64,
    pub ring_drops: u64,
    pub tsfn_drops: u64,
    pub truncated_bytes: u64,
}

static CAPTURED_CHUNKS: AtomicU64 = AtomicU64::new(0);
static CAPTURED_BYTES: AtomicU64 = AtomicU64::new(0);
static RING_DROPS: AtomicU64 = AtomicU64::new(0);
static TSFN_DROPS: AtomicU64 = AtomicU64::new(0);
static TRUNCATED_BYTES: AtomicU64 = AtomicU64::new(0);

static AUDIO_PRODUCER: ArcSwapOption<AudioProducer> = ArcSwapOption::const_empty();
static AUDIO_RING_LIFECYCLE: Mutex<Option<AudioRingSession>> = Mutex::new(None);
static AUDIO_CALLBACK: ArcSwapOption<AudioThreadsafeFunction> = ArcSwapOption::const_empty();

pub fn set_audio_data_callback(callback: Arc<AudioThreadsafeFunction>) -> napi::Result<()> {
    AUDIO_CALLBACK.store(Some(callback));
    Ok(())
}

pub fn clear_audio_data_callback() {
    AUDIO_CALLBACK.store(None);
}

fn align_up(value: usize, alignment: usize) -> usize {
    if alignment == 0 {
        return value;
    }
    let rem = value % alignment;
    if rem == 0 {
        value
    } else {
        value.saturating_add(alignment - rem)
    }
}

fn align_down(value: usize, alignment: usize) -> usize {
    if alignment == 0 {
        return value;
    }
    (value / alignment) * alignment
}

/// Calculates slot capacity in bytes from audio parameters, aligned to PCM frame boundaries.
pub fn calculate_slot_capacity(
    sample_rate: u32,
    channels: u16,
    sample_bytes: usize,
    max_interval_ms: u32,
    headroom_factor: usize,
) -> usize {
    let frame_size = (channels as usize).saturating_mul(sample_bytes);
    if frame_size == 0 {
        return DEFAULT_SLOT_CAPACITY;
    }
    let bytes_per_sec = (sample_rate as usize).saturating_mul(frame_size);
    let base_bytes = (bytes_per_sec.saturating_mul(max_interval_ms as usize)) / 1000;
    let target = base_bytes.saturating_mul(headroom_factor.max(1));
    let min_cap = align_up(8192, frame_size);
    let target = target.max(min_cap);
    align_down(target, frame_size)
}

/// RT-safe lock-free non-blocking push of PCM audio bytes into the global audio ring buffer.
/// Safe to call directly from PipeWire or WASAPI real-time process callbacks.
pub fn push_pcm_bytes(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }

    let producer_guard = AUDIO_PRODUCER.load();
    let Some(producer) = producer_guard.as_ref() else {
        return;
    };

    let frame_size = if producer.frame_size > 0 {
        producer.frame_size
    } else {
        PCM_FRAME_SIZE
    };

    let slot_capacity = if producer.slot_capacity > 0 {
        producer.slot_capacity
    } else {
        DEFAULT_SLOT_CAPACITY
    };

    let unaligned = bytes.len() % frame_size;
    let (payload, trailing_unaligned) = if unaligned != 0 {
        (&bytes[..bytes.len() - unaligned], unaligned)
    } else {
        (bytes, 0)
    };

    if trailing_unaligned > 0 {
        TRUNCATED_BYTES.fetch_add(trailing_unaligned as u64, Ordering::Relaxed);
    }

    if payload.is_empty() {
        return;
    }

    CAPTURED_CHUNKS.fetch_add(1, Ordering::Relaxed);
    CAPTURED_BYTES.fetch_add(payload.len() as u64, Ordering::Relaxed);

    let mut pushed = false;
    for chunk in payload.chunks(slot_capacity) {
        let Some(mut slot) = producer.free_slots.pop() else {
            RING_DROPS.fetch_add(1, Ordering::Relaxed);
            TRUNCATED_BYTES.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            continue;
        };

        slot.clear();
        slot.extend_from_slice(chunk);

        if let Err(returned_slot) = producer.data_queue.push(slot) {
            RING_DROPS.fetch_add(1, Ordering::Relaxed);
            TRUNCATED_BYTES.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            let _ = producer.free_slots.push(returned_slot);
        } else {
            pushed = true;
        }
    }

    if pushed {
        producer.worker_thread.unpark();
    }
}

pub fn start_audio_ring() -> Result<(), String> {
    let calculated_capacity = calculate_slot_capacity(
        DEFAULT_SAMPLE_RATE,
        DEFAULT_CHANNELS,
        DEFAULT_SAMPLE_BYTES,
        100,
        2,
    );
    start_audio_ring_with_capacity(calculated_capacity, PCM_FRAME_SIZE)
}

pub fn start_audio_ring_with_capacity(
    slot_capacity: usize,
    frame_size: usize,
) -> Result<(), String> {
    let mut guard = AUDIO_RING_LIFECYCLE
        .lock()
        .map_err(|e| format!("AUDIO_RING_LIFECYCLE mutex poisoned: {e}"))?;

    stop_audio_ring_internal(&mut guard);
    reset_audio_ring_stats();

    let frame_size = if frame_size == 0 {
        PCM_FRAME_SIZE
    } else {
        frame_size
    };
    let min_cap = align_up(8192, frame_size);
    let slot_capacity = align_down(slot_capacity.max(min_cap), frame_size);

    let data_queue = Arc::new(crossbeam_queue::ArrayQueue::<Vec<u8>>::new(
        AUDIO_QUEUE_CAPACITY,
    ));
    let free_slots = Arc::new(crossbeam_queue::ArrayQueue::<Vec<u8>>::new(
        AUDIO_QUEUE_CAPACITY,
    ));

    for _ in 0..AUDIO_QUEUE_CAPACITY {
        let _ = free_slots.push(Vec::with_capacity(slot_capacity));
    }

    let data_queue_worker = data_queue.clone();
    let free_slots_worker = free_slots.clone();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();

    let worker = thread::Builder::new()
        .name("audio-ring-worker".into())
        .spawn(move || {
            let mut spins: u32 = 0;
            while !stop_clone.load(Ordering::Relaxed) {
                match data_queue_worker.pop() {
                    Some(chunk) => {
                        spins = 0;
                        if !chunk.is_empty() {
                            let cb_guard = AUDIO_CALLBACK.load();
                            if let Some(cb) = cb_guard.as_ref() {
                                let status = cb.call(
                                    Ok(Buffer::from(chunk.as_slice())),
                                    ThreadsafeFunctionCallMode::NonBlocking,
                                );
                                if status != napi::Status::Ok {
                                    TSFN_DROPS.fetch_add(1, Ordering::Relaxed);
                                    TRUNCATED_BYTES
                                        .fetch_add(chunk.len() as u64, Ordering::Relaxed);
                                }
                            }
                        }
                        let _ = free_slots_worker.push(chunk);
                    }
                    None => {
                        if spins < 32 {
                            spins += 1;
                            std::hint::spin_loop();
                        } else {
                            thread::park_timeout(Duration::from_millis(2));
                            spins = 0;
                        }
                    }
                }
            }
        })
        .map_err(|e| format!("Failed to spawn audio ring worker thread: {e}"))?;

    let producer = AudioProducer {
        data_queue,
        free_slots,
        slot_capacity,
        frame_size,
        worker_thread: worker.thread().clone(),
    };

    *guard = Some(AudioRingSession {
        stop,
        worker: Some(worker),
    });

    AUDIO_PRODUCER.store(Some(Arc::new(producer)));

    Ok(())
}

fn stop_audio_ring_internal(guard: &mut Option<AudioRingSession>) {
    let old_producer = AUDIO_PRODUCER.swap(None);

    if let Some(mut session) = guard.take() {
        session.stop.store(true, Ordering::Relaxed);
        if let Some(join) = session.worker.take() {
            join.thread().unpark();
            let _ = join.join();
        }
    }

    drop(old_producer);
}

pub fn stop_audio_ring() {
    let Ok(mut guard) = AUDIO_RING_LIFECYCLE.lock() else {
        return;
    };
    stop_audio_ring_internal(&mut guard);
}

pub fn get_audio_ring_stats() -> AudioRingStats {
    AudioRingStats {
        captured_chunks: CAPTURED_CHUNKS.load(Ordering::Relaxed),
        captured_bytes: CAPTURED_BYTES.load(Ordering::Relaxed),
        ring_drops: RING_DROPS.load(Ordering::Relaxed),
        tsfn_drops: TSFN_DROPS.load(Ordering::Relaxed),
        truncated_bytes: TRUNCATED_BYTES.load(Ordering::Relaxed),
    }
}

pub fn reset_audio_ring_stats() {
    CAPTURED_CHUNKS.store(0, Ordering::Relaxed);
    CAPTURED_BYTES.store(0, Ordering::Relaxed);
    RING_DROPS.store(0, Ordering::Relaxed);
    TSFN_DROPS.store(0, Ordering::Relaxed);
    TRUNCATED_BYTES.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_slot_capacity() {
        let cap = calculate_slot_capacity(48_000, 2, 2, 100, 2);
        assert_eq!(cap % PCM_FRAME_SIZE, 0);
        assert!(cap >= 8192);
        assert_eq!(cap, 38400);

        // Frame size 6 (not dividing 8192)
        let cap_6 = calculate_slot_capacity(48_000, 3, 2, 10, 1);
        assert_eq!(cap_6 % 6, 0);
        assert!(cap_6 >= 8192);
    }

    #[test]
    fn test_push_pcm_bytes_alignment_and_oversized() {
        start_audio_ring_with_capacity(8192, 4).unwrap_or_else(|e| panic!("ring start: {e}"));
        reset_audio_ring_stats();

        // Push 18 bytes (4 frames = 16 bytes, 2 unaligned trailing bytes)
        let data = vec![0u8; 18];
        push_pcm_bytes(&data);

        let stats = get_audio_ring_stats();
        assert_eq!(stats.captured_chunks, 1);
        assert_eq!(stats.captured_bytes, 16);
        assert_eq!(stats.truncated_bytes, 2);

        stop_audio_ring();
    }

    #[test]
    fn test_oversized_payload_splitting() {
        start_audio_ring_with_capacity(8192, 4).unwrap_or_else(|e| panic!("ring start: {e}"));
        reset_audio_ring_stats();

        // Push 20,000 bytes (larger than 8,192 byte slot capacity)
        let data = vec![1u8; 20000];
        push_pcm_bytes(&data);

        let stats = get_audio_ring_stats();
        assert_eq!(stats.captured_chunks, 1);
        assert_eq!(stats.captured_bytes, 20000);
        assert_eq!(stats.ring_drops, 0);

        stop_audio_ring();
    }
}
