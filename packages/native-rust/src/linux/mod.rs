use crate::{AudioApp, AudioAppLevel};
mod kwin;
mod mpris;
use napi::{Either, Result as NapiResult};
use pipewire::properties::{PropertiesBox, properties};
use pipewire::registry::GlobalObject;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw, AudioInfoRawFlags};
use pipewire::spa::param::format::MediaType;
use pipewire::spa::param::{ParamType, format_utils};
use pipewire::spa::pod::{Object, Pod, Value};
use pipewire::spa::utils::{SpaTypes, dict::DictRef};
use pipewire::stream::{StreamFlags, StreamListener, StreamRc};
use pipewire::types::ObjectType;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const CAPTURE_NODE_NAME: &str = "Slopcast-Window-Audio";
const CAPTURE_NODE_DESCRIPTION: &str = "Slopcast-Window-Audio";
const ADAPTER_FACTORY: &str = "adapter";
const LINK_FACTORY: &str = "link-factory";

struct PwCtx {
    main_loop: pipewire::main_loop::MainLoopRc,
    core: pipewire::core::CoreRc,
}

fn pw_init() -> Result<PwCtx, String> {
    let main_loop =
        pipewire::main_loop::MainLoopRc::new(None).map_err(|e| format!("MainLoop: {e}"))?;
    let context =
        pipewire::context::ContextRc::new(&main_loop, None).map_err(|e| format!("Context: {e}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| format!("Connect: {e}"))?;
    Ok(PwCtx { main_loop, core })
}

fn sync_registry(core: &pipewire::core::Core, main_loop: &pipewire::main_loop::MainLoopRc) {
    let done = Rc::new(RefCell::new(false));
    let done_clone = done.clone();
    let Some(pending) = core.sync(0).ok() else {
        return;
    };
    let main_loop_weak = main_loop.downgrade();
    let _listener = core
        .add_listener_local()
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
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(10)));
    }
}

struct ProcEntry {
    pid: i32,
    comm: String,
    cmdline: String,
}

fn iter_proc() -> Vec<ProcEntry> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| {
            let pid: i32 = e.file_name().to_str()?.parse().ok()?;
            if pid <= 0 {
                return None;
            }
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            let cmdline =
                std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            Some(ProcEntry {
                pid,
                comm: comm.trim().into(),
                cmdline,
            })
        })
        .collect()
}

fn parse_target_id(target: &Either<String, i32>) -> NapiResult<Option<u32>> {
    match target {
        Either::B(n) if *n < 0 => Ok(None),
        Either::B(n) => Ok(Some((*n).cast_unsigned())),
        Either::A(s) => {
            let n = s.trim().parse::<u32>().map_err(|_| {
                napi::Error::from_reason("A PipeWire node ID or -1 (system audio) is required")
            })?;
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
struct SessionShared {
    capture_node_id: Option<u32>,
}

static CAPTURE_STATE: Mutex<Option<CaptureState>> = Mutex::new(None);

#[derive(Default)]
struct TargetSpec {
    node_id: Option<u32>,
    pid: Option<u32>,
    binary: Option<String>,
    client_id: Option<u32>,
    system_audio: bool,
}

impl TargetSpec {
    fn learn(&mut self, node_id: u32, info: &AppNodeInfo) {
        if Some(node_id) != self.node_id {
            return;
        }
        self.pid = self.pid.or(info.pid);
        if self.binary.is_none() {
            self.binary.clone_from(&info.binary);
        }
        self.client_id = self.client_id.or(info.client_id);
    }

    fn matches(&self, node_id: u32, info: &AppNodeInfo) -> bool {
        Some(node_id) == self.node_id
            || self.client_id.is_some_and(|c| info.client_id == Some(c))
            || self.pid.is_some_and(|p| info.pid == Some(p))
            || self
                .binary
                .as_deref()
                .is_some_and(|b| info.binary.as_deref() == Some(b))
    }
}

#[derive(Default)]
struct AppNodeInfo {
    pid: Option<u32>,
    binary: Option<String>,
    client_id: Option<u32>,
}

impl AppNodeInfo {
    fn from_props(props: &DictRef) -> Self {
        let pid = props
            .get("application.process.id")
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|p| is_valid_pid((*p).cast_signed()));
        let binary = props
            .get("application.process.binary")
            .map(str::to_string)
            .filter(|b| !b.is_empty());
        let client_id = props
            .get("client.id")
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|id| *id > 0);
        Self {
            pid,
            binary,
            client_id,
        }
    }

    fn fallback_pid(&self) -> Option<u32> {
        self.binary
            .as_deref()
            .and_then(resolve_pid_by_binary)
            .map(i32::cast_unsigned)
            .filter(|p| *p > 0)
    }
}

/// Channel layout of the capture node, as an `audio.position` property value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChannelLayout {
    position: String,
}

impl ChannelLayout {
    fn stereo() -> Self {
        Self {
            position: "FL,FR".into(),
        }
    }

    fn from_audio_info(info: &AudioInfoRaw) -> Option<Self> {
        if info.flags().contains(AudioInfoRawFlags::UNPOSITIONED) {
            return None;
        }
        let channels = info.channels();
        if channels == 0 || channels > 8 {
            return None;
        }
        let position = info.position();
        let mut names = Vec::with_capacity(channels as usize);
        for &channel in &position[..channels as usize] {
            let name = channel_short_name(channel)?;
            if name.starts_with("AUX") {
                return None;
            }
            names.push(name);
        }
        Some(Self {
            position: names.join(","),
        })
    }
}

fn channel_short_name(channel: u32) -> Option<String> {
    Some(
        match channel {
            0 => "UNK",
            1 => "NA",
            2 => "MONO",
            3 => "FL",
            4 => "FR",
            5 => "FC",
            6 => "LFE",
            7 => "SL",
            8 => "SR",
            9 => "FLC",
            10 => "FRC",
            11 => "RC",
            12 => "RL",
            13 => "RR",
            14 => "TC",
            15 => "TFL",
            16 => "TFC",
            17 => "TFR",
            18 => "TRL",
            19 => "TRC",
            20 => "TRR",
            21 => "RLC",
            22 => "RRC",
            23 => "FLW",
            24 => "FRW",
            25 => "LFE2",
            26 => "FLH",
            27 => "FCH",
            28 => "FRH",
            29 => "TFLC",
            30 => "TFRC",
            31 => "TSL",
            32 => "TSR",
            33 => "LLFE",
            34 => "RLFE",
            35 => "BC",
            36 => "BLC",
            37 => "BRC",
            n @ 0x1000..=0x1027 => return Some(format!("AUX{}", n - 0x1000)),
            _ => return None,
        }
        .into(),
    )
}

struct PortInfo {
    node: u32,
    is_output: bool,
    channel: Option<String>,
}

struct DefaultSinkWatch {
    id: u32,
    _proxy: pipewire::node::Node,
    _listener: pipewire::node::NodeListener,
}

struct GraphTracker {
    target: TargetSpec,
    core: pipewire::core::CoreRc,
    factory_name: Rc<RefCell<Option<String>>>,
    capture_node_id: Option<u32>,
    app_nodes: HashMap<u32, AppNodeInfo>,
    ports: HashMap<u32, PortInfo>,
    linked_pairs: HashSet<(u32, u32)>,
    pending_pairs: HashSet<(u32, u32)>,
    created_links: HashMap<u32, (u32, u32)>,
    link_proxies: HashMap<(u32, u32), pipewire::link::Link>,
    system_sinks: HashMap<u32, (String, GlobalObject<PropertiesBox>)>,
    default_sink_name: Option<String>,
    pending_metadata: Option<GlobalObject<PropertiesBox>>,
    client_pids: HashMap<u32, u32>,
}

impl GraphTracker {
    fn new(
        target: TargetSpec,
        core: pipewire::core::CoreRc,
        factory_name: Rc<RefCell<Option<String>>>,
    ) -> Self {
        Self {
            target,
            core,
            factory_name,
            capture_node_id: None,
            app_nodes: HashMap::new(),
            ports: HashMap::new(),
            linked_pairs: HashSet::new(),
            pending_pairs: HashSet::new(),
            created_links: HashMap::new(),
            link_proxies: HashMap::new(),
            system_sinks: HashMap::new(),
            default_sink_name: None,
            pending_metadata: None,
            client_pids: HashMap::new(),
        }
    }

    fn add_global(&mut self, global: &GlobalObject<&DictRef>, shared: &Arc<Mutex<SessionShared>>) {
        let Some(props) = global.props else { return };
        match &global.type_ {
            ObjectType::Node => self.add_node(global, props, shared),
            ObjectType::Port => self.add_port(global.id, props),
            ObjectType::Link => self.add_link(global.id, props),
            ObjectType::Factory => self.track_link_factory(props),
            ObjectType::Metadata => self.track_metadata(global, props),
            ObjectType::Client => self.add_client(global, props),
            _ => {}
        }
    }

    fn remove_global(&mut self, id: u32, shared: &Arc<Mutex<SessionShared>>) {
        if self.capture_node_id == Some(id) {
            self.capture_node_id = None;
            if let Ok(mut s) = shared.lock() {
                s.capture_node_id = None;
            }
        }
        self.system_sinks.remove(&id);
        self.app_nodes.remove(&id);
        if self.ports.remove(&id).is_some() {
            self.pending_pairs
                .retain(|(out, inp)| *out != id && *inp != id);
        }
        if let Some(pair) = self.created_links.remove(&id) {
            self.linked_pairs.remove(&pair);
            self.link_proxies.remove(&pair);
        }
    }

    fn add_node(
        &mut self,
        global: &GlobalObject<&DictRef>,
        props: &DictRef,
        shared: &Arc<Mutex<SessionShared>>,
    ) {
        let media_class = props.get("media.class").unwrap_or("");
        let node_name = props.get("node.name").unwrap_or("");
        if node_name == CAPTURE_NODE_NAME {
            self.capture_node_id = Some(global.id);
            if let Ok(mut s) = shared.lock() {
                s.capture_node_id = Some(global.id);
            }
            return;
        }
        match media_class {
            "Audio/Sink" => {
                self.system_sinks
                    .insert(global.id, (node_name.into(), global.to_owned()));
            }
            "Stream/Output/Audio" => {
                let mut info = AppNodeInfo::from_props(props);
                if info.pid.is_none_or(|p| p == 0)
                    && let Some(cid) = props.get("client.id").and_then(|v| v.parse::<u32>().ok())
                    && let Some(&p) = self
                        .client_pids
                        .get(&cid)
                        .filter(|p| is_valid_pid((**p).cast_signed()))
                {
                    info.pid = Some(p);
                }
                if info.pid.is_none_or(|p| p == 0) {
                    info.pid = info.fallback_pid();
                }
                if info.pid.is_none_or(|p| p == 0) {
                    let name = props.get("application.name").unwrap_or("");
                    if !name.is_empty() {
                        info.pid = resolve_pid_by_name(name).map(i32::cast_unsigned);
                    }
                }
                let our_pid = std::process::id() as u32;
                let is_slopcast = info.pid.is_some_and(|p| p == our_pid)
                    || props
                        .get("application.name")
                        .unwrap_or("")
                        .to_lowercase()
                        .contains("slopcast")
                    || node_name.to_lowercase().contains("slopcast");
                if !is_slopcast {
                    self.target.learn(global.id, &info);
                    self.app_nodes.insert(global.id, info);
                }
            }
            _ => {}
        }
    }

    fn add_client(&mut self, global: &GlobalObject<&DictRef>, props: &DictRef) {
        let pid = client_sec_pid(props)
            .or_else(|| {
                props
                    .get("application.process.id")
                    .and_then(|v| v.parse::<i32>().ok())
                    .filter(|p| is_valid_pid(*p))
            })
            .or_else(|| {
                let name = props.get("application.name").unwrap_or("");
                (!name.is_empty())
                    .then(|| resolve_pid_by_name(name))
                    .flatten()
            });
        if let Some(pid) = pid {
            self.client_pids.insert(global.id, pid.cast_unsigned());
        }
    }

    fn add_port(&mut self, id: u32, props: &DictRef) {
        let Some(node) = props.get("node.id").and_then(|v| v.parse::<u32>().ok()) else {
            return;
        };
        let is_output = match props.get("port.direction") {
            Some("out") => true,
            Some("in") => false,
            _ => return,
        };
        let channel = props
            .get("audio.channel")
            .map(Into::into)
            .or_else(|| port_channel_from_name(props.get("port.name")));
        self.ports.insert(
            id,
            PortInfo {
                node,
                is_output,
                channel,
            },
        );

        let port = &self.ports[&id];
        if !port.is_output && self.capture_node_id == Some(port.node) {
            let candidates: Vec<u32> = self
                .ports
                .iter()
                .filter(|(pid, p)| **pid != id && p.is_output && self.is_linkable_app(p.node))
                .map(|(pid, _)| *pid)
                .collect();
            for pid in candidates {
                self.try_link(pid);
            }
        } else {
            self.try_link(id);
        }
    }

    fn add_link(&mut self, id: u32, props: &DictRef) {
        let output = props
            .get("link.output.port")
            .and_then(|v| v.parse::<u32>().ok());
        let input = props
            .get("link.input.port")
            .and_then(|v| v.parse::<u32>().ok());
        if let (Some(output), Some(input)) = (output, input) {
            let pair = (output, input);
            self.linked_pairs.insert(pair);
            if self.pending_pairs.remove(&pair) {
                self.created_links.insert(id, pair);
            }
        }
    }

    fn track_link_factory(&mut self, props: &DictRef) {
        if props.get("factory.type.name") == Some(ObjectType::Link.to_str())
            && let Some(name) = props.get("factory.name")
        {
            *self.factory_name.borrow_mut() = Some(name.into());
        }
    }

    fn track_metadata(&mut self, global: &GlobalObject<&DictRef>, props: &DictRef) {
        if props.get("metadata.name") == Some("default") {
            self.pending_metadata = Some(global.to_owned());
        }
    }

    fn set_default_sink_name(&mut self, name: String) {
        self.default_sink_name = Some(name);
    }
    fn take_pending_metadata(&mut self) -> Option<GlobalObject<PropertiesBox>> {
        self.pending_metadata.take()
    }

    fn default_sink_id(&self) -> Option<u32> {
        let name = self.default_sink_name.as_deref()?;
        self.system_sinks
            .iter()
            .find_map(|(id, (sn, _))| (sn == name).then_some(*id))
    }

    fn system_sink_global(&self, id: u32) -> Option<&GlobalObject<PropertiesBox>> {
        self.system_sinks.get(&id).map(|(_, g)| g)
    }

    fn is_linkable_app(&self, node: u32) -> bool {
        self.target.system_audio
            || self
                .app_nodes
                .get(&node)
                .is_some_and(|info| self.target.matches(node, info))
    }

    fn try_link(&mut self, port_id: u32) {
        let Some(capture) = self.capture_node_id else {
            return;
        };
        let Some(port) = self.ports.get(&port_id) else {
            return;
        };
        if !port.is_output || !self.is_linkable_app(port.node) {
            return;
        }
        let Some(channel) = port.channel.as_deref() else {
            return;
        };
        let Some(capture_port) = self.ports.iter().find_map(|(pid, p)| {
            (!p.is_output && p.node == capture && p.channel.as_deref() == Some(channel))
                .then_some(*pid)
        }) else {
            return;
        };
        let pair = (port_id, capture_port);
        if self.linked_pairs.contains(&pair) || self.pending_pairs.contains(&pair) {
            return;
        }
        if self.create_link(port_id, capture_port, port.node, capture) {
            self.pending_pairs.insert(pair);
        }
    }

    fn create_link(
        &mut self,
        output_port: u32,
        input_port: u32,
        output_node: u32,
        input_node: u32,
    ) -> bool {
        let factory = self
            .factory_name
            .borrow()
            .clone()
            .unwrap_or_else(|| LINK_FACTORY.into());
        match self.core.create_object::<pipewire::link::Link>(
            &factory,
            &properties! {
                "link.output.port" => output_port.to_string(),
                "link.input.port" => input_port.to_string(),
                "link.output.node" => output_node.to_string(),
                "link.input.node" => input_node.to_string(),
                "object.linger" => "false",
            },
        ) {
            Ok(link) => {
                self.link_proxies.insert((output_port, input_port), link);
                true
            }
            Err(_) => false,
        }
    }

    fn on_capture_node_destroyed(&mut self, shared: &Arc<Mutex<SessionShared>>) {
        let Some(capture) = self.capture_node_id.take() else {
            return;
        };
        let capture_ports: HashSet<u32> = self
            .ports
            .iter()
            .filter(|(_, p)| p.node == capture)
            .map(|(pid, _)| *pid)
            .collect();
        self.ports.retain(|_, p| p.node != capture);
        self.pending_pairs
            .retain(|(out, inp)| !capture_ports.contains(out) && !capture_ports.contains(inp));
        let dead: Vec<u32> = self
            .created_links
            .iter()
            .filter(|(_, (out, inp))| capture_ports.contains(out) || capture_ports.contains(inp))
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            if let Some(pair) = self.created_links.remove(&id) {
                self.linked_pairs.remove(&pair);
                self.link_proxies.remove(&pair);
            }
        }
        if let Ok(mut s) = shared.lock() {
            s.capture_node_id = None;
        }
    }

    fn drain_links(&mut self) -> Vec<pipewire::link::Link> {
        self.created_links.clear();
        self.linked_pairs.clear();
        self.pending_pairs.clear();
        self.link_proxies.drain().map(|(_, link)| link).collect()
    }

    fn change_target(&mut self, new_target: TargetSpec, core: &pipewire::core::Core) {
        for (_, link) in self.link_proxies.drain() {
            let _ = core.destroy_object(link);
        }
        self.created_links.clear();
        self.linked_pairs.clear();
        self.pending_pairs.clear();

        self.target = new_target;

        let output_ports: Vec<u32> = self
            .ports
            .iter()
            .filter(|(_, p)| p.is_output && self.is_linkable_app(p.node))
            .map(|(id, _)| *id)
            .collect();
        for port_id in output_ports {
            self.try_link(port_id);
        }
    }
}

fn port_channel_from_name(port_name: Option<&str>) -> Option<String> {
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

    for _ in 0..100 {
        pw.main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(10)));
        if shared
            .lock()
            .ok()
            .is_some_and(|s| s.capture_node_id.is_some_and(|id| id != 0))
        {
            break;
        }
    }

    let _ = ready_tx.send(Ok(()));

    let mut metadata_watch: Option<(
        pipewire::metadata::Metadata,
        pipewire::metadata::MetadataListener,
    )> = None;
    let mut sink_watch: Option<DefaultSinkWatch> = None;

    while !stop.load(Ordering::SeqCst) {
        pw.main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(10)));

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
                .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(10)));
        }
    }

    if let Ok(mut s) = shared.lock() {
        s.capture_node_id = None;
    }
}

fn spawn_capture_session(target: TargetSpec) -> NapiResult<CaptureSession> {
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
                napi::Error::from_reason(format!("Failed to spawn PipeWire worker: {e}"))
            })?
    };

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            return Err(napi::Error::from_reason(reason));
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            return Err(napi::Error::from_reason(
                "Timed out waiting for PipeWire session",
            ));
        }
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if shared
            .lock()
            .ok()
            .is_some_and(|s| s.capture_node_id.is_some())
        {
            break;
        }
        if Instant::now() >= deadline {
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            return Err(napi::Error::from_reason(format!(
                "Virtual source '{CAPTURE_NODE_NAME}' did not appear"
            )));
        }
        thread::sleep(Duration::from_millis(10));
    }

    Ok(CaptureSession {
        stop,
        join,
        target_tx,
    })
}

fn stop_session(state: &mut CaptureState) {
    if let Some(session) = state.session.take() {
        session.stop.store(true, Ordering::SeqCst);
        let _ = session.join.join();
    }
    state.is_active = false;
}

fn is_valid_pid(pid: i32) -> bool {
    pid > 0 && std::path::Path::new(&format!("/proc/{pid}")).exists()
}

fn is_pipewire_daemon(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .is_ok_and(|c| matches!(c.trim(), "pipewire" | "pipewire-pulse" | "wireplumber"))
}

fn client_sec_pid(props: &DictRef) -> Option<i32> {
    let pid = props
        .get("pipewire.sec.pid")
        .and_then(|v| v.parse::<i32>().ok())
        .filter(|pid| is_valid_pid(*pid))?;
    if is_pipewire_daemon(pid) {
        None
    } else {
        Some(pid)
    }
}

fn resolve_pid_by_binary(binary: &str) -> Option<i32> {
    if binary.is_empty() {
        return None;
    }
    let lower = binary.to_lowercase();
    let candidates: Vec<i32> = iter_proc()
        .into_iter()
        .filter(|e| {
            e.comm.to_lowercase() == lower
                || std::path::Path::new(e.cmdline.split('\0').next().unwrap_or(""))
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.to_lowercase() == lower)
        })
        .map(|e| e.pid)
        .collect();
    (candidates.len() == 1).then_some(candidates[0])
}

fn resolve_pid_by_name(name: &str) -> Option<i32> {
    if name.is_empty() {
        return None;
    }
    let search_key = name.split_whitespace().next().filter(|s| s.len() >= 2)?;
    let search_lower = search_key.to_lowercase();

    iter_proc()
        .into_iter()
        .find(|e| {
            let comm_lower = e.comm.to_lowercase();
            comm_lower == search_lower
                || comm_lower.starts_with(&search_lower)
                || std::path::Path::new(e.cmdline.split('\0').next().unwrap_or(""))
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|base| {
                        let b = base.to_lowercase();
                        b == search_lower
                            || b.starts_with(&search_lower)
                            || search_lower.starts_with(&b)
                    })
        })
        .map(|e| e.pid)
}

/// Best-effort window/tab title for an audio stream node, used by the UI to tell
/// same-named applications apart. Browsers put the tab title in `media.name`;
/// values that just restate the app name or a generic role are not titles.
fn stream_window_title(props: &DictRef, app_name: &str) -> Option<String> {
    const GENERIC: [&str; 11] = [
        "playback",
        "playback stream",
        "playback streams",
        "playstream",
        "audio stream",
        "audiostream",
        "output",
        "output stream",
        "record",
        "audio",
        "stream",
    ];
    for key in [
        "media.name",
        "media.title",
        "window.title",
        "node.description",
    ] {
        let Some(value) = props.get(key).filter(|v| !v.is_empty()) else {
            continue;
        };
        if value == app_name || GENERIC.contains(&value.to_lowercase().as_str()) {
            continue;
        }
        return Some(value.into());
    }
    None
}

#[allow(
    clippy::too_many_lines,
    reason = "PipeWire registry event handling for client PID collection is inherently long and operates as a single logical unit; splitting would harm readability"
)]
fn collect_client_pids(
    core: &pipewire::core::CoreRc,
    main_loop: &pipewire::main_loop::MainLoopRc,
    registry: &pipewire::registry::Registry,
    apps: &Rc<RefCell<Vec<AudioApp>>>,
) -> HashMap<u32, u32> {
    let client_pids: Rc<RefCell<HashMap<u32, u32>>> = Rc::new(RefCell::new(HashMap::new()));
    let cp = client_pids.clone();
    let ap = apps.clone();
    // Node info props (e.g. `media.name`, where browsers put the tab title) are
    // not part of the registry advertisement — they only arrive after binding
    // the node. Audio stream nodes are bound here and kept until the second
    // sync round below has delivered their info events.
    let bound_nodes: Rc<RefCell<Vec<(pipewire::node::Node, pipewire::node::NodeListener)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let bindings = bound_nodes.clone();
    let bind_registry: Rc<RefCell<Option<pipewire::registry::RegistryRc>>> =
        Rc::new(RefCell::new(None));
    let reg_cell = bind_registry.clone();
    let core_rc = core.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props else { return };

            if global.type_ == ObjectType::Client {
                let app_name = props.get("application.name").unwrap_or("");
                let pid = client_sec_pid(props)
                    .or_else(|| {
                        props
                            .get("application.process.id")
                            .and_then(|v| v.parse::<i32>().ok())
                            .filter(|p| is_valid_pid(*p))
                    })
                    .or_else(|| {
                        (!app_name.is_empty())
                            .then(|| resolve_pid_by_name(app_name))
                            .flatten()
                    });
                if let Some(pid) = pid {
                    cp.borrow_mut().insert(global.id, pid.cast_unsigned());
                }
            }

            let media_class = props.get("media.class").unwrap_or("");
            let stream_name = props
                .get("application.name")
                .or_else(|| props.get("node.name"))
                .or_else(|| props.get("media.name"))
                .unwrap_or("");

            if media_class == "Stream/Output/Audio"
                && !stream_name.is_empty()
                && !stream_name.contains(CAPTURE_NODE_NAME)
            {
                let our_pid = std::process::id().cast_signed();
                let pid = props
                    .get("application.process.id")
                    .and_then(|v| v.parse::<i32>().ok())
                    .filter(|p| is_valid_pid(*p))
                    .or_else(|| {
                        let cid = props.get("client.id").and_then(|c| c.parse::<u32>().ok())?;
                        let p = *cp.borrow().get(&cid)?;
                        is_valid_pid(p.cast_signed()).then_some(p.cast_signed())
                    })
                    .or_else(|| {
                        resolve_pid_by_binary(props.get("application.process.binary").unwrap_or(""))
                    })
                    .or_else(|| resolve_pid_by_name(stream_name))
                    .unwrap_or(0);

                if pid == our_pid || stream_name.to_lowercase().contains("slopcast") {
                    return;
                }

                let mut list = ap.borrow_mut();
                if !list.iter().any(|a| a.id == global.id.cast_signed()) {
                    let client_id = props
                        .get("client.id")
                        .and_then(|v| v.parse::<u32>().ok())
                        .filter(|id| *id > 0);

                    list.push(AudioApp {
                        id: global.id.cast_signed(),
                        name: stream_name.into(),
                        process_id: pid,
                        bundle_id: None,
                        window_title: stream_window_title(props, stream_name),
                        client_id: client_id.map(u32::cast_signed),
                        media_title: None,
                    });
                    drop(list);

                    if reg_cell.borrow().is_none() {
                        *reg_cell.borrow_mut() = core_rc.get_registry_rc().ok();
                    }
                    let Some(node) = reg_cell
                        .borrow()
                        .as_ref()
                        .and_then(|reg| reg.bind::<pipewire::node::Node, _>(global).ok())
                    else {
                        return;
                    };
                    let node_id = global.id;
                    let apps_info = ap.clone();
                    let listener = node
                        .add_listener_local()
                        .info(move |info| {
                            let Some(props) = info.props() else { return };
                            let mut list = apps_info.borrow_mut();
                            let Some(app) = list.iter_mut().find(|a| a.id == node_id.cast_signed())
                            else {
                                return;
                            };
                            let title = stream_window_title(props, &app.name);
                            app.window_title = title;
                        })
                        .register();
                    bindings.borrow_mut().push((node, listener));
                }
            }
        })
        .register();

    sync_registry(core, main_loop);
    // Second round trip: bound node proxies deliver their info events.
    sync_registry(core, main_loop);
    client_pids.take()
}

/// Normalize a string for fuzzy matching: lowercase, strip non-alphanumeric.
fn norm(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Annotate audio apps with MPRIS now-playing titles.
/// MPRIS players are matched to apps by PID (when the player's bus-owner PID
/// matches the audio stream's `process_id`), then by fuzzy name containment
/// (identity/desktop-entry vs app name). Among matching players, the one with
/// `PlaybackStatus == "Playing"` wins; otherwise the first is used.
fn annotate_mpris_titles(apps: &mut [AudioApp]) {
    let players = mpris::list_players();
    if players.is_empty() {
        return;
    }
    for app in apps.iter_mut() {
        let candidates: Vec<&mpris::MprisPlayer> = players
            .iter()
            .filter(|p| {
                if p.pid
                    .is_some_and(|pid| pid > 0 && pid == app.process_id.cast_unsigned())
                {
                    return true;
                }
                let app_norm = norm(&app.name);
                let id_norm = norm(&p.identity);
                if contains_fuzzy(&app_norm, &id_norm) {
                    return true;
                }
                if let Some(de) = &p.desktop_entry {
                    let de_norm = norm(de);
                    if contains_fuzzy(&app_norm, &de_norm) {
                        return true;
                    }
                }
                false
            })
            .collect();
        let best = candidates
            .iter()
            .copied()
            .find(|p| p.playing)
            .or_else(|| candidates.first().copied());
        if let Some(player) = best
            && let Some(title) = &player.title
        {
            app.media_title = Some(title.into());
        }
    }
}

/// True when either string subsumes the other (min length 3).
fn contains_fuzzy(a: &str, b: &str) -> bool {
    if a.len() < 3 || b.len() < 3 {
        return false;
    }
    a.contains(b) || b.contains(a)
}

pub(crate) fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
    pipewire::init();
    let pw = pw_init().map_err(|e| napi::Error::from_reason(format!("PipeWire init: {e}")))?;
    let registry = pw
        .core
        .get_registry()
        .map_err(|e| napi::Error::from_reason(format!("Registry: {e}")))?;
    let apps = Rc::new(RefCell::new(Vec::<AudioApp>::new()));
    collect_client_pids(&pw.core, &pw.main_loop, &registry, &apps);
    let mut apps = apps.take();
    annotate_mpris_titles(&mut apps);
    Ok(apps)
}

pub(crate) fn start_audio_capture(target_app_id: &Either<String, i32>) -> NapiResult<bool> {
    let node_id = parse_target_id(target_app_id)?;
    let mut state_guard = CAPTURE_STATE
        .lock()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
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

pub(crate) fn switch_audio_capture(target_app_id: &Either<String, i32>) -> NapiResult<bool> {
    let node_id = parse_target_id(target_app_id)?;
    let mut state_guard = CAPTURE_STATE
        .lock()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let Some(state) = state_guard.as_mut() else {
        return Err(napi::Error::from_reason(
            "No active audio capture session to switch",
        ));
    };
    let Some(session) = &state.session else {
        return Err(napi::Error::from_reason(
            "No active audio capture session to switch",
        ));
    };

    let target = TargetSpec {
        node_id,
        system_audio: node_id.is_none(),
        ..TargetSpec::default()
    };
    session.target_tx.send(target).map_err(|e| {
        napi::Error::from_reason(format!("Failed to send audio target switch: {e}"))
    })?;
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

pub(crate) fn resolve_audio_app_for_x11_window(window_id: u32) -> Option<AudioApp> {
    // SAFETY: null selects the default display from $DISPLAY; failure returns
    // null, which is checked immediately.
    let display = unsafe { x11::xlib::XOpenDisplay(std::ptr::null()) };
    if display.is_null() {
        return None;
    }

    let atom_name = std::ffi::CString::new("_NET_WM_PID").ok()?;
    // SAFETY: `display` is a valid open display and `atom_name` is a valid
    // NUL-terminated C string; both outlive the call.
    let atom = unsafe { x11::xlib::XInternAtom(display, atom_name.as_ptr(), 1) };
    if atom == 0 {
        // SAFETY: balances the XOpenDisplay above; `display` is valid.
        unsafe { x11::xlib::XCloseDisplay(display) };
        return None;
    }

    let mut actual_type: x11::xlib::Atom = 0;
    let mut actual_format: std::os::raw::c_int = 0;
    let mut nitems: std::os::raw::c_ulong = 0;
    let mut bytes_after: std::os::raw::c_ulong = 0;
    let mut prop: *mut u8 = std::ptr::null_mut();

    // SAFETY: `display` and `atom` are valid; every out-pointer references a
    // live stack variable and `prop` starts null, so a failed call leaves
    // nothing to free.
    let status = unsafe {
        x11::xlib::XGetWindowProperty(
            display,
            x11::xlib::Window::from(window_id),
            atom,
            0,
            1,
            0,
            x11::xlib::XA_CARDINAL,
            &raw mut actual_type,
            &raw mut actual_format,
            &raw mut nitems,
            &raw mut bytes_after,
            &raw mut prop,
        )
    };

    let pid = if status == 0 && !prop.is_null() && nitems > 0 && actual_format == 32 {
        #[allow(
            clippy::cast_ptr_alignment,
            reason = "Xlib guarantees 4-byte alignment for 32-bit format data"
        )]
        // SAFETY: the call succeeded with one 32-bit item returned, so `prop`
        // points to at least one Xlib-allocated u32.
        Some(unsafe { *(prop as *const u32) })
    } else {
        None
    };

    if !prop.is_null() {
        // SAFETY: `prop` was allocated by Xlib in the XGetWindowProperty above.
        unsafe { x11::xlib::XFree(prop.cast::<std::ffi::c_void>()) };
    }
    // SAFETY: balances the XOpenDisplay above; `display` is still valid.
    unsafe { x11::xlib::XCloseDisplay(display) };

    let pid = pid?;
    let apps = list_audio_applications().ok()?;
    apps.into_iter()
        .find(|app| app.process_id == pid.cast_signed())
}

fn portal_window_name(props: &DictRef) -> Option<String> {
    for &key in &[
        "portal.screencast.application",
        "portal.screencast.title",
        "window.name",
        "pipewire.access.portal.app_id",
    ] {
        if let Some(v) = props.get(key).filter(|v| !v.is_empty()) {
            return Some(v.into());
        }
    }

    if let Some(v) = props
        .get("media.name")
        .filter(|v| !v.is_empty() && !v.contains("pipewire") && *v != "kwin_wayland")
    {
        return Some(v.into());
    }

    if let Some(v) = props.get("node.name").filter(|v| !v.is_empty()) {
        let lower = v.to_lowercase();
        if lower != "kwin_wayland" && lower != "gnome-shell" && !lower.contains("pipewire") {
            return Some(v.into());
        }
    }

    if let Some(v) = props.get("application.name").filter(|v| !v.is_empty()) {
        let lower = v.to_lowercase();
        if lower != "xdg-desktop-portal"
            && lower != "kwin_wayland"
            && lower != "gnome-shell"
            && !lower.contains("pipewire")
        {
            return Some(v.into());
        }
    }

    None
}

fn is_kde_screencast_window(suffix: &str) -> bool {
    if suffix.is_empty() {
        return false;
    }
    let parts: Vec<&str> = suffix.split('-').collect();
    if parts.len() < 2 {
        return true;
    }
    !(parts[0].chars().all(|c| c.is_ascii_uppercase())
        && parts[1].chars().all(|c| c.is_ascii_digit()))
}

fn resolve_kde_screencast_audio(media_name: &str) -> Option<AudioApp> {
    let suffix = media_name.strip_prefix("kwin-screencast-")?;
    if !is_kde_screencast_window(suffix) {
        return None;
    }
    let apps = list_audio_applications().ok()?;
    // KWin reports the exact PID over D-Bus; when the suffix is a window
    // caption rather than a UUID this fails and name matching applies.
    if let Some(pid) = kwin::window_uuid_to_pid(suffix).filter(|p| *p > 0)
        && let Some(app) = apps.iter().find(|app| app.process_id == pid.cast_signed())
    {
        return Some(app.clone());
    }
    crate::find_best_audio_match(&apps, suffix)
}

/// Snapshot of the `PipeWire` video nodes relevant to an active portal capture.
#[derive(Default)]
struct VideoScan {
    de: Option<&'static str>,
    source_type: Option<&'static str>,
    media_name: Option<String>,
    video_node_count: u32,
    kde_media_names: Vec<String>,
    capture_names: Vec<String>,
}

fn inspect_video_graph() -> Option<VideoScan> {
    pipewire::init();
    let pw = pw_init().ok()?;
    let registry = pw.core.get_registry().ok()?;

    let scan = Rc::new(RefCell::new(VideoScan::default()));
    let scan_clone = scan.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props else { return };
            let media_class = props.get("media.class").unwrap_or("");
            if !media_class.starts_with("Video/") && !media_class.starts_with("Stream/Output/Video")
            {
                return;
            }
            let mut scan = scan_clone.borrow_mut();
            scan.video_node_count += 1;
            let mn = props.get("media.name").unwrap_or("");
            if !mn.is_empty() {
                scan.media_name = Some(mn.into());
            }
            if let Some(suffix) = mn.strip_prefix("kwin-screencast-") {
                scan.de = Some("kde");
                scan.source_type = Some(if is_kde_screencast_window(suffix) {
                    "window"
                } else {
                    "monitor"
                });
                if !scan.kde_media_names.iter().any(|s| s == mn) {
                    scan.kde_media_names.push(mn.into());
                }
                return;
            }
            if props
                .get("portal.screencast.application")
                .is_some_and(|v| !v.is_empty())
            {
                scan.de = Some("gnome");
            }
            if let Some(name) = portal_window_name(props)
                && !scan.capture_names.contains(&name)
            {
                scan.capture_names.push(name);
            }
        })
        .register();

    sync_registry(&pw.core, &pw.main_loop);
    Some(scan.take())
}

fn resolve_from_video_scan(scan: &VideoScan) -> Option<AudioApp> {
    for mn in &scan.kde_media_names {
        if let Some(app) = resolve_kde_screencast_audio(mn) {
            return Some(app);
        }
    }
    let apps = list_audio_applications().ok()?;
    scan.capture_names
        .iter()
        .find_map(|name| crate::find_best_audio_match(&apps, name))
}

pub(crate) fn resolve_audio_app_for_captured_window() -> Option<AudioApp> {
    let scan = inspect_video_graph()?;
    resolve_from_video_scan(&scan)
}

pub(crate) fn get_capture_context() -> NapiResult<crate::CaptureContext> {
    let scan = inspect_video_graph()
        .ok_or_else(|| napi::Error::from_reason("PipeWire video node introspection failed"))?;
    let app = resolve_from_video_scan(&scan);
    Ok(crate::CaptureContext {
        de: scan.de.unwrap_or("unknown").into(),
        source_type: scan.source_type.unwrap_or("unknown").into(),
        media_name: scan.media_name,
        video_node_count: scan.video_node_count.cast_signed(),
        app,
    })
}

// ---------- Per-app audio level metering ----------

struct MeterStream {
    _stream: StreamRc,
    _listener: StreamListener<Arc<AtomicU32>>,
}

struct MeterSession {
    stop: Arc<AtomicBool>,
    join: thread::JoinHandle<()>,
    levels: Arc<Mutex<HashMap<u32, Arc<AtomicU32>>>>,
}

static METER_STATE: Mutex<Option<MeterSession>> = Mutex::new(None);

/// `EnumFormat` param offering exactly F32LE with native rate/channels, so meter
/// streams never force format conversion in the graph.
fn meter_format_param() -> Option<Vec<u8>> {
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

/// Creates a capture stream tapped into the given application node. AUTOCONNECT
/// links the app's output ports to the meter's input ports additively — the
/// app's existing links (speaker playback) are never touched.
fn meter_stream(
    core: &pipewire::core::CoreRc,
    node_id: u32,
    level: Arc<AtomicU32>,
) -> Option<MeterStream> {
    let stream = StreamRc::new(
        core.clone(),
        "slopcast-audio-meter",
        properties! {
            "media.class" => "Stream/Input/Audio",
            "node.name" => format!("slopcast-meter-{node_id}"),
            "node.description" => "Slopcast Audio Meter",
            "node.dont-move" => "true",
            "node.dont-reconnect" => "true",
            "node.dont-fallback" => "true",
        },
    )
    .ok()?;

    let listener = stream
        .add_local_listener_with_user_data(level)
        .process(|stream, level| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let mut peak: f32 = 0.0;
            for data in buffer.datas_mut() {
                let start = data.chunk().offset() as usize;
                let size = data.chunk().size() as usize;
                let Some(bytes) = data.data() else { continue };
                let end = start.saturating_add(size).min(bytes.len());
                let Some(slice) = bytes.get(start..end) else {
                    continue;
                };
                for sample in slice.chunks_exact(4) {
                    let mag =
                        f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]).abs();
                    peak = peak.max(mag);
                }
            }
            level.store(peak.to_bits(), Ordering::Relaxed);
        })
        .register()
        .ok()?;

    let values = meter_format_param()?;
    let pod = Pod::from_bytes(&values)?;
    let mut params = [pod];
    stream
        .connect(
            pipewire::spa::utils::Direction::Input,
            Some(node_id),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .ok()?;

    Some(MeterStream {
        _stream: stream,
        _listener: listener,
    })
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "run_meter_session is spawned in a thread::spawn(move || ...) closure that must own all Arc values"
)]
fn run_meter_session(
    stop: Arc<AtomicBool>,
    levels: Arc<Mutex<HashMap<u32, Arc<AtomicU32>>>>,
    ready_tx: mpsc::Sender<Result<(), String>>,
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

    let meters: Rc<RefCell<HashMap<u32, MeterStream>>> = Rc::new(RefCell::new(HashMap::new()));
    let our_pid = std::process::id();

    let _reg_listener = registry
        .add_listener_local()
        .global({
            let meters = meters.clone();
            let levels = levels.clone();
            let core = pw.core.clone();
            move |global| {
                let Some(props) = global.props else { return };
                if global.type_ != ObjectType::Node {
                    return;
                }
                if props.get("media.class") != Some("Stream/Output/Audio") {
                    return;
                }
                let name = props
                    .get("application.name")
                    .or_else(|| props.get("node.name"))
                    .or_else(|| props.get("media.name"))
                    .unwrap_or("");
                if name.is_empty()
                    || name.contains(CAPTURE_NODE_NAME)
                    || name.to_lowercase().contains("slopcast")
                {
                    return;
                }
                if props
                    .get("application.process.id")
                    .and_then(|v| v.parse::<u32>().ok())
                    == Some(our_pid)
                {
                    return;
                }

                let mut map = meters.borrow_mut();
                if map.contains_key(&global.id) {
                    return;
                }
                let level = Arc::new(AtomicU32::new(0));
                let Some(meter) = meter_stream(&core, global.id, level.clone()) else {
                    return;
                };
                map.insert(global.id, meter);
                drop(map);
                if let Ok(mut l) = levels.lock() {
                    l.insert(global.id, level);
                }
            }
        })
        .global_remove({
            let meters = meters.clone();
            let levels = levels.clone();
            move |id| {
                meters.borrow_mut().remove(&id);
                if let Ok(mut l) = levels.lock() {
                    l.remove(&id);
                }
            }
        })
        .register();

    let _ = ready_tx.send(Ok(()));

    while !stop.load(Ordering::SeqCst) {
        pw.main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(10)));
    }

    meters.borrow_mut().clear();
    if let Ok(mut l) = levels.lock() {
        l.clear();
    }
}

pub(crate) fn start_audio_metering() -> NapiResult<bool> {
    let mut guard = METER_STATE
        .lock()
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    if guard.is_some() {
        return Ok(true);
    }

    let stop = Arc::new(AtomicBool::new(false));
    let levels = Arc::new(Mutex::new(HashMap::new()));
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

    let join = {
        let stop = stop.clone();
        let levels = levels.clone();
        thread::Builder::new()
            .name("pw-audio-metering".into())
            .spawn(move || run_meter_session(stop, levels, ready_tx))
            .map_err(|e| {
                napi::Error::from_reason(format!("Failed to spawn metering worker: {e}"))
            })?
    };

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            return Err(napi::Error::from_reason(reason));
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            return Err(napi::Error::from_reason(
                "Timed out starting audio metering",
            ));
        }
    }

    *guard = Some(MeterSession { stop, join, levels });
    Ok(true)
}

pub(crate) fn stop_audio_metering() -> bool {
    let Ok(mut guard) = METER_STATE.lock() else {
        eprintln!("[meter] state lock poisoned; nothing to stop");
        return true;
    };
    if let Some(session) = guard.take() {
        session.stop.store(true, Ordering::SeqCst);
        let _ = session.join.join();
    }
    true
}

pub(crate) fn get_audio_levels() -> Vec<AudioAppLevel> {
    let Ok(guard) = METER_STATE.lock() else {
        eprintln!("[meter] state lock poisoned; reporting no levels");
        return Vec::new();
    };
    let Some(session) = guard.as_ref() else {
        return Vec::new();
    };
    let Ok(levels) = session.levels.lock() else {
        eprintln!("[meter] levels lock poisoned; reporting no levels");
        return Vec::new();
    };
    levels
        .iter()
        .map(|(id, level)| {
            let raw = f32::from_bits(level.load(Ordering::Relaxed));
            AudioAppLevel {
                id: (*id).cast_signed(),
                level: if raw.is_finite() {
                    f64::from(raw.clamp(0.0, 1.0))
                } else {
                    0.0
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_spa_function_accessible() {
        // SAFETY: pure lookup into a static translation table; any channel
        // enum value is valid input.
        let _ = unsafe { pipewire::spa::sys::spa_type_audio_channel_to_short_name(3) };
    }
}
