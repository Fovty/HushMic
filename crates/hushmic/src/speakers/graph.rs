//! What the speakers cleaner reads from a `pw-dump` snapshot: the real
//! outputs, the system default output, whether its own nodes are up, where
//! its cleaned sound goes and what plays into it. Pure parsing, plus the
//! one call HushMic's own players make to stay off the speakers chain.

use super::{PLAYBACK, SINK};
use crate::pipewire::{self, Source};
use std::time::Duration;

/// One `pw-dump` snapshot, as far as the speakers cleaner cares.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    /// The outputs it may play to: every `Audio/Sink` but HushMic Speakers
    /// itself, anything named like a monitor, and one that feeds a source
    /// HushMic captures ([`Graph::feeders`]). Hardware first (ALSA, which
    /// USB is, and Bluetooth), then other outputs, then virtual ones, each
    /// in dump order: the first is the fallback.
    pub sinks: Vec<Source>,
    /// Of [`Graph::sinks`], the virtual ones (`node.virtual`, one half of a
    /// loopback or filter chain, a null sink): shown, and played to, only
    /// as the system default or when picked by name.
    pub virtual_sinks: Vec<String>,
    /// Outputs whose sound reaches a source HushMic captures (the sink side
    /// of a virtual microphone, a sink whose monitor it records): never
    /// played to, so the cleaned sound cannot loop into the microphone.
    pub feeders: Vec<String>,
    /// The effective system default output (`default.audio.sink`, the one
    /// the session manager links streams to).
    pub default_effective: Option<String>,
    /// The configured one (`default.configured.audio.sink`, the user's pick).
    pub default_configured: Option<String>,
    /// `hushmic_sink` is in the graph.
    pub sink_up: bool,
    /// `hushmic_playback` is in the graph.
    pub playback_up: bool,
    /// Its node id (what a re-target's metadata is keyed by).
    pub playback_id: Option<u32>,
    /// What `hushmic_playback` is linked into (the output it plays on).
    pub playing_to: Vec<String>,
    /// The streams linked into `hushmic_sink` (what plays through it).
    pub fed_by: Vec<String>,
    /// The same streams, with what they say about themselves.
    pub writers: Vec<pipewire::Reader>,
    /// Every playback stream but HushMic's own.
    pub out_streams: Vec<OutStream>,
    /// HushMic's own players.
    pub own_players: Vec<OutStream>,
    /// The node id of every `Audio/Sink`, HushMic Speakers included.
    pub sink_ids: Vec<(String, u32)>,
    /// `hushmic_sink`'s scheduling state.
    pub sink_state: Option<String>,
    /// The `node.force-quantum` on `hushmic_sink` (0: unpinned).
    pub sink_quantum: Option<u32>,
    /// The routing targets in the `default` metadata: (node id, key,
    /// value as text) for `target.object` and `target.node`.
    pub targets: Vec<(u32, String, String)>,
    /// `hushmic_sink`'s object serial.
    pub sink_serial: Option<u64>,
}

impl Graph {
    /// Read `dump`; an unparseable one reads as an empty graph.
    pub fn read(dump: &str) -> Graph {
        Graph::read_for(dump, None)
    }

    /// [`Graph::read`], knowing the microphone HushMic is set to (`mic`,
    /// a source's node.name) when no link shows it now.
    pub fn read_for(dump: &str, mic: Option<&str>) -> Graph {
        let (default_effective, default_configured) = parse_default_sink(dump);
        let kinds = sink_kinds(dump, mic);
        Graph {
            sinks: kinds.sinks,
            virtual_sinks: kinds.virtual_sinks,
            feeders: kinds.feeders,
            default_effective,
            default_configured,
            sink_up: pipewire::parse_node_id(dump, SINK).is_some(),
            playback_up: pipewire::parse_node_id(dump, PLAYBACK).is_some(),
            playback_id: pipewire::parse_node_id(dump, PLAYBACK),
            playing_to: parse_consumers(dump, PLAYBACK),
            fed_by: pipewire::parse_feeders(dump, SINK),
            writers: pipewire::parse_writers(dump, SINK),
            out_streams: parse_out_streams(dump),
            own_players: parse_own_players(dump),
            sink_ids: parse_sink_ids(dump),
            sink_state: pipewire::node_state(dump, SINK),
            sink_quantum: node_quantum(dump, SINK),
            targets: parse_targets(dump),
            sink_serial: pipewire::parse_node_serial(dump, SINK),
        }
    }

    /// The routing targets that name the node `id` (serial `serial`) by
    /// number, each as it is read: `target.node` = the id, and on the same
    /// stream `target.object` = the serial (a mixer writes both; WirePlumber
    /// reads a number there as a serial). A `target.object` alone is left:
    /// a serial is never given again. With `keep_pairs` a stream whose
    /// `target.node` has the serial beside it is left too: WirePlumber 0.4.13
    /// and newer go by the serial and never fall back to the id, and the
    /// pair is what keeps its saved pick for the app. Pure.
    pub fn targets_naming(
        &self,
        id: u32,
        serial: Option<u64>,
        keep_pairs: bool,
    ) -> Vec<(u32, String, String)> {
        targets_naming(&self.targets, id, serial, keep_pairs)
    }

    /// The value of `key` in the `default` metadata for node `id`.
    pub fn target_of(&self, id: u32, key: &str) -> Option<&str> {
        self.targets
            .iter()
            .find(|(i, k, _)| *i == id && k == key)
            .map(|(_, _, v)| v.as_str())
    }

    /// An output's node id (HushMic Speakers' too).
    pub fn node_id(&self, name: &str) -> Option<u32> {
        self.sink_ids
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
    }

    /// Whether `name` is one of the outputs it may play to.
    pub fn has_sink(&self, name: &str) -> bool {
        self.sinks.iter().any(|s| s.name == name)
    }

    /// Whether `name` feeds a source HushMic captures.
    pub fn is_feeder(&self, name: &str) -> bool {
        self.feeders.iter().any(|s| s == name)
    }

    /// Whether a stream's own target (a name, or a node id as text) is an
    /// output that feeds a source HushMic captures.
    pub fn names_feeder(&self, target: &str) -> bool {
        self.is_feeder(target)
            || self
                .feeders
                .iter()
                .any(|f| self.node_id(f).is_some_and(|id| id.to_string() == target))
    }

    /// HushMic Speakers' playback is linked into an output that feeds a
    /// source HushMic captures: the session manager put it there with
    /// nothing else left (it never asks for one).
    pub fn plays_into_mic(&self) -> bool {
        self.playing_to.iter().any(|t| self.is_feeder(t))
    }

    /// Where the playback plays, outputs that feed the microphone left out.
    pub fn playing_on(&self) -> Option<&str> {
        self.playing_to
            .iter()
            .find(|t| !self.is_feeder(t))
            .map(String::as_str)
    }

    /// The outputs to offer: the hardware and other real ones, and a
    /// virtual one only as the system default (now, or the last real one
    /// `last_default`) or as `chosen`.
    pub fn offered(&self, chosen: Option<&str>, last_default: Option<&str>) -> Vec<Source> {
        let defaults = [
            self.default_effective.as_deref(),
            self.default_configured.as_deref(),
            last_default,
            chosen,
        ];
        self.sinks
            .iter()
            .filter(|s| {
                !self.virtual_sinks.contains(&s.name) || defaults.contains(&Some(s.name.as_str()))
            })
            .cloned()
            .collect()
    }

    /// The system default output as the session manager has it: the
    /// effective key, else the configured one.
    pub fn default_sink(&self) -> Option<&str> {
        self.default_effective
            .as_deref()
            .or(self.default_configured.as_deref())
    }

    /// The system default output, when it is a real output that is there
    /// (not HushMic Speakers, not a device that is gone).
    pub fn real_default(&self) -> Option<&str> {
        [
            self.default_effective.as_deref(),
            self.default_configured.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|d| self.has_sink(d))
    }

    /// The session manager links streams to HushMic Speakers now: the
    /// effective default output is ours.
    pub fn default_is_ours(&self) -> bool {
        self.default_sink() == Some(SINK)
    }

    /// The user made HushMic Speakers the system default output: the
    /// configured key says so (it stays while the sink is down, when the
    /// session manager falls back to a real output for the moment), or the
    /// effective one does without a configured key.
    pub fn user_default_is_ours(&self) -> bool {
        match self.default_configured.as_deref() {
            Some(c) => c == SINK,
            None => self.default_effective.as_deref() == Some(SINK),
        }
    }

    /// A real output's description (its name when it has none).
    pub fn description_of(&self, name: &str) -> Option<&str> {
        self.sinks
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.description.as_str())
    }
}

/// Every `Audio/Sink` node as (node.name, id). Pure.
pub fn parse_sink_ids(dump: &str) -> Vec<(String, u32)> {
    let Ok(serde_json::Value::Array(v)) = serde_json::from_str(dump) else {
        return Vec::new();
    };
    v.iter()
        .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Node"))
        .filter_map(|o| {
            let props = o.get("info")?.get("props")?;
            if props.get("media.class")?.as_str()? != "Audio/Sink" {
                return None;
            }
            Some((
                props.get("node.name")?.as_str()?.to_string(),
                u32::try_from(o.get("id")?.as_u64()?).ok()?,
            ))
        })
        .collect()
}

/// A playback stream: its node id, object.serial, and the outputs
/// (`Audio/Sink` nodes, by name) it is linked into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutStream {
    pub id: u32,
    pub serial: u64,
    pub to: Vec<String>,
    /// The output the stream asked for in its own properties
    /// (`target.object`, or the older `node.target`: a name, or a node id
    /// or serial as text); None when it follows the default.
    pub own_target: Option<String>,
}

impl OutStream {
    /// It plays into HushMic Speakers now.
    pub fn on_sink(&self) -> bool {
        self.to.iter().any(|t| t == SINK)
    }

    /// Its own properties pick HushMic Speakers as it is now (`sink_id`:
    /// the sink's node id), so it stays there with no metadata of ours.
    pub fn targets_sink(&self, sink_id: Option<u32>) -> bool {
        match self.own_target.as_deref() {
            Some(t) if t == SINK => true,
            Some(t) => sink_id.is_some_and(|id| t == id.to_string()),
            None => false,
        }
    }
}

/// Every playback stream (`Stream/Output/Audio`) but HushMic's own, with
/// the outputs it plays into. Pure.
pub fn parse_out_streams(dump: &str) -> Vec<OutStream> {
    streams_out(dump, |n| !n.starts_with("hushmic"))
}

/// HushMic's own players (Mic check's replay, the Files player: node
/// names `hushmic-*`; the chain's own nodes are `hushmic_*`), with the
/// outputs they play into. Pure.
pub fn parse_own_players(dump: &str) -> Vec<OutStream> {
    streams_out(dump, |n| n.starts_with("hushmic-"))
}

fn streams_out(dump: &str, pick: impl Fn(&str) -> bool) -> Vec<OutStream> {
    let Ok(serde_json::Value::Array(v)) = serde_json::from_str(dump) else {
        return Vec::new();
    };
    let props_of = |o: &serde_json::Value| o.get("info")?.get("props").cloned();
    let mut sinks = std::collections::HashMap::new();
    for o in &v {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(props) = props_of(o) else { continue };
        if props.get("media.class").and_then(|c| c.as_str()) != Some("Audio/Sink") {
            continue;
        }
        if let (Some(id), Some(name)) = (
            o.get("id").and_then(|i| i.as_u64()),
            props.get("node.name").and_then(|n| n.as_str()),
        ) {
            sinks.insert(id, name.to_string());
        }
    }
    let mut links: Vec<(u64, u64)> = Vec::new();
    for o in &v {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Link") {
            continue;
        }
        let info = o.get("info");
        if let (Some(from), Some(to)) = (
            info.and_then(|i| i.get("output-node-id"))
                .and_then(|n| n.as_u64()),
            info.and_then(|i| i.get("input-node-id"))
                .and_then(|n| n.as_u64()),
        ) {
            links.push((from, to));
        }
    }
    v.iter()
        .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Node"))
        .filter_map(|o| {
            let props = o.get("info")?.get("props")?;
            if props.get("media.class")?.as_str()? != "Stream/Output/Audio" {
                return None;
            }
            if !pick(
                props
                    .get("node.name")
                    .and_then(|n| n.as_str())
                    .unwrap_or(""),
            ) {
                return None;
            }
            let serial = props.get("object.serial")?;
            let serial = serial.as_u64().or_else(|| serial.as_str()?.parse().ok())?;
            let id = o.get("id")?.as_u64()?;
            let mut to: Vec<String> = Vec::new();
            for (from, into) in &links {
                if *from == id {
                    if let Some(name) = sinks.get(into) {
                        if !to.contains(name) {
                            to.push(name.clone());
                        }
                    }
                }
            }
            let own_target = ["target.object", "node.target"]
                .iter()
                .filter_map(|k| match props.get(*k)? {
                    serde_json::Value::String(s) => Some(s.trim().to_string()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .find(|t| !t.is_empty() && t != "-1");
            Some(OutStream {
                id: u32::try_from(id).ok()?,
                serial,
                to,
                own_target,
            })
        })
        .collect()
}

/// [`Graph::sinks`] alone (no microphone known beyond what the dump shows).
pub fn parse_sinks(dump: &str) -> Vec<Source> {
    sink_kinds(dump, None).sinks
}

/// The outputs of a dump, sorted and sorted out.
#[derive(Debug, Default)]
pub struct SinkKinds {
    pub sinks: Vec<Source>,
    pub virtual_sinks: Vec<String>,
    pub feeders: Vec<String>,
}

/// Every `Audio/Sink` node except HushMic Speakers and monitors, with its
/// description (else its nick, else its name), hardware first; which are
/// virtual; and which feed a source HushMic captures: one linked into a
/// HushMic node, the default source, or `mic`. A sink feeds it when it is
/// the other half of that source's loopback (the same `node.link-group`,
/// made by the same client) or is itself linked into a HushMic node (its
/// monitor recorded). Pure.
pub fn sink_kinds(dump: &str, mic: Option<&str>) -> SinkKinds {
    let Ok(serde_json::Value::Array(v)) = serde_json::from_str(dump) else {
        return SinkKinds::default();
    };
    let text = |p: &serde_json::Value, k: &str| -> Option<String> {
        p.get(k).and_then(|x| match x {
            serde_json::Value::String(s) => Some(s.trim().to_string()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    };
    let flag = |p: &serde_json::Value, k: &str| {
        p.get(k)
            .is_some_and(|x| x.as_bool().unwrap_or_else(|| x.as_str() == Some("true")))
    };
    struct N {
        id: u64,
        class: String,
        name: String,
        props: serde_json::Value,
    }
    let nodes: Vec<N> = v
        .iter()
        .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Node"))
        .filter_map(|o| {
            let props = o.get("info")?.get("props")?.clone();
            Some(N {
                id: o.get("id")?.as_u64()?,
                class: text(&props, "media.class").unwrap_or_default(),
                name: text(&props, "node.name").unwrap_or_default(),
                props,
            })
        })
        .collect();
    let name_of = |id: u64| nodes.iter().find(|n| n.id == id).map(|n| n.name.as_str());
    // what is linked into one of HushMic's nodes (its microphone's capture)
    let mut into_hushmic: Vec<u64> = Vec::new();
    for o in &v {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Link") {
            continue;
        }
        let info = o.get("info");
        let from = info
            .and_then(|i| i.get("output-node-id"))
            .and_then(|n| n.as_u64());
        let to = info
            .and_then(|i| i.get("input-node-id"))
            .and_then(|n| n.as_u64());
        if let (Some(from), Some(to)) = (from, to) {
            if name_of(to).is_some_and(|n| n.starts_with("hushmic")) {
                into_hushmic.push(from);
            }
        }
    }
    let default_sources = parse_default_source(dump);
    // A loopback's two halves share a link group and the process that
    // made them (PipeWire 0.3.48 gives every pw-loopback process the same
    // group name, so the group alone would join unrelated loopbacks).
    let half_of = |n: &N| -> Option<(String, Option<String>)> {
        Some((
            text(&n.props, "node.link-group")?,
            text(&n.props, "client.id"),
        ))
    };
    let captured_groups: Vec<(String, Option<String>)> = nodes
        .iter()
        .filter(|n| n.class.starts_with("Audio/Source") && !n.name.starts_with("hushmic"))
        .filter(|n| {
            into_hushmic.contains(&n.id)
                || default_sources.contains(&n.name)
                || mic == Some(n.name.as_str())
        })
        .filter_map(half_of)
        .collect();
    let same_loopback = |a: &(String, Option<String>), b: &(String, Option<String>)| {
        a.0 == b.0 && (a.1.is_none() || b.1.is_none() || a.1 == b.1)
    };
    let mut out = SinkKinds::default();
    let mut ranked: Vec<(u8, Source)> = Vec::new();
    for n in &nodes {
        if n.class != "Audio/Sink" {
            continue;
        }
        if n.name.is_empty() || n.name == SINK || n.name.ends_with(".monitor") {
            continue;
        }
        let group = text(&n.props, "node.link-group");
        let half = half_of(n);
        if into_hushmic.contains(&n.id)
            || half
                .as_ref()
                .is_some_and(|h| captured_groups.iter().any(|c| same_loopback(h, c)))
        {
            out.feeders.push(n.name.clone());
            continue;
        }
        let virtual_ = flag(&n.props, "node.virtual")
            || group.is_some()
            || text(&n.props, "factory.name").as_deref() == Some("support.null-audio-sink");
        let device = text(&n.props, "device.api").is_some()
            || text(&n.props, "device.id").is_some()
            || ["alsa_", "bluez_"].iter().any(|p| n.name.starts_with(p));
        let rank = if virtual_ {
            out.virtual_sinks.push(n.name.clone());
            2
        } else if device {
            0
        } else {
            1
        };
        let description = text(&n.props, "node.description")
            .or_else(|| text(&n.props, "node.nick"))
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| n.name.clone());
        ranked.push((
            rank,
            Source {
                name: n.name.clone(),
                description,
            },
        ));
    }
    ranked.sort_by_key(|(r, _)| *r);
    out.sinks = ranked.into_iter().map(|(_, s)| s).collect();
    out
}

/// The default input keys (effective, configured) of a dump's `default`
/// metadata, those present. Pure.
fn parse_default_source(dump: &str) -> Vec<String> {
    let Ok(serde_json::Value::Array(v)) = serde_json::from_str(dump) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for o in &v {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Metadata") {
            continue;
        }
        if o.get("props")
            .and_then(|p| p.get("metadata.name"))
            .and_then(|n| n.as_str())
            != Some("default")
        {
            continue;
        }
        for e in o
            .get("metadata")
            .and_then(|m| m.as_array())
            .into_iter()
            .flatten()
        {
            let key = e.get("key").and_then(|k| k.as_str());
            if !matches!(
                key,
                Some("default.audio.source") | Some("default.configured.audio.source")
            ) {
                continue;
            }
            if let Some(name) = e
                .get("value")
                .and_then(|val| val.get("name"))
                .and_then(|n| n.as_str())
            {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// The default output keys of a `pw-dump` snapshot's `default` metadata:
/// (effective `default.audio.sink`, configured
/// `default.configured.audio.sink`). Pure.
pub fn parse_default_sink(dump: &str) -> (Option<String>, Option<String>) {
    let Ok(serde_json::Value::Array(v)) = serde_json::from_str(dump) else {
        return (None, None);
    };
    let mut effective = None;
    let mut configured = None;
    for o in &v {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Metadata") {
            continue;
        }
        if o.get("props")
            .and_then(|p| p.get("metadata.name"))
            .and_then(|n| n.as_str())
            != Some("default")
        {
            continue;
        }
        let Some(entries) = o.get("metadata").and_then(|m| m.as_array()) else {
            continue;
        };
        for e in entries {
            let name = e
                .get("value")
                .and_then(|v| v.get("name"))
                .and_then(|n| n.as_str())
                .filter(|n| !n.is_empty())
                .map(str::to_string);
            match e.get("key").and_then(|k| k.as_str()) {
                Some("default.audio.sink") if effective.is_none() => effective = name,
                Some("default.configured.audio.sink") if configured.is_none() => configured = name,
                _ => {}
            }
        }
    }
    (effective, configured)
}

/// Of `targets` (as [`parse_targets`] gives them), the ones that name the
/// node `id` (serial `serial`) by number: see [`Graph::targets_naming`].
/// Pure.
pub fn targets_naming(
    targets: &[(u32, String, String)],
    id: u32,
    serial: Option<u64>,
    keep_pairs: bool,
) -> Vec<(u32, String, String)> {
    let (id_s, serial_s) = (id.to_string(), serial.map(|s| s.to_string()));
    let paired = |s: u32| {
        serial_s.as_deref().is_some_and(|serial| {
            targets
                .iter()
                .any(|(t, k, v)| *t == s && k == "target.object" && v == serial)
        })
    };
    // the streams whose target.node is the sink's id; their target.object
    // only as its companion (the sink's serial, as a mixer writes both)
    let streams: Vec<u32> = targets
        .iter()
        .filter(|(_, k, v)| k == "target.node" && *v == id_s)
        .map(|t| t.0)
        .filter(|s| !(keep_pairs && paired(*s)))
        .collect();
    targets
        .iter()
        .filter(|(s, k, v)| {
            streams.contains(s)
                && (k == "target.node"
                    || (k == "target.object" && Some(v.as_str()) == serial_s.as_deref()))
        })
        .cloned()
        .collect()
}

/// The `target.object` / `target.node` entries of a `pw-metadata -n
/// default` listing (`update: id:110 key:'target.node' value:'95' ...`),
/// as [`parse_targets`] gives them. Pure.
pub fn parse_metadata_targets(listing: &str) -> Vec<(u32, String, String)> {
    let mut out = Vec::new();
    for line in listing.lines() {
        let quoted = |name: &str| {
            let rest = &line[line.find(&format!("{name}:'"))? + name.len() + 2..];
            Some(rest[..rest.find('\'')?].to_string())
        };
        let Some(subject) = line
            .split_whitespace()
            .find_map(|w| w.strip_prefix("id:"))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let (Some(key), Some(value)) = (quoted("key"), quoted("value")) else {
            continue;
        };
        if key == "target.object" || key == "target.node" {
            out.push((subject, key, value));
        }
    }
    out
}

/// The `target.object` / `target.node` entries of the `default` metadata:
/// (subject, key, value as text). Pure.
pub fn parse_targets(dump: &str) -> Vec<(u32, String, String)> {
    let Ok(serde_json::Value::Array(v)) = serde_json::from_str(dump) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for o in &v {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Metadata")
            || o.get("props")
                .and_then(|p| p.get("metadata.name"))
                .and_then(|n| n.as_str())
                != Some("default")
        {
            continue;
        }
        for e in o
            .get("metadata")
            .and_then(|m| m.as_array())
            .into_iter()
            .flatten()
        {
            let (Some(subject), Some(key)) = (
                e.get("subject").and_then(|s| s.as_u64()),
                e.get("key").and_then(|k| k.as_str()),
            ) else {
                continue;
            };
            if key != "target.object" && key != "target.node" {
                continue;
            }
            let value = match e.get("value") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Number(n)) => n.to_string(),
                _ => continue,
            };
            out.push((subject as u32, key.to_string(), value));
        }
    }
    out
}

/// The node names `node_name` has links INTO, deduplicated. Pure.
pub fn parse_consumers(dump: &str, node_name: &str) -> Vec<String> {
    let Ok(serde_json::Value::Array(arr)) = serde_json::from_str(dump) else {
        return Vec::new();
    };
    let mut names = std::collections::HashMap::new();
    for o in &arr {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let (Some(id), Some(name)) = (
            o.get("id").and_then(|i| i.as_u64()),
            o.get("info")
                .and_then(|i| i.get("props"))
                .and_then(|p| p.get("node.name"))
                .and_then(|n| n.as_str()),
        ) else {
            continue;
        };
        names.insert(id, name.to_string());
    }
    let mut out: Vec<String> = Vec::new();
    for o in &arr {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Link") {
            continue;
        }
        let info = o.get("info");
        let (Some(from), Some(to)) = (
            info.and_then(|i| i.get("output-node-id"))
                .and_then(|n| n.as_u64()),
            info.and_then(|i| i.get("input-node-id"))
                .and_then(|n| n.as_u64()),
        ) else {
            continue;
        };
        if names.get(&from).map(String::as_str) != Some(node_name) {
            continue;
        }
        if let Some(name) = names.get(&to) {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
    }
    out
}

/// `node.force-quantum` on `node_name` (Some(0): unpinned; None: no such
/// node). Pure.
fn node_quantum(dump: &str, node_name: &str) -> Option<u32> {
    let Ok(serde_json::Value::Array(arr)) = serde_json::from_str(dump) else {
        return None;
    };
    let props = arr
        .iter()
        .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Node"))
        .filter_map(|o| o.get("info")?.get("props"))
        .find(|p| p.get("node.name").and_then(|n| n.as_str()) == Some(node_name))?;
    Some(match props.get("node.force-quantum") {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0) as u32,
        Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    })
}

/// Where HushMic's own players (Mic check's replay, the Files player) must
/// play: None = the system default, as any player; a real output while
/// the system default is HushMic Speakers, so what they play is never
/// cleaned a second time (that would break both comparisons). It is the
/// output the speakers chain plays on, else the first real one. Pure.
pub fn own_player_target(g: &Graph) -> Option<String> {
    if !g.default_is_ours() {
        return None;
    }
    g.playing_to
        .iter()
        .find(|d| g.has_sink(d))
        .cloned()
        .or_else(|| g.sinks.first().map(|s| s.name.clone()))
}

/// The output a HushMic player starts on: [`own_player_target`] while the
/// default is HushMic Speakers; while HushMic Speakers runs otherwise, the
/// real default itself, named, so a target the session manager saved for
/// the player's media.role (pw-cat's Music, the same as most apps') never
/// sends it elsewhere, into HushMic Speakers least of all (WirePlumber
/// restores a saved target only for a stream that names none). Without
/// HushMic Speakers: None, the default as any player. Pure.
pub fn own_player_pick(g: &Graph) -> Option<String> {
    if g.default_is_ours() {
        return own_player_target(g);
    }
    if !g.sink_up {
        return None;
    }
    g.real_default().map(str::to_string)
}

/// [`own_player_pick`] read now: one `pw-dump`. None (play to the
/// default) whenever PipeWire does not answer in time.
pub fn own_player_target_now() -> Option<String> {
    const BUDGET: Duration = Duration::from_millis(1500);
    let dump = pipewire::pw_dump_within(BUDGET)?;
    let name = own_player_pick(&Graph::read(&dump))?;
    // pw-cat before 0.3.64 takes only a node id
    if pipewire::supports_target_object() {
        Some(name)
    } else {
        pipewire::parse_node_id(&dump, &name)
            .map(|id| id.to_string())
            .or(Some(name))
    }
}
