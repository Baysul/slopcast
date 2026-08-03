use arc_swap::ArcSwapOption;

use crate::DmaBufFrameCallback;

// Registered by the Electron main process to forward video frames from the
// PipeWire capture thread to native-livekit's VideoTrackSource. ArcSwapOption
// (not a Mutex) so the per-frame path takes no lock: frame rate is unbounded
// by the main process's ability to consume.
static DMA_BUF_CALLBACK: ArcSwapOption<DmaBufFrameCallback> = ArcSwapOption::const_empty();

pub(crate) fn set_dmabuf_callback(callback: std::sync::Arc<DmaBufFrameCallback>) {
    DMA_BUF_CALLBACK.store(Some(callback));
}

pub(crate) fn invoke_dmabuf_callback(fd: i32, width: i32, height: i32, format: i32, pts_ns: i64) {
    let guard = DMA_BUF_CALLBACK.load();
    let Some(cb) = guard.as_ref() else {
        // SAFETY: `fd` is a descriptor this function owns (duplicated from a
        // live PipeWire buffer); closing it here, and only here, prevents the
        // leak when no callback is registered to take ownership.
        unsafe { libc::close(fd) };
        return;
    };

    let lo = (pts_ns & 0xFFFF_FFFF) as i32;
    let hi = ((pts_ns >> 32) & 0xFFFF_FFFF) as i32;
    let status = cb.call(
        Ok((fd, width, height, format, lo, hi)),
        napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
    );
    if status != napi::Status::Ok {
        // SAFETY: same ownership as above — the dispatch failed to transfer
        // the descriptor to JS, so it must be closed here.
        unsafe { libc::close(fd) };
    }
}

pub(crate) fn clear_dmabuf_callback() {
    DMA_BUF_CALLBACK.store(None);
}
