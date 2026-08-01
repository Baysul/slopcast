#![allow(dead_code)]

use crossbeam_channel::{Receiver, Sender, bounded};
use napi::bindgen_prelude::Buffer;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// Bounded lock-free ring queue for up to 64 audio frame chunks
const AUDIO_QUEUE_CAPACITY: usize = 64;

struct AudioRingSession {
    sender: Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

static AUDIO_RING_SESSION: Mutex<Option<AudioRingSession>> = Mutex::new(None);
static AUDIO_CALLBACK: Mutex<Option<Arc<ThreadsafeFunction<Buffer, ()>>>> = Mutex::new(None);

pub fn set_audio_data_callback(callback: Arc<ThreadsafeFunction<Buffer, ()>>) -> napi::Result<()> {
    let Ok(mut guard) = AUDIO_CALLBACK.lock() else {
        return Err(napi::Error::from_reason("Lock poisoned"));
    };
    *guard = Some(callback);
    Ok(())
}

/// RT-safe non-blocking lock-free push of PCM audio bytes into the global audio ring buffer.
/// Safe to call directly from PipeWire or WASAPI real-time process callbacks.
pub fn push_pcm_bytes(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let Ok(guard) = AUDIO_RING_SESSION.lock() else {
        return;
    };
    if let Some(ref session) = *guard {
        // Non-blocking lock-free try_send: drops frame if queue is full rather than blocking RT thread
        let _ = session.sender.try_send(bytes.to_vec());
    }
}

pub fn start_audio_ring() {
    stop_audio_ring();

    let (sender, receiver): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = bounded(AUDIO_QUEUE_CAPACITY);

    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();

    let worker = thread::spawn(move || {
        while !stop_clone.load(Ordering::Relaxed) {
            match receiver.recv_timeout(Duration::from_millis(10)) {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        continue;
                    }
                    if let Ok(guard) = AUDIO_CALLBACK.lock() {
                        if let Some(ref cb) = *guard {
                            let _ = cb.call(
                                Ok(Buffer::from(chunk)),
                                ThreadsafeFunctionCallMode::NonBlocking,
                            );
                        }
                    }
                }
                Err(_) => {
                    // Timeout, keep checking stop
                }
            }
        }
    });

    let Ok(mut guard) = AUDIO_RING_SESSION.lock() else {
        return;
    };
    *guard = Some(AudioRingSession {
        sender,
        stop,
        worker: Some(worker),
    });
}

pub fn stop_audio_ring() {
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
