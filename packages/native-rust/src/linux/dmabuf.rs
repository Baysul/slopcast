use arc_swap::ArcSwapOption;
use napi::threadsafe_function::ThreadsafeFunction;

// ── DMA-BUF Video Frame Callback ─────────────────────────────────────────
// Registered by the Electron main process to forward video frames from the
// PipeWire capture thread to native-livekit's VideoTrackSource. ArcSwapOption
// (not a Mutex) so the per-frame path takes no lock: frame rate is unbounded
// by the main process's ability to consume.
static DMA_BUF_CALLBACK: ArcSwapOption<ThreadsafeFunction<(i32, i32, i32, i32, i32, i32), ()>> =
    ArcSwapOption::const_empty();

pub(crate) fn set_dmabuf_callback(
    callback: std::sync::Arc<ThreadsafeFunction<(i32, i32, i32, i32, i32, i32), ()>>,
) -> napi::Result<()> {
    DMA_BUF_CALLBACK.store(Some(callback));
    Ok(())
}

pub(crate) fn invoke_dmabuf_callback(fd: i32, width: i32, height: i32, format: i32, pts_ns: i64) {
    let guard = DMA_BUF_CALLBACK.load();
    let Some(cb) = guard.as_ref() else {
        // Close duplicated descriptor if no callback is registered.
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
        // Close duplicated descriptor if thread-safe function dispatch fails.
        unsafe { libc::close(fd) };
    }
}

pub(crate) fn clear_dmabuf_callback() {
    DMA_BUF_CALLBACK.store(None);
}
