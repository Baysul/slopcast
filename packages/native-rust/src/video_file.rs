#[cfg(feature = "ffmpeg")]
pub(crate) mod ffmpeg {
    use ffmpeg_next::frame;
    use ffmpeg_next::media::Type;
    use ffmpeg_next::software::scaling::{Context as ScalerContext, Flags};
    use napi::bindgen_prelude::Buffer;
    use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    struct HwAccelContext {
        device_ctx: *mut ffmpeg_next::ffi::AVBufferRef,
        pix_fmt: ffmpeg_next::ffi::AVPixelFormat,
    }

    impl Drop for HwAccelContext {
        fn drop(&mut self) {
            if !self.device_ctx.is_null() {
                // SAFETY: self.device_ctx is a valid AVBufferRef allocated by
                // av_hwdevice_ctx_create and owned by HwAccelContext.
                unsafe {
                    ffmpeg_next::ffi::av_buffer_unref(&mut self.device_ctx);
                }
            }
        }
    }

    // SAFETY: hw_get_format conforms to FFmpeg get_format callback signature
    unsafe extern "C" fn hw_get_format(
        ctx: *mut ffmpeg_next::ffi::AVCodecContext,
        pix_fmts: *const ffmpeg_next::ffi::AVPixelFormat,
    ) -> ffmpeg_next::ffi::AVPixelFormat {
        if ctx.is_null() || pix_fmts.is_null() {
            return ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NONE;
        }

        // SAFETY: (*ctx).opaque is set to a valid pointer to target_pix_fmt during decoder init
        let target_fmt = unsafe {
            if (*ctx).opaque.is_null() {
                ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NONE
            } else {
                *((*ctx).opaque.cast::<ffmpeg_next::ffi::AVPixelFormat>())
            }
        };

        let mut p = pix_fmts;
        // SAFETY: p points to a null-terminated array of AVPixelFormat ending with AV_PIX_FMT_NONE
        while unsafe { *p } != ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if unsafe { *p } == target_fmt {
                return target_fmt;
            }
            // SAFETY: advancing pointer in null-terminated AVPixelFormat array
            p = unsafe { p.add(1) };
        }

        // SAFETY: fallback to default software format offered by decoder
        unsafe { *pix_fmts }
    }

    struct VideoFileSession {
        stop: Arc<AtomicBool>,
        loop_enabled: Arc<AtomicBool>,
        join: Option<thread::JoinHandle<()>>,
        seek_target: Arc<Mutex<Option<i64>>>,
        paused: Arc<AtomicBool>,
    }

    static VIDEO_FILE_STATE: Mutex<Option<VideoFileSession>> = Mutex::new(None);

    static VIDEO_FRAME_CALLBACK: Mutex<Option<Arc<ThreadsafeFunction<Buffer, ()>>>> =
        Mutex::new(None);
    static AUDIO_FRAME_CALLBACK: Mutex<Option<Arc<ThreadsafeFunction<Buffer, ()>>>> =
        Mutex::new(None);

    pub(crate) fn set_video_frame_callback(
        callback: std::sync::Arc<ThreadsafeFunction<Buffer, ()>>,
    ) -> napi::Result<()> {
        let Ok(mut guard) = VIDEO_FRAME_CALLBACK.lock() else {
            return Err(napi::Error::from_reason("Lock poisoned"));
        };
        *guard = Some(callback);
        Ok(())
    }

    pub(crate) fn set_audio_frame_callback(
        callback: std::sync::Arc<ThreadsafeFunction<Buffer, ()>>,
    ) -> napi::Result<()> {
        let Ok(mut guard) = AUDIO_FRAME_CALLBACK.lock() else {
            return Err(napi::Error::from_reason("Lock poisoned"));
        };
        *guard = Some(callback);
        Ok(())
    }

    fn invoke_video_callback(rgba: Vec<u8>) {
        let Ok(guard) = VIDEO_FRAME_CALLBACK.lock() else {
            return;
        };
        let Some(ref cb) = *guard else {
            return;
        };
        let _ = cb.call(
            Ok(Buffer::from(rgba)),
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }

    fn invoke_audio_callback(pcm: Vec<u8>) {
        let Ok(guard) = AUDIO_FRAME_CALLBACK.lock() else {
            return;
        };
        let Some(ref cb) = *guard else {
            return;
        };
        let _ = cb.call(
            Ok(Buffer::from(pcm)),
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }

    #[derive(Debug, Clone)]
    pub struct FileInfo {
        pub width: u32,
        pub height: u32,
        pub duration_ms: i64,
        pub has_audio: bool,
    }

    fn percent_decode_str(s: &str) -> String {
        let mut bytes = Vec::with_capacity(s.len());
        let s_bytes = s.as_bytes();
        let mut i = 0;
        while i < s_bytes.len() {
            if s_bytes[i] == b'%' && i + 2 < s_bytes.len() {
                if let Ok(hex) = u8::from_str_radix(
                    std::str::from_utf8(&s_bytes[i + 1..i + 3]).unwrap_or(""),
                    16,
                ) {
                    bytes.push(hex);
                    i += 3;
                    continue;
                }
            }
            bytes.push(s_bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&bytes).to_string()
    }

    fn normalize_ffmpeg_path(path: &str) -> String {
        let mut target = path;
        if let Some(stripped) = target.strip_prefix("file://") {
            target = stripped;
            if let Some(no_host) = target.strip_prefix("localhost") {
                target = no_host;
            } else if let Some(no_host) = target.strip_prefix("127.0.0.1") {
                target = no_host;
            }
        } else if let Some(stripped) = target.strip_prefix("file:") {
            target = stripped;
        }

        let unencoded = percent_decode_str(target);
        let p = std::path::Path::new(&unencoded);
        if p.exists() {
            if unencoded.len() >= 2 && unencoded.as_bytes()[1] == b':' {
                let drive = unencoded.chars().next().unwrap_or_default();
                if drive.is_ascii_alphabetic() {
                    let normalized_slashes = unencoded.replace('\\', "/");
                    return format!("file:{normalized_slashes}");
                }
            }
            return unencoded;
        }

        if unencoded.len() >= 2 && unencoded.as_bytes()[1] == b':' {
            let drive = unencoded.chars().next().unwrap_or_default();
            if drive.is_ascii_alphabetic() {
                let normalized_slashes = unencoded.replace('\\', "/");
                return format!("file:{normalized_slashes}");
            }
        }

        unencoded
    }

    struct CustomIo {
        file: std::fs::File,
    }

    pub(crate) struct CustomInputContext {
        input: Option<ffmpeg_next::format::context::Input>,
        avio_ctx: *mut ffmpeg_next::ffi::AVIOContext,
        opaque: *mut CustomIo,
    }

    impl Drop for CustomInputContext {
        fn drop(&mut self) {
            self.input.take();
            // SAFETY: `avio_ctx` and `opaque` are owned by this struct: avio_ctx
            // came from avio_alloc_context (and is intentionally not freed by
            // avformat_close_input because AVFMT_FLAG_CUSTOM_IO is set) and
            // `opaque` from Box::into_raw. Each is freed exactly once here.
            unsafe {
                if !self.avio_ctx.is_null() {
                    ffmpeg_next::ffi::avio_context_free(&mut self.avio_ctx);
                }
                if !self.opaque.is_null() {
                    let _ = Box::from_raw(self.opaque);
                    self.opaque = std::ptr::null_mut();
                }
            }
        }
    }

    impl std::ops::Deref for CustomInputContext {
        type Target = ffmpeg_next::format::context::Input;
        fn deref(&self) -> &Self::Target {
            match self.input.as_ref() {
                Some(i) => i,
                None => unreachable!("CustomInputContext input is initialized on creation"),
            }
        }
    }

    impl std::ops::DerefMut for CustomInputContext {
        fn deref_mut(&mut self) -> &mut Self::Target {
            match self.input.as_mut() {
                Some(i) => i,
                None => unreachable!("CustomInputContext input is initialized on creation"),
            }
        }
    }

    fn open_ffmpeg_input_custom_io(path: &str) -> Result<CustomInputContext, String> {
        use ffmpeg_next::ffi::*;
        use std::ffi::CString;
        use std::io::{Read, Seek, SeekFrom};
        use std::ptr;

        let normalized = normalize_ffmpeg_path(path);
        let file = std::fs::File::open(&normalized)
            .map_err(|e| format!("std::fs::File::open({normalized}) failed: {e}"))?;

        let custom_io = Box::new(CustomIo { file });
        let opaque = Box::into_raw(custom_io);

        // SAFETY: opaque is a valid raw pointer to CustomIo, and buf is a valid writable buffer allocated by FFmpeg
        unsafe extern "C" fn read_cb(
            opaque: *mut std::ffi::c_void,
            buf: *mut u8,
            buf_size: i32,
        ) -> i32 {
            if opaque.is_null() || buf.is_null() || buf_size <= 0 {
                return -1;
            }
            // SAFETY: opaque was created from Box::into_raw and is valid for the duration of CustomIo
            let custom_io = unsafe { &mut *(opaque as *mut CustomIo) };
            // SAFETY: buf is allocated by FFmpeg with at least buf_size bytes
            let slice = unsafe { std::slice::from_raw_parts_mut(buf, buf_size as usize) };
            match custom_io.file.read(slice) {
                Ok(0) => AVERROR_EOF,
                Ok(n) => n as i32,
                Err(_) => -1,
            }
        }

        // SAFETY: opaque is a valid raw pointer to CustomIo
        unsafe extern "C" fn seek_cb(
            opaque: *mut std::ffi::c_void,
            offset: i64,
            whence: i32,
        ) -> i64 {
            if opaque.is_null() {
                return -1;
            }
            // SAFETY: opaque was created from Box::into_raw and is valid for the duration of CustomIo
            let custom_io = unsafe { &mut *(opaque as *mut CustomIo) };

            // AVSEEK_SIZE check
            if whence & AVSEEK_SIZE != 0 {
                if let Ok(meta) = custom_io.file.metadata() {
                    return meta.len() as i64;
                }
                return -1;
            }

            let seek_from = match whence & 0xff {
                0 => SeekFrom::Start(offset as u64), // SEEK_SET
                1 => SeekFrom::Current(offset),      // SEEK_CUR
                2 => SeekFrom::End(offset),          // SEEK_END
                _ => return -1,
            };

            match custom_io.file.seek(seek_from) {
                Ok(pos) => pos as i64,
                Err(_) => -1,
            }
        }

        let buffer_size = 32768;
        // SAFETY: av_malloc returns either a valid writable buffer of buffer_size
        // bytes or null (checked immediately below).
        let buffer = unsafe { av_malloc(buffer_size) as *mut u8 };
        if buffer.is_null() {
            // SAFETY: this is the unique owner of the opaque allocation and it is
            // reclaimed exactly once here.
            unsafe {
                let _ = Box::from_raw(opaque);
            }
            return Err("Failed to allocate AVIO buffer".to_string());
        }

        // SAFETY: `buffer`/`opaque` are valid and owned by the returned AVIOContext;
        // the read/seek callbacks are safe for the reasons documented on their own
        // SAFETY comments and live as long as the context.
        let avio_ctx = unsafe {
            avio_alloc_context(
                buffer,
                buffer_size as i32,
                0, // read-only
                opaque as *mut std::ffi::c_void,
                Some(read_cb),
                None, // write
                Some(seek_cb),
            )
        };

        if avio_ctx.is_null() {
            // SAFETY: avio_alloc_context failed, so it never took ownership;
            // `buffer` is freed and `opaque` reclaimed exactly once each.
            unsafe {
                av_free(buffer as *mut _);
                let _ = Box::from_raw(opaque);
            }
            return Err("Failed to allocate AVIOContext".to_string());
        }

        let mut ctx = CustomInputContext {
            input: None,
            avio_ctx,
            opaque,
        };

        // SAFETY: avformat_alloc_context returns a null pointer on failure (checked
        // below) and an owned context otherwise, which is wrapped/freed on all paths.
        let mut ps = unsafe { avformat_alloc_context() };
        if ps.is_null() {
            return Err("Failed to allocate AVFormatContext".to_string());
        }

        // SAFETY: `ps` is valid and owned by ctx; assigning our custom avio_ctx and
        // flagging CUSTOM_IO makes close_input skip freeing pb, keeping the pointer
        // valid until ctx's Drop runs.
        unsafe {
            (*ps).pb = avio_ctx;
            // AVFMT_FLAG_CUSTOM_IO so FFmpeg knows pb was custom allocated
            (*ps).flags |= AVFMT_FLAG_CUSTOM_IO;
        }

        let c_path = CString::new(normalized.clone()).unwrap_or_else(|_| c"custom_io".to_owned());

        // SAFETY: `ps` is a valid in/out pointer to an owned AVFormatContext and
        // `c_path` is a valid NUL-terminated C string with a static probe options arg.
        let res = unsafe {
            avformat_open_input(&mut ps, c_path.as_ptr(), ptr::null_mut(), ptr::null_mut())
        };

        if res < 0 {
            // SAFETY: on failure avformat_open_input leaves `ps` unmodified and owned
            // by us; it is freed exactly once here (pb/opaque freed by ctx's Drop).
            unsafe {
                avformat_free_context(ps);
            }
            return Err(format!("avformat_open_input failed: code {res}"));
        }

        // SAFETY: `ps` is now owned by the open format context and is a valid pointer
        // for stream-info probing.
        let res_stream = unsafe { avformat_find_stream_info(ps, ptr::null_mut()) };
        if res_stream < 0 {
            // SAFETY: with AVFMT_FLAG_CUSTOM_IO close_input frees the format context
            // but keeps pb alive; pb and opaque are freed by ctx's Drop below.
            unsafe {
                avformat_close_input(&mut ps);
            }
            return Err(format!(
                "avformat_find_stream_info failed: code {res_stream}"
            ));
        }

        // SAFETY: `ps` is a valid, fully-opened AVFormatContext; wrap takes sole
        // ownership and frees it (via avformat_close_input) when the Input drops.
        ctx.input = Some(unsafe { ffmpeg_next::format::context::Input::wrap(ps) });
        Ok(ctx)
    }

    pub(crate) fn probe_file(path: &str) -> Result<FileInfo, String> {
        ffmpeg_next::init().map_err(|e| format!("ffmpeg init: {e}"))?;
        let _ = ffmpeg_next::format::network::init();
        let mut ictx =
            open_ffmpeg_input_custom_io(path).map_err(|e| format!("Cannot open input: {e}"))?;

        let mut video_index = None;
        let mut audio_index = None;
        let mut vstream_duration = 0;
        let mut vstream_time_base = ffmpeg_next::Rational(0, 1);

        for stream in ictx.streams() {
            match stream.parameters().medium() {
                Type::Video if video_index.is_none() => {
                    video_index = Some(stream.index());
                    vstream_duration = stream.duration();
                    vstream_time_base = stream.time_base();
                }
                Type::Audio if audio_index.is_none() => {
                    audio_index = Some(stream.index());
                }
                _ => {}
            }
        }

        let video_index = video_index.ok_or_else(|| "No video stream found".to_string())?;
        let stream = ictx
            .stream(video_index)
            .ok_or_else(|| "Video stream not found".to_string())?;
        let mut width = unsafe { (*stream.parameters().as_ptr()).width as u32 };
        let mut height = unsafe { (*stream.parameters().as_ptr()).height as u32 };

        // If width/height is 0 from container metadata (e.g. WebM VP8/VP9/AV1),
        // try decoding packets until the first video frame to obtain actual dimensions.
        if width == 0 || height == 0 {
            if let Ok(context) =
                ffmpeg_next::codec::context::Context::from_parameters(stream.parameters())
            {
                if let Ok(mut vid) = context.decoder().video() {
                    let mut decoded = frame::Video::empty();
                    let mut packet_count = 0;
                    for (st, packet) in ictx.packets() {
                        packet_count += 1;
                        if packet_count > 200 {
                            break;
                        }
                        if st.index() == video_index {
                            if vid.send_packet(&packet).is_ok() {
                                if vid.receive_frame(&mut decoded).is_ok() {
                                    width = decoded.width();
                                    height = decoded.height();
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        let max_dim = width.max(height);
        if max_dim > 1920 {
            let scale = 1920.0 / max_dim as f32;
            width = ((width as f32 * scale) as u32) & !1;
            height = ((height as f32 * scale) as u32) & !1;
        }

        let mut duration_ms = ictx.duration() as i64 * 1000 / ffmpeg_next::ffi::AV_TIME_BASE as i64;
        if duration_ms <= 0 && vstream_duration > 0 && vstream_time_base.denominator() > 0 {
            duration_ms = (vstream_duration as f64
                * (vstream_time_base.numerator() as f64 / vstream_time_base.denominator() as f64)
                * 1000.0) as i64;
        }
        if duration_ms < 0 {
            duration_ms = 0;
        }

        let has_audio = audio_index.is_some();

        Ok(FileInfo {
            width,
            height,
            duration_ms,
            has_audio,
        })
    }

    pub(crate) fn start_playback(path: &str, loop_enabled: bool) -> Result<(), String> {
        stop_playback();

        ffmpeg_next::init().map_err(|e| format!("ffmpeg init: {e}"))?;
        let _ = ffmpeg_next::format::network::init();

        let stop = Arc::new(AtomicBool::new(false));
        let loop_flag = Arc::new(AtomicBool::new(loop_enabled));
        let seek_target = Arc::new(Mutex::new(None::<i64>));
        let paused = Arc::new(AtomicBool::new(false));

        let stop_clone = stop.clone();
        let loop_clone = loop_flag.clone();
        let seek_target_clone = seek_target.clone();
        let paused_clone = paused.clone();
        let path_owned = path.to_string();

        let join = thread::spawn(move || {
            let mut ictx = match open_ffmpeg_input_custom_io(&path_owned) {
                Ok(ctx) => ctx,
                Err(e) => {
                    eprintln!("Cannot open input: {e}");
                    invoke_video_callback(Vec::new());
                    invoke_audio_callback(Vec::new());
                    return;
                }
            };

            let input = match ictx.streams().best(Type::Video) {
                Some(s) => s,
                None => {
                    eprintln!("No video stream found");
                    invoke_video_callback(Vec::new());
                    invoke_audio_callback(Vec::new());
                    return;
                }
            };
            let video_stream_index = input.index();
            let audio_stream_index = ictx.streams().best(Type::Audio).map(|s| s.index());

            let mut fps = input.avg_frame_rate();
            if fps.denominator() == 0 || fps.numerator() == 0 {
                fps = input.rate();
            }
            let frame_delay = if fps.denominator() > 0 && fps.numerator() > 0 {
                let secs = fps.denominator() as f64 / fps.numerator() as f64;
                std::time::Duration::from_secs_f64(secs.clamp(0.016, 0.2))
            } else {
                std::time::Duration::from_millis(33)
            };

            let mut context =
                match ffmpeg_next::codec::context::Context::from_parameters(input.parameters()) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Video decoder params: {e}");
                        invoke_video_callback(Vec::new());
                        invoke_audio_callback(Vec::new());
                        return;
                    }
                };

            let mut hw_accel: Option<HwAccelContext> = None;
            let mut target_pix_fmt = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NONE;

            // SAFETY: context.as_ptr() returns a valid AVCodecContext pointer
            let codec_ptr = unsafe { (*context.as_ptr()).codec };
            if !codec_ptr.is_null() {
                let mut i = 0;
                loop {
                    // SAFETY: query hardware configs supported for the video codec
                    let config = unsafe { ffmpeg_next::ffi::avcodec_get_hw_config(codec_ptr, i) };
                    if config.is_null() {
                        break;
                    }
                    // SAFETY: reading fields of valid AVCodecHWConfig pointer
                    let methods = unsafe { (*config).methods };
                    if (methods
                        & (ffmpeg_next::ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32))
                        != 0
                    {
                        // SAFETY: reading device_type and pix_fmt from valid AVCodecHWConfig pointer
                        let dev_type = unsafe { (*config).device_type };
                        let pix_fmt = unsafe { (*config).pix_fmt };
                        let mut buf_ref: *mut ffmpeg_next::ffi::AVBufferRef = std::ptr::null_mut();
                        // SAFETY: attempt to create hardware device context for candidate dev_type
                        let ret = unsafe {
                            ffmpeg_next::ffi::av_hwdevice_ctx_create(
                                &mut buf_ref,
                                dev_type,
                                std::ptr::null(),
                                std::ptr::null_mut(),
                                0,
                            )
                        };
                        if ret == 0 && !buf_ref.is_null() {
                            // SAFETY: retrieve human-readable device type name for log
                            let type_name_ptr =
                                unsafe { ffmpeg_next::ffi::av_hwdevice_get_type_name(dev_type) };
                            let type_name = if type_name_ptr.is_null() {
                                "unknown"
                            } else {
                                // SAFETY: convert C string to str
                                unsafe {
                                    std::ffi::CStr::from_ptr(type_name_ptr)
                                        .to_str()
                                        .unwrap_or("unknown")
                                }
                            };
                            eprintln!(
                                "[Native Rust FFmpeg] HWAccel enabled using {type_name} device context"
                            );
                            hw_accel = Some(HwAccelContext {
                                device_ctx: buf_ref,
                                pix_fmt,
                            });
                            target_pix_fmt = pix_fmt;
                            break;
                        }
                    }
                    i += 1;
                }
            }

            if let Some(ref hw) = hw_accel {
                // SAFETY: context.as_mut_ptr() returns a valid mutable AVCodecContext pointer
                let codec_ctx_mut = unsafe { context.as_mut_ptr() };
                // SAFETY: attach hw_device_ctx and get_format callback to AVCodecContext prior to opening decoder
                unsafe {
                    (*codec_ctx_mut).hw_device_ctx = ffmpeg_next::ffi::av_buffer_ref(hw.device_ctx);
                    (*codec_ctx_mut).opaque = (&target_pix_fmt
                        as *const ffmpeg_next::ffi::AVPixelFormat)
                        .cast_mut()
                        .cast();
                    (*codec_ctx_mut).get_format = Some(hw_get_format);
                }
            }

            let mut decoder = match context.decoder().video() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Video decoder open: {e}");
                    invoke_video_callback(Vec::new());
                    invoke_audio_callback(Vec::new());
                    return;
                }
            };

            let mut scaler: Option<ScalerContext> = None;
            let mut scaler_info: Option<(ffmpeg_next::format::Pixel, u32, u32, u32, u32)> = None;

            let mut audio_decoder = audio_stream_index.and_then(|_| {
                let audio_input = ictx.streams().best(Type::Audio)?;
                let audio_ctx =
                    ffmpeg_next::codec::context::Context::from_parameters(audio_input.parameters())
                        .ok()?;
                audio_ctx.decoder().audio().ok()
            });

            let mut audio_resampler: Option<ffmpeg_next::software::resampling::Context> = None;
            let mut audio_resampler_info: Option<(ffmpeg_next::format::Sample, u32, i32)> = None;

            let (video_tx, video_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
            let video_stop = stop_clone.clone();
            let video_emitter_join = thread::spawn(move || {
                let mut next_target = std::time::Instant::now() + frame_delay;
                while let Ok(packed) = video_rx.recv() {
                    if video_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    invoke_video_callback(packed);
                    let now = std::time::Instant::now();
                    if next_target > now {
                        thread::sleep(next_target - now);
                        next_target += frame_delay;
                    } else {
                        next_target = std::time::Instant::now() + frame_delay;
                    }
                }
            });

            let mut decoded = frame::Video::empty();
            let mut aframe = frame::Audio::empty();

            let mut playback_start = std::time::Instant::now();
            let mut video_frames_sent: u64 = 0;

            loop {
                if stop_clone.load(Ordering::Relaxed) {
                    break;
                }

                let mut did_seek = false;
                {
                    let mut seek = match seek_target_clone.lock() {
                        Ok(guard) => guard,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    if let Some(ts_ms) = seek.take() {
                        let seek_ts = ts_ms * (ffmpeg_next::ffi::AV_TIME_BASE as i64) / 1000;
                        let _ = ictx.seek(seek_ts, ..);
                        decoder.flush();
                        if let Some(ref mut ad) = audio_decoder {
                            ad.flush();
                        }
                        audio_resampler = None;
                        audio_resampler_info = None;
                        let valid_ts = ts_ms.max(0) as f64;
                        let fps_val = fps.numerator() as f64 / (fps.denominator() as f64).max(1.0);
                        video_frames_sent = ((valid_ts / 1000.0) * fps_val) as u64;
                        playback_start = std::time::Instant::now()
                            - std::time::Duration::from_millis(valid_ts as u64);
                        did_seek = true;
                    }
                }

                if paused_clone.load(Ordering::Relaxed) && !did_seek {
                    thread::sleep(std::time::Duration::from_millis(5));
                    let fps_val = fps.numerator() as f64 / (fps.denominator() as f64).max(1.0);
                    let fps_val = if fps_val <= 0.0 { 30.0 } else { fps_val };
                    let cur_stream_sec = video_frames_sent as f64 / fps_val;
                    playback_start = std::time::Instant::now()
                        - std::time::Duration::from_secs_f64(cur_stream_sec);
                    continue;
                }

                let packet_result = ictx.packets().next();
                match packet_result {
                    None => {
                        if loop_clone.load(Ordering::Relaxed) {
                            if ictx.seek(0, ..).is_err() {
                                if let Ok(new_ctx) = open_ffmpeg_input_custom_io(&path_owned) {
                                    ictx = new_ctx;
                                } else {
                                    break;
                                }
                            }
                            decoder.flush();
                            if let Some(ref mut ad) = audio_decoder {
                                ad.flush();
                            }
                            audio_resampler = None;
                            audio_resampler_info = None;
                            video_frames_sent = 0;
                            playback_start = std::time::Instant::now();
                            continue;
                        } else {
                            break;
                        }
                    }
                    Some((stream, packet)) => {
                        if stream.index() == video_stream_index {
                            if decoder.send_packet(&packet).is_err() {
                                continue;
                            }
                            loop {
                                match decoder.receive_frame(&mut decoded) {
                                    Ok(()) => {}
                                    Err(ffmpeg_next::Error::Other {
                                        errno: ffmpeg_next::error::EAGAIN,
                                    }) => break,
                                    Err(_) => break,
                                }

                                // SAFETY: decoded.as_ptr() returns a valid AVFrame pointer
                                let is_hw_frame = hw_accel.as_ref().is_some_and(|hw| unsafe {
                                    (*decoded.as_ptr()).format == hw.pix_fmt as std::os::raw::c_int
                                });

                                let mut sw_frame = frame::Video::empty();
                                let src_frame = if is_hw_frame {
                                    // SAFETY: transfer decoded frame from GPU VRAM to system RAM
                                    let transfer_res = unsafe {
                                        ffmpeg_next::ffi::av_hwframe_transfer_data(
                                            sw_frame.as_mut_ptr(),
                                            decoded.as_ptr(),
                                            0,
                                        )
                                    };
                                    if transfer_res == 0 {
                                        &sw_frame
                                    } else {
                                        &decoded
                                    }
                                } else {
                                    &decoded
                                };

                                let cur_fmt = src_frame.format();
                                let cur_w = src_frame.width();
                                let cur_h = src_frame.height();
                                if cur_w == 0 || cur_h == 0 {
                                    continue;
                                }

                                let max_dim = cur_w.max(cur_h);
                                let (out_w, out_h) = if max_dim > 1920 {
                                    let scale = 1920.0 / max_dim as f32;
                                    (
                                        ((cur_w as f32 * scale) as u32) & !1,
                                        ((cur_h as f32 * scale) as u32) & !1,
                                    )
                                } else {
                                    (cur_w, cur_h)
                                };

                                if scaler.is_none()
                                    || scaler_info != Some((cur_fmt, cur_w, cur_h, out_w, out_h))
                                {
                                    match ScalerContext::get(
                                        cur_fmt,
                                        cur_w,
                                        cur_h,
                                        ffmpeg_next::format::Pixel::RGBA,
                                        out_w,
                                        out_h,
                                        Flags::FAST_BILINEAR,
                                    ) {
                                        Ok(s) => {
                                            scaler = Some(s);
                                            scaler_info =
                                                Some((cur_fmt, cur_w, cur_h, out_w, out_h));
                                        }
                                        Err(e) => {
                                            eprintln!("Scaler init error: {e}");
                                            continue;
                                        }
                                    }
                                }

                                let Some(ref mut s) = scaler else {
                                    continue;
                                };

                                let mut rgb_frame = frame::Video::empty();
                                if s.run(src_frame, &mut rgb_frame).is_err() {
                                    continue;
                                };

                                let data = rgb_frame.data(0);
                                let linesize = rgb_frame.stride(0);
                                let row_bytes = (out_w * 4) as usize;
                                let total_bytes = row_bytes * out_h as usize;
                                let mut packed = Vec::with_capacity(total_bytes);

                                if linesize == row_bytes && total_bytes <= data.len() {
                                    packed.extend_from_slice(&data[..total_bytes]);
                                } else {
                                    for row in 0..out_h as usize {
                                        let offset = row * linesize;
                                        if offset + row_bytes <= data.len() {
                                            packed.extend_from_slice(
                                                &data[offset..offset + row_bytes],
                                            );
                                        }
                                    }
                                }

                                if !packed.is_empty() {
                                    video_frames_sent += 1;
                                    let shared_buf =
                                        crate::video_ring::get_or_create_shared_buffer();
                                    if let Some((slot_idx, _, _)) =
                                        shared_buf.write_frame(out_w, out_h, |slot| {
                                            let copy_len = packed.len().min(slot.len());
                                            slot[..copy_len].copy_from_slice(&packed[..copy_len]);
                                        })
                                    {
                                        crate::invoke_shared_video_callback(
                                            crate::SharedFrameMetadata {
                                                slot_index: slot_idx as u32,
                                                width: out_w,
                                                height: out_h,
                                                timestamp_us: 0,
                                            },
                                        );
                                    } else if video_tx.try_send(packed).is_err() {
                                        if stop_clone.load(Ordering::Relaxed) {
                                            break;
                                        }
                                    }
                                }
                            }
                        } else if Some(stream.index()) == audio_stream_index {
                            if let Some(ref mut ad) = audio_decoder {
                                if ad.send_packet(&packet).is_err() {
                                    continue;
                                }

                                loop {
                                    match ad.receive_frame(&mut aframe) {
                                        Ok(()) => {}
                                        Err(ffmpeg_next::Error::Other {
                                            errno: ffmpeg_next::error::EAGAIN,
                                        }) => break,
                                        Err(_) => break,
                                    }

                                    if aframe.samples() > 0 {
                                        let cur_fmt = aframe.format();
                                        let cur_rate = aframe.rate();
                                        let cur_channels = aframe.channels() as i32;

                                        if audio_resampler.is_none()
                                            || audio_resampler_info
                                                != Some((cur_fmt, cur_rate, cur_channels))
                                        {
                                            let in_layout = if aframe.channel_layout().is_empty() {
                                                ffmpeg_next::channel_layout::ChannelLayout::default(
                                                    cur_channels,
                                                )
                                            } else {
                                                aframe.channel_layout()
                                            };

                                            match ffmpeg_next::software::resampling::Context::get(
                                                cur_fmt,
                                                in_layout,
                                                cur_rate,
                                                ffmpeg_next::format::Sample::I16(
                                                    ffmpeg_next::format::sample::Type::Packed,
                                                ),
                                                ffmpeg_next::channel_layout::ChannelLayout::STEREO,
                                                48000,
                                            ) {
                                                Ok(r) => {
                                                    audio_resampler = Some(r);
                                                    audio_resampler_info =
                                                        Some((cur_fmt, cur_rate, cur_channels));
                                                }
                                                Err(e) => {
                                                    eprintln!("Audio resampler init error: {e}");
                                                    continue;
                                                }
                                            }
                                        }

                                        if let Some(ref mut resampler) = audio_resampler {
                                            let mut resampled = frame::Audio::empty();
                                            if resampler.run(&aframe, &mut resampled).is_ok() {
                                                if resampled.samples() > 0 {
                                                    let data = resampled.data(0);
                                                    let bytes_needed =
                                                        (resampled.samples() * 4) as usize;
                                                    let byte_len = bytes_needed.min(data.len());
                                                    if byte_len > 0 {
                                                        invoke_audio_callback(
                                                            data[..byte_len].to_vec(),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Real-time pacing: ensure playback stays within 50ms of wall-clock time
                        let fps_val = fps.numerator() as f64 / (fps.denominator() as f64).max(1.0);
                        let fps_val = if fps_val <= 0.0 { 30.0 } else { fps_val };
                        let stream_sec = video_frames_sent as f64 / fps_val;
                        let elapsed_sec = playback_start.elapsed().as_secs_f64();
                        if stream_sec > elapsed_sec + 0.05 {
                            let ahead_ms =
                                (((stream_sec - elapsed_sec - 0.05) * 1000.0) as u64).min(50);
                            if ahead_ms > 0 {
                                std::thread::sleep(std::time::Duration::from_millis(ahead_ms));
                            }
                        }
                    }
                }
            }

            drop(video_tx);
            let _ = video_emitter_join.join();

            invoke_video_callback(Vec::new());
            invoke_audio_callback(Vec::new());
        });

        let session = VideoFileSession {
            stop,
            loop_enabled: loop_flag,
            join: Some(join),
            seek_target,
            paused,
        };

        let Ok(mut guard) = VIDEO_FILE_STATE.lock() else {
            return Err("Lock poisoned".to_string());
        };
        *guard = Some(session);

        Ok(())
    }

    pub(crate) fn stop_playback() {
        let session = if let Ok(mut guard) = VIDEO_FILE_STATE.lock() {
            guard.take()
        } else {
            None
        };

        if let Some(mut session) = session {
            session.stop.store(true, Ordering::Relaxed);
            if let Some(join) = session.join.take() {
                let _ = join.join();
            }
        }
    }

    pub(crate) fn seek_playback(ts_ms: i64) {
        if let Ok(guard) = VIDEO_FILE_STATE.lock() {
            if let Some(ref session) = *guard {
                let Ok(mut target) = session.seek_target.lock() else {
                    return;
                };
                *target = Some(ts_ms);
            }
        }
    }

    pub(crate) fn set_playback_paused(paused: bool) {
        if let Ok(guard) = VIDEO_FILE_STATE.lock() {
            if let Some(ref session) = *guard {
                session.paused.store(paused, Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn set_playback_loop(loop_enabled: bool) {
        if let Ok(guard) = VIDEO_FILE_STATE.lock() {
            if let Some(ref session) = *guard {
                session.loop_enabled.store(loop_enabled, Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn is_playback_active() -> bool {
        if let Ok(guard) = VIDEO_FILE_STATE.lock() {
            guard.is_some()
        } else {
            false
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_probe_real_file_variants() {
            let paths = [
                "/tmp/test_sample.mp4",
                "file:/tmp/test_sample.mp4",
                "file:///tmp/test_sample.mp4",
                "file://localhost/tmp/test_sample.mp4",
            ];
            for path in paths {
                let res = probe_file(path);
                println!("probe_file({path}) -> {:?}", res);
                assert!(res.is_ok(), "probe_file({path}) failed: {:?}", res);
            }
        }

        #[test]
        fn test_dragon_mp4() {
            let path = "/home/basil/Downloads/dragon.mp4";
            if std::path::Path::new(path).exists() {
                let info = probe_file(path).unwrap();
                println!("dragon.mp4 info: {:?}", info);
                let start_res = start_playback(path, true);
                println!("start_playback res: {:?}", start_res);
                assert!(start_res.is_ok());
                std::thread::sleep(std::time::Duration::from_secs(2));
                stop_playback();
                println!("dragon.mp4 played 2s successfully!");
            }
        }
    }
}

#[cfg(not(feature = "ffmpeg"))]
pub(crate) mod ffmpeg {
    use napi::bindgen_prelude::Buffer;
    use napi::threadsafe_function::ThreadsafeFunction;
    use std::sync::Arc;

    #[derive(Debug, Clone)]
    pub struct FileInfo {
        pub width: u32,
        pub height: u32,
        pub duration_ms: i64,
        pub has_audio: bool,
    }

    pub(crate) fn set_video_frame_callback(
        _callback: Arc<ThreadsafeFunction<Buffer, ()>>,
    ) -> napi::Result<()> {
        Err(napi::Error::from_reason(
            "FFmpeg is not available (build without ffmpeg feature)",
        ))
    }

    pub(crate) fn set_audio_frame_callback(
        _callback: Arc<ThreadsafeFunction<Buffer, ()>>,
    ) -> napi::Result<()> {
        Err(napi::Error::from_reason(
            "FFmpeg is not available (build without ffmpeg feature)",
        ))
    }

    pub(crate) fn probe_file(_path: &str) -> Result<FileInfo, String> {
        Err("FFmpeg is not available (build without ffmpeg feature)".to_string())
    }

    pub(crate) fn start_playback(_path: &str, _loop_enabled: bool) -> Result<(), String> {
        Err("FFmpeg is not available (build without ffmpeg feature)".to_string())
    }

    pub(crate) fn stop_playback() {}

    pub(crate) fn seek_playback(_ts_ms: i64) {}

    pub(crate) fn set_playback_paused(_paused: bool) {}

    pub(crate) fn set_playback_loop(_loop_enabled: bool) {}

    pub(crate) fn is_playback_active() -> bool {
        false
    }
}
