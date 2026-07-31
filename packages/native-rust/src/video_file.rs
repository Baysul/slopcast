#[cfg(feature = "ffmpeg")]
pub(crate) mod ffmpeg {
    use ffmpeg_next::frame;
    use ffmpeg_next::media::Type;
    use ffmpeg_next::software::scaling::{Context as ScalerContext, Flags};
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

    static VIDEO_FRAME_CALLBACK: Mutex<Option<Arc<ThreadsafeFunction<Vec<u8>, ()>>>> =
        Mutex::new(None);
    static AUDIO_FRAME_CALLBACK: Mutex<Option<Arc<ThreadsafeFunction<Vec<u8>, ()>>>> =
        Mutex::new(None);

    pub(crate) fn set_video_frame_callback(
        callback: std::sync::Arc<ThreadsafeFunction<Vec<u8>, ()>>,
    ) -> napi::Result<()> {
        let Ok(mut guard) = VIDEO_FRAME_CALLBACK.lock() else {
            return Err(napi::Error::from_reason("Lock poisoned"));
        };
        *guard = Some(callback);
        Ok(())
    }

    pub(crate) fn set_audio_frame_callback(
        callback: std::sync::Arc<ThreadsafeFunction<Vec<u8>, ()>>,
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
        let _ = cb.call(Ok(rgba), ThreadsafeFunctionCallMode::NonBlocking);
    }

    fn invoke_audio_callback(pcm: Vec<u8>) {
        let Ok(guard) = AUDIO_FRAME_CALLBACK.lock() else {
            return;
        };
        let Some(ref cb) = *guard else {
            return;
        };
        let _ = cb.call(Ok(pcm), ThreadsafeFunctionCallMode::NonBlocking);
    }

    #[derive(Debug, Clone)]
    pub struct FileInfo {
        pub width: u32,
        pub height: u32,
        pub duration_ms: i64,
        pub has_audio: bool,
    }

    pub(crate) fn probe_file(path: &str) -> Result<FileInfo, String> {
        ffmpeg_next::init().map_err(|e| format!("ffmpeg init: {e}"))?;
        let ictx =
            ffmpeg_next::format::input(&path).map_err(|e| format!("Cannot open input: {e}"))?;

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
        let context =
            ffmpeg_next::codec::context::Context::from_parameters(video.parameters())
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

        let stop = Arc::new(AtomicBool::new(false));
        let seek_target = Arc::new(Mutex::new(None::<i64>));
        let paused = Arc::new(AtomicBool::new(false));

        let stop_clone = stop.clone();
        let seek_target_clone = seek_target.clone();
        let paused_clone = paused.clone();
        let path_owned = path.to_string();

        let join = thread::spawn(move || {
            let mut ictx = match ffmpeg_next::format::input(&path_owned) {
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

            let context = match ffmpeg_next::codec::context::Context::from_parameters(
                input.parameters(),
            ) {
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
                let audio_ctx = ffmpeg_next::codec::context::Context::from_parameters(
                    audio_input.parameters(),
                )
                .ok()?;
                audio_ctx.decoder().audio().ok()
            });

            let mut decoded = frame::Video::empty();
            let mut aframe = frame::Audio::empty();

            loop {
                if stop_clone.load(Ordering::Relaxed) {
                    break;
                }

                if paused_clone.load(Ordering::Relaxed) {
                    thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }

                {
                    let mut seek = seek_target_clone.lock().unwrap();
                    if let Some(ts_ms) = seek.take() {
                        let seek_ts = ts_ms * (ffmpeg_next::ffi::AV_TIME_BASE as i64) / 1000;
                        let _ = ictx.seek(seek_ts, seek_ts..);
                    }
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
                                    packed
                                        .extend_from_slice(&data[offset..offset + row_bytes]);
                                }

                                if !packed.is_empty() {
                                    invoke_video_callback(packed);
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
                                        let samples: usize =
                                            aframe.samples().try_into().unwrap_or(0);
                                        let channels: usize =
                                            aframe.channels().try_into().unwrap_or(2);
                                        let mut interleaved =
                                            Vec::with_capacity(samples * 2 * 2);

                                        if aframe.is_planar() {
                                            for i in 0..samples {
                                                for ch in 0..channels.min(2) {
                                                    let val = if ch < aframe.planes() {
                                                        let plane_data = aframe.data(ch);
                                                        let sample_bytes = &plane_data
                                                            [i * 4..i * 4 + 4];
                                                        f32::from_le_bytes(
                                                            sample_bytes
                                                                .try_into()
                                                                .unwrap_or_default(),
                                                        )
                                                    } else {
                                                        0.0f32
                                                    };
                                                    let s16 =
                                                        (val.clamp(-1.0, 1.0) * 32767.0)
                                                            .round() as i16;
                                                    interleaved
                                                        .extend_from_slice(&s16.to_le_bytes());
                                                }
                                            }
                                        } else {
                                            let data = aframe.data(0);
                                            let byte_size =
                                                samples * channels.min(2) * 4;
                                            let byte_size = byte_size.min(data.len());
                                            for chunk in
                                                data[..byte_size].chunks_exact(4)
                                            {
                                                let f = f32::from_le_bytes(
                                                    chunk.try_into().unwrap_or_default(),
                                                );
                                                let s16 = (f.clamp(-1.0, 1.0) * 32767.0)
                                                    .round() as i16;
                                                interleaved
                                                    .extend_from_slice(&s16.to_le_bytes());
                                            }
                                        }

                                        if !interleaved.is_empty() {
                                            invoke_audio_callback(interleaved);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

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
}

#[cfg(not(feature = "ffmpeg"))]
pub(crate) mod ffmpeg {
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
        _callback: Arc<ThreadsafeFunction<Vec<u8>, ()>>,
    ) -> napi::Result<()> {
        Err(napi::Error::from_reason(
            "FFmpeg is not available (build without ffmpeg feature)",
        ))
    }

    pub(crate) fn set_audio_frame_callback(
        _callback: Arc<ThreadsafeFunction<Vec<u8>, ()>>,
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
