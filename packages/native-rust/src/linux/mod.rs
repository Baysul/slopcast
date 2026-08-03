use crate::{AudioApp, AudioAppWave};
use crossbeam_queue::ArrayQueue;
mod kwin;
mod mpris;
mod video;
use napi::threadsafe_function::ThreadsafeFunction;
use napi::{Either, Result as NapiResult};
use pipewire::properties::{PropertiesBox, properties};
use pipewire::registry::GlobalObject;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw, AudioInfoRawFlags};
use pipewire::spa::param::format::MediaType;
use pipewire::spa::param::{ParamType, format_utils};
use pipewire::spa::pod::{Object, Pod, Value};
use pipewire::spa::utils::{SpaTypes, dict::DictRef};
use pipewire::stream::{StreamFlags, StreamListener, StreamRc, StreamState};
use pipewire::types::ObjectType;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
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
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(50)));
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
    app_name: Option<String>,
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
        if self.app_name.is_none() {
            self.app_name.clone_from(&info.app_name);
        }
    }

    fn matches(&self, node_id: u32, info: &AppNodeInfo) -> bool {
        Some(node_id) == self.node_id
            || self.client_id.is_some_and(|c| info.client_id == Some(c))
            || self.pid.is_some_and(|p| info.pid == Some(p))
            || self.app_name.as_deref().is_some_and(|a| {
                info.app_name
                    .as_deref()
                    .is_some_and(|i| i.eq_ignore_ascii_case(a))
            })
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
    app_name: Option<String>,
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
        let app_name = props
            .get("application.name")
            .map(str::to_string)
            .filter(|n| !n.is_empty());
        Self {
            pid,
            binary,
            client_id,
            app_name,
        }
    }

    fn fallback_pid(&self, procs: &[ProcEntry]) -> Option<u32> {
        self.binary
            .as_deref()
            .and_then(|bin| resolve_pid_by_binary(procs, bin))
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
    procs: Vec<ProcEntry>,
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
            procs: iter_proc(),
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
                    info.pid = info.fallback_pid(&self.procs);
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
                    .then(|| resolve_pid_by_name(&self.procs, name))
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

    let setup_pcm_stream = |node_id: u32,
                            ready_cell: &Rc<RefCell<Option<mpsc::Sender<Result<(), String>>>>>|
     -> Option<(StreamRc, StreamListener<()>)> {
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

    if capture_node_id != 0 {
        if let Some((s, l)) = setup_pcm_stream(capture_node_id, &ready_tx_cell) {
            _pcm_stream = Some(s);
            _pcm_listener = Some(l);
        }
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

fn spawn_capture_session(target: TargetSpec) -> NapiResult<CaptureSession> {
    crate::audio_ring::start_audio_ring()
        .map_err(|e| napi::Error::from_reason(format!("Failed to start audio ring: {e}")))?;
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
                napi::Error::from_reason(format!("Failed to spawn PipeWire worker: {e}"))
            })?
    };

    match ready_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            crate::audio_ring::stop_audio_ring();
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            return Err(napi::Error::from_reason(reason));
        }
        Err(_) => {
            crate::audio_ring::stop_audio_ring();
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            return Err(napi::Error::from_reason(
                "Timed out waiting for PipeWire session",
            ));
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
        let _ = session.join.join();
    }
    state.is_active = false;
}

fn is_system_or_session_daemon(pid: u32) -> bool {
    if pid <= 1 {
        return true;
    }
    let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else {
        return true;
    };
    let name = comm.trim().to_lowercase();
    matches!(
        name.as_str(),
        "systemd"
            | "systemd-executor"
            | "init"
            | "dbus-daemon"
            | "dbus-broker"
            | "pipewire"
            | "pipewire-pulse"
            | "wireplumber"
            | "gnome-session"
            | "gnome-session-b"
            | "gnome-shell"
            | "plasmashell"
            | "kwin_wayland"
            | "kwin_x11"
            | "xdg-desktop-por"
            | "bash"
            | "zsh"
            | "sh"
            | "fish"
            | "tmux"
            | "screen"
    )
}

fn get_parent_pid(pid: u32) -> Option<u32> {
    if pid == 0 || is_system_or_session_daemon(pid) {
        return None;
    }
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("PPid:") {
            return rest.trim().parse::<u32>().ok();
        }
    }
    None
}

fn get_ancestor_pids(pid: u32) -> Vec<u32> {
    let mut ancestors = Vec::with_capacity(8);
    let mut current = pid;
    for _ in 0..16 {
        if current <= 1 || is_system_or_session_daemon(current) {
            break;
        }
        ancestors.push(current);
        let Some(ppid) = get_parent_pid(current) else {
            break;
        };
        if ppid == current || ppid <= 1 || is_system_or_session_daemon(ppid) {
            break;
        }
        current = ppid;
    }
    ancestors
}

fn are_processes_related(pid_a: u32, pid_b: u32) -> bool {
    if pid_a <= 1
        || pid_b <= 1
        || is_system_or_session_daemon(pid_a)
        || is_system_or_session_daemon(pid_b)
    {
        return false;
    }
    if pid_a == pid_b {
        return true;
    }
    let ancestors_a = get_ancestor_pids(pid_a);
    let ancestors_b = get_ancestor_pids(pid_b);
    ancestors_a.iter().any(|a| ancestors_b.contains(a))
}

fn is_generic_launcher(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    let norm = lower.replace('\\', "/");
    let stem = std::path::Path::new(&norm)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&norm)
        .trim_end_matches(".exe");
    matches!(
        stem,
        "steam"
            | "steamwebhelper"
            | "wine"
            | "wine64"
            | "wine64-preloader"
            | "wineserver"
            | "pv-bwrap"
            | "pressure-vessel"
            | "reaper"
            | "gamemoded"
            | "explorer"
            | "services"
            | "plugplay"
            | "winedevice"
            | "svchost"
            | "kwin_wayland"
            | "gnome-shell"
            | "xdg-desktop-portal"
    )
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

fn resolve_pid_by_binary(procs: &[ProcEntry], binary: &str) -> Option<i32> {
    if binary.is_empty() {
        return None;
    }
    let norm_bin = binary.replace('\\', "/");
    let lower = std::path::Path::new(&norm_bin)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&norm_bin)
        .to_lowercase();
    let lower_stem = lower.trim_end_matches(".exe");

    let candidates: Vec<i32> = procs
        .iter()
        .filter(|e| {
            let comm_lower = e.comm.to_lowercase();
            let comm_stem = comm_lower.trim_end_matches(".exe");
            if comm_lower == lower || comm_stem == lower_stem {
                return true;
            }
            if comm_lower.len() == 15
                && (lower.starts_with(&comm_lower) || lower_stem.starts_with(&comm_lower))
            {
                return true;
            }
            let norm_cmd = e.cmdline.replace('\\', "/");
            let cmd_bin = std::path::Path::new(norm_cmd.split('\0').next().unwrap_or(""))
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            let cmd_stem = cmd_bin.trim_end_matches(".exe");
            cmd_bin == lower || cmd_stem == lower_stem
        })
        .map(|e| e.pid)
        .collect();
    (candidates.len() == 1).then_some(candidates[0])
}

fn resolve_pid_by_name(procs: &[ProcEntry], name: &str) -> Option<i32> {
    if name.is_empty() {
        return None;
    }
    let search_key = name.split_whitespace().next().filter(|s| s.len() >= 2)?;
    let search_lower = search_key.to_lowercase();
    let search_stem = search_lower.trim_end_matches(".exe");

    procs
        .iter()
        .find(|e| {
            let comm_lower = e.comm.to_lowercase();
            let comm_stem = comm_lower.trim_end_matches(".exe");
            if comm_lower == search_lower
                || comm_stem == search_stem
                || comm_lower.starts_with(search_stem)
                || search_stem.starts_with(&comm_lower)
            {
                return true;
            }
            let norm_cmd = e.cmdline.replace('\\', "/");
            let base = std::path::Path::new(norm_cmd.split('\0').next().unwrap_or(""))
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            let b_stem = base.trim_end_matches(".exe");
            b_stem == search_stem
                || b_stem.starts_with(search_stem)
                || search_stem.starts_with(b_stem)
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

    let procs = Rc::new(iter_proc());
    let procs_cb = procs.clone();

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
                            .then(|| resolve_pid_by_name(&procs_cb, app_name))
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
                        resolve_pid_by_binary(
                            &procs_cb,
                            props.get("application.process.binary").unwrap_or(""),
                        )
                    })
                    .or_else(|| resolve_pid_by_name(&procs_cb, stream_name))
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

/// Full property dictionaries of every live `Stream/Output/Audio` node —
/// registry props merged with bound-node info props, the same view `pw-dump`
/// prints. Debugging aid for auto-resolve misses: the renderer logs these when
/// a capture starts so the captured window can be matched against real nodes.
pub(crate) fn dump_audio_sources() -> NapiResult<Vec<HashMap<String, String>>> {
    pipewire::init();
    let pw = pw_init().map_err(|e| napi::Error::from_reason(format!("PipeWire init: {e}")))?;
    let registry = pw
        .core
        .get_registry()
        .map_err(|e| napi::Error::from_reason(format!("Registry: {e}")))?;
    let nodes: Rc<RefCell<Vec<(u32, HashMap<String, String>)>>> = Rc::new(RefCell::new(Vec::new()));
    let bindings: Rc<RefCell<Vec<(pipewire::node::Node, pipewire::node::NodeListener)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let bind_core = pw.core.clone();
    let nodes_cb = nodes.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props else { return };
            if props.get("media.class") != Some("Stream/Output/Audio") {
                return;
            }
            let node_id = global.id;
            let merged: HashMap<String, String> = props
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let Some(node) = bind_core
                .get_registry_rc()
                .ok()
                .and_then(|reg| reg.bind::<pipewire::node::Node, _>(global).ok())
            else {
                return;
            };
            let nodes_cell = nodes_cb.clone();
            let listener = node
                .add_listener_local()
                .info(move |info| {
                    let Some(info_props) = info.props() else {
                        return;
                    };
                    let mut list = nodes_cell.borrow_mut();
                    let Some((_, map)) = list.iter_mut().find(|(id, _)| *id == node_id) else {
                        return;
                    };
                    for (k, v) in info_props.iter() {
                        map.insert(k.to_string(), v.to_string());
                    }
                })
                .register();
            bindings.borrow_mut().push((node, listener));
            nodes_cb.borrow_mut().push((node_id, merged));
        })
        .register();

    sync_registry(&pw.core, &pw.main_loop);
    // Second round trip: bound node proxies deliver their info events.
    sync_registry(&pw.core, &pw.main_loop);
    Ok(nodes.take().into_iter().map(|(_, map)| map).collect())
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

pub(crate) fn stop_audio_capture() -> NapiResult<bool> {
    let Ok(mut state_guard) = CAPTURE_STATE.lock() else {
        eprintln!("[capture] state lock poisoned; nothing to stop");
        return Ok(true);
    };
    if let Some(state) = state_guard.as_mut() {
        stop_session(state);
    }
    Ok(true)
}

pub(crate) fn is_audio_capture_active() -> NapiResult<bool> {
    let Ok(mut state_guard) = CAPTURE_STATE.lock() else {
        eprintln!("[capture] state lock poisoned; reporting inactive");
        return Ok(false);
    };
    let Some(state) = state_guard.as_mut() else {
        return Ok(false);
    };
    if state.session.as_ref().is_some_and(|s| s.join.is_finished()) {
        stop_session(state);
    }
    Ok(state.is_active)
}

pub(crate) fn resolve_audio_app_for_x11_window(window_id: u32) -> Option<AudioApp> {
    // SAFETY: null selects the default display from $DISPLAY; failure returns
    // null, which is checked immediately.
    let display = unsafe { x11::xlib::XOpenDisplay(std::ptr::null()) };
    if display.is_null() {
        return None;
    }

    let atom_name = match std::ffi::CString::new("_NET_WM_PID") {
        Ok(name) => name,
        Err(_) => {
            // SAFETY: balances the XOpenDisplay above; display is valid.
            unsafe { x11::xlib::XCloseDisplay(display) };
            return None;
        }
    };
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
    apps.into_iter().find(|app| {
        let app_pid = app.process_id.cast_unsigned();
        are_processes_related(app_pid, pid)
    })
}

fn extract_portal_window_name_from_map<F>(get_prop: F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    for &key in &[
        "portal.screencast.application",
        "portal.screencast.title",
        "window.name",
        "pipewire.access.portal.app_id",
    ] {
        if let Some(v) = get_prop(key).filter(|v| !v.is_empty()) {
            return Some(v);
        }
    }

    if let Some(v) = get_prop("media.name")
        .filter(|v| !v.is_empty() && !v.contains("pipewire") && v != "kwin_wayland")
    {
        return Some(v);
    }

    if let Some(v) = get_prop("node.name").filter(|v| !v.is_empty()) {
        let lower = v.to_lowercase();
        if lower != "kwin_wayland" && lower != "gnome-shell" && !lower.contains("pipewire") {
            return Some(v);
        }
    }

    if let Some(v) = get_prop("application.name").filter(|v| !v.is_empty()) {
        let lower = v.to_lowercase();
        if lower != "xdg-desktop-portal"
            && lower != "kwin_wayland"
            && lower != "gnome-shell"
            && !lower.contains("pipewire")
        {
            return Some(v);
        }
    }

    None
}

fn portal_window_name(props: &DictRef) -> Option<String> {
    extract_portal_window_name_from_map(|key| props.get(key).map(Into::into))
}

/// What a `kwin-screencast-<suffix>` stream captures, derived from the suffix.
/// `KWin` names window streams after the window's desktop file name, monitor
/// streams after the output (`DP-1`, `HDMI-A-1`, `eDP-1`, …), and region
/// streams after the geometry (`x,y WxH`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KdeScreencast {
    Window,
    Monitor,
    Region,
}

fn classify_kde_screencast(suffix: &str) -> KdeScreencast {
    // An empty suffix is a window whose desktop file name is empty (or a
    // restored portal session) — monitor and region names are never empty.
    if suffix.is_empty() {
        return KdeScreencast::Window;
    }
    if suffix.contains(',') {
        return KdeScreencast::Region;
    }
    // Output names end in a digit group after a dash (`DP-3`, `HDMI-A-1`),
    // while desktop file names carry dots or underscores (`org.kde.dolphin`,
    // `steam_app_default`) or no dash at all (`codium`, `signal`).
    let output_like = suffix.split('-').nth(1).is_some()
        && suffix
            .rsplit('-')
            .next()
            .is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        && !suffix.contains(['.', '_']);
    if output_like {
        KdeScreencast::Monitor
    } else {
        KdeScreencast::Window
    }
}

fn resolve_kde_screencast_audio(media_name: &str) -> Option<AudioApp> {
    let suffix = media_name.strip_prefix("kwin-screencast-")?;
    // Empty suffix means KWin didn't encode a window identity (e.g. restored
    // portal session with persist) — can't resolve any specific app. Monitors
    // and regions never map to a single application.
    if suffix.is_empty() || classify_kde_screencast(suffix) != KdeScreencast::Window {
        return None;
    }
    let apps = list_audio_applications().ok()?;
    // The suffix is the captured window's desktop file name; KWin reports the
    // owning PID and window caption for it over D-Bus.
    if let Some(win) = kwin::resolve_window(suffix) {
        // Layer 1: Process hierarchy & related process tree match — check if
        // the audio process and window owner process are identical, parent/child,
        // or share a launcher/container ancestor (e.g. Proton/Wine, Steam/bwrap).
        if let Some(app) = apps.iter().find(|app| {
            let app_pid = app.process_id.cast_unsigned();
            are_processes_related(app_pid, win.pid)
        }) {
            return Some(app.clone());
        }

        // Layer 1b: Match audio app by checking process binary/cmdline of running
        // audio apps against the suffix or window caption (normalizing Windows backslashes
        // and .exe extensions).
        let clean_suffix = suffix.trim_end_matches(".exe").trim_end_matches(".EXE");
        for app in &apps {
            let app_pid = app.process_id;
            if app_pid <= 0 {
                continue;
            }
            if let Ok(cmdline) = std::fs::read_to_string(format!("/proc/{app_pid}/cmdline")) {
                let norm_cmd = cmdline.replace('\\', "/");
                let exe = std::path::Path::new(norm_cmd.split('\0').next().unwrap_or(""))
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .trim_end_matches(".exe")
                    .trim_end_matches(".EXE");
                if !exe.is_empty()
                    && (exe.eq_ignore_ascii_case(clean_suffix)
                        || (!win.caption.is_empty()
                            && win.caption.to_lowercase().contains(&exe.to_lowercase())))
                {
                    return Some(app.clone());
                }
            }
        }

        // Layer 2: Match by non-generic window process candidates.
        let comm = std::fs::read_to_string(format!("/proc/{}/comm", win.pid)).ok();
        let cmdline = std::fs::read_to_string(format!("/proc/{}/cmdline", win.pid)).ok();
        let mut candidates: Vec<String> = Vec::new();
        if let Some(ref comm) = comm {
            let name = comm.trim();
            if !name.is_empty() && !is_generic_launcher(name) {
                candidates.push(name.to_string());
            }
        }
        if let Some(ref cmdline) = cmdline {
            let norm_cmd = cmdline.replace('\\', "/");
            let binary = norm_cmd.split('\0').next().unwrap_or("").trim();
            if !binary.is_empty()
                && let Some(stem) = std::path::Path::new(binary)
                    .file_stem()
                    .and_then(|s| s.to_str())
                && !stem.is_empty()
                && !is_generic_launcher(stem)
                && !candidates.iter().any(|c| c == stem)
            {
                candidates.push(stem.to_string());
            }
        }
        for candidate in &candidates {
            if let Some(app) = crate::find_best_audio_match(&apps, candidate) {
                return Some(app);
            }
        }

        // Layer 3: Window caption match.
        if !win.caption.is_empty()
            && let Some(app) = crate::find_best_audio_match(&apps, &win.caption)
        {
            return Some(app);
        }
    }

    // Layer 4: Match desktop file name suffix if not generic.
    if !is_generic_launcher(suffix) {
        if let Some(app) = crate::find_best_audio_match(&apps, suffix) {
            return Some(app);
        }
    }

    None
}

/// Snapshot of the `PipeWire` video nodes relevant to an active portal capture.
#[derive(Default)]
struct VideoScan {
    de: Option<&'static str>,
    source_type: Option<&'static str>,
    media_name: Option<String>,
    video_node_count: u32,
    /// Highest `object.serial` observed among screencast nodes — ensures the
    /// active stream (most recently created) is chosen over lingering nodes.
    highest_serial: u64,
    /// `(object.serial, media.name)` per KDE screencast node — the serial
    /// orders streams by creation time.
    kde_media_names: Vec<(u64, String)>,
    capture_names: Vec<String>,
    screencast_node_id: Option<u32>,
    /// xdg-desktop-portal screencast metadata (`portal.screencast.*`) of the
    /// captured window, read off the screencast video node.
    portal_props: Option<HashMap<String, String>>,
}

/// Collect only the xdg-desktop-portal metadata keys (`portal.screencast.*`)
/// from the screencast video node's registry + info props — the portal's own
/// record of the captured window, without dumping the whole PipeWire node.
fn merge_portal_props(
    out: &mut Option<HashMap<String, String>>,
    registry: &HashMap<String, String>,
    info: &DictRef,
) {
    let merged = registry
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .chain(info.iter())
        .filter(|(k, _)| k.starts_with("portal.screencast."))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    *out = Some(merged);
}

fn inspect_video_graph() -> Option<VideoScan> {
    pipewire::init();
    let pw = pw_init().ok()?;
    let registry = pw.core.get_registry().ok()?;

    let scan = Rc::new(RefCell::new(VideoScan::default()));
    let bindings: Rc<RefCell<Vec<(pipewire::node::Node, pipewire::node::NodeListener)>>> =
        Rc::new(RefCell::new(Vec::new()));
    let bind_core = pw.core.clone();
    let bind_registry: Rc<RefCell<Option<pipewire::registry::RegistryRc>>> =
        Rc::new(RefCell::new(None));
    let reg_cell = bind_registry.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global({
            let scan = scan.clone();
            let bind_core = bind_core.clone();
            let reg_cell = reg_cell.clone();
            let bindings = bindings.clone();
            move |global| {
                let Some(props) = global.props else { return };
                let media_class = props.get("media.class").unwrap_or("");
                // Only producer/output nodes carry capture-source metadata;
                // consumer nodes (Stream/Input/Video) are not relevant.
                if !media_class.starts_with("Video/")
                    && !media_class.starts_with("Stream/Output/Video")
                {
                    return;
                }
                let scan_info = scan.clone();
                let mut scan = scan.borrow_mut();
                scan.video_node_count += 1;

                let has_reg = reg_cell.borrow().is_some();
                if !has_reg {
                    *reg_cell.borrow_mut() = bind_core.get_registry_rc().ok();
                }
                let reg_binding = reg_cell.borrow();
                let Some(reg) = reg_binding.as_ref() else {
                    return;
                };
                let Ok(node) = reg.bind::<pipewire::node::Node, _>(global) else {
                    return;
                };
                let media_class_owned: String = media_class.into();
                let node_id = global.id;
                let registry_props: HashMap<String, String> = props
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                let listener = node
                    .add_listener_local()
                    .info(move |info| {
                        let Some(p) = info.props() else { return };
                        let mut scan = scan_info.borrow_mut();
                        let mn = p.get("media.name").unwrap_or("");
                        let serial = p
                            .get("object.serial")
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(0);

                        if let Some(suffix) = mn.strip_prefix("kwin-screencast-") {
                            scan.de = Some("kde");
                            if !scan.kde_media_names.iter().any(|(_, s)| s == mn) {
                                scan.kde_media_names.push((serial, mn.into()));
                            }
                            if serial >= scan.highest_serial {
                                scan.highest_serial = serial;
                                scan.media_name = Some(mn.into());
                                scan.source_type = Some(match classify_kde_screencast(suffix) {
                                    KdeScreencast::Window => "window",
                                    KdeScreencast::Monitor => "monitor",
                                    KdeScreencast::Region => "region",
                                });
                                scan.screencast_node_id = Some(node_id);
                                merge_portal_props(&mut scan.portal_props, &registry_props, p);
                            }
                            return;
                        }
                        if p.get("portal.screencast.application")
                            .is_some_and(|v| !v.is_empty())
                        {
                            scan.de = Some("gnome");
                            if serial >= scan.highest_serial {
                                scan.highest_serial = serial;
                                scan.media_name =
                                    if mn.is_empty() { None } else { Some(mn.into()) };
                                scan.source_type = Some("window");
                                scan.screencast_node_id = Some(node_id);
                                merge_portal_props(&mut scan.portal_props, &registry_props, p);
                            }
                            if let Some(name) = portal_window_name(p)
                                && !scan.capture_names.contains(&name)
                            {
                                scan.capture_names.push(name);
                            }
                        } else if media_class_owned == "Video/Source" && scan.source_type.is_none()
                        {
                            let has_app_meta =
                                p.get("application.name").is_some_and(|v| !v.is_empty())
                                    || p.get("pipewire.access.portal.app_id")
                                        .is_some_and(|v| !v.is_empty())
                                    || p.get("window.name").is_some_and(|v| !v.is_empty());
                            if !has_app_meta && serial >= scan.highest_serial {
                                scan.highest_serial = serial;
                                scan.source_type = Some("monitor");
                            }
                        }
                    })
                    .register();
                bindings.borrow_mut().push((node, listener));
            }
        })
        .register();

    sync_registry(&pw.core, &pw.main_loop);
    // Second round for bound nodes to deliver their info events.
    sync_registry(&pw.core, &pw.main_loop);
    Some(scan.take())
}

fn resolve_from_video_scan(scan: &VideoScan) -> Option<AudioApp> {
    // 1. For KDE screencast streams (`kwin-screencast-*`), evaluate strictly
    // the single active (most recently created) stream based on object.serial.
    if !scan.kde_media_names.is_empty() {
        let mut kde_names: Vec<&(u64, String)> = scan.kde_media_names.iter().collect();
        kde_names.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        let active_mn = &kde_names[0].1;

        // If the active stream is a monitor or region, return None directly.
        if let Some(suffix) = active_mn.strip_prefix("kwin-screencast-") {
            let class = classify_kde_screencast(suffix);
            if class == KdeScreencast::Monitor || class == KdeScreencast::Region {
                return None;
            }
        }

        // Resolve audio only for the active stream. If it returns None (e.g. VSCodium
        // has no audio process), return None directly — do NOT loop through older
        // lingering screencast streams (e.g. Steam/FFXIV).
        return resolve_kde_screencast_audio(active_mn);
    }

    // 2. Monitors and regions for non-KDE environments are screen displays — return None.
    if scan.source_type == Some("monitor") || scan.source_type == Some("region") {
        return None;
    }

    // 3. For GNOME / XDG portal screencast streams, match against running audio apps.
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
    let node_id = scan.screencast_node_id;
    let app = resolve_from_video_scan(&scan);
    let (window_pid, window_caption) = if scan.de == Some("kde") {
        scan.media_name
            .as_deref()
            .and_then(|mn| mn.strip_prefix("kwin-screencast-"))
            .filter(|suffix| classify_kde_screencast(suffix) == KdeScreencast::Window)
            .and_then(kwin::resolve_window)
            .map_or((None, None), |w| {
                (Some(w.pid.cast_signed()), Some(w.caption))
            })
    } else {
        (None, None)
    };
    Ok(crate::CaptureContext {
        de: scan.de.unwrap_or("unknown").into(),
        source_type: scan.source_type.unwrap_or("unknown").into(),
        media_name: scan.media_name,
        video_node_count: scan.video_node_count.cast_signed(),
        app,
        screencast_node_id: node_id,
        portal_props: scan.portal_props,
        window_pid,
        window_caption,
    })
}

// ---------- Per-app audio waveform metering ----------

const METER_WAVE_COLUMNS: usize = 96;
/// ~85 ms of mono audio at 48 kHz: long enough for an organic, filled
/// waveform strip rather than a jittery oscilloscope trace.
const METER_WAVE_WINDOW: usize = 4096;
const METER_WAVE_INTERVAL_MS: u64 = 33;
const METER_DEFAULT_RATE: u32 = 48_000;
const METER_DEFAULT_CHANNELS: u16 = 2;
// A meter whose ring has received no samples for this long is considered
// paused: its rolling window holds stale audio, so the pass publishes silence
// instead of re-decimating old samples.
const METER_STALE_MS: u64 = 150;
// Mono sample queue between the process callback and the wave pass. Holds over
// two worst-case pass gaps (~2 × 50 ms × 48 kHz); a stalled pass only drops
// the newest samples, which the decimated waveform renders invisible.
const METER_RING_CAPACITY: usize = 4096;

/// Per-app meter state shared between the worker thread and the JS thread.
/// The process callback pushes mono samples into the lock-free ring and the
/// wave pass drains it; `wave` is published to the JS thread under its mutex.
struct MeterLevel {
    samples: ArrayQueue<f32>,
    rate: AtomicU32,
    channels: AtomicU16,
    /// `METER_WAVE_COLUMNS` interleaved (min, max) amplitude pairs in [-1, 1].
    wave: Mutex<Vec<f32>>,
}

impl MeterLevel {
    fn new() -> Self {
        Self {
            samples: ArrayQueue::new(METER_RING_CAPACITY),
            rate: AtomicU32::new(METER_DEFAULT_RATE),
            channels: AtomicU16::new(METER_DEFAULT_CHANNELS),
            wave: Mutex::new(vec![0.0; METER_WAVE_COLUMNS * 2]),
        }
    }
}

struct MeterStream {
    _stream: StreamRc,
    _listener: StreamListener<Arc<MeterLevel>>,
    level: Arc<MeterLevel>,
    /// Rolling mono window drained from `level.samples`, capped at
    /// `METER_WAVE_WINDOW`. Worker-thread only, so it is a plain `Vec`.
    window: Vec<f32>,
    /// Pass timestamp of the last time new samples were drained from the ring.
    /// Worker-thread only.
    last_feed: Instant,
}

struct MeterSession {
    stop: Arc<AtomicBool>,
    join: thread::JoinHandle<()>,
    levels: Arc<Mutex<HashMap<u32, Arc<MeterLevel>>>>,
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

/// Downmixes the meter's latest buffer quantum to mono and pushes it into the
/// lock-free ring. Pushing drops the newest samples when the ring is full (a
/// stalled wave pass) rather than blocking; the decimated envelope renders
/// such a few-ms gap invisible.
fn meter_process_quantum(stream: &pipewire::stream::Stream, level: &Arc<MeterLevel>) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let channels = usize::from(level.channels.load(Ordering::Relaxed).max(1));
    let inv_channels = 1.0 / f32::from(level.channels.load(Ordering::Relaxed).max(1));
    // Negotiated F32LE buffers are interleaved in a single data; the
    // multi-data branch is a fallback that treats each data as its own mono
    // stream.
    let interleaved = buffer.datas_mut().len() <= 1;
    for data in buffer.datas_mut() {
        let start = data.chunk().offset() as usize;
        let size = data.chunk().size() as usize;
        let Some(bytes) = data.data() else { continue };
        let end = start.saturating_add(size).min(bytes.len());
        let Some(slice) = bytes.get(start..end) else {
            continue;
        };
        if interleaved && channels > 1 {
            for frame in slice.chunks_exact(channels * 4) {
                let mut sum = 0.0f32;
                for ch in 0..channels {
                    let off = ch * 4;
                    sum += f32::from_le_bytes([
                        frame[off],
                        frame[off + 1],
                        frame[off + 2],
                        frame[off + 3],
                    ]);
                }
                if level.samples.push(sum * inv_channels).is_err() {
                    return;
                }
            }
        } else {
            for sample in slice.chunks_exact(4) {
                let s = f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
                if level.samples.push(s).is_err() {
                    return;
                }
            }
        }
    }
}

/// Creates a capture stream tapped into the given application node. AUTOCONNECT
/// links the app's output ports to the meter's input ports additively — the
/// app's existing links (speaker playback) are never touched.
///
/// The process callback only downmixes each quantum to mono and queues it; all
/// FFT work happens on the worker thread between loop iterations, so the audio
/// path stays allocation-free.
fn meter_stream(
    core: &pipewire::core::CoreRc,
    node_id: u32,
    level: Arc<MeterLevel>,
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
        .add_local_listener_with_user_data(level.clone())
        .param_changed(|_stream, level, _id, param| {
            let Some(pod) = param else { return };
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
            let rate = info.rate();
            if rate > 0 {
                level.rate.store(rate, Ordering::Relaxed);
            }
            let channels = u16::try_from(info.channels()).unwrap_or(METER_DEFAULT_CHANNELS);
            if channels > 0 {
                level.channels.store(channels, Ordering::Relaxed);
            }
        })
        .process(|stream, level| meter_process_quantum(stream, level))
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
        level,
        window: Vec::with_capacity(METER_WAVE_WINDOW),
        last_feed: Instant::now(),
    })
}

/// Decimates every meter's sample window into `METER_WAVE_COLUMNS` interleaved
/// (min, max) amplitude pairs and publishes them. Runs on the worker thread
/// only; raw per-column extrema need no smoothing — silence renders as a flat
/// line, and overlapping windows (~60% at 48 kHz) keep motion continuous.
/// Meters whose ring has been silent for `METER_STALE_MS` publish zeros: their
/// window still holds pre-pause audio, and re-decimating it would pin the bars
/// instead of letting them flatline.
fn run_wave_pass(meters: &mut HashMap<u32, MeterStream>) {
    for meter in meters.values_mut() {
        let level = &meter.level;
        let mut fed = false;
        while let Some(sample) = level.samples.pop() {
            meter.window.push(sample);
            fed = true;
        }
        let overflow = meter.window.len().saturating_sub(METER_WAVE_WINDOW);
        if overflow > 0 {
            meter.window.drain(0..overflow);
        }
        if meter.window.is_empty() {
            continue;
        }

        let now = Instant::now();
        if fed {
            meter.last_feed = now;
        }
        let stale = now.duration_since(meter.last_feed) > Duration::from_millis(METER_STALE_MS);

        let Ok(mut wave) = level.wave.lock() else {
            continue;
        };
        if stale {
            wave.fill(0.0);
            continue;
        }
        let len = meter.window.len();
        let bucket = len.div_ceil(METER_WAVE_COLUMNS);
        for c in 0..METER_WAVE_COLUMNS {
            let start = c * bucket;
            let end = ((c + 1) * bucket).min(len);
            if start >= end {
                wave[c * 2] = 0.0;
                wave[c * 2 + 1] = 0.0;
                continue;
            }
            let mut min = f32::MAX;
            let mut max = f32::MIN;
            for sample in &meter.window[start..end] {
                min = min.min(*sample);
                max = max.max(*sample);
            }
            wave[c * 2] = min;
            wave[c * 2 + 1] = max;
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "run_meter_session is spawned in a thread::spawn(move || ...) closure that must own all Arc values"
)]
fn run_meter_session(
    stop: Arc<AtomicBool>,
    levels: Arc<Mutex<HashMap<u32, Arc<MeterLevel>>>>,
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
                let level = Arc::new(MeterLevel::new());
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

    let mut last_wave = Instant::now();

    let _ = ready_tx.send(Ok(()));

    while !stop.load(Ordering::SeqCst) {
        // 50ms bound only limits idle sleeping; with live audio the loop wakes
        // on stream buffer events as they arrive, so metering latency is
        // unaffected while a tight timeout would just spin the thread.
        pw.main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(50)));

        let now = Instant::now();
        if now.duration_since(last_wave) >= Duration::from_millis(METER_WAVE_INTERVAL_MS) {
            last_wave = now;
            let mut meter_map = meters.borrow_mut();
            run_wave_pass(&mut meter_map);
        }
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

pub(crate) fn stop_audio_metering() -> NapiResult<bool> {
    let Ok(mut guard) = METER_STATE.lock() else {
        eprintln!("[meter] state lock poisoned; nothing to stop");
        return Ok(true);
    };
    if let Some(session) = guard.take() {
        session.stop.store(true, Ordering::SeqCst);
        let _ = session.join.join();
    }
    Ok(true)
}

pub(crate) fn get_audio_wave() -> NapiResult<Vec<AudioAppWave>> {
    let Ok(guard) = METER_STATE.lock() else {
        eprintln!("[meter] state lock poisoned; reporting no wave");
        return Ok(Vec::new());
    };
    let Some(session) = guard.as_ref() else {
        return Ok(Vec::new());
    };
    let Ok(levels) = session.levels.lock() else {
        eprintln!("[meter] levels lock poisoned; reporting no wave");
        return Ok(Vec::new());
    };
    // Non-destructive read: the waveform is a continuous signal, so sampling
    // it at the push cadence loses nothing and main-process timing jitter
    // cannot distort the values.
    let mut out = Vec::with_capacity(levels.len());
    for (id, level) in levels.iter() {
        let Ok(wave) = level.wave.lock() else {
            eprintln!("[meter] wave lock poisoned; skipping app {id}");
            continue;
        };
        out.push(AudioAppWave {
            id: (*id).cast_signed(),
            columns: wave.iter().map(|&v| f64::from(v)).collect(),
        });
    }
    Ok(out)
}

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

// ── DMA-BUF Video Frame Callback ─────────────────────────────────────────
// Registered by the Electron main process to forward video frames from the
// PipeWire capture thread to native-livekit's VideoTrackSource.
static DMA_BUF_CALLBACK: Mutex<
    Option<Arc<ThreadsafeFunction<(i32, i32, i32, i32, i32, i32), ()>>>,
> = Mutex::new(None);

pub(crate) fn set_dmabuf_callback(
    callback: std::sync::Arc<ThreadsafeFunction<(i32, i32, i32, i32, i32, i32), ()>>,
) -> napi::Result<()> {
    let Ok(mut guard) = DMA_BUF_CALLBACK.lock() else {
        return Err(napi::Error::from_reason("Lock poisoned"));
    };
    *guard = Some(callback);
    Ok(())
}

pub(crate) fn invoke_dmabuf_callback(fd: i32, width: i32, height: i32, format: i32, pts_ns: i64) {
    let cb = {
        let Ok(guard) = DMA_BUF_CALLBACK.lock() else {
            // Close duplicated descriptor if lock fails.
            unsafe { libc::close(fd) };
            return;
        };
        guard.clone()
    };

    let Some(cb) = cb else {
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
    if let Ok(mut guard) = DMA_BUF_CALLBACK.lock() {
        *guard = None;
    }
}

// ── Video Capture (Linux PipeWire) ──────────────────────────────────────

pub(crate) use video::{start_video_capture, stop_video_capture};

#[cfg(test)]
mod tests {
    use super::{
        KdeScreencast, ProcEntry, VideoScan, are_processes_related, classify_kde_screencast,
        extract_portal_window_name_from_map, is_generic_launcher, resolve_from_video_scan,
        resolve_pid_by_binary, resolve_pid_by_name,
    };
    use crate::AudioApp;
    use std::collections::HashMap;

    #[test]
    fn test_spa_function_accessible() {
        // SAFETY: pure lookup into a static translation table; any channel
        // enum value is valid input.
        let _ = unsafe { pipewire::spa::sys::spa_type_audio_channel_to_short_name(3) };
    }

    #[test]
    fn identifies_generic_launchers() {
        assert!(is_generic_launcher("steam"));
        assert!(is_generic_launcher("wine64-preloader"));
        assert!(is_generic_launcher("wineserver"));
        assert!(is_generic_launcher("pv-bwrap"));
        assert!(is_generic_launcher(
            "C:\\windows\\system32\\wine64-preloader.exe"
        ));

        assert!(!is_generic_launcher("ZenlessZoneZero.exe"));
        assert!(!is_generic_launcher("Z:\\games\\ZenlessZoneZero.exe"));
        assert!(!is_generic_launcher("ffxiv_dx11.exe"));
        assert!(!is_generic_launcher("discord"));
        assert!(!is_generic_launcher("spotify"));
    }

    #[test]
    fn verifies_process_descendant_check() {
        let our_pid = std::process::id();
        assert!(are_processes_related(our_pid, our_pid));
        assert!(!are_processes_related(0, our_pid));
        assert!(!are_processes_related(our_pid, 0));
        assert!(!are_processes_related(1, 1));
    }

    #[test]
    fn resolves_pid_by_binary_with_wine_cmdline_and_truncated_comm() {
        let procs = vec![
            ProcEntry {
                pid: 100,
                comm: "steam".into(),
                cmdline: "/usr/bin/steam\0".into(),
            },
            ProcEntry {
                pid: 200,
                comm: "ZenlessZoneZero".into(), // 15-char kernel truncation
                cmdline:
                    "Z:\\SteamLibrary\\steamapps\\common\\ZenlessZoneZero\\ZenlessZoneZero.exe\0"
                        .into(),
            },
            ProcEntry {
                pid: 234,
                comm: "ffxiv_dx11.exe".into(),
                cmdline:
                    "Z:\\SteamLibrary\\steamapps\\common\\FINAL FANTASY XIV\\game\\ffxiv_dx11.exe\0"
                        .into(),
            },
        ];

        // Should match exact binary name despite backslashes and .exe
        assert_eq!(
            resolve_pid_by_binary(&procs, "ZenlessZoneZero.exe"),
            Some(200)
        );
        assert_eq!(
            resolve_pid_by_binary(&procs, "Z:\\path\\ZenlessZoneZero.exe"),
            Some(200)
        );
        assert_eq!(resolve_pid_by_name(&procs, "ZenlessZoneZero"), Some(200));
        assert_eq!(resolve_pid_by_binary(&procs, "ffxiv_dx11.exe"), Some(234));
    }

    #[test]
    fn matches_pipewire_portal_screencast_node_properties() {
        // Sample PipeWire properties from xdg-desktop-portal / getDisplayMedia() window pickers
        let mut ffxiv_props = HashMap::new();
        ffxiv_props.insert("portal.screencast.title", "FINAL FANTASY XIV".to_string());
        ffxiv_props.insert(
            "portal.screencast.application",
            "ffxiv_dx11.exe".to_string(),
        );
        ffxiv_props.insert("window.name", "FINAL FANTASY XIV".to_string());

        let name = extract_portal_window_name_from_map(|k| ffxiv_props.get(k).cloned());
        assert_eq!(name.as_deref(), Some("ffxiv_dx11.exe"));

        let mut zzz_props = HashMap::new();
        zzz_props.insert("window.name", "Zenless Zone Zero".to_string());
        zzz_props.insert("application.name", "ZenlessZoneZero.exe".to_string());

        let name = extract_portal_window_name_from_map(|k| zzz_props.get(k).cloned());
        assert_eq!(name.as_deref(), Some("Zenless Zone Zero"));

        let mut portal_wrapper = HashMap::new();
        portal_wrapper.insert("application.name", "xdg-desktop-portal".to_string());
        portal_wrapper.insert("node.name", "pipewire_system".to_string());

        let name = extract_portal_window_name_from_map(|k| portal_wrapper.get(k).cloned());
        assert_eq!(name, None);
    }

    #[test]
    fn matches_pipewire_getdisplaymedia_video_scan_objects() {
        let apps = vec![
            AudioApp {
                id: 234,
                name: "ffxiv_dx11.exe".to_string(),
                process_id: 54321,
                bundle_id: None,
                window_title: Some("Playback".to_string()),
                client_id: Some(230),
                media_title: None,
            },
            AudioApp {
                id: 101,
                name: "ZenlessZoneZero.exe".to_string(),
                process_id: 60000,
                bundle_id: None,
                window_title: None,
                client_id: Some(100),
                media_title: None,
            },
        ];

        // Simulated PipeWire VideoScan from a getDisplayMedia() portal screencast
        let scan = VideoScan {
            de: Some("kde"),
            source_type: Some("window"),
            media_name: Some("kwin-screencast-ffxiv_dx11.exe".to_string()),
            video_node_count: 1,
            highest_serial: 100,
            kde_media_names: vec![(100, "kwin-screencast-ffxiv_dx11.exe".to_string())],
            capture_names: vec!["FINAL FANTASY XIV".to_string()],
            screencast_node_id: Some(50),
            portal_props: None,
        };

        let matched = scan
            .capture_names
            .iter()
            .find_map(|name| crate::find_best_audio_match(&apps, name));
        assert_eq!(matched.map(|a| a.id), Some(234));

        // Matching Zenless Zone Zero by window name
        let matched_zzz = crate::find_best_audio_match(&apps, "Zenless Zone Zero");
        assert_eq!(matched_zzz.map(|a| a.id), Some(101));
    }

    #[test]
    fn classifies_kde_window_names() {
        // KWin names window streams after the window's desktop file name.
        for suffix in [
            "codium",
            "signal",
            "discord",
            "org.kde.dolphin",
            "brave-origin",
            "spotify-launcher",
            "com.mitchellh.ghostty",
            "io.ente.auth",
            "gitbutler-tauri",
            "steam_app_default",
            // Window with no desktop file name at all.
            "",
        ] {
            assert_eq!(
                classify_kde_screencast(suffix),
                KdeScreencast::Window,
                "{suffix:?} must classify as a window"
            );
        }
    }

    #[test]
    fn classifies_kde_monitor_names() {
        // KWin names monitor streams after the output connector.
        for suffix in ["DP-1", "DP-3", "HDMI-A-1", "eDP-1", "DVI-D-1", "Virtual-1"] {
            assert_eq!(
                classify_kde_screencast(suffix),
                KdeScreencast::Monitor,
                "{suffix:?} must classify as a monitor"
            );
        }
    }

    #[test]
    fn classifies_kde_region_name() {
        assert_eq!(
            classify_kde_screencast("0,0 1920x1080"),
            KdeScreencast::Region
        );
    }

    #[test]
    fn resolve_from_video_scan_rejects_monitors_and_regions() {
        let monitor_scan = VideoScan {
            de: Some("kde"),
            source_type: Some("monitor"),
            media_name: Some("kwin-screencast-DP-3".to_string()),
            video_node_count: 18,
            highest_serial: 224,
            kde_media_names: vec![(224, "kwin-screencast-DP-3".to_string())],
            capture_names: vec!["Spotify".to_string()],
            screencast_node_id: Some(224),
            portal_props: None,
        };
        assert!(resolve_from_video_scan(&monitor_scan).is_none());

        let region_scan = VideoScan {
            de: Some("kde"),
            source_type: Some("region"),
            media_name: Some("kwin-screencast-0,0 1920x1080".to_string()),
            video_node_count: 5,
            highest_serial: 225,
            kde_media_names: vec![(225, "kwin-screencast-0,0 1920x1080".to_string())],
            capture_names: vec!["Spotify".to_string()],
            screencast_node_id: Some(225),
            portal_props: None,
        };
        assert!(resolve_from_video_scan(&region_scan).is_none());
    }

    #[test]
    fn resolve_from_video_scan_does_not_fall_through_kde_window_to_unrelated_capture_names() {
        // KDE scan for a window without matching audio (e.g. VSCodium), where capture_names
        // accidentally captured Spotify from another video node.
        let scan = VideoScan {
            de: Some("kde"),
            source_type: Some("window"),
            media_name: Some("kwin-screencast-codium".to_string()),
            video_node_count: 10,
            highest_serial: 200,
            kde_media_names: vec![(200, "kwin-screencast-codium".to_string())],
            capture_names: vec!["Spotify".to_string()],
            screencast_node_id: Some(50),
            portal_props: None,
        };
        assert!(resolve_from_video_scan(&scan).is_none());
    }

    #[test]
    fn resolve_from_video_scan_evaluates_only_newest_stream_ignoring_older_lingering_streams() {
        // Active selection is VSCodium (serial 200), older lingering stream is Steam/FFXIV (serial 100).
        let scan = VideoScan {
            de: Some("kde"),
            source_type: Some("window"),
            media_name: Some("kwin-screencast-codium".to_string()),
            video_node_count: 2,
            highest_serial: 200,
            kde_media_names: vec![
                (100, "kwin-screencast-steam_app_default".to_string()),
                (200, "kwin-screencast-codium".to_string()),
            ],
            capture_names: vec![],
            screencast_node_id: Some(150),
            portal_props: None,
        };
        // Active stream is codium (no audio) — must return None without evaluating the older steam stream.
        assert!(resolve_from_video_scan(&scan).is_none());
    }
}
