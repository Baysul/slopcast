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

    struct VideoFileSession {
        stop: Arc<AtomicBool>,
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

    fn open_ffmpeg_input_custom_io(
        path: &str,
    ) -> Result<ffmpeg_next::format::context::Input, String> {
        use ffmpeg_next::ffi::*;
        use std::ffi::CString;
        use std::io::{Read, Seek, SeekFrom};
        use std::ptr;

        let normalized = normalize_ffmpeg_path(path);
        let file = std::fs::File::open(&normalized)
            .map_err(|e| format!("std::fs::File::open({normalized}) failed: {e}"))?;

        struct CustomIo {
            file: std::fs::File,
        }

        let custom_io = Box::new(CustomIo { file });
        let opaque = Box::into_raw(custom_io) as *mut std::ffi::c_void;

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
        let buffer = unsafe { av_malloc(buffer_size) as *mut u8 };
        if buffer.is_null() {
            unsafe {
                let _ = Box::from_raw(opaque as *mut CustomIo);
            }
            return Err("Failed to allocate AVIO buffer".to_string());
        }

        let avio_ctx = unsafe {
            avio_alloc_context(
                buffer,
                buffer_size as i32,
                0, // read-only
                opaque,
                Some(read_cb),
                None, // write
                Some(seek_cb),
            )
        };

        if avio_ctx.is_null() {
            unsafe {
                av_free(buffer as *mut _);
                let _ = Box::from_raw(opaque as *mut CustomIo);
            }
            return Err("Failed to allocate AVIOContext".to_string());
        }

        let mut ps = unsafe { avformat_alloc_context() };
        if ps.is_null() {
            unsafe {
                avio_context_free(&mut (avio_ctx as *mut _));
                let _ = Box::from_raw(opaque as *mut CustomIo);
            }
            return Err("Failed to allocate AVFormatContext".to_string());
        }

        unsafe {
            (*ps).pb = avio_ctx;
            // AVFMT_FLAG_CUSTOM_IO so FFmpeg knows pb was custom allocated
            (*ps).flags |= AVFMT_FLAG_CUSTOM_IO;
        }

        let c_path =
            CString::new(normalized.clone()).unwrap_or_else(|_| CString::new("custom_io").unwrap());

        let res = unsafe {
            avformat_open_input(&mut ps, c_path.as_ptr(), ptr::null_mut(), ptr::null_mut())
        };

        if res < 0 {
            unsafe {
                avformat_free_context(ps);
                avio_context_free(&mut (avio_ctx as *mut _));
                let _ = Box::from_raw(opaque as *mut CustomIo);
            }
            return Err(format!("avformat_open_input failed: code {res}"));
        }

        let res_stream = unsafe { avformat_find_stream_info(ps, ptr::null_mut()) };
        if res_stream < 0 {
            unsafe {
                avformat_close_input(&mut ps);
                let _ = Box::from_raw(opaque as *mut CustomIo);
            }
            return Err(format!(
                "avformat_find_stream_info failed: code {res_stream}"
            ));
        }

        Ok(unsafe { ffmpeg_next::format::context::Input::wrap(ps) })
    }

    pub(crate) fn probe_file(path: &str) -> Result<FileInfo, String> {
        ffmpeg_next::init().map_err(|e| format!("ffmpeg init: {e}"))?;
        let _ = ffmpeg_next::format::network::init();
        let ictx =
            open_ffmpeg_input_custom_io(path).map_err(|e| format!("Cannot open input: {e}"))?;

        let mut video_stream = None;
        let mut audio_stream = None;

        for stream in ictx.streams() {
            match stream.parameters().medium() {
                Type::Video if video_stream.is_none() => video_stream = Some(stream),
                Type::Audio if audio_stream.is_none() => audio_stream = Some(stream),
                _ => {}
            }
        }

        let video = video_stream.ok_or_else(|| "No video stream found".to_string())?;
        let context = ffmpeg_next::codec::context::Context::from_parameters(video.parameters())
            .map_err(|e| format!("Decoder params: {e}"))?;
        let decoder = context.decoder();
        let vid = decoder
            .video()
            .map_err(|e| format!("Video decoder open: {e}"))?;

        let width = vid.width();
        let height = vid.height();

        let duration_ms = ictx.duration() as i64 * 1000 / ffmpeg_next::ffi::AV_TIME_BASE as i64;

        let has_audio = audio_stream.is_some();

        Ok(FileInfo {
            width,
            height,
            duration_ms,
            has_audio,
        })
    }

    pub(crate) fn start_playback(path: &str) -> Result<(), String> {
        stop_playback();

        ffmpeg_next::init().map_err(|e| format!("ffmpeg init: {e}"))?;
        let _ = ffmpeg_next::format::network::init();

        let stop = Arc::new(AtomicBool::new(false));
        let seek_target = Arc::new(Mutex::new(None::<i64>));
        let paused = Arc::new(AtomicBool::new(false));

        let stop_clone = stop.clone();
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

            let fps = input.avg_frame_rate();
            let frame_delay = if fps.denominator() > 0 && fps.numerator() > 0 {
                let secs = fps.denominator() as f64 / fps.numerator() as f64;
                std::time::Duration::from_secs_f64(secs.clamp(0.016, 0.2))
            } else {
                std::time::Duration::from_millis(33)
            };

            let context =
                match ffmpeg_next::codec::context::Context::from_parameters(input.parameters()) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("Video decoder params: {e}");
                        invoke_video_callback(Vec::new());
                        invoke_audio_callback(Vec::new());
                        return;
                    }
                };
            let mut decoder = match context.decoder().video() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Video decoder open: {e}");
                    invoke_video_callback(Vec::new());
                    invoke_audio_callback(Vec::new());
                    return;
                }
            };

            let src_width = decoder.width();
            let src_height = decoder.height();

            let mut scaler = match ScalerContext::get(
                decoder.format(),
                src_width,
                src_height,
                ffmpeg_next::format::Pixel::RGBA,
                src_width,
                src_height,
                Flags::BILINEAR,
            ) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Scaler init: {e}");
                    invoke_video_callback(Vec::new());
                    invoke_audio_callback(Vec::new());
                    return;
                }
            };

            let mut audio_decoder = audio_stream_index.and_then(|_| {
                let audio_input = ictx.streams().best(Type::Audio)?;
                let audio_ctx =
                    ffmpeg_next::codec::context::Context::from_parameters(audio_input.parameters())
                        .ok()?;
                audio_ctx.decoder().audio().ok()
            });

            let mut audio_resampler = audio_decoder.as_ref().and_then(|dec| {
                ffmpeg_next::software::resampling::Context::get(
                    dec.format(),
                    dec.channel_layout(),
                    dec.rate(),
                    ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed),
                    ffmpeg_next::channel_layout::ChannelLayout::STEREO,
                    48000,
                )
                .ok()
            });

            let (video_tx, video_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
            let video_stop = stop_clone.clone();
            let video_emitter_join = thread::spawn(move || {
                while let Ok(packed) = video_rx.recv() {
                    if video_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let frame_start = std::time::Instant::now();
                    invoke_video_callback(packed);
                    let elapsed = frame_start.elapsed();
                    if let Some(remaining) = frame_delay.checked_sub(elapsed) {
                        thread::sleep(remaining);
                    }
                }
            });

            let mut decoded = frame::Video::empty();
            let mut aframe = frame::Audio::empty();

            loop {
                if stop_clone.load(Ordering::Relaxed) {
                    break;
                }

                let mut did_seek = false;
                {
                    let mut seek = seek_target_clone.lock().unwrap();
                    if let Some(ts_ms) = seek.take() {
                        let seek_ts = ts_ms * (ffmpeg_next::ffi::AV_TIME_BASE as i64) / 1000;
                        let _ = ictx.seek(seek_ts, seek_ts..);
                        did_seek = true;
                    }
                }

                if paused_clone.load(Ordering::Relaxed) && !did_seek {
                    thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }

                let packet_result = ictx.packets().next();
                match packet_result {
                    None => break,
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

                                let mut rgb_frame = frame::Video::empty();
                                if scaler.run(&decoded, &mut rgb_frame).is_err() {
                                    continue;
                                }

                                let data = rgb_frame.data(0);
                                let linesize = rgb_frame.stride(0);
                                let row_bytes = (src_width * 4) as usize;
                                let mut packed =
                                    Vec::with_capacity(row_bytes * src_height as usize);
                                for row in 0..src_height as usize {
                                    let offset = row * linesize;
                                    packed.extend_from_slice(&data[offset..offset + row_bytes]);
                                }

                                if !packed.is_empty() {
                                    if video_tx.send(packed).is_err() {
                                        break;
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
        if let Ok(mut guard) = VIDEO_FILE_STATE.lock() {
            if let Some(session) = guard.take() {
                session.stop.store(true, Ordering::Relaxed);
                if let Some(join) = session.join {
                    let _ = join.join();
                }
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

    pub(crate) fn start_playback(_path: &str) -> Result<(), String> {
        Err("FFmpeg is not available (build without ffmpeg feature)".to_string())
    }

    pub(crate) fn stop_playback() {}

    pub(crate) fn seek_playback(_ts_ms: i64) {}

    pub(crate) fn set_playback_paused(_paused: bool) {}

    pub(crate) fn is_playback_active() -> bool {
        false
    }
}
