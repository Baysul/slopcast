use arc_swap::ArcSwapOption;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Bounded lock-free ring queue for up to 8 audio frame chunks (~0.7 s of
/// 48 kHz stereo audio at the default 16 KiB slot size). Kept small on
/// purpose: a stall upstream must not be able to buffer seconds of stale
/// audio that would then play out behind the live video (drop-oldest at
/// the consumer keeps the backlog bounded; this caps how much it can
/// hold in the first place).
const AUDIO_QUEUE_CAPACITY: usize = 8;

const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_CHANNELS: u16 = 2;
const DEFAULT_SAMPLE_BYTES: usize = 2; // i16
pub const PCM_FRAME_SIZE: usize = (DEFAULT_CHANNELS as usize) * DEFAULT_SAMPLE_BYTES; // 4 bytes

/// Default slot capacity: 16 KiB, ~85 ms of 48 kHz stereo 16-bit PCM (192,000 B/s).
pub const DEFAULT_SLOT_CAPACITY: usize = 16_384;

/// PCM data callback: 48 kHz stereo 16-bit signed integer samples. The ring
/// worker converts each frame-aligned byte chunk to `i16` samples before
/// invoking the callback; the boxed `Fn` (not a thread-safe function) is
/// called synchronously on the ring worker thread, so it must never block.
pub type AudioDataCallback = dyn Fn(Vec<i16>) + Send + Sync;

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
static AUDIO_CALLBACK: ArcSwapOption<Box<AudioDataCallback>> = ArcSwapOption::const_empty();

pub fn set_audio_data_callback(callback: Box<dyn Fn(Vec<i16>) + Send + Sync>) {
    AUDIO_CALLBACK.store(Some(Arc::new(callback)));
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

/// Lock-free non-blocking push; safe to call directly from PipeWire/WASAPI
/// real-time process callbacks.
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
        // No free slot: drop the chunk and count it as truncated.
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
                                // Chunks are frame-aligned (the push path
                                // truncates unaligned tails), so every pair of
                                // bytes decodes exactly to one i16 sample.
                                let samples: Vec<i16> = chunk
                                    .chunks_exact(2)
                                    .map(|c| i16::from_le_bytes([c[0], c[1]]))
                                    .collect();
                                cb(samples);
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
            // Reap the worker on a detached thread: it synchronously runs
            // the user PCM callback, and joining it while
            // `AUDIO_RING_LIFECYCLE` (and `CAPTURE_STATE` above it) is held
            // would deadlock the moment that callback ever re-enters
            // capture control. Same detached-reaper pattern as the capture
            // and metering workers. The worker drains and exits on its stop
            // flag; `AUDIO_PRODUCER` is already swapped to `None`, so no new
            // chunks can arrive.
            let _ = thread::Builder::new()
                .name("audio-ring-reaper".into())
                .spawn(move || {
                    let _ = join.join();
                });
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
    use std::sync::Mutex;

    // The ring's counters and producer are process-global statics, so tests
    // that start/stop the ring or read stats race each other when run in
    // parallel. Serialize them.
    static RING_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_ring_tests() -> std::sync::MutexGuard<'static, ()> {
        RING_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

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
    fn align_up_rounds_up_to_alignment() {
        assert_eq!(align_up(0, 4), 0);
        assert_eq!(align_up(1, 4), 4);
        assert_eq!(align_up(4, 4), 4);
        assert_eq!(align_up(17, 8), 24);
        assert_eq!(align_up(42, 0), 42);
        // Saturating: usize::MAX stays usize::MAX instead of wrapping.
        assert_eq!(align_up(usize::MAX, 4), usize::MAX);
    }

    #[test]
    fn align_down_floors_to_alignment() {
        assert_eq!(align_down(0, 4), 0);
        assert_eq!(align_down(1, 4), 0);
        assert_eq!(align_down(4, 4), 4);
        assert_eq!(align_down(23, 8), 16);
        assert_eq!(align_down(42, 0), 42);
    }

    #[test]
    fn slot_capacity_falls_back_when_channels_zero() {
        // Zero channels would divide by nothing; the default slot size must
        // be returned instead of 0 (a zero-sized slot would make every
        // push_pcm_bytes chunk a no-op).
        assert_eq!(
            calculate_slot_capacity(48_000, 0, 2, 100, 2),
            DEFAULT_SLOT_CAPACITY
        );
        assert_eq!(
            calculate_slot_capacity(48_000, 2, 0, 100, 2),
            DEFAULT_SLOT_CAPACITY
        );
    }

    #[test]
    fn slot_capacity_respects_minimum_floor_and_headroom_clamp() {
        // A tiny interval must still clear the 8 KiB floor.
        let cap = calculate_slot_capacity(8_000, 1, 1, 1, 1);
        // 8 bytes/s of mono u8: far below the floor, so exactly 8192 remains.
        assert_eq!(cap, 8192);

        // headroom 0 is treated as 1 (max(1)), not as "zero everything".
        let zero_headroom = calculate_slot_capacity(48_000, 2, 2, 100, 0);
        assert_eq!(zero_headroom, calculate_slot_capacity(48_000, 2, 2, 100, 1));

        // max_interval_ms 0 still yields the floor, not 0.
        assert!(calculate_slot_capacity(48_000, 2, 2, 0, 2) >= 8192);
    }

    #[test]
    fn push_without_started_ring_is_a_noop() {
        let _guard = lock_ring_tests();
        stop_audio_ring();
        reset_audio_ring_stats();
        push_pcm_bytes(&[1, 2, 3, 4]);
        let stats = get_audio_ring_stats();
        assert_eq!(stats.captured_chunks, 0);
        assert_eq!(stats.captured_bytes, 0);
        assert_eq!(stats.ring_drops, 0);
        assert_eq!(stats.truncated_bytes, 0);
    }

    #[test]
    fn push_empty_bytes_is_a_noop() {
        let _guard = lock_ring_tests();
        start_audio_ring_with_capacity(8192, 4).unwrap_or_else(|e| panic!("ring start: {e}"));
        reset_audio_ring_stats();
        push_pcm_bytes(&[]);
        let stats = get_audio_ring_stats();
        assert_eq!(stats.captured_chunks, 0);
        assert_eq!(stats.captured_bytes, 0);
        stop_audio_ring();
    }

    #[test]
    fn push_fully_unaligned_payload_counts_truncation_only() {
        let _guard = lock_ring_tests();
        start_audio_ring_with_capacity(8192, 4).unwrap_or_else(|e| panic!("ring start: {e}"));
        reset_audio_ring_stats();
        // 2 bytes < one 4-byte frame: nothing capturable remains.
        push_pcm_bytes(&[9, 9]);
        let stats = get_audio_ring_stats();
        assert_eq!(stats.captured_chunks, 0);
        assert_eq!(stats.captured_bytes, 0);
        assert_eq!(stats.truncated_bytes, 2);
        stop_audio_ring();
    }

    #[test]
    fn stats_reset_zeroes_every_counter() {
        let _guard = lock_ring_tests();
        start_audio_ring_with_capacity(8192, 4).unwrap_or_else(|e| panic!("ring start: {e}"));
        push_pcm_bytes(&[0u8; 64]);
        reset_audio_ring_stats();
        let stats = get_audio_ring_stats();
        assert_eq!(stats.captured_chunks, 0);
        assert_eq!(stats.captured_bytes, 0);
        assert_eq!(stats.ring_drops, 0);
        assert_eq!(stats.tsfn_drops, 0);
        assert_eq!(stats.truncated_bytes, 0);
        stop_audio_ring();
    }

    #[test]
    fn test_push_pcm_bytes_alignment_and_oversized() {
        let _guard = lock_ring_tests();
        start_audio_ring_with_capacity(8192, 4).unwrap_or_else(|e| panic!("ring start: {e}"));
        reset_audio_ring_stats();

        // Push 18 bytes (4 frames = 16 bytes, 2 unaligned trailing bytes)
        let payload = vec![0u8; 18];
        push_pcm_bytes(&payload);

        let stats = get_audio_ring_stats();
        assert_eq!(stats.captured_chunks, 1);
        assert_eq!(stats.captured_bytes, 16);
        assert_eq!(stats.truncated_bytes, 2);

        stop_audio_ring();
    }

    #[test]
    fn test_oversized_payload_splitting() {
        let _guard = lock_ring_tests();
        start_audio_ring_with_capacity(8192, 4).unwrap_or_else(|e| panic!("ring start: {e}"));
        reset_audio_ring_stats();

        // Push 20,000 bytes (larger than 8,192 byte slot capacity)
        let payload = vec![1u8; 20000];
        push_pcm_bytes(&payload);

        let stats = get_audio_ring_stats();
        assert_eq!(stats.captured_chunks, 1);
        assert_eq!(stats.captured_bytes, 20000);
        assert_eq!(stats.ring_drops, 0);

        stop_audio_ring();
    }
}
