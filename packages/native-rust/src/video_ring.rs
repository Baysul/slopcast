#![allow(dead_code)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

pub const NUM_SLOTS: usize = 4;
pub const SLOT_DATA_SIZE: usize = 1920 * 1080 * 4; // 8,294,400 bytes
pub const HEADER_SIZE: usize = 64;
pub const TOTAL_BUFFER_SIZE: usize = HEADER_SIZE + (NUM_SLOTS * SLOT_DATA_SIZE);

pub struct SharedVideoBuffer {
    data: Vec<u8>,
}

static SHARED_VIDEO_BUFFER: Mutex<Option<Arc<SharedVideoBuffer>>> = Mutex::new(None);

pub fn get_or_create_shared_buffer() -> Arc<SharedVideoBuffer> {
    let Ok(mut guard) = SHARED_VIDEO_BUFFER.lock() else {
        panic!("SHARED_VIDEO_BUFFER lock poisoned");
    };
    if let Some(ref buf) = *guard {
        buf.clone()
    } else {
        let buf = Arc::new(SharedVideoBuffer {
            data: vec![0u8; TOTAL_BUFFER_SIZE],
        });
        *guard = Some(buf.clone());
        buf
    }
}

impl SharedVideoBuffer {
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.data.as_ptr()
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.data.as_ptr().cast_mut()
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Acquires the next slot for writing, executes `fill_fn` with the mutable slot slice,
    /// and marks the slot as ready. Returns `(slot_index, slot_offset, data_len)`.
    pub fn write_frame<F>(
        &self,
        width: u32,
        height: u32,
        fill_fn: F,
    ) -> Option<(usize, usize, usize)>
    where
        F: FnOnce(&mut [u8]),
    {
        let data_len = (width * height * 4) as usize;
        if data_len > SLOT_DATA_SIZE {
            return None;
        }

        // Header atomic offsets
        let header_ptr = self.as_mut_ptr();
        // SAFETY: header_ptr is valid and at least HEADER_SIZE bytes
        let write_idx_atomic = unsafe { &*header_ptr.cast::<AtomicU32>() };

        let current_write = write_idx_atomic.fetch_add(1, Ordering::Relaxed);
        let slot_index = (current_write as usize) % NUM_SLOTS;

        let slot_offset = HEADER_SIZE + (slot_index * SLOT_DATA_SIZE);
        // SAFETY: slot_offset .. slot_offset + data_len is within TOTAL_BUFFER_SIZE bounds
        let slot_slice =
            unsafe { std::slice::from_raw_parts_mut(self.as_mut_ptr().add(slot_offset), data_len) };

        fill_fn(slot_slice);

        // Mark slot as ready (state = 2) at header offset 4 + (slot_index * 4)
        let slot_state_offset = 4 + (slot_index * 4);
        // SAFETY: slot_state_offset is within HEADER_SIZE bounds
        let slot_state_atomic = unsafe { &*header_ptr.add(slot_state_offset).cast::<AtomicU32>() };
        slot_state_atomic.store(2, Ordering::Release);

        Some((slot_index, slot_offset, data_len))
    }
}
