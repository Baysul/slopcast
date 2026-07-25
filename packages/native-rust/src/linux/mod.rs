use crate::AudioApp;
use napi::{Either, Result as NapiResult};
use pipewire::properties::{PropertiesBox, properties};
use pipewire::registry::GlobalObject;
use pipewire::spa::param::audio::{AudioInfoRaw, AudioInfoRawFlags};
use pipewire::spa::param::format::MediaType;
use pipewire::spa::param::{ParamType, format_utils};
use pipewire::spa::utils::dict::DictRef;
use pipewire::types::ObjectType;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const CAPTURE_NODE_NAME: &str = "Slopcast-Window-Audio";
const CAPTURE_NODE_DESCRIPTION: &str = "Slopcast Window Audio";
const ADAPTER_FACTORY: &str = "adapter";
const NULL_SINK_FACTORY: &str = "support.null-audio-sink";
const CAPTURE_MEDIA_CLASS: &str = "Audio/Source/Virtual";
const LINK_FACTORY: &str = "link-factory";

struct CaptureState {
    is_active: bool,
    target_app_id: Option<i32>,
    virtual_sink_id: Option<i32>,
    active_links: Vec<i32>,
    session: Option<CaptureSession>,
}

impl CaptureState {
    fn new() -> Self {
        Self {
            is_active: false,
            target_app_id: None,
            virtual_sink_id: None,
            active_links: Vec::new(),
            session: None,
        }
    }
}

struct CaptureSession {
    stop: Arc<AtomicBool>,
    join: thread::JoinHandle<()>,
    shared: Arc<Mutex<SessionShared>>,
}

#[derive(Default)]
struct SessionShared {
    sink_id: Option<u32>,
    link_ids: Vec<i32>,
}

static CAPTURE_STATE: Mutex<Option<CaptureState>> = Mutex::new(None);

#[derive(Default)]
struct TargetSpec {
    node_id: Option<u32>,
    pid: Option<u32>,
    binary: Option<String>,
    system_audio: bool,
}

impl TargetSpec {
    fn learn(&mut self, node_id: u32, info: &AppNodeInfo) {
        if Some(node_id) != self.node_id {
            return;
        }
        if self.pid.is_none() {
            self.pid = info.pid;
        }
        if self.binary.is_none() {
            self.binary = info.binary.clone();
        }
    }

    fn matches(&self, node_id: u32, info: &AppNodeInfo) -> bool {
        if Some(node_id) == self.node_id {
            return true;
        }
        if let (Some(want), Some(pid)) = (self.pid, info.pid)
            && want == pid
        {
            return true;
        }
        if let (Some(want), Some(binary)) = (&self.binary, &info.binary)
            && want == binary
        {
            return true;
        }
        false
    }
}

#[derive(Default)]
struct AppNodeInfo {
    pid: Option<u32>,
    binary: Option<String>,
}

impl AppNodeInfo {
    fn from_props(props: &DictRef) -> Self {
        let pid = props
            .get("application.process.id")
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|p| *p > 0 && is_valid_pid(*p as i32));
        let binary =
            props.get("application.process.binary").map(str::to_string).filter(|b| !b.is_empty());
        Self { pid, binary }
    }

    fn fallback_pid(&self) -> Option<u32> {
        self.binary.as_deref().and_then(resolve_pid_by_binary).map(|p| p as u32).filter(|p| *p > 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChannelLayout {
    channels: u32,
    position: String,
}

impl ChannelLayout {
    fn stereo() -> Self {
        Self { channels: 2, position: "FL,FR".to_string() }
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
        Some(Self { channels, position: names.join(",") })
    }
}

fn channel_short_name(channel: u32) -> Option<String> {
    // SAFETY: pipewire-rs sys function returns null or a valid NUL-terminated C string.
    let ptr = unsafe { pipewire::spa::sys::spa_type_audio_channel_to_short_name(channel) };
    if ptr.is_null() {
        return None;
    }
    // SAFETY: ptr is non-null and points to a NUL-terminated C string.
    let cstr = unsafe { std::ffi::CStr::from_ptr(ptr) };
    cstr.to_str().ok().map(str::to_string)
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
    sink_node: Option<u32>,
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
            sink_node: None,
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
        let props = match global.props {
            Some(props) => props,
            None => return,
        };
        match &global.type_ {
            ObjectType::Node => self.add_node(global, props, shared),
            ObjectType::Port => self.add_port(global.id, props),
            ObjectType::Link => self.add_link(global.id, props, shared),
            ObjectType::Factory => self.track_link_factory(props),
            ObjectType::Metadata => self.track_metadata(global, props),
            ObjectType::Client => self.add_client(global, props),
            _ => {}
        }
    }

    fn remove_global(&mut self, id: u32, shared: &Arc<Mutex<SessionShared>>) {
        if self.sink_node == Some(id) {
            self.sink_node = None;
            if let Ok(mut shared) = shared.lock() {
                shared.sink_id = None;
            }
        }
        self.system_sinks.remove(&id);
        self.app_nodes.remove(&id);
        if self.ports.remove(&id).is_some() {
            self.pending_pairs.retain(|(out, inp)| *out != id && *inp != id);
        }
        if let Some(pair) = self.created_links.remove(&id) {
            self.linked_pairs.remove(&pair);
            self.link_proxies.remove(&pair);
            self.publish_links(shared);
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
            self.sink_node = Some(global.id);
            if let Ok(mut shared) = shared.lock() {
                shared.sink_id = Some(global.id);
            }
            return;
        }
        match media_class {
            "Audio/Sink" => {
                self.system_sinks.insert(global.id, (node_name.to_string(), global.to_owned()));
            }
            "Stream/Output/Audio" => {
                let mut info = AppNodeInfo::from_props(props);
                if (info.pid.is_none() || info.pid == Some(0))
                    && let Some(cid) = props.get("client.id").and_then(|v| v.parse::<u32>().ok())
                {
                    let p = self.client_pids.get(&cid).copied();
                    if p.is_some_and(|pid| is_valid_pid(pid as i32)) {
                        info.pid = p;
                    }
                }
                if info.pid.is_none() || info.pid == Some(0) {
                    info.pid = info.fallback_pid();
                }
                if info.pid.is_none() || info.pid == Some(0) {
                    let name = props.get("application.name").unwrap_or("");
                    if !name.is_empty() {
                        info.pid = resolve_pid_by_name(name).map(|p| p as u32);
                    }
                }
                self.target.learn(global.id, &info);
                self.app_nodes.insert(global.id, info);
            }
            _ => {}
        }
    }

    fn add_client(&mut self, _global: &GlobalObject<&DictRef>, props: &DictRef) {
        let pid = client_sec_pid(props)
            .or_else(|| {
                props
                    .get("application.process.id")
                    .and_then(|v| v.parse::<i32>().ok())
                    .filter(|p| is_valid_pid(*p))
            })
            .or_else(|| {
                let name = props.get("application.name").unwrap_or("");
                if !name.is_empty() { resolve_pid_by_name(name) } else { None }
            });
        if let Some(pid) = pid {
            self.client_pids.insert(_global.id, pid as u32);
        }
    }

    fn add_port(&mut self, id: u32, props: &DictRef) {
        let node = match props.get("node.id").and_then(|v| v.parse::<u32>().ok()) {
            Some(node) => node,
            None => return,
        };
        let is_output = match props.get("port.direction") {
            Some("out") => true,
            Some("in") => false,
            _ => return,
        };
        let channel = props
            .get("audio.channel")
            .map(str::to_string)
            .or_else(|| port_channel_from_name(props.get("port.name")));
        self.ports.insert(id, PortInfo { node, is_output, channel });

        let port = &self.ports[&id];
        let is_sink_input = !port.is_output && self.sink_node == Some(port.node);
        if is_sink_input {
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

    fn add_link(&mut self, id: u32, props: &DictRef, shared: &Arc<Mutex<SessionShared>>) {
        let output = props.get("link.output.port").and_then(|v| v.parse::<u32>().ok());
        let input = props.get("link.input.port").and_then(|v| v.parse::<u32>().ok());
        if let (Some(output), Some(input)) = (output, input) {
            let pair = (output, input);
            self.linked_pairs.insert(pair);
            if self.pending_pairs.remove(&pair) {
                self.created_links.insert(id, pair);
                self.publish_links(shared);
            }
        }
    }

    fn track_link_factory(&mut self, props: &DictRef) {
        if props.get("factory.type.name") == Some(ObjectType::Link.to_str())
            && let Some(name) = props.get("factory.name")
        {
            *self.factory_name.borrow_mut() = Some(name.to_string());
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
        self.system_sinks.iter().find_map(|(id, (sink_name, _))| (sink_name == name).then_some(*id))
    }

    fn system_sink_global(&self, id: u32) -> Option<&GlobalObject<PropertiesBox>> {
        self.system_sinks.get(&id).map(|(_, global)| global)
    }

    fn is_linkable_app(&self, node: u32) -> bool {
        if self.target.system_audio {
            return true;
        }
        self.app_nodes.get(&node).is_some_and(|info| self.target.matches(node, info))
    }

    fn try_link(&mut self, port_id: u32) {
        let sink = match self.sink_node {
            Some(sink) => sink,
            None => return,
        };
        let (node, channel) = match self.ports.get(&port_id) {
            Some(p) if p.is_output && self.is_linkable_app(p.node) => {
                let channel = match &p.channel {
                    Some(channel) => channel.as_str(),
                    None => return,
                };
                (p.node, channel)
            }
            _ => return,
        };
        let sink_port = match self.ports.iter().find_map(|(pid, p)| {
            if !p.is_output && p.node == sink && p.channel.as_deref() == Some(channel) {
                Some(*pid)
            } else {
                None
            }
        }) {
            Some(pid) => pid,
            None => return,
        };
        let pair = (port_id, sink_port);
        if self.linked_pairs.contains(&pair) || self.pending_pairs.contains(&pair) {
            return;
        }
        if self.create_link(port_id, sink_port, node, sink) {
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
        let factory = self.factory_name.borrow().as_deref().unwrap_or(LINK_FACTORY).to_string();
        let props = properties! {
            "link.output.port" => output_port.to_string(),
            "link.input.port" => input_port.to_string(),
            "link.output.node" => output_node.to_string(),
            "link.input.node" => input_node.to_string(),
            "object.linger" => "false",
        };
        match self.core.create_object::<pipewire::link::Link>(factory.as_str(), &props) {
            Ok(link) => {
                self.link_proxies.insert((output_port, input_port), link);
                true
            }
            Err(_) => false,
        }
    }

    fn on_sink_destroyed(&mut self, shared: &Arc<Mutex<SessionShared>>) {
        let sink = match self.sink_node.take() {
            Some(sink) => sink,
            None => return,
        };
        let sink_ports: HashSet<u32> =
            self.ports.iter().filter(|(_, p)| p.node == sink).map(|(pid, _)| *pid).collect();
        self.ports.retain(|_, p| p.node != sink);
        self.pending_pairs
            .retain(|(out, inp)| !sink_ports.contains(out) && !sink_ports.contains(inp));
        let dead: Vec<u32> = self
            .created_links
            .iter()
            .filter(|(_, (out, inp))| sink_ports.contains(out) || sink_ports.contains(inp))
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            if let Some(pair) = self.created_links.remove(&id) {
                self.linked_pairs.remove(&pair);
                self.link_proxies.remove(&pair);
            }
        }
        self.publish_links(shared);
        if let Ok(mut shared) = shared.lock() {
            shared.sink_id = None;
        }
    }

    fn drain_links(&mut self) -> Vec<pipewire::link::Link> {
        self.created_links.clear();
        self.linked_pairs.clear();
        self.pending_pairs.clear();
        self.link_proxies.drain().map(|(_, link)| link).collect()
    }

    fn publish_links(&self, shared: &Arc<Mutex<SessionShared>>) {
        if let Ok(mut shared) = shared.lock() {
            shared.link_ids = self.created_links.keys().map(|id| *id as i32).collect();
        }
    }
}

fn port_channel_from_name(port_name: Option<&str>) -> Option<String> {
    let name = port_name?;
    let underscore = name.rfind('_')?;
    let suffix = &name[underscore + 1..];
    if suffix.is_empty() { None } else { Some(suffix.to_string()) }
}

fn create_capture_sink(
    core: &pipewire::core::CoreRc,
    layout: &ChannelLayout,
) -> Result<pipewire::node::Node, pipewire::Error> {
    core.create_object::<pipewire::node::Node>(
        ADAPTER_FACTORY,
        &properties! {
            "factory.name" => NULL_SINK_FACTORY,
            "node.name" => CAPTURE_NODE_NAME,
            "node.description" => CAPTURE_NODE_DESCRIPTION,
            "device.description" => CAPTURE_NODE_DESCRIPTION,
            "media.class" => CAPTURE_MEDIA_CLASS,
            "audio.channels" => layout.channels.to_string(),
            "audio.position" => layout.position.clone(),
            "monitor.channel-volumes" => "true",
        },
    )
}

fn destroy_capture_sink(
    core: &pipewire::core::CoreRc,
    tracker: &Rc<RefCell<GraphTracker>>,
    sink_proxy: &mut Option<pipewire::node::Node>,
    shared: &Arc<Mutex<SessionShared>>,
) {
    tracker.borrow_mut().on_sink_destroyed(shared);
    if let Some(old) = sink_proxy.take() {
        let _ = core.destroy_object(old);
    }
}

fn bind_default_metadata(
    registry: &pipewire::registry::Registry,
    global: &GlobalObject<PropertiesBox>,
    tracker: &Rc<RefCell<GraphTracker>>,
) -> Option<(pipewire::metadata::Metadata, pipewire::metadata::MetadataListener)> {
    let metadata = registry.bind::<pipewire::metadata::Metadata, _>(global).ok()?;
    let tracker = tracker.clone();
    let listener = metadata
        .add_listener_local()
        .property(move |subject, key, _type, value| {
            if subject == pipewire::core::PW_ID_CORE
                && key == Some("default.audio.sink")
                && let Some(name) = value.and_then(json_object_name)
            {
                tracker.borrow_mut().set_default_sink_name(name);
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
        let tracker = tracker.borrow();
        let global = tracker.system_sink_global(id)?;
        registry.bind::<pipewire::node::Node, _>(global).ok()?
    };
    let desired = desired_layout.clone();
    let listener = proxy
        .add_listener_local()
        .param(move |_seq, param_type, _index, _next, pod| {
            if param_type != ParamType::EnumFormat {
                return;
            }
            let pod = match pod {
                Some(pod) => pod,
                None => return,
            };
            if let Ok((media_type, _)) = format_utils::parse_format(pod) {
                if media_type != MediaType::Audio {
                    return;
                }
            } else {
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
    Some(DefaultSinkWatch { id, _proxy: proxy, _listener: listener })
}

fn json_object_name(json: &str) -> Option<String> {
    let key_pos = json.find("\"name\"")?;
    let after_key = &json[key_pos + 6..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim_start();
    if !after_colon.starts_with('"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = after_colon[1..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out),
            _ => out.push(c),
        }
    }
    None
}

fn run_capture_session(
    target: TargetSpec,
    shared: Arc<Mutex<SessionShared>>,
    stop: Arc<AtomicBool>,
    ready_tx: mpsc::Sender<Result<(), String>>,
) {
    pipewire::init();

    let main_loop = match pipewire::main_loop::MainLoopRc::new(None) {
        Ok(m) => m,
        Err(e) => {
            let _ = ready_tx.send(Err(e.to_string()));
            return;
        }
    };
    let context = match pipewire::context::ContextRc::new(&main_loop, None) {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(e.to_string()));
            return;
        }
    };
    let core = match context.connect_rc(None) {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(e.to_string()));
            return;
        }
    };
    let registry = match core.get_registry() {
        Ok(r) => r,
        Err(e) => {
            let _ = ready_tx.send(Err(e.to_string()));
            return;
        }
    };

    let link_factory_name: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let tracker = Rc::new(RefCell::new(GraphTracker::new(target, core.clone(), link_factory_name)));

    let _reg_listener = registry
        .add_listener_local()
        .global({
            let tracker = tracker.clone();
            let shared = shared.clone();
            move |global| tracker.borrow_mut().add_global(global, &shared)
        })
        .global_remove({
            let tracker = tracker.clone();
            let shared = shared.clone();
            move |id| tracker.borrow_mut().remove_global(id, &shared)
        })
        .register();

    let desired_layout = Rc::new(RefCell::new(ChannelLayout::stereo()));
    let mut sink_layout = ChannelLayout::stereo();

    let mut sink_proxy = match create_capture_sink(&core, &sink_layout) {
        Ok(node) => Some(node),
        Err(e) => {
            let _ = ready_tx.send(Err(e.to_string()));
            return;
        }
    };

    let _ = ready_tx.send(Ok(()));

    let mut metadata_watch: Option<(
        pipewire::metadata::Metadata,
        pipewire::metadata::MetadataListener,
    )> = None;
    let mut sink_watch: Option<DefaultSinkWatch> = None;

    while !stop.load(Ordering::SeqCst) {
        let timeout = Duration::from_millis(10);
        main_loop.loop_().iterate(pipewire::loop_::Timeout::Finite(timeout));

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

        let layout = desired_layout.borrow().clone();
        if layout != sink_layout {
            destroy_capture_sink(&core, &tracker, &mut sink_proxy, &shared);
            if let Ok(node) = create_capture_sink(&core, &layout) {
                sink_proxy = Some(node);
                sink_layout = layout;
            }
        }
    }

    drop(sink_watch);
    drop(metadata_watch);

    for link in tracker.borrow_mut().drain_links() {
        let _ = core.destroy_object(link);
    }
    if let Some(sink) = sink_proxy.take() {
        let _ = core.destroy_object(sink);
    }

    if let Ok(pending) = core.sync(0) {
        let flush_done = Rc::new(RefCell::new(false));
        let flush_done_clone = flush_done.clone();
        let main_loop_weak = main_loop.downgrade();
        let _core_listener = core
            .add_listener_local()
            .done(move |id, seq| {
                if id == pipewire::core::PW_ID_CORE && seq == pending {
                    *flush_done_clone.borrow_mut() = true;
                    if let Some(ml) = main_loop_weak.upgrade() {
                        ml.quit();
                    }
                }
            })
            .register();

        for _ in 0..50 {
            if *flush_done.borrow() {
                break;
            }
            main_loop.loop_().iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(10)));
        }
    }

    if let Ok(mut shared) = shared.lock() {
        shared.sink_id = None;
        shared.link_ids.clear();
    }
}

fn spawn_capture_session(target: TargetSpec) -> NapiResult<CaptureSession> {
    let shared = Arc::new(Mutex::new(SessionShared::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

    let join = {
        let shared = shared.clone();
        let stop = stop.clone();
        thread::Builder::new()
            .name("pw-window-audio-capture".to_string())
            .spawn(move || run_capture_session(target, shared, stop, ready_tx))
            .map_err(|e| {
                napi::Error::from_reason(format!("Failed to spawn PipeWire worker thread: {}", e))
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
                "Timed out waiting for the PipeWire capture session to start",
            ));
        }
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let sink_known = shared.lock().map(|s| s.sink_id.is_some()).unwrap_or(false);
        if sink_known {
            break;
        }
        if Instant::now() >= deadline {
            stop.store(true, Ordering::SeqCst);
            let _ = join.join();
            return Err(napi::Error::from_reason(format!(
                "PipeWire virtual capture source '{}' did not appear in the registry",
                CAPTURE_NODE_NAME
            )));
        }
        thread::sleep(Duration::from_millis(10));
    }

    Ok(CaptureSession { stop, join, shared })
}

fn stop_session(state: &mut CaptureState) {
    if let Some(session) = state.session.take() {
        session.stop.store(true, Ordering::SeqCst);
        let _ = session.join.join();
    }
    state.is_active = false;
    state.active_links.clear();
    state.virtual_sink_id = None;
    state.target_app_id = None;
}

fn is_valid_pid(pid: i32) -> bool {
    pid > 0 && std::path::Path::new(&format!("/proc/{}", pid)).exists()
}

fn is_pipewire_daemon(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .map(|c| matches!(c.trim(), "pipewire" | "pipewire-pulse" | "wireplumber"))
        .unwrap_or(false)
}

/// `pipewire.sec.pid` is authenticated by the server from socket credentials,
/// so it's always present in registry globals (unlike `application.process.id`).
/// PipeWire-pulse proxied clients point at the daemon PID; reject those so the
/// caller resolves through stream properties instead.
fn client_sec_pid(props: &DictRef) -> Option<i32> {
    let pid = props
        .get("pipewire.sec.pid")
        .and_then(|v| v.parse::<i32>().ok())
        .filter(|pid| is_valid_pid(*pid))?;
    if is_pipewire_daemon(pid) { None } else { Some(pid) }
}

fn resolve_pid_by_binary(binary: &str) -> Option<i32> {
    if binary.is_empty() {
        return None;
    }
    let binary_lower = binary.to_lowercase();
    let mut candidates = Vec::new();
    let dir = std::fs::read_dir("/proc").ok()?;
    for entry in dir.flatten() {
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        let pid: i32 = match name_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if pid <= 0 {
            continue;
        }
        let comm_path = format!("/proc/{}/comm", pid);
        if let Ok(comm) = std::fs::read_to_string(&comm_path)
            && comm.trim().to_lowercase() == binary_lower
        {
            candidates.push(pid);
            continue;
        }
        let cmdline_path = format!("/proc/{}/cmdline", pid);
        if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path)
            && let Some(first) = cmdline.split('\0').next()
            && let Some(base) = std::path::Path::new(first).file_name().and_then(|s| s.to_str())
            && base.to_lowercase() == binary_lower
        {
            candidates.push(pid);
        }
    }
    (candidates.len() == 1).then_some(candidates[0])
}

fn resolve_pid_by_name(name: &str) -> Option<i32> {
    if name.is_empty() {
        return None;
    }
    let search_key = name.split_whitespace().next()?;
    if search_key.len() < 2 {
        return None;
    }
    let search_lower = search_key.to_lowercase();

    let mut candidates = Vec::new();
    let dir = std::fs::read_dir("/proc").ok()?;
    for entry in dir.flatten() {
        let name_str = match entry.file_name().to_str() {
            Some(s) => s.to_owned(),
            None => continue,
        };
        let pid: i32 = match name_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if pid <= 0 {
            continue;
        }
        let comm_path = format!("/proc/{}/comm", pid);
        if let Ok(comm) = std::fs::read_to_string(&comm_path) {
            let comm = comm.trim().to_lowercase();
            if comm == search_lower || comm.starts_with(search_lower.as_str()) {
                candidates.push(pid);
                continue;
            }
        }
        let cmdline_path = format!("/proc/{}/cmdline", pid);
        if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path)
            && let Some(first) = cmdline.split('\0').next()
            && let Some(base) = std::path::Path::new(first).file_name().and_then(|s| s.to_str())
        {
            let base = base.to_lowercase();
            if base == search_lower
                || base.starts_with(&search_lower)
                || search_lower.starts_with(&base)
            {
                candidates.push(pid);
            }
        }
    }
    candidates.into_iter().next()
}

fn node_pid(props: &DictRef) -> Option<i32> {
    props
        .get("application.process.id")
        .and_then(|v| v.parse::<i32>().ok())
        .filter(|pid| is_valid_pid(*pid))
}

fn client_pid(props: &DictRef, client_pids: &HashMap<u32, i32>) -> Option<i32> {
    let cid = props.get("client.id").and_then(|cid| cid.parse::<u32>().ok())?;
    let pid = *client_pids.get(&cid)?;
    is_valid_pid(pid).then_some(pid)
}

fn collect_client_pids(
    registry: &pipewire::registry::Registry,
    main_loop: &pipewire::main_loop::MainLoopRc,
    core: &pipewire::core::Core,
    apps: &Rc<RefCell<Vec<AudioApp>>>,
) -> HashMap<u32, i32> {
    let client_pids = Rc::new(RefCell::new(HashMap::<u32, i32>::new()));
    let client_pids_clone = client_pids.clone();
    let apps_clone = apps.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            let props = match global.props {
                Some(props) => props,
                None => return,
            };

            if global.type_ == ObjectType::Client {
                let app_name = props.get("application.name").unwrap_or("").to_string();
                let pid = client_sec_pid(props)
                    .or_else(|| {
                        props
                            .get("application.process.id")
                            .and_then(|v| v.parse::<i32>().ok())
                            .filter(|pid| is_valid_pid(*pid))
                    })
                    .or_else(|| {
                        if !app_name.is_empty() { resolve_pid_by_name(&app_name) } else { None }
                    });
                if let Some(pid) = pid {
                    client_pids_clone.borrow_mut().insert(global.id, pid);
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
                let pid = node_pid(props)
                    .or_else(|| client_pid(props, &client_pids_clone.borrow()))
                    .or_else(|| {
                        resolve_pid_by_binary(props.get("application.process.binary").unwrap_or(""))
                    })
                    .or_else(|| resolve_pid_by_name(stream_name))
                    .unwrap_or(0);

                let mut list = apps_clone.borrow_mut();
                if !list.iter().any(|a| a.id == global.id as i32) {
                    list.push(AudioApp {
                        id: global.id as i32,
                        name: stream_name.to_string(),
                        process_id: pid,
                        bundle_id: None,
                    });
                }
            }
        })
        .register();

    let done = Rc::new(RefCell::new(false));
    let done_clone = done.clone();
    let pending = core.sync(0).ok();
    if let Some(pending) = pending {
        let main_loop_weak = main_loop.downgrade();
        let _core_listener = core
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
            main_loop.loop_().iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(10)));
        }
    }

    client_pids.take()
}

pub fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
    pipewire::init();

    let main_loop = pipewire::main_loop::MainLoopRc::new(None).map_err(|e| {
        napi::Error::from_reason(format!("Failed to create PipeWire main loop: {}", e))
    })?;

    let context = pipewire::context::ContextRc::new(&main_loop, None).map_err(|e| {
        napi::Error::from_reason(format!("Failed to create PipeWire context: {}", e))
    })?;

    let core = context.connect(None).map_err(|e| {
        napi::Error::from_reason(format!("Failed to connect to PipeWire core: {}", e))
    })?;

    let registry = core
        .get_registry()
        .map_err(|e| napi::Error::from_reason(format!("Failed to get PipeWire registry: {}", e)))?;

    let apps = Rc::new(RefCell::new(Vec::<AudioApp>::new()));

    collect_client_pids(&registry, &main_loop, &core, &apps);

    let result = apps.take();
    Ok(result)
}

pub fn start_audio_capture(target_app_id: &Either<String, i32>) -> NapiResult<bool> {
    let (node_id, system_audio) = match target_app_id {
        Either::B(-1) => (None, true),
        Either::B(n) if *n >= 0 => (Some(*n as u32), false),
        Either::A(s) if s.trim() == "__system_audio__" => (None, true),
        Either::A(s) => {
            let n = s.trim().parse::<u32>().ok().ok_or_else(|| {
                napi::Error::from_reason("A PipeWire node ID or -1 (system audio) is required")
            })?;
            (Some(n), false)
        }
        _ => {
            return Err(napi::Error::from_reason(
                "A PipeWire node ID or -1 (system audio) is required",
            ));
        }
    };

    let mut state_guard =
        CAPTURE_STATE.lock().map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let state = state_guard.get_or_insert_with(CaptureState::new);

    stop_session(state);

    let target = TargetSpec { node_id, system_audio, ..TargetSpec::default() };
    let session = spawn_capture_session(target)?;
    state.virtual_sink_id = session.shared.lock().ok().and_then(|s| s.sink_id).map(|id| id as i32);
    state.target_app_id = node_id.map(|id| id as i32);
    state.active_links.clear();
    state.is_active = true;
    state.session = Some(session);

    Ok(true)
}

pub fn stop_audio_capture() -> NapiResult<bool> {
    let mut state_guard =
        CAPTURE_STATE.lock().map_err(|e| napi::Error::from_reason(e.to_string()))?;
    if let Some(state) = state_guard.as_mut() {
        stop_session(state);
    }
    Ok(true)
}

pub fn is_audio_capture_active() -> NapiResult<bool> {
    let mut state_guard =
        CAPTURE_STATE.lock().map_err(|e| napi::Error::from_reason(e.to_string()))?;
    if let Some(state) = state_guard.as_mut() {
        let finished = state.session.as_ref().map(|s| s.join.is_finished()).unwrap_or(false);
        if finished {
            stop_session(state);
        }
        if let Some(session) = &state.session
            && let Ok(shared) = session.shared.lock()
        {
            state.active_links = shared.link_ids.clone();
            if state.virtual_sink_id.is_none() {
                state.virtual_sink_id = shared.sink_id.map(|id| id as i32);
            }
        }
        Ok(state.is_active)
    } else {
        Ok(false)
    }
}

pub fn resolve_audio_by_x11_window(window_id: u32) -> Option<AudioApp> {
    // SAFETY: X11 C library calls. We check all return values before dereferencing.
    let display = unsafe { x11::xlib::XOpenDisplay(std::ptr::null()) };
    if display.is_null() {
        return None;
    }

    let atom_name = std::ffi::CString::new("_NET_WM_PID").ok()?;
    let atom = unsafe { x11::xlib::XInternAtom(display, atom_name.as_ptr(), 1) };
    if atom == 0 {
        // SAFETY: display was successfully opened above.
        unsafe { x11::xlib::XCloseDisplay(display) };
        return None;
    }

    let mut actual_type: x11::xlib::Atom = 0;
    let mut actual_format: std::os::raw::c_int = 0;
    let mut nitems: std::os::raw::c_ulong = 0;
    let mut bytes_after: std::os::raw::c_ulong = 0;
    let mut prop: *mut u8 = std::ptr::null_mut();

    // SAFETY: XGetWindowProperty is called with valid display and atom.
    // prop is only dereferenced after checking status == 0 and prop != null.
    let status = unsafe {
        x11::xlib::XGetWindowProperty(
            display,
            window_id as x11::xlib::Window,
            atom,
            0,
            1,
            0,
            x11::xlib::XA_CARDINAL,
            &mut actual_type,
            &mut actual_format,
            &mut nitems,
            &mut bytes_after,
            &mut prop,
        )
    };

    // SAFETY: prop is valid only when status == 0 and the format/type match.
    let pid = if status == 0 && !prop.is_null() && nitems > 0 && actual_format == 32 {
        Some(unsafe { *(prop as *const u32) })
    } else {
        None
    };

    if !prop.is_null() {
        // SAFETY: XFree is called with the same pointer returned by XGetWindowProperty.
        unsafe { x11::xlib::XFree(prop as *mut std::ffi::c_void) };
    }
    // SAFETY: display is still valid from the open above.
    unsafe { x11::xlib::XCloseDisplay(display) };

    let pid = pid?;
    let apps = list_audio_applications().ok()?;
    apps.into_iter().find(|app| app.process_id == pid as i32)
}

fn portal_window_name(props: &DictRef) -> Option<String> {
    for key in &[
        "portal.screencast.application",
        "portal.screencast.title",
        "window.name",
        "pipewire.access.portal.app_id",
    ] {
        if let Some(v) = props.get(key).filter(|v| !v.is_empty()) {
            return Some(v.to_string());
        }
    }

    if let Some(v) = props.get("media.name").filter(|v| !v.is_empty())
        && !v.contains("pipewire")
        && v != "kwin_wayland"
    {
        return Some(v.to_string());
    }

    if let Some(v) = props.get("node.name").filter(|v| !v.is_empty()) {
        let lower = v.to_lowercase();
        if lower != "kwin_wayland" && lower != "gnome-shell" && !lower.contains("pipewire") {
            return Some(v.to_string());
        }
    }

    if let Some(v) = props.get("application.name").filter(|v| !v.is_empty()) {
        let lower = v.to_lowercase();
        if lower != "xdg-desktop-portal"
            && lower != "kwin_wayland"
            && lower != "gnome-shell"
            && !lower.contains("pipewire")
        {
            return Some(v.to_string());
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
    let is_monitor = parts[0].chars().all(|c| c.is_ascii_uppercase())
        && parts[1].chars().all(|c| c.is_ascii_digit());
    !is_monitor
}

fn kde_window_uuid_to_pid(uuid: &str) -> Option<u32> {
    use std::process::Command;

    let output = Command::new("kdotool").args(["getwindowpid", uuid]).output().ok()?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        return stdout.trim().parse::<u32>().ok();
    }

    None
}

fn resolve_kde_screencast_audio(media_name: &str) -> Option<AudioApp> {
    if !media_name.starts_with("kwin-screencast-") {
        return None;
    }

    let suffix = media_name.strip_prefix("kwin-screencast-")?;

    if !is_kde_screencast_window(suffix) {
        return None;
    }

    let pid = kde_window_uuid_to_pid(suffix)?;
    if pid == 0 {
        return None;
    }

    let apps = list_audio_applications().ok()?;
    apps.into_iter().find(|app| app.process_id == pid as i32)
}

pub fn resolve_audio_by_captured_window() -> Option<AudioApp> {
    pipewire::init();

    let main_loop = pipewire::main_loop::MainLoopRc::new(None).ok()?;
    let context = pipewire::context::ContextRc::new(&main_loop, None).ok()?;
    let core = context.connect(None).ok()?;
    let registry = core.get_registry().ok()?;

    let capture_names = Rc::new(RefCell::new(Vec::<String>::new()));
    let kde_media_names = Rc::new(RefCell::new(Vec::<String>::new()));
    let capture_names_clone = capture_names.clone();
    let kde_media_names_clone = kde_media_names.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            let props = match global.props {
                Some(props) => props,
                None => return,
            };
            let media_class = props.get("media.class").unwrap_or("");
            if !media_class.starts_with("Video/") && !media_class.starts_with("Stream/Output/Video")
            {
                return;
            }
            let mn = props.get("media.name").unwrap_or("");
            if mn.starts_with("kwin-screencast-") {
                let mut list = kde_media_names_clone.borrow_mut();
                if !list.contains(&mn.to_string()) {
                    list.push(mn.to_string());
                }
                return;
            }
            if let Some(name) = portal_window_name(props) {
                let mut list = capture_names_clone.borrow_mut();
                if !list.contains(&name) {
                    list.push(name);
                }
            }
        })
        .register();

    let done = Rc::new(RefCell::new(false));
    let done_clone = done.clone();
    let pending = core.sync(0).ok()?;

    let main_loop_weak = main_loop.downgrade();
    let _core_listener = core
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
        main_loop.loop_().iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(10)));
    }

    for mn in kde_media_names.take().iter() {
        if let Some(app) = resolve_kde_screencast_audio(mn) {
            return Some(app);
        }
    }

    let names = capture_names.take();
    let apps = list_audio_applications().ok()?;
    for name in &names {
        if let Some(app) = crate::find_best_audio_match(&apps, name) {
            return Some(app);
        }
    }
    None
}
