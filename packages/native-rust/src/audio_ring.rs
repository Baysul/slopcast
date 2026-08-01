use arc_swap::ArcSwapOption;
use napi::bindgen_prelude::Buffer;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use ringbuf::{HeapRb, traits::*};
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// Bounded lock-free ring queue for up to 64 audio frame chunks
const AUDIO_QUEUE_CAPACITY: usize = 64;
// 64 KiB per slot covers up to ~340 ms of 48 kHz stereo 16-bit PCM (192,000 B/s)
const DEFAULT_SLOT_CAPACITY: usize = 65_536;

type RingProd = <HeapRb<Vec<u8>> as Split>::Prod;
type RingCons = <HeapRb<Vec<u8>> as Split>::Cons;

struct AudioProducer {
    data_prod: UnsafeCell<RingProd>,
    free_cons: UnsafeCell<RingCons>,
}

// SAFETY: `AudioProducer` is stored inside `ArcSwapOption` and accessed ONLY on the
// real-time audio thread executing `push_pcm_bytes`. PipeWire and WASAPI audio driver
// callbacks run sequentially on a single audio thread, guaranteeing single-producer access.
unsafe impl Sync for AudioProducer {}
unsafe impl Send for AudioProducer {}

struct AudioRingSession {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

static AUDIO_PRODUCER: ArcSwapOption<AudioProducer> = ArcSwapOption::const_empty();
static AUDIO_RING_SESSION: Mutex<Option<AudioRingSession>> = Mutex::new(None);
static AUDIO_CALLBACK: Mutex<Option<Arc<ThreadsafeFunction<Buffer, ()>>>> = Mutex::new(None);

pub fn set_audio_data_callback(callback: Arc<ThreadsafeFunction<Buffer, ()>>) -> napi::Result<()> {
    let Ok(mut guard) = AUDIO_CALLBACK.lock() else {
        return Err(napi::Error::from_reason("Lock poisoned"));
    };
    *guard = Some(callback);
    Ok(())
}

/// RT-safe lock-free non-blocking push of PCM audio bytes into the global audio ring buffer.
/// Safe to call directly from PipeWire or WASAPI real-time process callbacks.
pub fn push_pcm_bytes(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }

    // Lock-free, wait-free atomic load of active producer handle (0 syscalls/locks).
    let Some(producer) = AUDIO_PRODUCER.load_full() else {
        return;
    };

    let payload = if bytes.len() > DEFAULT_SLOT_CAPACITY {
        &bytes[..DEFAULT_SLOT_CAPACITY]
    } else {
        bytes
    };

    // SAFETY: Audio driver callbacks are strictly sequential on the real-time audio thread.
    // We obtain mutable references to data_prod and free_cons without locking a Mutex.
    let free_cons = unsafe { &mut *producer.free_cons.get() };
    let data_prod = unsafe { &mut *producer.data_prod.get() };

    // Non-blocking try to pop a pre-allocated vector slot from the free queue
    let Some(mut slot) = free_cons.try_pop() else {
        // Queue full (consumer worker thread hasn't drained yet): drop frame to maintain RT safety.
        return;
    };

    slot.clear();
    slot.extend_from_slice(payload);

    // Non-blocking push into data_prod
    if let Err(_returned_slot) = data_prod.try_push(slot) {
        // Overflow fallback: slot dropped if data_prod is full.
    }
}

pub fn start_audio_ring() {
    stop_audio_ring();

    let (data_prod, mut data_cons) = HeapRb::<Vec<u8>>::new(AUDIO_QUEUE_CAPACITY).split();
    let (mut free_prod, free_cons) = HeapRb::<Vec<u8>>::new(AUDIO_QUEUE_CAPACITY).split();

    // Pre-allocate vector slots in the free queue for zero-allocation recycling on RT thread
    for _ in 0..AUDIO_QUEUE_CAPACITY {
        let _ = free_prod.try_push(Vec::with_capacity(DEFAULT_SLOT_CAPACITY));
    }

    let producer = AudioProducer {
        data_prod: UnsafeCell::new(data_prod),
        free_cons: UnsafeCell::new(free_cons),
    };

    AUDIO_PRODUCER.store(Some(Arc::new(producer)));

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();

    let worker = thread::spawn(move || {
        while !stop_clone.load(Ordering::Relaxed) {
            match data_cons.try_pop() {
                Some(chunk) => {
                    if chunk.is_empty() {
                        let _ = free_prod.try_push(chunk);
                        continue;
                    }
                    let cb = {
                        if let Ok(guard) = AUDIO_CALLBACK.lock() {
                            guard.clone()
                        } else {
                            None
                        }
                    };
                    if let Some(cb) = cb {
                        let _ = cb.call(
                            Ok(Buffer::from(chunk.as_slice())),
                            ThreadsafeFunctionCallMode::NonBlocking,
                        );
                    }
                    // Return pre-allocated vector back to free queue for RT thread recycling
                    let _ = free_prod.try_push(chunk);
                }
                None => {
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }
    });

    let Ok(mut guard) = AUDIO_RING_SESSION.lock() else {
        return;
    };
    *guard = Some(AudioRingSession {
        stop,
        worker: Some(worker),
    });
}

pub fn stop_audio_ring() {
    AUDIO_PRODUCER.store(None);

    let session = if let Ok(mut guard) = AUDIO_RING_SESSION.lock() {
        guard.take()
    } else {
        None
    };

    if let Some(mut session) = session {
        session.stop.store(true, Ordering::Relaxed);
        if let Some(join) = session.worker.take() {
            let _ = join.join();
        }
    }
}
