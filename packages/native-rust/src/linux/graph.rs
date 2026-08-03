use super::capture::SessionShared;
use super::capture::port_channel_from_name;
use super::procinfo::{
    ProcEntry, client_sec_pid, is_valid_pid, iter_proc, resolve_pid_by_binary, resolve_pid_by_name,
};
use super::{CAPTURE_NODE_NAME, LINK_FACTORY};
use pipewire::properties::{PropertiesBox, properties};
use pipewire::registry::GlobalObject;
use pipewire::spa::param::audio::{AudioInfoRaw, AudioInfoRawFlags};
use pipewire::spa::sys::{
    SPA_AUDIO_CHANNEL_AUX0, SPA_AUDIO_CHANNEL_AUX63, SPA_AUDIO_CHANNEL_BC, SPA_AUDIO_CHANNEL_BLC,
    SPA_AUDIO_CHANNEL_BRC, SPA_AUDIO_CHANNEL_FC, SPA_AUDIO_CHANNEL_FCH, SPA_AUDIO_CHANNEL_FL,
    SPA_AUDIO_CHANNEL_FLC, SPA_AUDIO_CHANNEL_FLH, SPA_AUDIO_CHANNEL_FLW, SPA_AUDIO_CHANNEL_FR,
    SPA_AUDIO_CHANNEL_FRC, SPA_AUDIO_CHANNEL_FRH, SPA_AUDIO_CHANNEL_FRW, SPA_AUDIO_CHANNEL_LFE,
    SPA_AUDIO_CHANNEL_LFE2, SPA_AUDIO_CHANNEL_LLFE, SPA_AUDIO_CHANNEL_MONO, SPA_AUDIO_CHANNEL_NA,
    SPA_AUDIO_CHANNEL_RC, SPA_AUDIO_CHANNEL_RL, SPA_AUDIO_CHANNEL_RLC, SPA_AUDIO_CHANNEL_RLFE,
    SPA_AUDIO_CHANNEL_RR, SPA_AUDIO_CHANNEL_RRC, SPA_AUDIO_CHANNEL_SL, SPA_AUDIO_CHANNEL_SR,
    SPA_AUDIO_CHANNEL_TC, SPA_AUDIO_CHANNEL_TFC, SPA_AUDIO_CHANNEL_TFL, SPA_AUDIO_CHANNEL_TFLC,
    SPA_AUDIO_CHANNEL_TFR, SPA_AUDIO_CHANNEL_TFRC, SPA_AUDIO_CHANNEL_TRC, SPA_AUDIO_CHANNEL_TRL,
    SPA_AUDIO_CHANNEL_TRR, SPA_AUDIO_CHANNEL_TSL, SPA_AUDIO_CHANNEL_TSR,
};
use pipewire::spa::utils::dict::DictRef;
use pipewire::types::ObjectType;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(super) struct TargetSpec {
    pub(super) node_id: Option<u32>,
    pub(super) pid: Option<u32>,
    pub(super) binary: Option<String>,
    pub(super) client_id: Option<u32>,
    pub(super) app_name: Option<String>,
    pub(super) system_audio: bool,
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
pub(super) struct ChannelLayout {
    pub(super) position: String,
}

impl ChannelLayout {
    pub(super) fn stereo() -> Self {
        Self {
            position: "FL,FR".into(),
        }
    }

    pub(super) fn from_audio_info(info: &AudioInfoRaw) -> Option<Self> {
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
            let name = channel_short_name(channel);
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

/// Short names from SPA's static `spa_type_audio_channel` table
/// (`spa/param/audio/raw.h`). The table is ABI-stable across `PipeWire`
/// versions, but the `spa_type_audio_channel_to_short_name` lookup function is
/// only exported by libspa >= 1.4, so it is replicated here to keep the crate
/// buildable against older `PipeWire` (Ubuntu 24.04 ships 1.0.5).
fn channel_short_name(channel: u32) -> String {
    if (SPA_AUDIO_CHANNEL_AUX0..=SPA_AUDIO_CHANNEL_AUX63).contains(&channel) {
        return format!("AUX{}", channel - SPA_AUDIO_CHANNEL_AUX0);
    }
    match channel {
        SPA_AUDIO_CHANNEL_NA => "NA",
        SPA_AUDIO_CHANNEL_MONO => "MONO",
        SPA_AUDIO_CHANNEL_FL => "FL",
        SPA_AUDIO_CHANNEL_FR => "FR",
        SPA_AUDIO_CHANNEL_FC => "FC",
        SPA_AUDIO_CHANNEL_LFE => "LFE",
        SPA_AUDIO_CHANNEL_SL => "SL",
        SPA_AUDIO_CHANNEL_SR => "SR",
        SPA_AUDIO_CHANNEL_FLC => "FLC",
        SPA_AUDIO_CHANNEL_FRC => "FRC",
        SPA_AUDIO_CHANNEL_RC => "RC",
        SPA_AUDIO_CHANNEL_RL => "RL",
        SPA_AUDIO_CHANNEL_RR => "RR",
        SPA_AUDIO_CHANNEL_TC => "TC",
        SPA_AUDIO_CHANNEL_TFL => "TFL",
        SPA_AUDIO_CHANNEL_TFC => "TFC",
        SPA_AUDIO_CHANNEL_TFR => "TFR",
        SPA_AUDIO_CHANNEL_TRL => "TRL",
        SPA_AUDIO_CHANNEL_TRC => "TRC",
        SPA_AUDIO_CHANNEL_TRR => "TRR",
        SPA_AUDIO_CHANNEL_RLC => "RLC",
        SPA_AUDIO_CHANNEL_RRC => "RRC",
        SPA_AUDIO_CHANNEL_FLW => "FLW",
        SPA_AUDIO_CHANNEL_FRW => "FRW",
        SPA_AUDIO_CHANNEL_LFE2 => "LFE2",
        SPA_AUDIO_CHANNEL_FLH => "FLH",
        SPA_AUDIO_CHANNEL_FCH => "FCH",
        SPA_AUDIO_CHANNEL_FRH => "FRH",
        SPA_AUDIO_CHANNEL_TFLC => "TFLC",
        SPA_AUDIO_CHANNEL_TFRC => "TFRC",
        SPA_AUDIO_CHANNEL_TSL => "TSL",
        SPA_AUDIO_CHANNEL_TSR => "TSR",
        SPA_AUDIO_CHANNEL_LLFE => "LLFE",
        SPA_AUDIO_CHANNEL_RLFE => "RLFE",
        SPA_AUDIO_CHANNEL_BC => "BC",
        SPA_AUDIO_CHANNEL_BLC => "BLC",
        SPA_AUDIO_CHANNEL_BRC => "BRC",
        _ => "UNK",
    }
    .into()
}

struct PortInfo {
    node: u32,
    is_output: bool,
    channel: Option<String>,
}

pub(super) struct DefaultSinkWatch {
    pub(super) id: u32,
    pub(super) _proxy: pipewire::node::Node,
    pub(super) _listener: pipewire::node::NodeListener,
}

pub(super) struct GraphTracker {
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
    pub(super) fn new(
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

    pub(super) fn add_global(
        &mut self,
        global: &GlobalObject<&DictRef>,
        shared: &Arc<Mutex<SessionShared>>,
    ) {
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

    pub(super) fn remove_global(&mut self, id: u32, shared: &Arc<Mutex<SessionShared>>) {
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

    pub(super) fn set_default_sink_name(&mut self, name: String) {
        self.default_sink_name = Some(name);
    }
    pub(super) fn take_pending_metadata(&mut self) -> Option<GlobalObject<PropertiesBox>> {
        self.pending_metadata.take()
    }

    pub(super) fn default_sink_id(&self) -> Option<u32> {
        let name = self.default_sink_name.as_deref()?;
        self.system_sinks
            .iter()
            .find_map(|(id, (sn, _))| (sn == name).then_some(*id))
    }

    pub(super) fn system_sink_global(&self, id: u32) -> Option<&GlobalObject<PropertiesBox>> {
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

    pub(super) fn on_capture_node_destroyed(&mut self, shared: &Arc<Mutex<SessionShared>>) {
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

    pub(super) fn drain_links(&mut self) -> Vec<pipewire::link::Link> {
        self.created_links.clear();
        self.linked_pairs.clear();
        self.pending_pairs.clear();
        self.link_proxies.drain().map(|(_, link)| link).collect()
    }

    pub(super) fn change_target(&mut self, new_target: TargetSpec, core: &pipewire::core::Core) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use pipewire::spa::param::audio::{AudioInfoRaw, AudioInfoRawFlags};

    // Values from the SPA audio channel enum (spa/param/audio/raw.h).
    const CH_NA: u32 = 1;
    const CH_FL: u32 = 3;
    const CH_FR: u32 = 4;
    const CH_FC: u32 = 5;
    const CH_AUX0: u32 = 4096;
    const MAX_POSITION: usize = 64;

    fn app_node_info(
        pid: Option<u32>,
        binary: Option<&str>,
        client_id: Option<u32>,
        app_name: Option<&str>,
    ) -> AppNodeInfo {
        AppNodeInfo {
            pid,
            binary: binary.map(str::to_string),
            client_id,
            app_name: app_name.map(str::to_string),
        }
    }

    fn info_with_position(channels: u32, position: &[u32]) -> AudioInfoRaw {
        let mut info = AudioInfoRaw::new();
        info.set_channels(channels);
        let mut pos = [CH_NA; MAX_POSITION];
        pos[..position.len()].copy_from_slice(position);
        info.set_position(pos);
        info
    }

    #[test]
    fn target_matches_on_node_id() {
        let target = TargetSpec {
            node_id: Some(7),
            ..TargetSpec::default()
        };
        assert!(target.matches(7, &AppNodeInfo::default()));
        assert!(!target.matches(8, &AppNodeInfo::default()));
    }

    #[test]
    fn target_matches_on_client_id_pid_binary_and_name() {
        let target = TargetSpec {
            client_id: Some(42),
            pid: Some(1000),
            binary: Some("firefox".into()),
            app_name: Some("Firefox".into()),
            ..TargetSpec::default()
        };
        assert!(target.matches(1, &app_node_info(Some(1000), None, None, None)));
        assert!(target.matches(1, &app_node_info(None, None, Some(42), None)));
        assert!(target.matches(1, &app_node_info(None, Some("firefox"), None, None)));
        assert!(target.matches(1, &app_node_info(None, None, None, Some("firefox"))));
    }

    #[test]
    fn target_matches_app_name_case_insensitively() {
        let target = TargetSpec {
            app_name: Some("Spotify".into()),
            ..TargetSpec::default()
        };
        assert!(target.matches(1, &app_node_info(None, None, None, Some("spotify"))));
        assert!(!target.matches(1, &app_node_info(None, None, None, Some("VLC"))));
    }

    #[test]
    fn target_matches_binary_exactly() {
        let target = TargetSpec {
            binary: Some("zenlesszonezero.exe".into()),
            ..TargetSpec::default()
        };
        assert!(target.matches(
            1,
            &app_node_info(None, Some("zenlesszonezero.exe"), None, None)
        ));
        assert!(!target.matches(1, &app_node_info(None, Some("zenless.exe"), None, None)));
    }

    #[test]
    fn system_audio_is_not_part_of_matches() {
        // `system_audio` is honored one level up in `is_linkable_app`
        // (`self.target.system_audio || ...`); `matches` itself only compares
        // the learned target fields.
        let target = TargetSpec {
            system_audio: true,
            ..TargetSpec::default()
        };
        assert!(!target.matches(1, &app_node_info(None, None, None, Some("x"))));
    }

    #[test]
    fn learn_only_fills_matching_node_id() {
        let mut target = TargetSpec {
            node_id: Some(5),
            ..TargetSpec::default()
        };
        target.learn(4, &app_node_info(Some(9), None, None, None));
        assert!(target.pid.is_none());
        target.learn(5, &app_node_info(Some(9), None, None, None));
        assert_eq!(target.pid, Some(9));
    }

    #[test]
    fn learn_keeps_first_non_none_field() {
        let mut target = TargetSpec {
            node_id: Some(5),
            ..TargetSpec::default()
        };
        target.learn(5, &app_node_info(Some(9), Some("bin"), None, Some("name")));
        target.learn(5, &app_node_info(Some(10), Some("bin2"), Some(7), None));
        // First values win: the second learn must not overwrite pid/binary/name.
        assert_eq!(target.pid, Some(9));
        assert_eq!(target.binary.as_deref(), Some("bin"));
        assert_eq!(target.app_name.as_deref(), Some("name"));
        assert_eq!(target.client_id, Some(7));
    }

    #[test]
    fn layout_stereo_uses_fl_fr() {
        assert_eq!(ChannelLayout::stereo().position, "FL,FR");
    }

    #[test]
    fn layout_from_audio_info_requires_positioned_format() {
        let mut info = info_with_position(2, &[CH_FL, CH_FR]);
        info.set_flags(AudioInfoRawFlags::UNPOSITIONED);
        assert!(ChannelLayout::from_audio_info(&info).is_none());
    }

    #[test]
    fn layout_from_audio_info_stereo() {
        let info = info_with_position(2, &[CH_FL, CH_FR]);
        let layout =
            ChannelLayout::from_audio_info(&info).unwrap_or_else(|| panic!("stereo layout"));
        assert_eq!(layout.position, "FL,FR");
    }

    #[test]
    fn layout_from_audio_info_rejects_zero_or_oversized_channel_counts() {
        let zero = info_with_position(0, &[]);
        assert!(ChannelLayout::from_audio_info(&zero).is_none());

        let big = info_with_position(9, &[CH_NA; 9]);
        assert!(ChannelLayout::from_audio_info(&big).is_none());
    }

    #[test]
    fn layout_from_audio_info_rejects_aux_channels() {
        let aux = info_with_position(2, &[CH_AUX0, CH_AUX0]);
        assert!(ChannelLayout::from_audio_info(&aux).is_none());
    }

    #[test]
    fn layout_from_audio_info_keeps_na_channel_position() {
        // NA ("N/A, silent") is a valid position token in the SPA table and
        // passes through as "NA" rather than rejecting the layout.
        let na = info_with_position(2, &[CH_NA, CH_FC]);
        let layout =
            ChannelLayout::from_audio_info(&na).unwrap_or_else(|| panic!("layout with NA"));
        assert_eq!(layout.position, "NA,FC");
    }

    #[test]
    fn channel_short_name_maps_known_channels() {
        assert_eq!(channel_short_name(CH_FL), "FL");
        assert_eq!(channel_short_name(CH_FR), "FR");
        assert_eq!(channel_short_name(CH_FC), "FC");
        assert_eq!(channel_short_name(CH_NA), "NA");
        assert_eq!(channel_short_name(CH_AUX0), "AUX0");
    }

    #[test]
    fn channel_short_name_defaults_unknown_channels_to_unk() {
        // The SPA C table returns "UNK" (a non-null string) for values outside
        // the channel enum; document that so callers do not expect a failure.
        assert_eq!(channel_short_name(99), "UNK");
        assert_eq!(channel_short_name(u32::MAX), "UNK");
        // The AUX range ends at AUX63; anything beyond falls through to "UNK".
        assert_eq!(channel_short_name(SPA_AUDIO_CHANNEL_AUX63 + 1), "UNK");
    }

    #[test]
    fn channel_short_name_generates_aux_names_from_offset() {
        assert_eq!(
            channel_short_name(pipewire::spa::sys::SPA_AUDIO_CHANNEL_AUX42),
            "AUX42"
        );
        assert_eq!(channel_short_name(SPA_AUDIO_CHANNEL_AUX63), "AUX63");
    }
}
