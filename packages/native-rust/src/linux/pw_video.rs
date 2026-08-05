//! `pw_stream` screen-capture engine: the revived pre-migration `video.rs`
//! engine reworked around libwebrtc m144's `shared_screencast_stream.cc`
//! behavior — modifier-aware `EnumFormat` negotiation gated on client+server
//! `PipeWire` >= 0.3.33, the libwebrtc dequeue discipline (dequeue all, keep
//! the last), and DMA-BUF-only buffer processing with EGL readback into
//! linear BGRA frames.

use crate::linux::egl::{DRM_FORMAT_MOD_INVALID, DmabufPlane, EglDmaBuf};
use pipewire::properties::properties;
use pipewire::spa::buffer::{ChunkFlags, DataType};
use pipewire::spa::param::ParamType;
use pipewire::spa::param::video::{VideoFormat, VideoInfoRaw};
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{ChoiceValue, Object, Pod, Property, PropertyFlags, Value};
use pipewire::spa::utils::{
    Choice, ChoiceEnum, ChoiceFlags, Direction, Fraction, Id, Rectangle, SpaTypes,
};
use pipewire::stream::{StreamFlags, StreamRc, StreamState};
use std::cell::RefCell;
use std::os::fd::{FromRawFd, OwnedFd};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

/// Frame callback contract: `(width, height, bgra, pts_us)`.
///
/// `bgra` is a linear (row-major, row stride = `width*4`) BGRA frame of
/// `width*height*4` bytes; RGBA/RGBx sources are already channel-swapped to
/// BGRA before the callback fires (port of `ConvertRGBxToBGRx`). `pts_us` is
/// a monotonic microsecond timestamp. The callback runs on the capture
/// thread, synchronously inside the `PipeWire` process callback.
pub type VideoFrameCallback = Box<dyn FnMut(u32, u32, &[u8], i64) + Send>;

/// Modifiers are only offered when both ends are at least this version.
const DMA_BUF_MODIFIER_MIN_VERSION: PipeWireVersion = PipeWireVersion {
    major: 0,
    minor: 3,
    micro: 33,
};

/// libwebrtc's `PipeWireVersion`: `(major, minor, micro)` with the
/// "all-zero means unknown" rule — an unknown version never satisfies a `>=`
/// check, so a version-less library/server never enables the modifier path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PipeWireVersion {
    major: u32,
    minor: u32,
    micro: u32,
}

impl PipeWireVersion {
    /// `PipeWireVersion::Parse`: exactly three dot-separated numeric
    /// components; anything else parses as the all-zero "unknown" version.
    fn parse(version: &str) -> Self {
        let mut parts = version.split('.');
        let mut parsed = [0u32; 3];
        for slot in &mut parsed {
            let Some(part) = parts.next() else {
                return Self {
                    major: 0,
                    minor: 0,
                    micro: 0,
                };
            };
            let Ok(number) = part.parse::<u32>() else {
                return Self {
                    major: 0,
                    minor: 0,
                    micro: 0,
                };
            };
            *slot = number;
        }
        if parts.next().is_some() {
            return Self {
                major: 0,
                minor: 0,
                micro: 0,
            };
        }
        Self {
            major: parsed[0],
            minor: parsed[1],
            micro: parsed[2],
        }
    }
}

impl PartialOrd for PipeWireVersion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        // libwebrtc's `operator>=` only guards the left side: an all-zero
        // ("unknown") version reports no ordering, which makes every
        // comparison against it false.
        if (self.major, self.minor, self.micro) == (0, 0, 0) {
            return None;
        }
        Some((self.major, self.minor, self.micro).cmp(&(other.major, other.minor, other.micro)))
    }
}

/// Monotonic microsecond timestamp for frame delivery (the old engine's
/// `monotonic_pts_ns` scaled to µs; `CLOCK_MONOTONIC` never jumps).
fn monotonic_pts_us() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a fully initialized stack `timespec` that outlives the
    // call; CLOCK_MONOTONIC is always supported on Linux.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut ts);
    }
    ts.tv_sec * 1_000_000 + ts.tv_nsec / 1_000
}

/// libwebrtc's `ConvertRGBxToBGRx`: swap bytes 0 and 2 of every 4-byte pixel
/// in place. The scratch buffer is a linear RGBA/RGBx frame with no row
/// padding, so one flat pass equals the .cc's per-row loop.
fn convert_rgbx_to_bgrx(frame: &mut [u8]) {
    for pixel in frame.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

/// Builds one `EnumFormat` param pod (port of libwebrtc's `BuildFormat`):
/// mediaType/mediaSubtype/format ids, an optional modifier property, size
/// (plain rectangle, or a 1x1..`UINT32_MAX` choice range when unknown), and
/// the framerate fraction.
///
/// A single `DRM_FORMAT_MOD_INVALID` modifier becomes a mandatory plain Long
/// ("any modifier works"); anything else becomes a mandatory, non-fixated
/// `SPA_CHOICE_Enum` whose first value is the default — the first modifier
/// listed twice — followed by the rest.
#[allow(
    clippy::cast_possible_wrap,
    reason = "SPA fraction fields are i32 and DRM modifiers are 56-bit, so u32 fps and u64 modifiers always fit; libwebrtc makes the same casts"
)]
fn build_format(
    format: VideoFormat,
    modifiers: Option<&[u64]>,
    width: u32,
    height: u32,
    fps: u32,
) -> Option<Vec<u8>> {
    use pipewire::spa::sys as sps;

    let mut props = Vec::with_capacity(6);
    props.push(Property::new(
        sps::SPA_FORMAT_mediaType,
        Value::Id(Id(sps::SPA_MEDIA_TYPE_video)),
    ));
    props.push(Property::new(
        sps::SPA_FORMAT_mediaSubtype,
        Value::Id(Id(sps::SPA_MEDIA_SUBTYPE_raw)),
    ));
    props.push(Property::new(
        sps::SPA_FORMAT_VIDEO_format,
        Value::Id(Id(format.as_raw())),
    ));

    if let Some(mods) = modifiers {
        if mods.len() == 1 && mods[0] == DRM_FORMAT_MOD_INVALID {
            props.push(Property {
                key: sps::SPA_FORMAT_VIDEO_modifier,
                flags: PropertyFlags::from_bits_retain(sps::SPA_POD_PROP_FLAG_MANDATORY),
                value: Value::Long(mods[0] as i64),
            });
        } else {
            props.push(Property {
                key: sps::SPA_FORMAT_VIDEO_modifier,
                flags: PropertyFlags::from_bits_retain(
                    sps::SPA_POD_PROP_FLAG_MANDATORY | sps::SPA_POD_PROP_FLAG_DONT_FIXATE,
                ),
                value: Value::Choice(ChoiceValue::Long(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Enum {
                        default: mods[0] as i64,
                        alternatives: mods.iter().map(|m| *m as i64).collect(),
                    },
                ))),
            });
        }
    }

    let size = if width != 0 && height != 0 {
        Value::Rectangle(Rectangle { width, height })
    } else {
        // Unknown size: offer the whole range, exactly like libwebrtc's
        // `pw_min_screen_bounds` (1x1) and `pw_max_screen_bounds`
        // (UINT32_MAX x UINT32_MAX).
        Value::Choice(ChoiceValue::Rectangle(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range {
                default: Rectangle {
                    width: 1,
                    height: 1,
                },
                min: Rectangle {
                    width: 1,
                    height: 1,
                },
                max: Rectangle {
                    width: u32::MAX,
                    height: u32::MAX,
                },
            },
        )))
    };
    props.push(Property::new(sps::SPA_FORMAT_VIDEO_size, size));
    props.push(Property::new(
        sps::SPA_FORMAT_VIDEO_framerate,
        Value::Fraction(Fraction { num: fps, denom: 1 }),
    ));

    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: props,
    };
    let serialized =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj)).ok()?;
    Some(serialized.0.into_inner())
}

/// Builds the `EnumFormat` param list: for each of the four raw formats, a
/// modifier-aware variant when both `PipeWire` ends are >= 0.3.33 and the EGL
/// stack reports modifiers, always followed by the modifier-less fallback
/// (libwebrtc's renegotiation loop).
fn enum_format_params(
    egl: &EglDmaBuf,
    use_modifiers: bool,
    width: u32,
    height: u32,
    fps: u32,
) -> Vec<Vec<u8>> {
    let mut params = Vec::with_capacity(8);
    for format in [
        VideoFormat::BGRA,
        VideoFormat::RGBA,
        VideoFormat::BGRx,
        VideoFormat::RGBx,
    ] {
        if use_modifiers {
            let modifiers = egl.query_dma_buf_modifiers(format);
            if !modifiers.is_empty()
                && let Some(pod) = build_format(format, Some(&modifiers), width, height, fps)
            {
                params.push(pod);
            }
        }
        if let Some(pod) = build_format(format, None, width, height, fps) {
            params.push(pod);
        }
    }
    params
}

/// Client library version from `pw_get_library_version()`; unknown when the
/// string does not parse, which disables the modifier path.
fn client_pipewire_version() -> PipeWireVersion {
    let version = unsafe {
        // SAFETY: `pw_get_library_version` returns a non-null pointer to a
        // static, NUL-terminated version string owned by libpipewire;
        // `CStr::from_ptr` only reads up to the terminator.
        let ptr = pipewire::sys::pw_get_library_version();
        std::ffi::CStr::from_ptr(ptr)
    };
    PipeWireVersion::parse(version.to_str().unwrap_or("0.0.0"))
}

/// Server version captured via the core `info` event after a `sync` round
/// trip (libwebrtc's `OnCoreInfo` + `pw_core_sync` wait). Returns the
/// all-zero "unknown" version when the round trip never completes.
fn server_pipewire_version(
    core: &pipewire::core::CoreRc,
    main_loop: &pipewire::main_loop::MainLoopRc,
) -> PipeWireVersion {
    let version = Rc::new(RefCell::new(PipeWireVersion {
        major: 0,
        minor: 0,
        micro: 0,
    }));
    let version_clone = version.clone();
    let done = Rc::new(RefCell::new(false));
    let done_clone = done.clone();
    let Some(pending) = core.sync(0).ok() else {
        return PipeWireVersion {
            major: 0,
            minor: 0,
            micro: 0,
        };
    };
    let main_loop_weak = main_loop.downgrade();
    let _listener = core
        .add_listener_local()
        .info(move |info| {
            *version_clone.borrow_mut() = PipeWireVersion::parse(info.version());
        })
        .done(move |id, seq| {
            if id == pipewire::core::PW_ID_CORE && seq == pending {
                *done_clone.borrow_mut() = true;
                if let Some(ml) = main_loop_weak.upgrade() {
                    ml.quit();
                }
            }
        })
        .register();
    for _ in 0..100 {
        if *done.borrow() {
            break;
        }
        main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(50)));
    }
    *version.borrow()
}

/// libwebrtc's `OnStreamParamChanged` buffer reply: a `SPA_PARAM_Buffers`
/// object sized to the negotiated frame, with a buffer-count choice range and
/// the `dataType` flag mask (`DmaBuf` + `MemFd` when the format carries a
/// modifier, `MemFd` only otherwise — the exact libwebrtc `buffer_types`).
/// Without this reply the node keeps the stream configured but never
/// produces buffers.
fn buffers_param(width: u32, height: u32, has_modifier: bool) -> Option<Vec<u8>> {
    use pipewire::spa::sys as sps;

    // libwebrtc: stride = SPA_ROUND_UP_N(width * 4, 4) — BGRA rows are
    // already 4-aligned, so the plain product is the same value.
    let stride = width.checked_mul(4)?;
    let size = height.checked_mul(stride)?;
    let data_types = if has_modifier {
        (1 << sps::SPA_DATA_DmaBuf) | (1 << sps::SPA_DATA_MemFd)
    } else {
        1 << sps::SPA_DATA_MemFd
    };
    let obj = Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![
            Property::new(
                sps::SPA_PARAM_BUFFERS_size,
                Value::Int(i32::try_from(size).ok()?),
            ),
            Property::new(
                sps::SPA_PARAM_BUFFERS_stride,
                Value::Int(i32::try_from(stride).ok()?),
            ),
            Property::new(
                sps::SPA_PARAM_BUFFERS_buffers,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range {
                        default: 8,
                        min: 1,
                        max: 32,
                    },
                ))),
            ),
            Property::new(
                sps::SPA_PARAM_BUFFERS_dataType,
                Value::Choice(ChoiceValue::Int(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Flags {
                        default: data_types,
                        flags: Vec::new(),
                    },
                ))),
            ),
        ],
    };
    let serialized =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj)).ok()?;
    Some(serialized.0.into_inner())
}

/// Everything the stream callbacks touch. All of it lives on the capture
/// thread: the listener borrows it mutably per callback, so no locks are
/// needed between `param_changed` (writer) and `process` (reader).
struct CaptureUserData {
    egl: EglDmaBuf,
    on_frame: VideoFrameCallback,
    /// Negotiated `(width, height, format)` — the authoritative frame size,
    /// written by `param_changed` on first format negotiation.
    negotiated: (u32, u32, VideoFormat),
    /// Negotiated DMA-BUF modifier (`DRM_FORMAT_MOD_INVALID` when the format
    /// carries none).
    modifier: u64,
    /// Readback target, resized to `width * height * 4` on size changes.
    scratch: Vec<u8>,
    ready: Option<mpsc::Sender<Result<(), String>>>,
    /// Last `read_dmabuf` error string, so a persistent failure logs once
    /// instead of spamming the console every frame.
    last_error: Option<String>,
}

/// libwebrtc's `OnStreamProcess` + `ProcessBuffer`: dequeue all buffers, keep
/// the last, hand the earlier ones back immediately; then read the kept
/// buffer's first data if it is a DMA-BUF. `Buffer`'s `Drop` re-queues, so
/// dropping earlier buffers (or the kept one at the end) returns them.
fn process_buffer(stream: &pipewire::stream::Stream, user_data: &mut CaptureUserData) {
    let mut buffers = Vec::new();
    while let Some(buffer) = stream.dequeue_buffer() {
        buffers.push(buffer);
    }
    let Some(mut buffer) = buffers.pop() else {
        return;
    };
    // The earlier buffers drop here; each `Buffer::drop` re-queues itself.

    let datas = buffer.datas_mut();
    let Some(first) = datas.first() else {
        return;
    };
    let chunk = first.chunk();
    if chunk.size() == 0 || chunk.flags().contains(ChunkFlags::CORRUPTED) {
        return;
    }
    if first.type_() != DataType::DmaBuf {
        // MemFd buffers (the modifier-less fallback path) are not supported:
        // the engine reads DMA-BUFs only, and `MAP_BUFFERS` is never set.
        return;
    }

    // Duplicate every plane's fd and keep the duplicates alive for the whole
    // read, so EGL can still access them after PipeWire reuses the buffer.
    let mut owned_fds = Vec::with_capacity(datas.len());
    let mut planes = Vec::with_capacity(datas.len());
    for data in datas {
        // SAFETY: `data.fd()` is a valid open descriptor from a dequeued
        // PipeWire buffer; F_DUPFD_CLOEXEC returns a new valid descriptor or
        // -1, which is checked below.
        let dup_fd = unsafe { libc::fcntl(data.fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if dup_fd < 0 {
            return;
        }
        // SAFETY: `dup_fd` was just returned by fcntl(F_DUPFD_CLOEXEC) and is
        // the sole owner of the duplicated descriptor.
        owned_fds.push(unsafe { OwnedFd::from_raw_fd(dup_fd) });
        planes.push(DmabufPlane {
            fd: dup_fd,
            offset: data.chunk().offset(),
            stride: data.chunk().stride(),
        });
    }
    // `owned_fds` stays alive through the readback; `planes` holds raw fd
    // numbers, so it does not borrow from it.

    let (width, height, format) = user_data.negotiated;
    let needed = width as usize * height as usize * 4;
    if user_data.scratch.len() != needed {
        user_data.scratch.resize(needed, 0);
    }
    if let Err(e) = user_data.egl.read_dmabuf(
        width,
        height,
        format,
        &planes,
        user_data.modifier,
        &mut user_data.scratch,
    ) {
        if user_data.last_error.as_deref() != Some(e.as_str()) {
            eprintln!("[pw-video] DMA-BUF readback failed: {e}");
            user_data.last_error = Some(e);
        }
        return;
    }
    if format == VideoFormat::RGBA || format == VideoFormat::RGBx {
        convert_rgbx_to_bgrx(&mut user_data.scratch);
    }
    let pts_us = monotonic_pts_us();
    (user_data.on_frame)(width, height, &user_data.scratch, pts_us);
    // `buffer` drops here and is re-queued to the stream.
}

#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    reason = "one cohesive PipeWire stream setup sequence; the ready-channel error handling would scatter if split, and stop/ready_tx are consumed by a thread::spawn(move) closure that must own them"
)]
fn run_video_capture(
    portal_fd: Option<OwnedFd>,
    node_id: u32,
    width: u32,
    height: u32,
    fps: u32,
    stop: Arc<AtomicBool>,
    on_frame: VideoFrameCallback,
    ready_tx: mpsc::Sender<Result<(), String>>,
) {
    pipewire::init();

    let main_loop = match pipewire::main_loop::MainLoopRc::new(None) {
        Ok(main_loop) => main_loop,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("MainLoop: {e}")));
            return;
        }
    };
    let context = match pipewire::context::ContextRc::new(&main_loop, None) {
        Ok(context) => context,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("Context: {e}")));
            return;
        }
    };
    // The fd-scoped remote (only the screencast node is visible) when the
    // portal handed us a capture fd; a full session connection otherwise.
    let core = match portal_fd {
        Some(fd) => match context.connect_fd_rc(fd, None) {
            Ok(core) => core,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("Connect (portal fd): {e}")));
                return;
            }
        },
        None => match context.connect_rc(None) {
            Ok(core) => core,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("Connect: {e}")));
                return;
            }
        },
    };

    let egl = match EglDmaBuf::new() {
        Ok(egl) => egl,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("EGL init: {e}")));
            return;
        }
    };

    let client_version = client_pipewire_version();
    let server_version = server_pipewire_version(&core, &main_loop);
    let use_modifiers = client_version >= DMA_BUF_MODIFIER_MIN_VERSION
        && server_version >= DMA_BUF_MODIFIER_MIN_VERSION;
    let param_bytes = enum_format_params(&egl, use_modifiers, width, height, fps);
    let mut params: Vec<&Pod> = Vec::with_capacity(param_bytes.len());
    for bytes in &param_bytes {
        let Some(pod) = Pod::from_bytes(bytes) else {
            let _ = ready_tx.send(Err("Failed to parse format pod".into()));
            return;
        };
        params.push(pod);
    }

    let stream = match StreamRc::new(
        core.clone(),
        &format!("slopcast-video-{node_id}"),
        properties! {
            "media.class" => "Stream/Input/Video",
            "node.name" => format!("Slopcast-Video-{node_id}"),
            "node.description" => "Slopcast Video Capture",
            "node.dont-move" => "true",
            "node.dont-reconnect" => "true",
        },
    ) {
        Ok(stream) => stream,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("Stream create: {e}")));
            return;
        }
    };

    let user_data = CaptureUserData {
        egl,
        on_frame,
        negotiated: (width, height, VideoFormat::BGRA),
        modifier: DRM_FORMAT_MOD_INVALID,
        scratch: Vec::with_capacity(width as usize * height as usize * 4),
        ready: Some(ready_tx.clone()),
        last_error: None,
    };

    let _listener = match stream
        .add_local_listener_with_user_data(user_data)
        .state_changed(|_stream, user_data, _old, state| match state {
            StreamState::Streaming | StreamState::Paused => {
                if let Some(tx) = user_data.ready.take() {
                    let _ = tx.send(Ok(()));
                }
            }
            StreamState::Unconnected | StreamState::Connecting => {}
            StreamState::Error(e) => {
                if let Some(tx) = user_data.ready.take() {
                    let _ = tx.send(Err(format!("Stream state error: {e}")));
                }
            }
        })
        .param_changed(|stream, user_data, id, param| {
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Some(pod) = param else {
                return;
            };
            let mut info = VideoInfoRaw::new();
            if info.parse(pod).is_err() {
                return;
            }
            let size = info.size();
            if size.width > 0 && size.height > 0 {
                user_data.negotiated = (size.width, size.height, info.format());
            }
            // The modifier is only meaningful when the negotiated format
            // carries the property (libwebrtc's `spa_pod_find_prop` guard):
            // a format without it must import with no modifier attributes.
            let has_modifier = pod.as_object().is_ok_and(|obj| {
                obj.find_prop(Id(pipewire::spa::sys::SPA_FORMAT_VIDEO_modifier))
                    .is_some()
            });
            user_data.modifier = if has_modifier {
                info.modifier()
            } else {
                DRM_FORMAT_MOD_INVALID
            };
            // Reply with the Buffers param so the node starts producing
            // (libwebrtc's `OnStreamParamChanged`); without it the stream
            // stays configured but idle.
            if size.width > 0
                && size.height > 0
                && let Some(bytes) = buffers_param(size.width, size.height, has_modifier)
                && let Some(pod) = Pod::from_bytes(&bytes)
            {
                let mut params = [pod];
                if let Err(e) = stream.update_params(&mut params) {
                    eprintln!("[pw-video] buffer param reply failed: {e}");
                }
            }
        })
        .process(process_buffer)
        .register()
    {
        Ok(listener) => listener,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("Stream listener: {e}")));
            return;
        }
    };

    if let Err(e) = stream.connect(
        Direction::Input,
        Some(node_id),
        // MAP_BUFFERS is forbidden: the EGL path reads DMA-BUFs directly.
        StreamFlags::AUTOCONNECT,
        &mut params,
    ) {
        let _ = ready_tx.send(Err(format!("Stream connect: {e}")));
        return;
    }

    while !stop.load(Ordering::Relaxed) {
        main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(50)));
    }
}

static VIDEO_CAPTURE_STATE: Mutex<Option<PwVideoCapture>> = Mutex::new(None);

/// `pw_stream` screen-capture engine (Phase B).
///
/// The whole engine — `PipeWire` main loop, EGL readback, frame callback —
/// runs on a dedicated `"desktop-capture"` thread. Only one capture may be
/// active at a time; the singleton slot is released by [`PwVideoCapture::stop`].
pub struct PwVideoCapture {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl PwVideoCapture {
    /// Spawns the `"desktop-capture"` thread and runs the full engine:
    ///
    /// 1. `pipewire::init()` on the thread;
    /// 2. main loop + context; when `portal_fd` is `Some`, connect with
    ///    `ContextRc::connect_fd_rc` (fd-scoped remote, only the screencast
    ///    node visible); otherwise `connect_rc` (fallback);
    /// 3. `EglDmaBuf::new()` on the same thread (Phase C readback);
    /// 4. client version via `pw_get_library_version()`, server version via
    ///    the core info listener after a `core.sync` round-trip;
    /// 5. `EnumFormat` params for `{BGRA, RGBA, BGRx, RGBx}` — with modifiers
    ///    when client+server >= 0.3.33 and `query_dma_buf_modifiers()` is
    ///    non-empty, always followed by a modifier-less variant;
    /// 6. `stream.connect(Direction::Input, Some(node_id), AUTOCONNECT, ...)`
    ///    — never `MAP_BUFFERS`;
    /// 7. main loop with a 50 ms timeout until `stop`.
    ///
    /// Blocks until the stream reaches `Streaming`/`Paused` (returns `Ok`)
    /// or fails (returns `Err`); on failure the thread is joined.
    ///
    /// `fps` is the requested framerate as `SPA_FRACTION(fps, 1)`; pass `0`
    /// for "no fixed rate". `KWin` screencast nodes offer a fixed ``0/1``
    /// framerate and `spa_pod_filter` requires fixed-vs-fixed equality, so a
    /// non-zero `fps` fails the format intersection on KDE — the compositor
    /// streams at its own refresh rate (`maxFramerate`) either way.
    ///
    /// # Errors
    ///
    /// Returns a human-readable reason when init/connect/negotiation fails,
    /// the stream does not reach a running state within 5 s, or a capture is
    /// already active.
    pub fn start(
        portal_fd: Option<OwnedFd>,
        node_id: u32,
        width: u32,
        height: u32,
        fps: u32,
        on_frame: VideoFrameCallback,
    ) -> Result<Self, String> {
        {
            let guard = VIDEO_CAPTURE_STATE
                .lock()
                .map_err(|e| format!("Video capture state lock poisoned: {e}"))?;
            if guard.is_some() {
                return Err("Video capture already active".into());
            }
        }

        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let thread_stop = stop.clone();
        let handle = thread::Builder::new()
            .name("desktop-capture".into())
            .spawn(move || {
                run_video_capture(
                    portal_fd,
                    node_id,
                    width,
                    height,
                    fps,
                    thread_stop,
                    on_frame,
                    ready_tx,
                );
            })
            .map_err(|e| format!("Thread spawn: {e}"))?;

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {
                let mut guard = VIDEO_CAPTURE_STATE
                    .lock()
                    .map_err(|e| format!("Video capture state lock poisoned: {e}"))?;
                // A concurrent start may have won the race while we waited;
                // the singleton must not be overwritten, or the winner's
                // `stop()` would reap this session instead of its own.
                if guard.is_some() {
                    stop.store(true, Ordering::Relaxed);
                    let _ = handle.join();
                    return Err("Video capture already active".into());
                }
                *guard = Some(PwVideoCapture {
                    stop: stop.clone(),
                    join: None,
                });
                Ok(PwVideoCapture {
                    stop,
                    join: Some(handle),
                })
            }
            Ok(Err(reason)) => {
                stop.store(true, Ordering::Relaxed);
                let _ = handle.join();
                Err(reason)
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                let _ = handle.join();
                Err(
                    "Timed out waiting for the video capture stream to reach an active state"
                        .into(),
                )
            }
        }
    }

    /// Stops the capture thread and joins it; also releases the singleton
    /// slot so a new capture can start.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
        // The registered session shares the same stop flag; it exits within
        // one loop iteration. Its sibling handle is join-less, so nothing to
        // join here — the owning instance above is the joiner.
        if let Ok(mut guard) = VIDEO_CAPTURE_STATE.lock()
            && let Some(mut session) = guard.take()
        {
            session.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = session.join.take() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipewire::spa::pod::deserialize::PodDeserializer;

    fn v(major: u32, minor: u32, micro: u32) -> PipeWireVersion {
        PipeWireVersion {
            major,
            minor,
            micro,
        }
    }

    #[test]
    fn parse_accepts_exactly_three_numeric_components() {
        assert_eq!(PipeWireVersion::parse("1.2.3"), v(1, 2, 3));
        assert_eq!(PipeWireVersion::parse("0.3.33"), v(0, 3, 33));
    }

    #[test]
    fn parse_rejects_malformed_versions_as_unknown() {
        let unknown = v(0, 0, 0);
        assert_eq!(PipeWireVersion::parse(""), unknown);
        assert_eq!(PipeWireVersion::parse("1.2"), unknown);
        assert_eq!(PipeWireVersion::parse("1.2.3.4"), unknown);
        assert_eq!(PipeWireVersion::parse("a.b.c"), unknown);
        assert_eq!(PipeWireVersion::parse("1.2.x"), unknown);
        assert_eq!(PipeWireVersion::parse("0.0.0"), unknown);
    }

    #[test]
    fn comparison_matches_libwebrtc_boundaries() {
        let min = v(0, 3, 33);
        assert!(v(0, 3, 33) >= min);
        assert!(v(0, 3, 34) >= min);
        assert!(v(0, 4, 0) >= min);
        assert!(v(1, 0, 0) >= min);
        assert!(v(0, 3, 32) < min);
        assert!(v(0, 2, 99) < min);
    }

    #[test]
    fn unknown_versions_never_satisfy_but_known_self_compares_numerically() {
        let unknown = v(0, 0, 0);
        let min = v(0, 3, 33);
        // An unknown version reports no ordering at all, so no comparison
        // against it (in either direction) can hold.
        assert_eq!(unknown.partial_cmp(&min), None);
        assert_eq!(unknown.partial_cmp(&unknown), None);
        // The .cc only guards the left side: a known version still compares
        // numerically against an all-zero right side.
        assert!(v(0, 3, 40) >= unknown);
        assert!(v(0, 2, 0) >= unknown);
    }

    #[test]
    fn no_modifier_format_round_trips_through_video_info_raw() {
        let Some(bytes) = build_format(VideoFormat::BGRA, None, 1280, 720, 60) else {
            panic!("serialization must succeed");
        };
        let Some(pod) = Pod::from_bytes(&bytes) else {
            panic!("bytes must parse as a pod");
        };
        let mut info = VideoInfoRaw::new();
        assert!(info.parse(pod).is_ok(), "EnumFormat pod must parse");
        assert_eq!(info.format(), VideoFormat::BGRA);
        assert_eq!(
            info.size(),
            Rectangle {
                width: 1280,
                height: 720
            }
        );
        assert_eq!(info.framerate(), Fraction { num: 60, denom: 1 });
    }

    fn le_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ])
    }

    fn le_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ])
    }

    /// Walks the serialized `EnumFormat` object (header 8 + `object_type`/id
    /// 8, then `[key u32][flags u32][pod: [size u32][type u32]...]` per
    /// property) and returns the flags and value pod of the first property
    /// with the given key.
    fn find_property(bytes: &[u8], key: u32) -> Option<(u32, &[u8])> {
        let mut offset = 16;
        while offset + 8 <= bytes.len() {
            let prop_key = le_u32(bytes, offset);
            let flags = le_u32(bytes, offset + 4);
            let pod_start = offset + 8;
            let pod_size = usize::try_from(le_u32(bytes, pod_start)).ok()?;
            let pod_end = pod_start + 8 + pod_size + (8 - pod_size % 8) % 8;
            if prop_key == key {
                return Some((flags, &bytes[pod_start..pod_end]));
            }
            offset = pod_end;
        }
        None
    }

    #[test]
    fn single_invalid_modifier_becomes_mandatory_plain_long() {
        let Some(bytes) =
            build_format(VideoFormat::BGRA, Some(&[DRM_FORMAT_MOD_INVALID]), 0, 0, 30)
        else {
            panic!("serialization must succeed");
        };
        let Some((flags, pod)) =
            find_property(&bytes, pipewire::spa::sys::SPA_FORMAT_VIDEO_modifier)
        else {
            panic!("modifier property must be present");
        };
        assert_eq!(flags, pipewire::spa::sys::SPA_POD_PROP_FLAG_MANDATORY);
        assert_eq!(le_u32(pod, 4), pipewire::spa::sys::SPA_TYPE_Long);
        // A single Long value equal to u64::MAX (DRM_FORMAT_MOD_INVALID).
        let value = le_u64(pod, 8);
        assert_eq!(value, DRM_FORMAT_MOD_INVALID);
    }

    #[test]
    fn multi_modifier_choice_duplicates_first_value_as_default() {
        let mods = [0x1u64, 0x2, 0x4, DRM_FORMAT_MOD_INVALID];
        let Some(bytes) = build_format(VideoFormat::BGRA, Some(&mods), 0, 0, 0) else {
            panic!("serialization must succeed");
        };
        let Some((flags, pod)) =
            find_property(&bytes, pipewire::spa::sys::SPA_FORMAT_VIDEO_modifier)
        else {
            panic!("modifier property must be present");
        };
        assert_eq!(
            flags,
            pipewire::spa::sys::SPA_POD_PROP_FLAG_MANDATORY
                | pipewire::spa::sys::SPA_POD_PROP_FLAG_DONT_FIXATE
        );
        // Choice pod: [size u32][type u32][choice_type u32][flags u32]
        //             [value_size u32][value_type u32][values i64...]
        let pod_size = le_u32(pod, 0) as usize;
        assert_eq!(le_u32(pod, 4), pipewire::spa::sys::SPA_TYPE_Choice);
        assert_eq!(le_u32(pod, 8), pipewire::spa::sys::SPA_CHOICE_Enum);
        let value_size = le_u32(pod, 16) as usize;
        assert_eq!(value_size, 8);
        let n_values = (pod_size - 16) / value_size;
        assert_eq!(n_values, mods.len() + 1);
        let values: Vec<u64> = (0..n_values)
            .map(|i| le_u64(pod, 24 + i * value_size))
            .collect();
        // The first value is the default: the first modifier listed twice.
        assert_eq!(values[0], mods[0]);
        assert_eq!(values[1], mods[0]);
        assert_eq!(&values[1..], mods);
    }

    #[test]
    fn modifier_choice_round_trips_through_libspa_deserializer() {
        let mods = [7u64, 3, 1];
        let choice = Value::Choice(ChoiceValue::Long(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Enum {
                default: mods[0].cast_signed(),
                alternatives: mods.iter().map(|m| m.cast_signed()).collect(),
            },
        )));
        let Ok(serialized) = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &choice)
        else {
            panic!("serialization must succeed");
        };
        let bytes = serialized.0.into_inner();
        let Ok((remaining, parsed)) = PodDeserializer::deserialize_from::<Choice<i64>>(&bytes)
        else {
            panic!("choice pod must deserialize");
        };
        assert!(remaining.is_empty());
        match parsed.1 {
            ChoiceEnum::Enum {
                default,
                alternatives,
            } => {
                assert_eq!(default, mods[0].cast_signed());
                assert_eq!(
                    alternatives,
                    mods.iter().map(|m| m.cast_signed()).collect::<Vec<_>>()
                );
            }
            other => panic!("expected an Enum choice, got {other:?}"),
        }
    }

    #[test]
    fn convert_rgbx_to_bgrx_swaps_red_and_blue_per_pixel() {
        let mut frame = vec![
            0x11, 0x22, 0x33, 0x44, // R G B A
            0xaa, 0xbb, 0xcc, 0xdd,
        ];
        convert_rgbx_to_bgrx(&mut frame);
        assert_eq!(frame, vec![0x33, 0x22, 0x11, 0x44, 0xcc, 0xbb, 0xaa, 0xdd]);
    }

    #[test]
    fn convert_rgbx_to_bgrx_leaves_trailing_bytes_untouched() {
        let mut frame = vec![1, 2, 3, 4, 5];
        convert_rgbx_to_bgrx(&mut frame);
        assert_eq!(frame, vec![3, 2, 1, 4, 5]);
    }

    #[test]
    fn stop_on_unstarted_instance_is_a_noop() {
        let mut capture = PwVideoCapture {
            stop: Arc::new(AtomicBool::new(false)),
            join: None,
        };
        capture.stop();
        // Nothing to join and no registered session: only the flag flips.
        assert!(capture.stop.load(Ordering::Relaxed));
        assert!(capture.join.is_none());
    }
}

/// Integration probe (gate 3 of SCREEN-CAPTURE-INHOUSE.md §10): drives the
/// full Phase A → B → C pipeline — portal handshake, `pw_stream` engine on
/// the portal fd, EGL/DMA-BUF readback — with NO `arm_pipewire_shims` call
/// (the probe binary links no libwebrtc, so `pw_init` binds the real
/// libpipewire). Requires `frames > 0` at 60 fps; the non-black ratio is
/// reported (the §5.7 bar) but asserted only as a warning, since an all-black
/// desktop is a legitimate selection. Run with:
///
/// ```sh
/// cargo test -p native-rust --release -- --ignored pw_video_probe --nocapture
/// ```
#[cfg(test)]
mod probe {
    use super::*;
    use crate::linux::portal::{ScreenCastPortal, StartOutcome};
    use std::sync::atomic::{AtomicU32, AtomicU64};

    #[test]
    #[ignore = "manual probe: requires a Wayland session with a portal picker to answer"]
    fn pw_video_probe() {
        let connection =
            zbus::blocking::Connection::session().unwrap_or_else(|e| panic!("session bus: {e}"));
        let portal =
            ScreenCastPortal::connect(connection).unwrap_or_else(|e| panic!("create session: {e}"));
        portal
            .select_sources()
            .unwrap_or_else(|e| panic!("select sources: {e}"));
        let stream = match portal.start(None).unwrap_or_else(|e| panic!("start: {e}")) {
            StartOutcome::Stream(stream) => stream,
            StartOutcome::Cancelled => {
                eprintln!("[pw-video-probe] cancelled by user");
                return;
            }
        };
        let fd = portal
            .open_pipewire_remote()
            .unwrap_or_else(|e| panic!("open pipewire remote: {e}"));
        // When the portal reports no size, offer no fixed size either — a
        // fixed value that does not match the source's resolution fails the
        // format intersection (KWin offers a choice-range around its actual
        // resolution); libwebrtc passes 0,0 and relies on the 1..UINT32_MAX
        // size range instead.
        let (width, height) = stream.size.map_or((0, 0), |(w, h)| {
            (u32::try_from(w).unwrap_or(0), u32::try_from(h).unwrap_or(0))
        });

        let frames = Arc::new(AtomicU64::new(0));
        let black_frames = Arc::new(AtomicU64::new(0));
        let last_width = Arc::new(AtomicU32::new(0));
        let last_height = Arc::new(AtomicU32::new(0));
        let frames_clone = frames.clone();
        let black_clone = black_frames.clone();
        let width_clone = last_width.clone();
        let height_clone = last_height.clone();
        let on_frame = move |w: u32, h: u32, bgra: &[u8], _pts_us: i64| {
            frames_clone.fetch_add(1, Ordering::Relaxed);
            width_clone.store(w, Ordering::Relaxed);
            height_clone.store(h, Ordering::Relaxed);
            // Spot-check a strided sample of pixels; a uniformly black frame
            // (all channels <= 8) counts as black.
            let mut non_black = false;
            for i in (0..bgra.len()).step_by(4096) {
                let pixel = &bgra[i..i + 3];
                if pixel.iter().any(|&c| c > 8) {
                    non_black = true;
                    break;
                }
            }
            if !non_black {
                black_clone.fetch_add(1, Ordering::Relaxed);
            }
        };

        // fps 0 requests no fixed rate: the negotiation fails when the
        // consumer fixes a non-zero framerate, because KWin offers a fixed
        // 0/1 (`SPA_FORMAT_VIDEO_framerate`) and spa_pod_filter requires
        // fixed-vs-fixed equality. KWin streams at its own refresh rate
        // (`maxFramerate`) regardless.
        let mut capture = PwVideoCapture::start(
            Some(fd),
            stream.node_id,
            width,
            height,
            0,
            Box::new(on_frame),
        )
        .unwrap_or_else(|e| panic!("capture start: {e}"));
        eprintln!(
            "[pw-video-probe] streaming node_id={} requested={}x{} portal-size={:?}",
            stream.node_id, width, height, stream.size,
        );

        for second in 0..20 {
            std::thread::sleep(Duration::from_secs(1));
            let total = frames.load(Ordering::Relaxed);
            eprintln!(
                "[pw-video-probe] t={second}s frames={total} black={} size={}x{}",
                black_frames.load(Ordering::Relaxed),
                last_width.load(Ordering::Relaxed),
                last_height.load(Ordering::Relaxed),
            );
        }

        capture.stop();
        drop(portal);

        let total = frames.load(Ordering::Relaxed);
        let black = black_frames.load(Ordering::Relaxed);
        let non_black_pct = (total - black).saturating_mul(100).checked_div(total);
        eprintln!(
            "[pw-video-probe] stopped — frames={total} black={black} non-black={non_black_pct:?}%",
        );
        assert!(total > 0, "frames must flow through the pw_stream engine");
        if non_black_pct.is_some_and(|pct| pct < 50) {
            eprintln!(
                "[pw-video-probe] WARNING: majority of frames are black — check the EGL readback"
            );
        }
    }
}
