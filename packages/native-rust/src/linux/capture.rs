use super::graph::{ChannelLayout, DefaultSinkWatch, GraphTracker, TargetSpec};
use super::{ADAPTER_FACTORY, CAPTURE_NODE_DESCRIPTION, CAPTURE_NODE_NAME, pw_init};
use crate::AudioTarget;
use pipewire::properties::{PropertiesBox, properties};
use pipewire::registry::GlobalObject;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pipewire::spa::param::format::MediaType;
use pipewire::spa::param::{ParamType, format_utils};
use pipewire::spa::pod::{Object, Pod, Value};
use pipewire::spa::utils::SpaTypes;
use pipewire::stream::{StreamFlags, StreamListener, StreamRc, StreamState};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

type ReadySenderCell = Rc<RefCell<Option<mpsc::Sender<Result<(), String>>>>>;

fn parse_target_id(target: &AudioTarget) -> Result<Option<u32>, String> {
    match target {
        AudioTarget::Id(n) if *n < 0 => Ok(None),
        AudioTarget::Id(n) => Ok(Some((*n).cast_unsigned())),
        AudioTarget::Label(s) => {
            let n = s
                .trim()
                .parse::<u32>()
                .map_err(|_| "A PipeWire node ID or -1 (system audio) is required".to_string())?;
            Ok(Some(n))
        }
    }
}

struct CaptureState {
    is_active: bool,
    session: Option<CaptureSession>,
}

impl CaptureState {
    const fn new() -> Self {
        Self {
            is_active: false,
            session: None,
        }
    }
}

struct CaptureSession {
    stop: Arc<AtomicBool>,
    join: thread::JoinHandle<()>,
    target_tx: mpsc::Sender<TargetSpec>,
}

#[derive(Default)]
pub(super) struct SessionShared {
    pub(super) capture_node_id: Option<u32>,
}

static CAPTURE_STATE: Mutex<Option<CaptureState>> = Mutex::new(None);

pub(super) fn port_channel_from_name(port_name: Option<&str>) -> Option<String> {
    let name = port_name?;
    let idx = name.rfind('_')?;
    let suffix = &name[idx + 1..];
    if suffix.is_empty() {
        None
    } else {
        Some(suffix.into())
    }
}

/// Creates the virtual node the target application's audio is linked into.
///
/// It is a virtual *source*, not a null sink: Chromium's `PulseAudio` backend
/// drops every source that is a sink monitor (`monitor_of_sink` set) when
/// enumerating input devices, so a null sink's `.monitor` can never reach
/// `enumerateDevices()` in the renderer. A virtual source is a first-class
/// `PulseAudio` source and still exposes input ports to link into.
fn create_capture_node(
    core: &pipewire::core::Core,
    layout: &ChannelLayout,
) -> Result<pipewire::node::Node, String> {
    core.create_object::<pipewire::node::Node>(
        ADAPTER_FACTORY,
        &properties! {
            "factory.name" => "support.null-audio-sink",
            "node.name" => CAPTURE_NODE_NAME,
            "node.description" => CAPTURE_NODE_DESCRIPTION,
            "media.class" => "Audio/Source/Virtual",
            "audio.position" => layout.position.as_str(),
            "object.linger" => "false",
        },
    )
    .map_err(|e| format!("Failed to create virtual capture node: {e}"))
}

fn destroy_capture_node(
    node: &mut Option<pipewire::node::Node>,
    core: &pipewire::core::Core,
    tracker: &Rc<RefCell<GraphTracker>>,
    shared: &Arc<Mutex<SessionShared>>,
) {
    tracker.borrow_mut().on_capture_node_destroyed(shared);
    if let Some(node) = node.take() {
        let _ = core.destroy_object(node);
    }
}

fn bind_default_metadata(
    registry: &pipewire::registry::Registry,
    global: &GlobalObject<PropertiesBox>,
    tracker: &Rc<RefCell<GraphTracker>>,
) -> Option<(
    pipewire::metadata::Metadata,
    pipewire::metadata::MetadataListener,
)> {
    let metadata = registry
        .bind::<pipewire::metadata::Metadata, _>(global)
        .ok()?;
    let t = tracker.clone();
    let listener = metadata
        .add_listener_local()
        .property(move |subject, key, _type, value| {
            if subject == pipewire::core::PW_ID_CORE
                && key == Some("default.audio.sink")
                && let Some(name) = value.and_then(parse_json_name)
            {
                t.borrow_mut().set_default_sink_name(name);
            }
            0
        })
        .register();
    Some((metadata, listener))
}

fn bind_default_sink(
    registry: &pipewire::registry::Registry,
    tracker: &Rc<RefCell<GraphTracker>>,
    id: u32,
    desired_layout: &Rc<RefCell<ChannelLayout>>,
) -> Option<DefaultSinkWatch> {
    let proxy = {
        let tracker_ref = tracker.borrow();
        let global = tracker_ref.system_sink_global(id)?;
        registry.bind::<pipewire::node::Node, _>(global).ok()?
    };
    let desired = desired_layout.clone();
    let listener = proxy
        .add_listener_local()
        .param(move |_seq, param_type, _index, _next, pod| {
            if param_type != ParamType::EnumFormat {
                return;
            }
            let Some(pod) = pod else { return };
            let Ok((media_type, _)) = format_utils::parse_format(pod) else {
                return;
            };
            if media_type != MediaType::Audio {
                return;
            }
            let mut info = AudioInfoRaw::new();
            if info.parse(pod).is_err() {
                return;
            }
            if let Some(layout) = ChannelLayout::from_audio_info(&info) {
                *desired.borrow_mut() = layout;
            }
        })
        .register();
    proxy.subscribe_params(&[ParamType::EnumFormat]);
    Some(DefaultSinkWatch {
        id,
        _proxy: proxy,
        _listener: listener,
    })
}

fn parse_json_name(json: &str) -> Option<String> {
    let key_pos = json.find(r#""name""#)?;
    let after_colon = json[key_pos + 6..].split_once(':')?.1.trim_start();
    let val = after_colon.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = val.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out),
            _ => out.push(c),
        }
    }
    None
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    reason = "run_capture_session is spawned in a thread::spawn(move || ...) closure that must own all Arc values; the PipeWire session orchestrator is intrinsically long"
)]
fn run_capture_session(
    target: TargetSpec,
    shared: Arc<Mutex<SessionShared>>,
    stop: Arc<AtomicBool>,
    ready_tx: mpsc::Sender<Result<(), String>>,
    target_rx: mpsc::Receiver<TargetSpec>,
) {
    pipewire::init();

    let Ok(pw) = pw_init() else {
        let _ = ready_tx.send(Err("PipeWire init failed".into()));
        return;
    };

    let registry = match pw.core.get_registry() {
        Ok(r) => r,
        Err(e) => {
            let _ = ready_tx.send(Err(e.to_string()));
            return;
        }
    };

    let link_factory_name: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let tracker = Rc::new(RefCell::new(GraphTracker::new(
        target,
        pw.core.clone(),
        link_factory_name,
    )));

    let _reg_listener = registry
        .add_listener_local()
        .global({
            let t = tracker.clone();
            let s = shared.clone();
            move |global| t.borrow_mut().add_global(global, &s)
        })
        .global_remove({
            let t = tracker.clone();
            let s = shared.clone();
            move |id| t.borrow_mut().remove_global(id, &s)
        })
        .register();

    let desired_layout = Rc::new(RefCell::new(ChannelLayout::stereo()));
    let mut capture_layout = ChannelLayout::stereo();
    let mut capture_node = match create_capture_node(&pw.core, &capture_layout) {
        Ok(node) => Some(node),
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    let mut node_found = false;
    for _ in 0..60 {
        pw.main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(50)));
        if shared
            .lock()
            .ok()
            .is_some_and(|s| s.capture_node_id.is_some_and(|id| id != 0))
        {
            node_found = true;
            break;
        }
    }

    if !node_found {
        let _ = ready_tx.send(Err("Virtual capture node failed to appear".into()));
        destroy_capture_node(&mut capture_node, &pw.core, &tracker, &shared);
        return;
    }

    let capture_node_id = shared
        .lock()
        .ok()
        .and_then(|s| s.capture_node_id)
        .unwrap_or(0);
    let mut _pcm_stream: Option<StreamRc> = None;
    let mut _pcm_listener: Option<StreamListener<()>> = None;

    let ready_tx_cell = Rc::new(RefCell::new(Some(ready_tx)));

    let setup_pcm_stream =
        |node_id: u32, ready_cell: &ReadySenderCell| -> Option<(StreamRc, StreamListener<()>)> {
            let values = create_audio_capture_format()?;
            let pod = Pod::from_bytes(&values)?;
            let mut params = [pod];

            let stream = StreamRc::new(
                pw.core.clone(),
                AUDIO_STREAM_NAME,
                properties! {
                    "media.class" => "Stream/Input/Audio",
                    "node.name" => AUDIO_STREAM_NAME,
                    "node.description" => "Slopcast Audio Capture",
                    "node.dont-move" => "true",
                    "node.dont-reconnect" => "true",
                },
            )
            .ok()?;

            let ready_cell_clone = ready_cell.clone();
            let listener = stream
                .add_local_listener_with_user_data(())
                .state_changed(move |_stream, _old, state, _error| match state {
                    StreamState::Streaming | StreamState::Paused => {
                        if let Some(tx) = ready_cell_clone.borrow_mut().take() {
                            let _ = tx.send(Ok(()));
                        }
                    }
                    StreamState::Unconnected | StreamState::Connecting => {}
                    StreamState::Error(e) => {
                        if let Some(tx) = ready_cell_clone.borrow_mut().take() {
                            let _ = tx.send(Err(format!("PCM stream error: {e}")));
                        }
                    }
                })
                .process(move |s, ()| {
                    let Some(mut buffer) = s.dequeue_buffer() else {
                        return;
                    };
                    thread_local! {
                        static PCM_SCRATCH: std::cell::RefCell<Vec<u8>> =
                            std::cell::RefCell::new(Vec::with_capacity(MAX_AUDIO_FRAME_BYTES));
                    }
                    PCM_SCRATCH.with(|cell| {
                        let mut all_bytes = cell.borrow_mut();
                        all_bytes.clear();
                        for data in buffer.datas_mut() {
                            let start = data.chunk().offset() as usize;
                            let size = data.chunk().size() as usize;
                            let Some(bytes) = data.data() else {
                                continue;
                            };
                            let end = start.saturating_add(size).min(bytes.len());
                            let Some(slice) = bytes.get(start..end) else {
                                continue;
                            };
                            all_bytes.extend_from_slice(slice);
                        }
                        if !all_bytes.is_empty() {
                            invoke_audio_data_callback(&all_bytes);
                        }
                    });
                })
                .register()
                .ok()?;

            if let Err(e) = stream.connect(
                pipewire::spa::utils::Direction::Input,
                Some(node_id),
                StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
                &mut params,
            ) {
                eprintln!("[audio-capture] stream connect: {e}");
                if let Some(tx) = ready_cell.borrow_mut().take() {
                    let _ = tx.send(Err(format!("PCM stream connect failed: {e}")));
                }
                None
            } else {
                Some((stream, listener))
            }
        };

    if capture_node_id != 0
        && let Some((s, l)) = setup_pcm_stream(capture_node_id, &ready_tx_cell)
    {
        _pcm_stream = Some(s);
        _pcm_listener = Some(l);
    }

    let mut metadata_watch: Option<(
        pipewire::metadata::Metadata,
        pipewire::metadata::MetadataListener,
    )> = None;
    let mut sink_watch: Option<DefaultSinkWatch> = None;
    let mut reconnect_pcm_pending = false;

    while !stop.load(Ordering::Relaxed) {
        pw.main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(50)));

        while let Ok(new_target) = target_rx.try_recv() {
            tracker.borrow_mut().change_target(new_target, &pw.core);
        }

        if metadata_watch.is_none()
            && let Some(global) = tracker.borrow_mut().take_pending_metadata()
        {
            metadata_watch = bind_default_metadata(&registry, &global, &tracker);
        }

        let want_watch = tracker.borrow().default_sink_id();
        if want_watch != sink_watch.as_ref().map(|w| w.id) {
            sink_watch = want_watch
                .and_then(|id| bind_default_sink(&registry, &tracker, id, &desired_layout));
        }

        if *desired_layout.borrow() != capture_layout {
            let layout = desired_layout.borrow().clone();
            destroy_capture_node(&mut capture_node, &pw.core, &tracker, &shared);
            if let Ok(node) = create_capture_node(&pw.core, &layout) {
                capture_node = Some(node);
                capture_layout = layout;
            }
            reconnect_pcm_pending = true;
        }

        if reconnect_pcm_pending {
            let new_node_id = shared
                .lock()
                .ok()
                .and_then(|s| s.capture_node_id)
                .unwrap_or(0);
            if new_node_id != 0 {
                _pcm_listener = None;
                _pcm_stream = None;
                let dummy_ready = Rc::new(RefCell::new(None));
                if let Some((s, l)) = setup_pcm_stream(new_node_id, &dummy_ready) {
                    _pcm_stream = Some(s);
                    _pcm_listener = Some(l);
                    reconnect_pcm_pending = false;
                }
            }
        }
    }

    drop(sink_watch);
    drop(metadata_watch);

    for link in tracker.borrow_mut().drain_links() {
        let _ = pw.core.destroy_object(link);
    }
    destroy_capture_node(&mut capture_node, &pw.core, &tracker, &shared);

    if let Ok(pending) = pw.core.sync(0) {
        let flush_done = Rc::new(RefCell::new(false));
        let fd = flush_done.clone();
        let ml_weak = pw.main_loop.downgrade();
        let _core_listener = pw
            .core
            .add_listener_local()
            .done(move |id, seq| {
                if id == pipewire::core::PW_ID_CORE && seq == pending {
                    *fd.borrow_mut() = true;
                    if let Some(ml) = ml_weak.upgrade() {
                        ml.quit();
                    }
                }
            })
            .register();
        for _ in 0..50 {
            if *flush_done.borrow() {
                break;
            }
            pw.main_loop
                .loop_()
                .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(50)));
        }
    }

    if let Ok(mut s) = shared.lock() {
        s.capture_node_id = None;
    }
}

fn spawn_capture_session(target: TargetSpec) -> Result<CaptureSession, String> {
    crate::audio_ring::start_audio_ring()
        .map_err(|e| format!("Failed to start audio ring: {e}"))?;
    let shared = Arc::new(Mutex::new(SessionShared::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let (target_tx, target_rx) = mpsc::channel::<TargetSpec>();

    let join = {
        let shared = shared.clone();
        let stop = stop.clone();
        thread::Builder::new()
            .name("pw-window-audio-capture".into())
            .spawn(move || run_capture_session(target, shared, stop, ready_tx, target_rx))
            .map_err(|e| {
                crate::audio_ring::stop_audio_ring();
                format!("Failed to spawn PipeWire worker: {e}")
            })?
    };

    match ready_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            crate::audio_ring::stop_audio_ring();
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            return Err(reason);
        }
        Err(_) => {
            crate::audio_ring::stop_audio_ring();
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            return Err("Timed out waiting for PipeWire session".into());
        }
    }
    // The worker thread only sends ready after confirming capture_node_id,
    // so the node is guaranteed available at this point. No need to poll.

    Ok(CaptureSession {
        stop,
        join,
        target_tx,
    })
}

fn stop_session(state: &mut CaptureState) {
    crate::audio_ring::stop_audio_ring();
    if let Some(session) = state.session.take() {
        session.stop.store(true, Ordering::SeqCst);
        // Reap on a detached thread: the capture worker's shutdown flush can
        // take up to 2.5 s (50 × 50 ms core-sync iterations), which must never
        // block the Electron main process. The worker holds no shared state
        // after stop_audio_ring, so a restarted session is unaffected.
        let _ = thread::Builder::new()
            .name("pw-capture-reaper".into())
            .spawn(move || {
                let _ = session.join.join();
            });
    }
    state.is_active = false;
}

pub(crate) fn start_audio_capture(target_app_id: &AudioTarget) -> Result<bool, String> {
    let node_id = parse_target_id(target_app_id)?;
    let mut state_guard = CAPTURE_STATE
        .lock()
        .map_err(|e| format!("Audio capture state lock poisoned: {e}"))?;
    let state = state_guard.get_or_insert_with(CaptureState::new);
    stop_session(state);

    let target = TargetSpec {
        node_id,
        system_audio: node_id.is_none(),
        ..TargetSpec::default()
    };
    let session = spawn_capture_session(target)?;
    state.is_active = true;
    state.session = Some(session);
    Ok(true)
}

pub(crate) fn switch_audio_capture(target_app_id: &AudioTarget) -> Result<bool, String> {
    let node_id = parse_target_id(target_app_id)?;
    let mut state_guard = CAPTURE_STATE
        .lock()
        .map_err(|e| format!("Audio capture state lock poisoned: {e}"))?;
    let Some(state) = state_guard.as_mut() else {
        return Err("No active audio capture session to switch".into());
    };
    let Some(session) = &state.session else {
        return Err("No active audio capture session to switch".into());
    };

    let target = TargetSpec {
        node_id,
        system_audio: node_id.is_none(),
        ..TargetSpec::default()
    };
    session
        .target_tx
        .send(target)
        .map_err(|e| format!("Failed to send audio target switch: {e}"))?;
    Ok(true)
}

pub(crate) fn stop_audio_capture() -> bool {
    let Ok(mut state_guard) = CAPTURE_STATE.lock() else {
        eprintln!("[capture] state lock poisoned; nothing to stop");
        return true;
    };
    if let Some(state) = state_guard.as_mut() {
        stop_session(state);
    }
    true
}

pub(crate) fn is_audio_capture_active() -> bool {
    let Ok(mut state_guard) = CAPTURE_STATE.lock() else {
        eprintln!("[capture] state lock poisoned; reporting inactive");
        return false;
    };
    let Some(state) = state_guard.as_mut() else {
        return false;
    };
    if state.session.as_ref().is_some_and(|s| s.join.is_finished()) {
        stop_session(state);
    }
    state.is_active
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "samples are clamped to [-1, 1] before scaling by 32767, so the product always fits in i16"
)]
fn invoke_audio_data_callback(data: &[u8]) {
    // PipeWire delivers f32 LE frames; we downmix to packed i16 LE bytes so the
    // N-API boundary transfers one binary buffer instead of ~960 JS numbers.
    thread_local! {
        static I16_SCRATCH: std::cell::RefCell<Vec<u8>> =
            std::cell::RefCell::new(Vec::with_capacity(MAX_AUDIO_FRAME_BYTES / 2));
    }
    I16_SCRATCH.with(|cell| {
        let mut i16_bytes = cell.borrow_mut();
        i16_bytes.clear();
        let num_samples = data.len() / 4;
        let out_bytes = num_samples * 2;
        i16_bytes.resize(out_bytes, 0);

        for (i, chunk) in data.chunks_exact(4).enumerate() {
            let bits = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let f = f32::from_bits(bits);
            let sample = (f.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            let sample_bytes = sample.to_le_bytes();
            i16_bytes[i * 2] = sample_bytes[0];
            i16_bytes[i * 2 + 1] = sample_bytes[1];
        }
        crate::audio_ring::push_pcm_bytes(&i16_bytes);
    });
}

fn create_audio_capture_format() -> Option<Vec<u8>> {
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let serialized = pipewire::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .ok()?;
    Some(serialized.0.into_inner())
}

const MAX_AUDIO_FRAME_BYTES: usize = 192_000;
const AUDIO_STREAM_NAME: &str = "slopcast-audio-capture";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_negative_number_means_system_audio() {
        assert_eq!(
            parse_target_id(&AudioTarget::Id(-1)).unwrap_or_else(|e| panic!("system audio: {e}")),
            None
        );
        assert_eq!(
            parse_target_id(&AudioTarget::Id(-5)).unwrap_or_else(|e| panic!("system audio: {e}")),
            None
        );
    }

    #[test]
    fn parse_positive_number_is_node_id() {
        assert_eq!(
            parse_target_id(&AudioTarget::Id(0)).unwrap_or_else(|e| panic!("node 0: {e}")),
            Some(0)
        );
        assert_eq!(
            parse_target_id(&AudioTarget::Id(42)).unwrap_or_else(|e| panic!("node 42: {e}")),
            Some(42)
        );
    }

    #[test]
    fn parse_numeric_string_is_node_id() {
        assert_eq!(
            parse_target_id(&AudioTarget::Label("123".into()))
                .unwrap_or_else(|e| panic!("node 123: {e}")),
            Some(123)
        );
        // Whitespace is trimmed before parsing.
        assert_eq!(
            parse_target_id(&AudioTarget::Label(" 7 ".into()))
                .unwrap_or_else(|e| panic!("node 7: {e}")),
            Some(7)
        );
    }

    #[test]
    fn parse_string_minus_one_is_not_system_audio() {
        // Only the numeric -1 selects system audio; a "-1" string is an
        // invalid node id, not a mode switch.
        assert!(parse_target_id(&AudioTarget::Label("-1".into())).is_err());
    }

    #[test]
    fn parse_non_numeric_string_is_an_error() {
        assert!(parse_target_id(&AudioTarget::Label("not-a-node".into())).is_err());
        assert!(parse_target_id(&AudioTarget::Label(String::new())).is_err());
    }

    #[test]
    fn port_channel_from_name_takes_last_underscore_suffix() {
        assert_eq!(
            port_channel_from_name(Some("playback_FL")),
            Some("FL".into())
        );
        assert_eq!(port_channel_from_name(Some("capture_1")), Some("1".into()));
    }

    #[test]
    fn port_channel_from_name_missing_or_trailing_underscore_is_none() {
        assert_eq!(port_channel_from_name(Some("playback")), None);
        assert_eq!(port_channel_from_name(Some("playback_")), None);
        assert_eq!(port_channel_from_name(None), None);
    }

    #[test]
    fn parse_json_name_extracts_plain_name() {
        let json = r#"{"name":"alsa_output.pci-0000_00_1f.3.analog-stereo","description":"Built-in Audio"}"#;
        assert_eq!(
            parse_json_name(json).as_deref(),
            Some("alsa_output.pci-0000_00_1f.3.analog-stereo")
        );
    }

    #[test]
    fn parse_json_name_handles_escaped_quotes_and_backslashes() {
        let json = r#"{"name":"weird \"name\" with \\ backslash"}"#;
        assert_eq!(
            parse_json_name(json).as_deref(),
            Some("weird \"name\" with \\ backslash")
        );
    }

    #[test]
    fn parse_json_name_rejects_malformed_input() {
        assert_eq!(parse_json_name(""), None);
        assert_eq!(parse_json_name(r#"{"description":"no name key"}"#), None);
        assert_eq!(parse_json_name(r#"{"name":"unterminated"#), None);
        assert_eq!(parse_json_name(r#"{"name": 42}"#), None);
    }
}
