use pipewire::spa::buffer::DataType;
use pipewire::spa::param::ParamType;
use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
use pipewire::spa::pod::{Object, Pod, Value};
use pipewire::spa::utils::{Fraction, Rectangle, SpaTypes};
use pipewire::stream::{StreamFlags, StreamRc};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

struct VideoSession {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

static VIDEO_STATE: Mutex<Option<VideoSession>> = Mutex::new(None);

fn video_enum_format(width: u32, height: u32, fps_num: u32, fps_denom: u32) -> Option<Vec<u8>> {
    let mut info = VideoInfoRaw::new();
    info.set_format(VideoFormat::BGRA);
    info.set_size(Rectangle { width, height });
    info.set_framerate(Fraction {
        num: fps_num,
        denom: fps_denom,
    });

    let props = video_format_props(&info);
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: props,
    };

    let serialized = pipewire::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .ok()?;
    Some(serialized.0.into_inner())
}

fn video_format_props(info: &VideoInfoRaw) -> Vec<pipewire::spa::pod::Property> {
    use pipewire::spa::sys as sps;
    use pipewire::spa::{pod::Property, utils::Id};

    let mut props = Vec::with_capacity(5);
    props.push(Property::new(
        sps::SPA_FORMAT_mediaType,
        Value::Id(Id(sps::SPA_MEDIA_TYPE_video)),
    ));
    props.push(Property::new(
        sps::SPA_FORMAT_mediaSubtype,
        Value::Id(Id(sps::SPA_MEDIA_SUBTYPE_raw)),
    ));
    if info.format() != VideoFormat::Unknown {
        props.push(Property::new(
            sps::SPA_FORMAT_VIDEO_format,
            Value::Id(Id(info.format().as_raw())),
        ));
    }
    let size = info.size();
    if size.width != 0 && size.height != 0 {
        props.push(Property::new(
            sps::SPA_FORMAT_VIDEO_size,
            Value::Rectangle(size),
        ));
    }
    let framerate = info.framerate();
    if framerate.num != 0 && framerate.denom != 0 {
        props.push(Property::new(
            sps::SPA_FORMAT_VIDEO_framerate,
            Value::Fraction(framerate),
        ));
    }
    props
}

fn run_video_capture(
    node_id: u32,
    width: u32,
    height: u32,
    fps: u32,
    stop: Arc<AtomicBool>,
    ready_tx: mpsc::Sender<Result<(), String>>,
) {
    pipewire::init();
    let Ok(pw) = super::pw_init() else {
        let _ = ready_tx.send(Err("PipeWire init failed".into()));
        return;
    };

    let stream = match StreamRc::new(
        pw.core.clone(),
        &format!("slopcast-video-{node_id}"),
        pipewire::properties::properties! {
            "media.class" => "Stream/Input/Video",
            "node.name" => format!("Slopcast-Video-{node_id}"),
            "node.description" => "Slopcast Video Capture",
            "node.dont-move" => "true",
            "node.dont-reconnect" => "true",
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("Stream create: {e}")));
            return;
        }
    };

    let w = width as i32;
    let h = height as i32;

    let _listener = stream
        .add_local_listener_with_user_data(())
        .process(move |s, ()| {
            let Some(mut buffer) = s.dequeue_buffer() else {
                return;
            };
            for data in buffer.datas_mut() {
                if data.type_() != DataType::DmaBuf {
                    continue;
                }
                let fd = data.fd();
                if fd < 0 {
                    continue;
                }
                let pts_ns = i64::from(data.chunk().as_raw().offset);

                super::invoke_dmabuf_callback(fd, w, h, 0, pts_ns);
                break;
            }
        })
        .register()
        .ok();

    let values = match video_enum_format(width, height, fps, 1) {
        Some(v) => v,
        None => {
            let _ = ready_tx.send(Err("Failed to build video format".into()));
            return;
        }
    };
    let pod = match Pod::from_bytes(&values) {
        Some(p) => p,
        None => {
            let _ = ready_tx.send(Err("Failed to parse video pod".into()));
            return;
        }
    };
    let mut params = [pod];

    if let Err(e) = stream.connect(
        pipewire::spa::utils::Direction::Input,
        Some(node_id),
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut params,
    ) {
        let _ = ready_tx.send(Err(format!("Stream connect: {e}")));
        return;
    }

    let _ = ready_tx.send(Ok(()));

    while !stop.load(Ordering::SeqCst) {
        pw.main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(50)));
    }
}

pub(crate) fn start_video_capture(
    node_id: u32,
    width: u32,
    height: u32,
    fps: u32,
) -> super::NapiResult<bool> {
    let mut guard = VIDEO_STATE
        .lock()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    if guard.is_some() {
        return Err(napi::Error::from_reason("Video capture already active"));
    }

    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let s = stop.clone();

    let handle = thread::Builder::new()
        .name("pw-video-capture".into())
        .spawn(move || run_video_capture(node_id, width, height, fps, s, ready_tx))
        .map_err(|e| napi::Error::from_reason(format!("Thread spawn: {e}")))?;

    match ready_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
            return Err(napi::Error::from_reason(reason));
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
            return Err(napi::Error::from_reason(
                "Timed out waiting for video capture",
            ));
        }
    }

    *guard = Some(VideoSession {
        stop,
        join: Some(handle),
    });
    Ok(true)
}

pub(crate) fn stop_video_capture() -> super::NapiResult<bool> {
    let Ok(mut guard) = VIDEO_STATE.lock() else {
        return Ok(true);
    };
    let Some(mut session) = guard.take() else {
        return Ok(true);
    };
    session.stop.store(true, Ordering::SeqCst);
    if let Some(handle) = session.join.take() {
        let _ = handle.join();
    }
    Ok(true)
}
