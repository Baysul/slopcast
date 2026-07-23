// Exclusive per-application audio capture for Linux via PipeWire.
//
// This module is modelled on the OBS Studio "Application Audio Capture
// (PipeWire)" source (https://github.com/dimtpap/obs-pipewire-audio-capture):
//
//  1. A dedicated virtual capture node is created through the adapter factory
//     (`support.null-audio-sink`) with `media.class = "Audio/Source/Virtual"`.
//     Its channel layout mirrors the system's default sink (discovered through
//     the "default" metadata and the default sink's EnumFormat params), falling
//     back to stereo when unknown.
//  2. The registry is tracked for `Stream/Output/Audio` application stream
//     nodes. ONLY the ports of nodes belonging to the selected target
//     application (the shared window's app) are linked, channel by channel,
//     into the virtual source. No other application's audio ever reaches the
//     node — the stream sent to spectators contains the shared window's
//     audio and nothing else.
//  3. The target application's existing links are never touched, so its
//     audio keeps playing through the user's physical output unaffected.
//
// Unlike the OBS plugin (which taps a capture sink with its own `pw_stream`),
// the captured audio is consumed by the Electron renderer via getUserMedia.
// Chromium deliberately filters Pulse/PipeWire *monitor* sources out of the
// microphone device list, so we expose the capture node as a virtual
// microphone (`Audio/Source/Virtual`) rather than an `Audio/Sink` monitor.

#![cfg(target_os = "linux")]

use crate::AudioApp;
use napi::{Either, Result as NapiResult};
use pipewire::properties::{properties, PropertiesBox};
use pipewire::registry::GlobalObject;
use pipewire::spa::param::audio::{AudioInfoRaw, AudioInfoRawFlags};
use pipewire::spa::param::format::MediaType;
use pipewire::spa::param::{format_utils, ParamType};
use pipewire::spa::utils::dict::DictRef;
use pipewire::types::ObjectType;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const CAPTURE_NODE_NAME: &str = "Screenshare-Window-Audio";
const CAPTURE_NODE_DESCRIPTION: &str = "Screenshare Window Audio";
const ADAPTER_FACTORY: &str = "adapter";
const NULL_SINK_FACTORY: &str = "support.null-audio-sink";
/// Virtual microphone media class. Chromium filters real sink-monitor
/// sources from getUserMedia; a Source/Virtual node is listed as a mic.
const CAPTURE_MEDIA_CLASS: &str = "Audio/Source/Virtual";
/// Fallback link factory name; the real name is discovered from the registry.
const LINK_FACTORY: &str = "link-factory";

// ---------------------------------------------------------------------------
// Capture session state shared with the NAPI side
// ---------------------------------------------------------------------------

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

/// Handle to the background PipeWire session that owns the virtual sink and
/// all created link proxies. All PipeWire objects live on the worker thread;
/// only this send-able handle crosses thread boundaries.
struct CaptureSession {
    stop: Arc<AtomicBool>,
    join: thread::JoinHandle<()>,
    shared: Arc<Mutex<SessionShared>>,
}

/// Graph facts published by the worker thread for the NAPI side to observe.
#[derive(Default)]
struct SessionShared {
    sink_id: Option<u32>,
    link_ids: Vec<i32>,
}

static CAPTURE_STATE: Mutex<Option<CaptureState>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// Target application matching
// ---------------------------------------------------------------------------

/// Identity of the application whose audio may be captured.
///
/// Matching starts from the PipeWire node ID picked by the user. Once that
/// node appears in the registry its process ID and process binary are
/// adopted, so every current and future stream of the same application
/// (e.g. additional tabs, or a stream re-created after an in-app restart) is
/// captured as well — mirroring how the OBS plugin matches targets by
/// application binary name.
#[derive(Default)]
struct TargetSpec {
    node_id: Option<u32>,
    pid: Option<u32>,
    binary: Option<String>,
    /// When true, every `Stream/Output/Audio` node is linked into the capture
    /// sink (system-wide audio capture). Individual `node_id`/`pid`/`binary`
    /// matching is skipped.
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
        if let (Some(want), Some(pid)) = (self.pid, info.pid) {
            if want == pid {
                return true;
            }
        }
        if let (Some(want), Some(binary)) = (&self.binary, &info.binary) {
            if want == binary {
                return true;
            }
        }
        false
    }
}

/// Identifying properties of a `Stream/Output/Audio` node.
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
        let binary = props
            .get("application.process.binary")
            .map(str::to_string)
            .filter(|b| !b.is_empty());
        Self { pid, binary }
    }

    /// Resolve PID using the last-resort `/proc` binary scan when the
    /// primary methods (node property, client ID mapping) all failed.
    fn fallback_pid(&self) -> Option<u32> {
        self.binary
            .as_deref()
            .and_then(resolve_pid_by_binary)
            .map(|p| p as u32)
            .filter(|p| *p > 0)
    }
}

// ---------------------------------------------------------------------------
// Channel layout (mirrors the default system sink, stereo fallback)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChannelLayout {
    channels: u32,
    /// Comma-separated SPA channel names, e.g. "FL,FR".
    position: String,
}

impl ChannelLayout {
    fn stereo() -> Self {
        Self {
            channels: 2,
            position: "FL,FR".to_string(),
        }
    }

    /// Derives a layout from a parsed raw audio format, or `None` when the
    /// format carries no usable channel positions (the caller then keeps the
    /// stereo fallback, same as the OBS plugin).
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
            // Pro-audio sinks expose AUX channels whose semantic mapping is
            // unknown; keep the stereo fallback (same as the OBS plugin).
            if name.starts_with("AUX") {
                return None;
            }
            names.push(name);
        }
        Some(Self {
            channels,
            position: names.join(","),
        })
    }
}

/// Resolves an SPA audio channel value to its short name ("FL", "FR", ...).
fn channel_short_name(channel: u32) -> Option<String> {
    let ptr = unsafe { pipewire::spa::sys::spa_type_audio_channel_to_short_name(channel) };
    if ptr.is_null() {
        None
    } else {
        unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_str()
            .ok()
            .map(str::to_string)
    }
}

// ---------------------------------------------------------------------------
// Registry-driven port/link tracking (lives entirely on the worker thread)
// ---------------------------------------------------------------------------

struct PortInfo {
    node: u32,
    is_output: bool,
    channel: Option<String>,
}

/// Keeps the bound default-sink node proxy and its param listener alive.
struct DefaultSinkWatch {
    id: u32,
    _proxy: pipewire::node::Node,
    _listener: pipewire::node::NodeListener,
}

/// Tracks the parts of the PipeWire graph relevant to the capture:
/// application stream nodes, the virtual capture sink, their audio ports,
/// and the links connecting the target application's outputs to the sink
/// inputs.
struct GraphTracker {
    target: TargetSpec,
    core: pipewire::core::CoreRc,
    factory_name: Rc<RefCell<Option<String>>>,
    sink_node: Option<u32>,
    app_nodes: HashMap<u32, AppNodeInfo>,
    ports: HashMap<u32, PortInfo>,
    /// (output_port, input_port) pairs that already exist in the graph.
    linked_pairs: HashSet<(u32, u32)>,
    /// Pairs we requested via `create_object`, awaiting registry confirmation.
    pending_pairs: HashSet<(u32, u32)>,
    /// Link global id -> pair, for the links created by this session.
    created_links: HashMap<u32, (u32, u32)>,
    /// Proxies of the links this session created, keyed by port pair.
    link_proxies: HashMap<(u32, u32), pipewire::link::Link>,
    /// Other `Audio/Sink` nodes (id -> (node.name, owned global)) used to
    /// resolve the default sink reported by the metadata.
    system_sinks: HashMap<u32, (String, GlobalObject<PropertiesBox>)>,
    default_sink_name: Option<String>,
    /// The "default" metadata global, waiting to be bound by the main loop.
    pending_metadata: Option<GlobalObject<PropertiesBox>>,
    /// PipeWire client ID -> process PID (resolved from Client globals).
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
            // The server side of this link is already gone; dropping the
            // proxy destroys it locally.
            self.link_proxies.remove(&pair);
            self.publish_links(shared);
        }
    }

    fn add_node(&mut self, global: &GlobalObject<&DictRef>, props: &DictRef, shared: &Arc<Mutex<SessionShared>>) {
        let media_class = props.get("media.class").unwrap_or("");
        let node_name = props.get("node.name").unwrap_or("");
        // Our virtual capture node is matched by name so we tolerate either the
        // intended Source/Virtual class or a legacy Sink class.
        if node_name == CAPTURE_NODE_NAME {
            self.sink_node = Some(global.id);
            if let Ok(mut shared) = shared.lock() {
                shared.sink_id = Some(global.id);
            }
            return;
        }
        match media_class {
            "Audio/Sink" => {
                self.system_sinks
                    .insert(global.id, (node_name.to_string(), global.to_owned()));
            }
            "Stream/Output/Audio" => {
                let mut info = AppNodeInfo::from_props(props);
                // Fallback: resolve via client.id -> client_pids for direct
                // PipeWire clients where the node does not carry the PID.
                if info.pid.is_none() || info.pid == Some(0) {
                    if let Some(cid) = props.get("client.id").and_then(|v| v.parse::<u32>().ok()) {
                        let p = self.client_pids.get(&cid).copied();
                        if p.is_some_and(|pid| is_valid_pid(pid as i32)) {
                            info.pid = p;
                        }
                    }
                }
                // Last resort: scan /proc by binary name (bridges cases
                // where PipeWire runs under systemd and the node's client.id
                // points to pipewire-pulse rather than the app).
                if info.pid.is_none() || info.pid == Some(0) {
                    info.pid = info.fallback_pid();
                }
                // Absolute last resort: match the node's application.name
                // via /proc (works when PipeWire omits both
                // application.process.id and application.process.binary
                // but DOES set application.name on the stream node).
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
        let mut pid: Option<u32> = None;
        if let Some(pid_str) = props.get("application.process.id") {
            if let Ok(p) = pid_str.parse::<u32>() {
                if p > 0 && is_valid_pid(p as i32) {
                    pid = Some(p);
                }
            }
        }
        // When PipeWire does not set application.process.id on Client
        // objects (e.g. KDE Plasma / Wayland), resolve via /proc name match.
        if pid.is_none() {
            let client_name = props.get("application.name").unwrap_or("");
            if !client_name.is_empty() {
                pid = resolve_pid_by_name(client_name).map(|p| p as u32);
            }
        }
        if let Some(pid) = pid {
            self.client_pids.insert(_global.id, pid);
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
            // A new sink input port appeared: attach every eligible
            // application output port with a matching channel.
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
        if props.get("factory.type.name") == Some(ObjectType::Link.to_str()) {
            if let Some(name) = props.get("factory.name") {
                *self.factory_name.borrow_mut() = Some(name.to_string());
            }
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

    /// Resolves the global ID of the current default sink, if it is both
    /// known from the metadata and present in the registry.
    fn default_sink_id(&self) -> Option<u32> {
        let name = self.default_sink_name.as_deref()?;
        self.system_sinks
            .iter()
            .find_map(|(id, (sink_name, _))| (sink_name == name).then_some(*id))
    }

    fn system_sink_global(&self, id: u32) -> Option<&GlobalObject<PropertiesBox>> {
        self.system_sinks.get(&id).map(|(_, global)| global)
    }

    fn is_linkable_app(&self, node: u32) -> bool {
        if self.target.system_audio {
            return true;
        }
        self.app_nodes
            .get(&node)
            .is_some_and(|info| self.target.matches(node, info))
    }

    /// Links the given application output port into the matching virtual sink
    /// input port, unless the port's node is not the capture target, the link
    /// already exists, or its creation is already pending.
    fn try_link(&mut self, port_id: u32) {
        let sink = match self.sink_node {
            Some(sink) => sink,
            None => return,
        };
        let (node, channel) = match self.ports.get(&port_id) {
            Some(p) if p.is_output && self.is_linkable_app(p.node) => {
                let channel = match &p.channel {
                    Some(channel) => channel.clone(),
                    None => return,
                };
                (p.node, channel)
            }
            _ => return,
        };
        let sink_port = self.ports.iter().find_map(|(pid, p)| {
            if !p.is_output && p.node == sink && p.channel.as_deref() == Some(channel.as_str()) {
                Some(*pid)
            } else {
                None
            }
        });
        let sink_port = match sink_port {
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

    /// Creates a link output_port -> input_port via `core.create_object`.
    /// `object.linger = false` (same as the OBS plugin) makes the server
    /// destroy the link as soon as this client disconnects, guaranteeing
    /// cleanup even on a hard crash. Returns true when the request was
    /// accepted by the client library.
    fn create_link(&mut self, output_port: u32, input_port: u32, output_node: u32, input_node: u32) -> bool {
        let factory = self
            .factory_name
            .borrow()
            .clone()
            .unwrap_or_else(|| LINK_FACTORY.to_string());
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

    /// Drops every link into the (about to be destroyed) capture sink so the
    /// ports of a recreated sink start from a clean slate. The server removes
    /// the links itself when the sink node is destroyed; here we only forget
    /// our local proxies and bookkeeping.
    fn on_sink_destroyed(&mut self, shared: &Arc<Mutex<SessionShared>>) {
        let sink = match self.sink_node.take() {
            Some(sink) => sink,
            None => return,
        };
        let sink_ports: HashSet<u32> = self
            .ports
            .iter()
            .filter(|(_, p)| p.node == sink)
            .map(|(pid, _)| *pid)
            .collect();
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

/// Derives an audio channel name (e.g. "FL") from a port name such as
/// "output_FL" when the `audio.channel` property is absent.
fn port_channel_from_name(port_name: Option<&str>) -> Option<String> {
    let name = port_name?;
    let suffix = name.rsplit('_').next()?;
    if suffix.is_empty() || suffix == name {
        None
    } else {
        Some(suffix.to_string())
    }
}

// ---------------------------------------------------------------------------
// Capture sink lifecycle
// ---------------------------------------------------------------------------

/// Creates the virtual capture microphone via the adapter factory with the
/// given channel layout. `media.class = Audio/Source/Virtual` makes pipewire-
/// pulse expose the node as a real microphone Chromium can open with
/// getUserMedia (sink monitors are filtered out of the device list).
/// Without `object.linger` the server destroys the node as soon as this
/// client disconnects, which guarantees cleanup even on a hard crash.
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

/// Destroys the current capture sink (and forgets its links) so that it can
/// be recreated with a different channel layout.
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

/// Binds the "default" metadata and feeds the default sink name back into
/// the tracker whenever WirePlumber reports a change.
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
            if subject == pipewire::core::PW_ID_CORE && key == Some("default.audio.sink") {
                if let Some(name) = value.and_then(json_object_name) {
                    tracker.borrow_mut().set_default_sink_name(name);
                }
            }
            0
        })
        .register();
    Some((metadata, listener))
}

/// Binds the default sink node and subscribes to its EnumFormat params so
/// the capture sink can mirror its channel layout.
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
    Some(DefaultSinkWatch {
        id,
        _proxy: proxy,
        _listener: listener,
    })
}

/// Extracts the value of the `"name"` key from a flat JSON object such as
/// `{"name":"alsa_output.pci-0000_00_1f.3.analog-stereo"}`.
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

// ---------------------------------------------------------------------------
// Background capture session
// ---------------------------------------------------------------------------

/// Worker thread entry point. Owns the main loop, the virtual sink node, the
/// default-sink watch and every link proxy until the stop flag is set, then
/// destroys the created objects and disconnects.
fn run_capture_session(
    target: TargetSpec,
    shared: Arc<Mutex<SessionShared>>,
    stop: Arc<AtomicBool>,
    ready_tx: mpsc::Sender<Result<(), String>>,
) {
    pipewire::init();

    let main_loop = match pipewire::main_loop::MainLoopRc::new(None) {
        Ok(main_loop) => main_loop,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("Failed to create PipeWire main loop: {}", e)));
            return;
        }
    };
    let context = match pipewire::context::ContextRc::new(&main_loop, None) {
        Ok(context) => context,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("Failed to create PipeWire context: {}", e)));
            return;
        }
    };
    let core = match context.connect_rc(None) {
        Ok(core) => core,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("Failed to connect to PipeWire core: {}", e)));
            return;
        }
    };
    let registry = match core.get_registry() {
        Ok(registry) => registry,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("Failed to get PipeWire registry: {}", e)));
            return;
        }
    };

    let link_factory_name: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let tracker = Rc::new(RefCell::new(GraphTracker::new(
        target,
        core.clone(),
        link_factory_name,
    )));

    let _reg_listener = registry
        .add_listener_local()
        .global({
            let tracker = tracker.clone();
            let shared = shared.clone();
            move |global| {
                tracker.borrow_mut().add_global(global, &shared);
            }
        })
        .global_remove({
            let tracker = tracker.clone();
            let shared = shared.clone();
            move |id| {
                tracker.borrow_mut().remove_global(id, &shared);
            }
        })
        .register();

    // The sink starts stereo; once the default metadata and the default
    // sink's EnumFormat params report the real channel layout, the sink is
    // recreated to match (same strategy as the OBS plugin).
    let desired_layout = Rc::new(RefCell::new(ChannelLayout::stereo()));
    let mut sink_layout = ChannelLayout::stereo();

    let mut sink_proxy = match create_capture_sink(&core, &sink_layout) {
        Ok(node) => Some(node),
        Err(e) => {
            let _ = ready_tx.send(Err(format!(
                "Failed to create virtual capture source '{}': {}",
                CAPTURE_NODE_NAME, e
            )));
            return;
        }
    };

    let _ = ready_tx.send(Ok(()));

    let mut metadata_watch: Option<(pipewire::metadata::Metadata, pipewire::metadata::MetadataListener)> =
        None;
    let mut sink_watch: Option<DefaultSinkWatch> = None;

    while !stop.load(Ordering::SeqCst) {
        main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(10)));

        // Bind the "default" metadata once it appears in the registry.
        if metadata_watch.is_none() {
            let pending = tracker.borrow_mut().take_pending_metadata();
            if let Some(global) = pending {
                metadata_watch = bind_default_metadata(&registry, &global, &tracker);
            }
        }

        // (Re)bind the default sink watch whenever the default sink changes.
        let want_watch = tracker.borrow().default_sink_id();
        if want_watch != sink_watch.as_ref().map(|w| w.id) {
            sink_watch =
                want_watch.and_then(|id| bind_default_sink(&registry, &tracker, id, &desired_layout));
        }

        // Recreate the capture sink when the default sink's layout differs.
        let layout = desired_layout.borrow().clone();
        if layout != sink_layout {
            destroy_capture_sink(&core, &tracker, &mut sink_proxy, &shared);
            match create_capture_sink(&core, &layout) {
                Ok(node) => {
                    sink_proxy = Some(node);
                    sink_layout = layout;
                }
                Err(e) => {
                    eprintln!(
                        "[native-rust] Failed to recreate capture sink with layout {:?}: {}",
                        layout, e
                    );
                }
            }
        }
    }

    // Teardown: stop watching first so no listener fires while objects are
    // destroyed, then destroy every link we created, then the virtual sink,
    // and flush the requests with a final core sync before disconnecting.
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

        let mut iterations = 0;
        while !*flush_done.borrow() && iterations < 50 {
            main_loop
                .loop_()
                .iterate(pipewire::loop_::Timeout::Finite(Duration::from_millis(10)));
            iterations += 1;
        }
    }

    if let Ok(mut shared) = shared.lock() {
        shared.sink_id = None;
        shared.link_ids.clear();
    }
}

/// Spawns the background session and waits until the virtual sink node has
/// been created and reported back through the registry.
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
            .map_err(|e| napi::Error::from_reason(format!("Failed to spawn PipeWire worker thread: {}", e)))?
    };

    // Wait for the worker to finish its setup (or report a failure).
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

    // Wait until the registry announces the virtual sink node (bounded).
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

/// Stops the running session (if any), waiting for the worker to destroy the
/// created links and the virtual sink before returning.
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

// ---------------------------------------------------------------------------
// Public module interface
// ---------------------------------------------------------------------------

/// Validates that a PID is a real running process by checking /proc/<pid>/comm.
fn is_valid_pid(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    std::path::Path::new(&format!("/proc/{}", pid)).exists()
}

/// Scans /proc for a running process whose `comm` or first `cmdline` token
/// contains the given binary name.  Returns the PID when exactly one matching
/// process is found.  This serves as a last-resort fallback when PipeWire
/// properties do not carry an `application.process.id`.
///
/// On Flatpak/Snap the `application.process.id` on PipeWire objects may be
/// the PID from an inner (namespace) PID namespace rather than the host PID.
/// This function returns only processes visible in the caller's /proc, so it
/// can bridge such cases when the outer PID is needed.
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
        // Check /proc/<pid>/comm for exact binary name
        let comm_path = format!("/proc/{}/comm", pid);
        if let Ok(comm) = std::fs::read_to_string(&comm_path) {
            if comm.trim().to_lowercase() == binary_lower {
                candidates.push(pid);
                continue;
            }
        }
        // Fallback: /proc/<pid>/cmdline first token
        let cmdline_path = format!("/proc/{}/cmdline", pid);
        if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
            if let Some(first) = cmdline.split('\0').next() {
                if let Some(base) = std::path::Path::new(first)
                    .file_name()
                    .and_then(|s| s.to_str())
                {
                    if base.to_lowercase() == binary_lower {
                        candidates.push(pid);
                    }
                }
            }
        }
    }
    if candidates.len() == 1 {
        Some(candidates[0])
    } else {
        None
    }
}

/// Scans /proc for a running process whose `comm` or `cmdline` matches the
/// given application name (case-insensitive prefix match on the first word).
///
/// This is a fallback for PipeWire setups (e.g. KDE Plasma on Wayland) that
/// do not set `application.process.id` on Client or Node objects.  Since
/// `application.name` IS available on clients, we match it against /proc.
///
/// The first word of the name is used for matching (e.g. "Brave input" -> "Brave",
/// "Chromium input" -> "Chromium", "WEBRTC VoiceEngine" -> "WEBRTC").
fn resolve_pid_by_name(name: &str) -> Option<i32> {
    if name.is_empty() {
        return None;
    }
    // Use the first word of the application name as the search key.
    // This handles cases like "Brave input", "Chromium input", etc.
    // where the first word is the actual executable identity.
    let search_key = name.split_whitespace().next()?;
    if search_key.len() < 2 {
        return None;
    }
    let search_lower = search_key.to_lowercase();

    let mut candidates = Vec::new();
    let dir = std::fs::read_dir("/proc").ok()?;
    for entry in dir.flatten() {
        let name_str = entry.file_name();
        let name_str = match name_str.to_str() {
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
        // Check /proc/<pid>/comm for prefix match (case-insensitive)
        let comm_path = format!("/proc/{}/comm", pid);
        if let Ok(comm) = std::fs::read_to_string(&comm_path) {
            let comm = comm.trim().to_lowercase();
            if comm == search_lower || comm.starts_with(search_lower.as_str()) {
                candidates.push(pid);
                continue;
            }
        }
        // Fallback: /proc/<pid>/cmdline first token
        let cmdline_path = format!("/proc/{}/cmdline", pid);
        if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
            if let Some(first) = cmdline.split('\0').next() {
                if let Some(base) = std::path::Path::new(first)
                    .file_name()
                    .and_then(|s| s.to_str())
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
        }
    }
    // Debug log to diagnose multi-match or no-match situations.
    // Many apps (Brave, Spotify, Steam) spawn child processes with the
    // same `comm` name, leading to multiple candidates.  In that case we
    // accept the first (any) match — the PID is needed only for cgroup /
    // session identification, and any process from the same app tree will
    // belong to the same systemd user slice.
    eprintln!(
        "[resolve_pid_by_name] name=\"{}\" search_key=\"{}\" candidates={} -> {:?}",
        name,
        search_key,
        candidates.len(),
        candidates.first().copied()
    );
    candidates.into_iter().next()
}

pub fn list_audio_applications() -> NapiResult<Vec<AudioApp>> {
    pipewire::init();

    let main_loop = pipewire::main_loop::MainLoopRc::new(None)
        .map_err(|e| napi::Error::from_reason(format!("Failed to create PipeWire main loop: {}", e)))?;

    let context = pipewire::context::ContextRc::new(&main_loop, None)
        .map_err(|e| napi::Error::from_reason(format!("Failed to create PipeWire context: {}", e)))?;

    let core = context.connect(None)
        .map_err(|e| napi::Error::from_reason(format!("Failed to connect to PipeWire core: {}", e)))?;

    let registry = core.get_registry()
        .map_err(|e| napi::Error::from_reason(format!("Failed to get PipeWire registry: {}", e)))?;

    let apps = Rc::new(RefCell::new(Vec::<AudioApp>::new()));
    let apps_clone = apps.clone();

    let client_pids = Rc::new(RefCell::new(HashMap::<u32, i32>::new()));
    let client_pids_clone = client_pids.clone();

    let _reg_listener = registry.add_listener_local()
        .global(move |global| {
            if let Some(props) = global.props {
                eprintln!(
                    "[list-audio-apps] global id={} type={} media.class=\"{}\" app.process.id=\"{}\" client.id=\"{}\" app.name=\"{}\"",
                    global.id,
                    global.type_,
                    props.get("media.class").unwrap_or("(none)"),
                    props.get("application.process.id").unwrap_or("(none)"),
                    props.get("client.id").unwrap_or("(none)"),
                    props.get("application.name").unwrap_or("(none)"),
                );

                // Track PipeWire client PIDs. The registry emits Client
                // globals before the Node globals that reference them, so by
                // the time we see an audio node its owning client is known.
                if global.type_ == ObjectType::Client {
                    let client_name = props.get("application.name").unwrap_or("");

                    if let Some(pid_str) = props.get("application.process.id") {
                        if let Ok(pid) = pid_str.parse::<i32>() {
                            if pid > 0 && is_valid_pid(pid) {
                                eprintln!("[list-audio-apps]   -> client id={} name=\"{}\" has app.process.id={}, tracking", global.id, client_name, pid);
                                client_pids_clone.borrow_mut().insert(global.id, pid);
                            } else if pid > 0 {
                                eprintln!("[list-audio-apps]   -> client id={} name=\"{}\" has app.process.id={} but PID invalid, will try name match", global.id, client_name, pid);
                            } else {
                                eprintln!("[list-audio-apps]   -> client id={} name=\"{}\" has pid=0, will try name match", global.id, client_name);
                            }
                        } else {
                            eprintln!("[list-audio-apps]   -> client id={}: failed to parse pid=\"{}\", will try name match", global.id, pid_str);
                        }
                    } else {
                        eprintln!("[list-audio-apps]   -> client id={} name=\"{}\": no app.process.id, will try name match", global.id, client_name);
                    }
                    // When PipeWire does not provide `application.process.id`
                    // on Client objects (e.g. KDE Plasma / some Wayland setups),
                    // resolve the PID by matching the client's `application.name`
                    // against running processes via /proc.
                    let already_mapped = client_pids_clone.borrow().contains_key(&global.id);
                    if !already_mapped && !client_name.is_empty() {
                        let pid = resolve_pid_by_name(client_name);
                        eprintln!("[list-audio-apps]   -> resolved client id={} name=\"{}\" via /proc -> pid={:?}", global.id, client_name, pid);
                        if let Some(pid) = pid {
                            client_pids_clone.borrow_mut().insert(global.id, pid);
                        }
                    }
                }

                let media_class = props.get("media.class").unwrap_or("");
                let app_name = props
                    .get("application.name")
                    .or_else(|| props.get("node.name"))
                    .or_else(|| props.get("media.name"))
                    .unwrap_or("");
                let app_binary = props.get("application.process.binary").unwrap_or("");

                let proc_id = if media_class == "Stream/Output/Audio" {
                    eprintln!("[list-audio-apps]   -> processing Stream/Output/Audio node id={}, name=\"{}\", binary=\"{}\"", global.id, app_name, app_binary);
                    let node_pid = props
                        .get("application.process.id")
                        .and_then(|v| v.parse::<i32>().ok())
                        .filter(|pid| *pid > 0);
                    eprintln!(
                        "[list-audio-apps]     tier1 (node app.process.id): {:?}",
                        node_pid
                    );
                    // Accept it only if it passes /proc validation
                    let mut pid = node_pid;
                    if pid.is_some_and(|p| !is_valid_pid(p)) {
                        eprintln!(
                            "[list-audio-apps]     tier1 PID {} not in /proc, discarding",
                            pid.unwrap()
                        );
                        pid = None;
                    }
                    // Fallback: resolve via client.id -> client_pids for
                    // direct PipeWire clients.
                    let pid = pid.or_else(|| {
                        let cid = props
                            .get("client.id")
                            .and_then(|cid| cid.parse::<u32>().ok())?;
                        eprintln!(
                            "[list-audio-apps]     tier2: look up client.id={} in client_pids (size={})",
                            cid,
                            client_pids_clone.borrow().len()
                        );
                        let p = client_pids_clone.borrow().get(&cid).copied()?;
                        eprintln!(
                            "[list-audio-apps]     tier2: client.id={} -> pid={}, valid={}",
                            cid,
                            p,
                            is_valid_pid(p)
                        );
                        if is_valid_pid(p) { Some(p) } else { None }
                    });
                    // Last resort: scan /proc by binary name.
                    let pid = pid.or_else(|| {
                        eprintln!(
                            "[list-audio-apps]     tier3: /proc binary scan for \"{}\"",
                            app_binary
                        );
                        let r = resolve_pid_by_binary(app_binary);
                        eprintln!(
                            "[list-audio-apps]     tier3: result={:?}",
                            r
                        );
                        r
                    });
                    // Absolute last resort: match node's application.name
                    // via /proc (works when PipeWire omits both
                    // application.process.id and application.process.binary
                    // but DOES set application.name on the stream node).
                    let pid = pid.or_else(|| {
                        eprintln!(
                            "[list-audio-apps]     tier4: /proc name scan for \"{}\"",
                            app_name
                        );
                        let r = resolve_pid_by_name(app_name);
                        eprintln!(
                            "[list-audio-apps]     tier4: result={:?}",
                            r
                        );
                        r
                    });
                    eprintln!(
                        "[list-audio-apps]     final pid={}",
                        pid.unwrap_or(0)
                    );
                    pid.unwrap_or(0)
                } else {
                    props
                        .get("application.process.id")
                        .and_then(|v| v.parse::<i32>().ok())
                        .unwrap_or(0)
                };

                if media_class == "Stream/Output/Audio"
                    && !app_name.is_empty()
                    && !app_name.contains(CAPTURE_NODE_NAME)
                {
                    let mut list = apps_clone.borrow_mut();
                    if !list.iter().any(|a| a.id == global.id as i32) {
                        list.push(AudioApp {
                            id: global.id as i32,
                            name: app_name.to_string(),
                            process_id: proc_id,
                            bundle_id: None,
                        });
                    }
                }
            }
        })
        .register();

    let done = Rc::new(RefCell::new(false));
    let done_clone = done.clone();
    let pending = core.sync(0).map_err(|e| napi::Error::from_reason(format!("PipeWire sync failed: {}", e)))?;

    let main_loop_weak = main_loop.downgrade();
    let _core_listener = core.add_listener_local()
        .done(move |id, seq| {
            if id == pipewire::core::PW_ID_CORE && seq == pending {
                *done_clone.borrow_mut() = true;
                if let Some(ml) = main_loop_weak.upgrade() {
                    ml.quit();
                }
            }
        })
        .register();

    let mut iterations = 0;
    while !*done.borrow() && iterations < 100 {
        main_loop.loop_().iterate(pipewire::loop_::Timeout::Finite(std::time::Duration::from_millis(10)));
        iterations += 1;
    }

    let result = apps.borrow().clone();
    eprintln!("[list-audio-apps] returning {} audio app(s):", result.len());
    for app in &result {
        eprintln!("[list-audio-apps]   id={} name=\"{}\" pid={}", app.id, app.name, app.process_id);
    }
    eprintln!("[list-audio-apps] client_pids map ({}/{} entries):", client_pids.borrow().len(), client_pids.borrow().len());
    for (cid, pid) in client_pids.borrow().iter() {
        eprintln!("[list-audio-apps]   client {} -> pid {}", cid, pid);
    }
    Ok(result)
}

/// Starts audio capture.
///
/// When `target_app_id` is a positive PipeWire node ID, only that
/// application's audio is captured (exclusive-per-app).  When `target_app_id`
/// is `-1`, every active `Stream/Output/Audio` node is linked into the virtual
/// capture sink — i.e. system-wide audio (all applications the user hears).
pub fn start_audio_capture(target_app_id: &Either<String, i32>) -> NapiResult<bool> {
    let (node_id, system_audio) = match target_app_id {
        Either::B(-1) => (None, true),
        Either::B(n) if *n >= 0 => (Some(*n as u32), false),
        Either::A(s) if s.trim() == "__system_audio__" => (None, true),
        Either::A(s) => {
            let n = s.trim().parse::<u32>().ok().ok_or_else(|| {
                napi::Error::from_reason(
                    "A PipeWire node ID or -1 (system audio) is required",
                )
            })?;
            (Some(n), false)
        }
        _ => {
            return Err(napi::Error::from_reason(
                "A PipeWire node ID or -1 (system audio) is required",
            ));
        }
    };

    let mut state_guard = CAPTURE_STATE.lock().map_err(|e| napi::Error::from_reason(e.to_string()))?;
    let state = state_guard.get_or_insert_with(CaptureState::new);

    // Restart semantics: tear down any previous session so that an updated
    // target takes effect immediately.
    stop_session(state);

    let target = TargetSpec {
        node_id,
        system_audio,
        ..TargetSpec::default()
    };
    let session = spawn_capture_session(target)?;
    state.virtual_sink_id = session
        .shared
        .lock()
        .ok()
        .and_then(|s| s.sink_id)
        .map(|id| id as i32);
    state.target_app_id = node_id.map(|id| id as i32);
    state.active_links.clear();
    state.is_active = true;
    state.session = Some(session);

    Ok(true)
}

pub fn stop_audio_capture() -> NapiResult<bool> {
    let mut state_guard = CAPTURE_STATE.lock().map_err(|e| napi::Error::from_reason(e.to_string()))?;
    if let Some(state) = state_guard.as_mut() {
        stop_session(state);
    }
    Ok(true)
}

pub fn is_audio_capture_active() -> NapiResult<bool> {
    let mut state_guard = CAPTURE_STATE.lock().map_err(|e| napi::Error::from_reason(e.to_string()))?;
    if let Some(state) = state_guard.as_mut() {
        // Detect an unexpectedly terminated worker thread.
        let finished = state.session.as_ref().map(|s| s.join.is_finished()).unwrap_or(false);
        if finished {
            stop_session(state);
        }
        if let Some(session) = &state.session {
            if let Ok(shared) = session.shared.lock() {
                state.active_links = shared.link_ids.clone();
                if state.virtual_sink_id.is_none() {
                    state.virtual_sink_id = shared.sink_id.map(|id| id as i32);
                }
            }
        }
        Ok(state.is_active)
    } else {
        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// Window-to-audio-source resolution
// ---------------------------------------------------------------------------

/// Resolves an X11 window ID to its owning process's PID via `_NET_WM_PID`
/// and returns the matching `AudioApp` if the process is producing audio
/// through PipeWire. Returns `None` when the window has no PID property,
/// the process is not emitting audio, or X11 is unavailable.
pub fn resolve_audio_by_x11_window(window_id: u32) -> Option<AudioApp> {
    unsafe {
        let display = x11::xlib::XOpenDisplay(std::ptr::null());
        if display.is_null() {
            return None;
        }

        let atom_name = std::ffi::CString::new("_NET_WM_PID").ok()?;
        let atom = x11::xlib::XInternAtom(display, atom_name.as_ptr(), 1);
        if atom == 0 {
            x11::xlib::XCloseDisplay(display);
            return None;
        }

        let mut actual_type: x11::xlib::Atom = 0;
        let mut actual_format: std::os::raw::c_int = 0;
        let mut nitems: std::os::raw::c_ulong = 0;
        let mut bytes_after: std::os::raw::c_ulong = 0;
        let mut prop: *mut u8 = std::ptr::null_mut();

        let status = x11::xlib::XGetWindowProperty(
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
        );

        let pid = if status == 0 && !prop.is_null() && nitems > 0 && actual_format == 32 {
            Some(*(prop as *const u32))
        } else {
            None
        };

        if !prop.is_null() {
            x11::xlib::XFree(prop as *mut std::ffi::c_void);
        }
        x11::xlib::XCloseDisplay(display);

        let pid = pid?;
        let apps = list_audio_applications().ok()?;
        apps.into_iter().find(|app| app.process_id == pid as i32)
    }
}

/// Finds an audio app whose name best matches the given label string (e.g.
/// a `MediaStreamTrack.label` from `getDisplayMedia` on Wayland). Returns
/// `None` when no reasonable match is found.
pub fn resolve_audio_by_name(label: &str) -> Option<AudioApp> {
    eprintln!("[resolve-by-name] label=\"{}\"", label);
    let apps = list_audio_applications().ok()?;
    eprintln!("[resolve-by-name] audio apps ({}):", apps.len());
    for app in &apps {
        eprintln!("[resolve-by-name]   name=\"{}\" id={} pid={}", app.name, app.id, app.process_id);
    }
    let result = find_best_audio_match(&apps, label);
    if let Some(ref app) = result {
        eprintln!("[resolve-by-name]   MATCHED -> \"{}\" (id={}, pid={})", app.name, app.id, app.process_id);
    } else {
        eprintln!("[resolve-by-name]   no match for \"{}\"", label);
    }
    result
}

fn find_best_audio_match(apps: &[AudioApp], query: &str) -> Option<AudioApp> {
    let query_lower = query.to_lowercase();

    // 1. Exact match (case-insensitive)
    if let Some(app) = apps
        .iter()
        .find(|a| a.name.to_lowercase() == query_lower)
    {
        return Some(app.clone());
    }

    // 2. Audio app name is a contiguous substring of the query
    //    e.g. "Firefox" inside "Mozilla Firefox"
    if let Some(app) = apps.iter().find(|a| {
        let name_lower = a.name.to_lowercase();
        query_lower.contains(&name_lower)
    }) {
        return Some(app.clone());
    }

    // 3. Query is a substring of the audio app name
    if let Some(app) = apps.iter().find(|a| {
        let name_lower = a.name.to_lowercase();
        name_lower.contains(&query_lower)
    }) {
        return Some(app.clone());
    }

    // 4. First word of the query matches part of any app name
    //    e.g. "Firefox" from "Firefox — example.com"
    let first_word = query_lower.split_whitespace().next()?;
    apps.iter()
        .find(|a| {
            let name_lower = a.name.to_lowercase();
            name_lower.contains(first_word)
                || first_word.contains(&name_lower)
        })
        .cloned()
}

/// Extracts a potential application-name string from a PipeWire node's
/// properties.  Checks several property keys that xdg-desktop-portal
/// implementations (GNOME, KDE, …) may set on the screencast stream.
fn portal_window_name(props: &DictRef) -> Option<String> {
    // Primary: well-known portal screencast metadata keys
    for key in &[
        "portal.screencast.application",
        "portal.screencast.title",
        "window.name",
    ] {
        if let Some(v) = props.get(key).filter(|v| !v.is_empty()) {
            return Some(v.to_string());
        }
    }

    // Secondary: PipeWire-standard node metadata (may be the portal's own
    // name or the captured application's name, depending on the DE).
    for key in &["application.name", "node.name", "media.name"] {
        if let Some(v) = props.get(key).filter(|v| !v.is_empty()) {
            // Skip the portal's own identity – it does not identify the
            // captured window.
            if v != "xdg-desktop-portal" && !v.contains("pipewire") {
                return Some(v.to_string());
            }
        }
    }

    None
}

/// Scans the PipeWire registry for a video/screen-capture node that was
/// created by xdg-desktop-portal and extracts the captured window's
/// application name, then matches it against the active audio application
/// list.  Returns `None` when no capture node is found, its properties
/// carry no meaningful name, or no audio app matches.
///
/// Called **after** `getDisplayMedia()` (Wayland portal) so the portal's
/// PipeWire stream node should be present in the registry.
pub fn resolve_audio_by_captured_window() -> Option<AudioApp> {
    pipewire::init();

    let main_loop = pipewire::main_loop::MainLoopRc::new(None).ok()?;
    let context = pipewire::context::ContextRc::new(&main_loop, None).ok()?;
    let core = context.connect(None).ok()?;
    let registry = core.get_registry().ok()?;

    eprintln!("[pw-introspect] scanning PipeWire registry for portal capture nodes");

    let capture_names = Rc::new(RefCell::new(Vec::<String>::new()));
    let capture_names_clone = capture_names.clone();

    let all_globals = Rc::new(RefCell::new(Vec::<String>::new()));
    let all_globals_clone = all_globals.clone();

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            let props = match global.props {
                Some(props) => props,
                None => {
                    eprintln!("[pw-introspect]   global id={} type={}: NO PROPS", global.id, global.type_);
                    return;
                },
            };
            let media_class = props.get("media.class").unwrap_or("(none)");
            let pid = props.get("application.process.id").unwrap_or("(none)");
            let node_name = props.get("node.name").unwrap_or("(none)");
            let app_name = props.get("application.name").unwrap_or("(none)");

            eprintln!(
                "[pw-introspect]   global id={} type={} media.class=\"{}\" node.name=\"{}\" application.name=\"{}\" app.pid={}",
                global.id, global.type_, media_class, node_name, app_name, pid
            );

            // Log ALL properties for video/screencast nodes
            if media_class.starts_with("Video/") || media_class.starts_with("Stream/Output/Video") {
                let mut log = format!("[pw-introspect]   -> ALL PROPS for id={}:", global.id);
                for (k, v) in props.iter() {
                    log.push_str(&format!(" {}={}", k, v));
                }
                eprintln!("{}", log);

                if let Some(name) = portal_window_name(props) {
                    let mut list = capture_names_clone.borrow_mut();
                    if !list.contains(&name) {
                        eprintln!("[pw-introspect]   -> extracted name: \"{}\"", name);
                        list.push(name);
                    }
                } else {
                    eprintln!("[pw-introspect]   -> portal_window_name returned None for this video node");
                }
            }

            let mut gl = all_globals_clone.borrow_mut();
            gl.push(format!("id={} type={} class={}", global.id, global.type_, media_class));
        })
        .register();

    // Pump the loop briefly so the registry listener fires.
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

    let mut iterations = 0;
    while !*done.borrow() && iterations < 100 {
        main_loop
            .loop_()
            .iterate(pipewire::loop_::Timeout::Finite(std::time::Duration::from_millis(10)));
        iterations += 1;
    }

    let global_list = all_globals.borrow();
    eprintln!("[pw-introspect] registry scan complete ({} global(s))", global_list.len());
    for g in global_list.iter() {
        eprintln!("[pw-introspect]   found: {}", g);
    }

    let names = capture_names.borrow().clone();
    eprintln!("[pw-introspect] extracted capture names: {:?}", names);

    let apps = list_audio_applications().ok()?;
    eprintln!("[pw-introspect] audio apps ({}):", apps.len());
    for app in &apps {
        eprintln!("[pw-introspect]   name=\"{}\" id={} pid={}", app.name, app.id, app.process_id);
    }

    for name in &names {
        eprintln!("[pw-introspect] trying to match name=\"{}\" against audio apps...", name);
        if let Some(app) = find_best_audio_match(&apps, name) {
            eprintln!("[pw-introspect]   MATCHED -> \"{}\" (id={}, pid={})", app.name, app.id, app.process_id);
            return Some(app);
        }
        eprintln!("[pw-introspect]   no match for \"{}\"", name);
    }

    eprintln!("[pw-introspect] all names exhausted, returning None");
    None
}
