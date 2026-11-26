use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub name: String,
    pub description: String,
}

/// Parse `pw-dump` JSON into the audio capture sources PipeWire exposes
/// (`media.class == "Audio/Source"` nodes).
///
/// hushmic is PipeWire-native: source enumeration and the watchdog liveness
/// check use `pw-dump` (and defaults use `pw-metadata`) — both ship in the
/// `pipewire`/`pipewire-bin` package set the project already depends on. They do
/// NOT use PulseAudio's `pactl`, which lives in the separate `pulseaudio-utils`
/// package that is absent on minimal installs and the Ubuntu live image; relying
/// on it would make `hushmic_source_present()` silently return false there, and
/// the watchdog would re-instantiate a perfectly healthy node forever.
///
/// Returns EVERY Audio/Source node (including monitors and our own
/// `hushmic_source`); callers filter as needed. Pure function — no I/O.
pub fn parse_pwdump_nodes(stdout: &str) -> Vec<Source> {
    let v: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for o in arr {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(props) = o.get("info").and_then(|i| i.get("props")) else {
            continue;
        };
        if props.get("media.class").and_then(|c| c.as_str()) != Some("Audio/Source") {
            continue;
        }
        let name = match props.get("node.name").and_then(|n| n.as_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        // Friendly label for the tray: node.description, else node.nick, else the
        // node name (mirrors the human-readable name pactl's Description gave).
        let description = props
            .get("node.description")
            .and_then(|d| d.as_str())
            .or_else(|| props.get("node.nick").and_then(|d| d.as_str()))
            .map(|s| s.to_string())
            .unwrap_or_else(|| name.clone());
        out.push(Source { name, description });
    }
    out
}

/// The `node.force-quantum` value on the RUNNING chain's output node
/// (issue #10; the source host's own pin is constant, so the chain's is
/// the one that says which conf runs). `None` = no chain output in the
/// dump (chain down) or unparseable dump; `Some(0)` = an unpinned chain. Returned as a value (not a bool) so the doctor can judge
/// it against the declared PINNED_QUANTUM — a stale pin from a different
/// hushmic version means the declared latency is wrong until a restart.
/// Pure function — no I/O.
pub fn chain_pins_quantum(stdout: &str) -> Option<u32> {
    let v: serde_json::Value = serde_json::from_str(stdout).ok()?;
    for o in v.as_array()? {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(props) = o.get("info").and_then(|i| i.get("props")) else {
            continue;
        };
        if props.get("node.name").and_then(|n| n.as_str()) != Some(CHAIN_OUTPUT) {
            continue;
        }
        // SPA props round-trip through pw-dump as numbers or strings
        // depending on the host version; accept both. 0 means unforced.
        let quantum = match props.get("node.force-quantum") {
            Some(x) => x
                .as_u64()
                .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
                .unwrap_or(0),
            None => 0,
        };
        return Some(quantum.min(u32::MAX as u64) as u32);
    }
    None
}

/// The DSP chain's capture node: its scheduling state is the chain's.
pub const CHAIN_INPUT: &str = "hushmic_input";
/// The DSP chain's output, linked into the source host (not a device).
pub const CHAIN_OUTPUT: &str = "hushmic_dsp_out";
/// The source host's capture, fed only by [`CHAIN_OUTPUT`].
pub const HOST_FEED: &str = "hushmic_feed";
/// The one link the daemon makes: the chain's output into the host.
pub const CHAIN_OUT_PORT: &str = "hushmic_dsp_out:output_MONO";
pub const HOST_IN_PORT: &str = "hushmic_feed:input_MONO";

/// What the guard of HushMic's streams does about one stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuardStep {
    /// Move it to the default output (or input): its metadata target
    /// becomes "follow the default" ([`move_to_default`]).
    Move { id: u32, name: String },
    /// It is linked there again after a move, or stays: its own
    /// properties name the node by number. Its link goes at every look.
    KeptOff { name: String },
}

/// The streams found linked where no stream belongs ([`ChainGraph::intruders`]),
/// across looks: each one's link goes at every look (it is in `foreign`)
/// and it is moved to the default. One found there again after a look
/// without it (its own settings link it back), or still there three looks
/// on, is moved once more and noted as kept off.
#[derive(Debug, Default)]
pub struct IntruderGuard {
    seen: Vec<Seen>,
    /// WirePlumber's links left to its own move at the last look.
    spared: Vec<u32>,
}

#[derive(Debug)]
struct Seen {
    id: u32,
    serial: Option<u64>,
    name: String,
    /// Looks in a row that found it there.
    streak: u32,
    /// It was gone from there at a look since it was moved.
    away: bool,
    kept_off: bool,
}

impl IntruderGuard {
    /// One look's intruders and the streams there (id, serial): what to do.
    pub fn observe(
        &mut self,
        intruders: &[Intruder],
        streams: &[(u32, Option<u64>)],
    ) -> Vec<GuardStep> {
        self.seen.retain(|e| streams.contains(&(e.id, e.serial)));
        for e in self.seen.iter_mut() {
            if !intruders
                .iter()
                .any(|i| i.id == e.id && i.serial == e.serial)
            {
                e.away = true;
                e.streak = 0;
            }
        }
        let mut steps = Vec::new();
        for i in intruders {
            let mv = GuardStep::Move {
                id: i.id,
                name: i.name.clone(),
            };
            match self
                .seen
                .iter_mut()
                .find(|e| e.id == i.id && e.serial == i.serial)
            {
                None => {
                    self.seen.push(Seen {
                        id: i.id,
                        serial: i.serial,
                        name: i.name.clone(),
                        streak: 1,
                        away: false,
                        kept_off: false,
                    });
                    steps.push(mv);
                }
                Some(e) => {
                    e.streak += 1;
                    if !e.kept_off && (e.away || e.streak >= 3) {
                        e.kept_off = true;
                        steps.push(mv);
                        steps.push(GuardStep::KeptOff {
                            name: i.name.clone(),
                        });
                    }
                    e.away = false;
                }
            }
        }
        steps
    }

    /// Of a look's `cuts`, the links to cut now (link id, from node, into
    /// node). One WirePlumber made for a stream moved at this look
    /// (`moving`) is left to that move once: the move takes it away, while
    /// on WirePlumber 0.4 a stream whose link was cut under it is linked
    /// nowhere after. Still there at a later look, it is cut, and so is
    /// every other one (made by hand, or kept linking back).
    pub fn to_cut(&mut self, cuts: &[Cut], moving: &[u32]) -> Vec<(u32, String, String)> {
        let spare =
            |c: &Cut| c.by_wp && moving.contains(&c.stream) && !self.spared.contains(&c.link);
        let spared: Vec<u32> = cuts.iter().filter(|c| spare(c)).map(|c| c.link).collect();
        let out = cuts
            .iter()
            .filter(|c| !spare(c))
            .map(|c| (c.link, c.from.clone(), c.into.clone()))
            .collect();
        self.spared = spared;
        out
    }

    /// The streams HushMic keeps off its own, by name, while they last.
    pub fn kept_off(&self) -> Vec<String> {
        self.seen
            .iter()
            .filter(|e| e.kept_off)
            .map(|e| e.name.clone())
            .collect()
    }
}

/// Point node `id`'s routing metadata at "follow the default": WirePlumber
/// 0.4 reads `target.node`, newer ones `target.object`; -1 in both moves a
/// stream to the default output or input on 0.4.8 to 0.5 (checked), even
/// one whose own properties name a node by number, and saves nothing.
pub fn move_to_default(id: u32) -> bool {
    ["target.node", "target.object"].iter().all(|k| {
        pw_metadata_within(
            &["-n", "default", "--", &id.to_string(), k, "-1", "Spa:Id"],
            CALL_BUDGET,
        )
        .is_some()
    })
}

/// The chain's and the source host's nodes and the link between them in
/// a pw-dump snapshot. `hushmic_source` lives in a host that outlives the
/// chain (apps stay on it through every chain restart); the chain's output
/// reaches it through one passive link the daemon makes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainGraph {
    pub input: bool,
    pub output: bool,
    pub feed: bool,
    pub source: bool,
    /// A passive link from the chain's output into the host's feed.
    pub linked: bool,
    /// Links that must go: any other link on the chain's output or the
    /// host's feed, a second link between them, or one that is not
    /// passive (it would keep the mic open with nobody listening).
    pub foreign: Vec<u32>,
    /// App playbacks linked into the chain's capture (`hushmic_input`) or
    /// the source host's feed (`hushmic_feed`): their sound would go into
    /// the call. WirePlumber 0.4 follows a stream's target by node id, and
    /// one left on an id of HushMic's that a restart handed to one of
    /// these nodes is linked there; anyone can link one by hand. They are
    /// moved off, and their links cut (`cuts`).
    pub intruders: Vec<Intruder>,
    /// The intruders' links (see [`IntruderGuard::to_cut`]).
    pub cuts: Vec<Cut>,
    /// Every stream node: its id and serial (what tells a stream still
    /// there from another one given its id).
    pub streams: Vec<(u32, Option<u64>)>,
}

/// A stream linked into or out of one of HushMic's streams (see
/// [`ChainGraph::intruders`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intruder {
    pub id: u32,
    pub serial: Option<u64>,
    /// The app's name, else the node's.
    pub name: String,
}

/// An intruder's link into HushMic's microphone input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cut {
    pub link: u32,
    /// The node names it joins: it is cut only while its id still does.
    pub from: String,
    pub into: String,
    /// The intruder's node id.
    pub stream: u32,
    /// WirePlumber made it (not a patchbay or `pw-link`).
    pub by_wp: bool,
}

impl ChainGraph {
    /// Both of the chain's nodes are there.
    pub fn chain(&self) -> bool {
        self.input && self.output
    }

    /// Both of the source host's nodes are there.
    pub fn host(&self) -> bool {
        self.source && self.feed
    }

    /// Apps recording HushMic get the chain's output.
    pub fn up(&self) -> bool {
        self.chain() && self.host() && self.linked
    }
}

/// [`ChainGraph`] from a pw-dump snapshot (an unparseable one reads as
/// nothing there). Pure function — no I/O.
pub fn chain_graph(stdout: &str) -> ChainGraph {
    let mut g = ChainGraph::default();
    let Ok(serde_json::Value::Array(v)) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return g;
    };
    let mut ids: HashMap<&str, Vec<u64>> = HashMap::new();
    // every playback stream node: (serial, app or node name)
    let mut streams: HashMap<u64, (Option<u64>, String)> = HashMap::new();
    let mut names: HashMap<u64, String> = HashMap::new();
    // the session manager's clients: the links it made
    let mut wp_clients: Vec<u64> = Vec::new();
    for o in &v {
        if o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Client") {
            let app = o
                .get("info")
                .and_then(|i| i.get("props"))
                .and_then(|p| p.get("application.name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            if let (true, Some(id)) = (
                app.to_ascii_lowercase().contains("wireplumber"),
                o.get("id").and_then(|i| i.as_u64()),
            ) {
                wp_clients.push(id);
            }
            continue;
        }
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let props = o.get("info").and_then(|i| i.get("props"));
        let prop = |k: &str| props.and_then(|p| p.get(k)).and_then(|n| n.as_str());
        let name = prop("node.name");
        let Some(id) = o.get("id").and_then(|i| i.as_u64()) else {
            continue;
        };
        if let Some(n) = name {
            ids.entry(n).or_default().push(id);
            names.insert(id, n.to_string());
        }
        if prop("media.class").is_some_and(|c| c.starts_with("Stream/")) {
            let serial = props
                .and_then(|p| p.get("object.serial"))
                .and_then(|s| s.as_u64().or_else(|| s.as_str()?.parse().ok()));
            if let Ok(i) = u32::try_from(id) {
                g.streams.push((i, serial));
            }
            if prop("media.class").is_some_and(|c| c.starts_with("Stream/Output")) {
                let shown = prop("application.name").or(name).unwrap_or("").to_string();
                streams.insert(id, (serial, shown));
            }
        }
    }
    let has = |n: &str| ids.contains_key(n);
    g.input = has(CHAIN_INPUT);
    g.output = has(CHAIN_OUTPUT);
    g.feed = has(HOST_FEED);
    g.source = has("hushmic_source");
    let of = |n: &str| ids.get(n).cloned().unwrap_or_default();
    let (outs, feeds) = (of(CHAIN_OUTPUT), of(HOST_FEED));
    let inputs = of(CHAIN_INPUT);
    let ours: Vec<u64> = ids
        .iter()
        .filter(|(n, _)| n.starts_with("hushmic"))
        .flat_map(|(_, i)| i.iter().copied())
        .collect();
    for o in &v {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Link") {
            continue;
        }
        let (Some(id), Some(info)) = (o.get("id").and_then(|i| i.as_u64()), o.get("info")) else {
            continue;
        };
        let node = |k: &str| info.get(k).and_then(|x| x.as_u64());
        let (from, to) = (node("output-node-id"), node("input-node-id"));
        // an app's playback (not one of HushMic's) into the capture or
        // the feed
        let intruder = match (from, to) {
            (Some(f), Some(t))
                if (inputs.contains(&t) || feeds.contains(&t))
                    && streams.contains_key(&f)
                    && !ours.contains(&f) =>
            {
                Some((f, t))
            }
            _ => None,
        };
        if let Some((n, t)) = intruder {
            if let (Ok(i), Some(a), Some(b)) = (u32::try_from(id), names.get(&n), names.get(&t)) {
                let by_wp = info
                    .get("props")
                    .and_then(|p| p.get("client.id"))
                    .and_then(|c| c.as_u64().or_else(|| c.as_str()?.parse().ok()))
                    .is_some_and(|c| wp_clients.contains(&c));
                if let Ok(stream) = u32::try_from(n) {
                    g.cuts.push(Cut {
                        link: i,
                        from: a.clone(),
                        into: b.clone(),
                        stream,
                        by_wp,
                    });
                }
            }
            if let (Ok(nid), Some((serial, name))) = (u32::try_from(n), streams.get(&n)) {
                if !g.intruders.iter().any(|x| x.id == nid) {
                    g.intruders.push(Intruder {
                        id: nid,
                        serial: *serial,
                        name: name.clone(),
                    });
                }
            }
            continue;
        }
        let from_out = from.is_some_and(|f| outs.contains(&f));
        let to_feed = to.is_some_and(|t| feeds.contains(&t));
        if !from_out && !to_feed {
            continue;
        }
        let passive = info
            .get("props")
            .and_then(|p| p.get("link.passive"))
            .is_some_and(|x| x.as_bool() == Some(true) || x.as_str() == Some("true"));
        if from_out && to_feed && passive && !g.linked {
            g.linked = true;
        } else if let Ok(id) = u32::try_from(id) {
            g.foreign.push(id);
        }
    }
    g
}

/// A stream's mixer controls as the graph shows them.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamControls {
    pub name: String,
    pub muted: bool,
    /// The loudest channel, as a mixer shows it (cubic: 1.0 = 100 %).
    pub volume: f32,
}

/// The mixer controls (`Props` mute, volume, channelVolumes) of the
/// chain's capture, its output and the host's feed: a mixer can mute or
/// turn them down, and WirePlumber restores that. Pure function — no I/O.
pub fn stream_controls(stdout: &str) -> Vec<StreamControls> {
    stream_controls_of(stdout, &[CHAIN_INPUT, CHAIN_OUTPUT, HOST_FEED])
}

/// [`stream_controls`] of the nodes `names`, in that order. Pure.
pub fn stream_controls_of(stdout: &str, names: &[&str]) -> Vec<StreamControls> {
    let Ok(serde_json::Value::Array(v)) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for &name in names {
        let props = v.iter().find_map(|o| {
            let info = o.get("info")?;
            (info.get("props")?.get("node.name")?.as_str()? == name).then_some(())?;
            info.get("params")?
                .get("Props")?
                .as_array()?
                .iter()
                .find(|p| p.get("mute").is_some())
        });
        let Some(p) = props else { continue };
        let master = p.get("volume").and_then(|x| x.as_f64()).unwrap_or(1.0);
        let channel = p
            .get("channelVolumes")
            .and_then(|x| x.as_array())
            .and_then(|a| a.iter().filter_map(|x| x.as_f64()).reduce(f64::max))
            .unwrap_or(1.0);
        out.push(StreamControls {
            name: name.to_string(),
            muted: p.get("mute").and_then(|x| x.as_bool()).unwrap_or(false),
            volume: ((master * channel).max(0.0).cbrt()) as f32,
        });
    }
    out
}

/// The scheduling state PipeWire reports for the node `name` (`info.state`:
/// `running`, `idle`, `suspended`, `creating`, `error`). `None` = no such
/// node, no state, or an unparseable dump. Pure function — no I/O.
pub fn node_state(stdout: &str, name: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stdout).ok()?;
    v.as_array()?
        .iter()
        .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Node"))
        .find(|o| {
            o.get("info")
                .and_then(|i| i.get("props"))
                .and_then(|p| p.get("node.name"))
                .and_then(|n| n.as_str())
                == Some(name)
        })
        .and_then(|o| o.get("info")?.get("state")?.as_str())
        .map(str::to_string)
}

/// Whether a `hushmic_source` [`node_state`] means PipeWire is not running
/// the chain (no CPU): `idle`, then `suspended` after the session
/// manager's suspend timeout. The capture stream is `node.passive`, so this
/// is the normal state while no active stream reads from HushMic (a corked
/// stream does not count; a muted-but-open one does). It is a scheduling
/// fact, not a list of apps: on PipeWire < 0.3.68 another client of the
/// mic keeps the chain `running` with nobody reading from it.
pub fn state_is_idle(state: &str) -> bool {
    matches!(state, "idle" | "suspended")
}

/// Resolve a node NAME to its numeric PipeWire global id from a `pw-dump`
/// snapshot. Pure — no I/O. `None` if the JSON is unparseable or no
/// `Audio` node carries that `node.name`.
///
/// Older pw-cat (< 0.3.64, e.g. Ubuntu 22.04's 0.3.48) rejects a node NAME as
/// `--target` — it prints `bad target option "<name>"` and exits before
/// emitting a byte, which leaves a recorder with no audio and a "failed to fill
/// whole buffer" parse error. The numeric id is pw-cat's original,
/// always-accepted target form (what `--list-targets` prints), so resolving
/// name→id makes the recorder target the chosen node on every PipeWire version.
pub fn parse_node_id(stdout: &str, name: &str) -> Option<u32> {
    let v: serde_json::Value = serde_json::from_str(stdout).ok()?;
    for o in v.as_array()? {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let node_name = o
            .get("info")
            .and_then(|i| i.get("props"))
            .and_then(|p| p.get("node.name"))
            .and_then(|n| n.as_str());
        if node_name == Some(name) {
            return o.get("id").and_then(|i| i.as_u64()).map(|n| n as u32);
        }
    }
    None
}

/// Resolve a node NAME to its `object.serial` from a `pw-dump` snapshot.
/// Pure — no I/O. The serial is what routing metadata (`target.object` as
/// `Spa:Id`) identifies nodes by; unlike the global id it is never reused.
pub fn parse_node_serial(stdout: &str, name: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(stdout).ok()?;
    for o in v.as_array()? {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let props = o.get("info").and_then(|i| i.get("props"))?;
        if props.get("node.name").and_then(|n| n.as_str()) == Some(name) {
            return props.get("object.serial").and_then(|s| s.as_u64());
        }
    }
    None
}

/// The node names with LINKS into `node_name`, deduplicated, from a
/// `pw-dump` snapshot. Pure — no I/O. The feeder map covers EVERY node
/// class (a stolen capture stream is typically fed by a virtual source or
/// a sink monitor, never an `Audio/Source`). Unparseable JSON is an empty
/// list, never a panic.
pub fn parse_feeders(stdout: &str, node_name: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut names = std::collections::HashMap::new();
    for o in arr {
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
    let target_ids: Vec<u64> = names
        .iter()
        .filter(|(_, n)| n.as_str() == node_name)
        .map(|(id, _)| *id)
        .collect();
    let mut out: Vec<String> = Vec::new();
    for o in arr {
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
        if !target_ids.contains(&to) {
            continue;
        }
        if let Some(name) = names.get(&from) {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
    }
    out
}

/// A capture stream reading from a node, as `pw-dump` describes it (see
/// [`parse_readers`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reader {
    /// The node's id (what routing metadata is keyed by).
    pub id: u32,
    pub node_name: String,
    /// `media.class` (an app's capture stream is `Stream/Input/Audio`).
    pub media_class: String,
    /// `info.state`: only a `running` stream moves audio (a corked one is
    /// `idle`).
    pub state: String,
    pub app_name: Option<String>,
    /// `application.id` (e.g. `org.kde.plasma-pa`).
    pub app_id: Option<String>,
    /// `application.process.binary`.
    pub binary: Option<String>,
    /// The Flatpak app id (`pipewire.access.portal.app_id`).
    pub flatpak_id: Option<String>,
    pub pid: Option<u32>,
    /// `object.serial`: never reused while PipeWire runs, unlike the id.
    pub serial: Option<u64>,
    /// A peak meter (pipewire-pulse `stream.monitor`): volume controls.
    pub monitor: bool,
    /// A processing stage rather than an app: `node.virtual`, or one half
    /// of a loopback / filter chain (`node.link-group`).
    pub stage: bool,
    /// The device the stream asked for (else it follows the default):
    /// `target.object` (or the older `node.target`) on the node, or a target
    /// remembered for it in the `default` metadata (a move in pavucontrol,
    /// restored by the session manager).
    pub target: Option<String>,
}

/// A capture stream as one graph read saw it: enough to know it again in
/// a later one (another stream may get a freed node id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamNode {
    pub id: u32,
    pub node_name: String,
    /// The app's process: the node's `application.process.id`, else its
    /// client's (a native PipeWire stream has it on the client only).
    pub pid: Option<u32>,
    /// `object.serial`, which PipeWire never gives to another object.
    pub serial: Option<u64>,
}

impl StreamNode {
    pub fn of(r: &Reader) -> StreamNode {
        StreamNode {
            id: r.id,
            node_name: r.node_name.clone(),
            pid: r.pid,
            serial: r.serial,
        }
    }

    /// The same stream in another read: the same node id and name, and
    /// the same process or the same serial (UX panel round 2, A107
    /// SF-A1: a freed id may go to another app's stream).
    pub fn same(&self, other: &StreamNode) -> bool {
        self.id == other.id
            && self.node_name == other.node_name
            && ((self.pid.is_some() && self.pid == other.pid)
                || (self.serial.is_some() && self.serial == other.serial))
    }
}

/// A number property as pw-dump writes it: a JSON number or a string.
fn prop_num(v: &serde_json::Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.trim().parse().ok())
}

/// Every capture stream (`Stream/Input/Audio`) in a `pw-dump` snapshot,
/// linked or not. Pure — no I/O; unparseable JSON is an empty list.
pub fn parse_capture_streams(stdout: &str) -> Vec<StreamNode> {
    let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(stdout)
    else {
        return Vec::new();
    };
    let is = |o: &serde_json::Value, t: &str| o.get("type").and_then(|x| x.as_str()) == Some(t);
    // the process as parse_readers finds it: on the node, else its client
    let clients: std::collections::HashMap<u64, &serde_json::Value> = arr
        .iter()
        .filter(|o| is(o, "PipeWire:Interface:Client"))
        .filter_map(|o| Some((o.get("id")?.as_u64()?, o.get("info")?.get("props")?)))
        .collect();
    arr.iter()
        .filter(|o| is(o, "PipeWire:Interface:Node"))
        .filter_map(|o| {
            let props = o.get("info")?.get("props")?;
            let text = |k: &str| props.get(k).and_then(|v| v.as_str());
            if text("media.class") != Some("Stream/Input/Audio") {
                return None;
            }
            let client = props
                .get("client.id")
                .and_then(prop_num)
                .and_then(|id| clients.get(&id).copied());
            Some(StreamNode {
                id: u32::try_from(o.get("id")?.as_u64()?).ok()?,
                node_name: text("node.name").unwrap_or_default().to_string(),
                pid: props
                    .get("application.process.id")
                    .or_else(|| client.and_then(|c| c.get("application.process.id")))
                    .and_then(prop_num)
                    .and_then(|n| u32::try_from(n).ok()),
                serial: props.get("object.serial").and_then(prop_num),
            })
        })
        .collect()
}

/// Every node with a LINK out of the node named `source`, with what it
/// says about itself. Pure — no I/O; unparseable JSON is an empty list.
pub fn parse_readers(stdout: &str, source: &str) -> Vec<Reader> {
    parse_linked(stdout, source, false)
}

/// Every node with a LINK into the node named `sink` (the streams playing
/// through it), as [`parse_readers`] describes them. Pure.
pub fn parse_writers(stdout: &str, sink: &str) -> Vec<Reader> {
    parse_linked(stdout, sink, true)
}

/// The nodes linked to `node`: out of it, or (`into`) into it.
fn parse_linked(stdout: &str, source: &str, into: bool) -> Vec<Reader> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let is = |o: &serde_json::Value, t: &str| o.get("type").and_then(|x| x.as_str()) == Some(t);
    let nodes: std::collections::HashMap<u64, &serde_json::Value> = arr
        .iter()
        .filter(|o| is(o, "PipeWire:Interface:Node"))
        .filter_map(|o| Some((o.get("id")?.as_u64()?, o)))
        .collect();
    let name_of = |o: &serde_json::Value| {
        o.get("info")?
            .get("props")?
            .get("node.name")?
            .as_str()
            .map(str::to_string)
    };
    let source_ids: Vec<u64> = nodes
        .iter()
        .filter(|(_, o)| name_of(o).as_deref() == Some(source))
        .map(|(id, _)| *id)
        .collect();
    // Clients: app properties a node may lack (a Flatpak's portal app id
    // sits on its client).
    let clients: std::collections::HashMap<u64, &serde_json::Value> = arr
        .iter()
        .filter(|o| is(o, "PipeWire:Interface:Client"))
        .filter_map(|o| Some((o.get("id")?.as_u64()?, o.get("info")?.get("props")?)))
        .collect();
    // Targets the session manager keeps for streams: `default` metadata,
    // subject = the stream's node id.
    let mut meta_targets: std::collections::HashMap<u64, String> = Default::default();
    for o in arr.iter().filter(|o| is(o, "PipeWire:Interface:Metadata")) {
        let named_default = o
            .get("props")
            .and_then(|p| p.get("metadata.name"))
            .and_then(|n| n.as_str())
            == Some("default");
        if !named_default {
            continue;
        }
        for m in o
            .get("metadata")
            .and_then(|m| m.as_array())
            .into_iter()
            .flatten()
        {
            let key = m.get("key").and_then(|k| k.as_str());
            if !matches!(key, Some("target.object" | "target.node")) {
                continue;
            }
            let (Some(subject), Some(value)) = (
                m.get("subject").and_then(|s| s.as_u64()),
                m.get("value").and_then(target_value),
            ) else {
                continue;
            };
            // target.object (a name) wins over target.node (an id).
            if key == Some("target.object") || !meta_targets.contains_key(&subject) {
                meta_targets.insert(subject, value);
            }
        }
    }
    let mut seen: Vec<u64> = Vec::new();
    let mut out = Vec::new();
    for o in arr.iter().filter(|o| is(o, "PipeWire:Interface:Link")) {
        let info = o.get("info");
        let (Some(from), Some(to)) = (
            info.and_then(|i| i.get("output-node-id"))
                .and_then(|n| n.as_u64()),
            info.and_then(|i| i.get("input-node-id"))
                .and_then(|n| n.as_u64()),
        ) else {
            continue;
        };
        let (at, to) = if into { (to, from) } else { (from, to) };
        if !source_ids.contains(&at) || seen.contains(&to) {
            continue;
        }
        let Some(node) = nodes.get(&to) else { continue };
        seen.push(to);
        let props = node.get("info").and_then(|i| i.get("props"));
        let client = props
            .and_then(|p| p.get("client.id"))
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
            .and_then(|id| clients.get(&id).copied());
        let node_text = |k: &str| {
            props
                .and_then(|p| p.get(k))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        // App properties: the node's, else its client's.
        let text = |k: &str| {
            node_text(k).or_else(|| {
                client
                    .and_then(|c| c.get(k))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
        };
        // Booleans arrive as JSON bools or as "true" strings, by module.
        let flag = |k: &str| {
            props
                .and_then(|p| p.get(k))
                .is_some_and(|v| v.as_bool().unwrap_or_else(|| v.as_str() == Some("true")))
        };
        let pid = props
            .and_then(|p| p.get("application.process.id"))
            .or_else(|| client.and_then(|c| c.get("application.process.id")))
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
            .and_then(|n| u32::try_from(n).ok());
        out.push(Reader {
            id: u32::try_from(to).unwrap_or(u32::MAX),
            node_name: text("node.name").unwrap_or_default(),
            media_class: text("media.class").unwrap_or_default(),
            state: node
                .get("info")
                .and_then(|i| i.get("state"))
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string(),
            app_name: text("application.name"),
            app_id: text("application.id"),
            binary: text("application.process.binary"),
            flatpak_id: text("pipewire.access.portal.app_id"),
            pid,
            serial: props
                .and_then(|p| p.get("object.serial"))
                .and_then(prop_num),
            monitor: flag("stream.monitor"),
            stage: flag("node.virtual")
                || props.is_some_and(|p| p.get("node.link-group").is_some()),
            target: node_text("target.object")
                .or_else(|| node_text("node.target"))
                .filter(|t| t != "-1")
                .or_else(|| meta_targets.get(&to).cloned()),
        });
    }
    out
}

/// A `default` metadata target: a node name or id (a string, a number, or
/// a JSON object with a `name`); `-1` and empty mean none.
fn target_value(v: &serde_json::Value) -> Option<String> {
    let s = match v {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Object(o) => o.get("name")?.as_str()?.trim().to_string(),
        _ => return None,
    };
    (!s.is_empty() && s != "-1").then_some(s)
}

/// The default audio source from a `pw-dump` snapshot's `default`
/// metadata object, snapshot-consistent with the dump's link graph (no
/// extra subprocess per tick, no dump-vs-now race). Prefers the EFFECTIVE
/// `default.audio.source` — what the session manager actually links
/// streams to — over the `configured` preference: live dumps can carry
/// either key alone. Pure — no I/O.
pub fn parse_default_source(stdout: &str) -> Option<String> {
    let (effective, configured) = parse_default_keys(stdout);
    effective.or(configured)
}

/// The EFFECTIVE default source of a `pw-dump` snapshot (the one
/// WirePlumber links streams to; it may differ from the configured one).
/// Pure.
pub fn parse_effective_default_source(stdout: &str) -> Option<String> {
    parse_default_keys(stdout).0
}

/// The CONFIGURED default source of a `pw-dump` snapshot (the user's
/// pick, as opposed to the effective one WirePlumber computed). Pure.
pub fn parse_configured_default_source(stdout: &str) -> Option<String> {
    parse_default_keys(stdout).1
}

/// Both default-source keys of a `pw-dump` snapshot: (effective,
/// configured). Pure.
fn parse_default_keys(stdout: &str) -> (Option<String>, Option<String>) {
    let Ok(serde_json::Value::Array(v)) = serde_json::from_str(stdout) else {
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
                .and_then(|n| n.as_str());
            match e.get("key").and_then(|k| k.as_str()) {
                Some("default.audio.source") if effective.is_none() => {
                    effective = name.map(str::to_string);
                }
                Some("default.configured.audio.source") => {
                    configured = name.map(str::to_string);
                }
                _ => {}
            }
        }
    }
    (effective, configured)
}

/// WirePlumber's saved default sources, newest first: its `default-nodes`
/// state file keeps the configured source and the ones configured before
/// it (`default.configured.audio.source`, then `.0`, `.1`, …; 0.4 puts the
/// current one at `.0` too, 0.5 starts `.0` at the one before). Repeats
/// are dropped. Pure.
pub fn parse_wp_source_history(text: &str) -> Vec<String> {
    parse_wp_history(text, "default.configured.audio.source")
}

/// [`parse_wp_source_history`] for any of WirePlumber's default keys
/// (`default.configured.audio.sink` for outputs). Pure.
pub fn parse_wp_history(text: &str, key: &str) -> Vec<String> {
    let mut keyed: Vec<(i64, String)> = Vec::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let rank = if k == key {
            -1
        } else if let Some(n) = k.strip_prefix(key).and_then(|r| r.strip_prefix('.')) {
            match n.parse::<i64>() {
                Ok(n) if n >= 0 => n,
                _ => continue,
            }
        } else {
            continue;
        };
        let v = keyfile_unescape(v.trim());
        if !v.is_empty() {
            keyed.push((rank, v));
        }
    }
    keyed.sort_by_key(|(r, _)| *r);
    let mut out: Vec<String> = Vec::new();
    for (_, v) in keyed {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

/// A GLib key file value's escapes (`\s`, `\n`, `\t`, `\r`, `\\`).
fn keyfile_unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    out
}

/// The mic to restore when the configured default is already ours at
/// start (UX panel A26: a session that ended with the default on HushMic,
/// before WirePlumber saved the restore): the newest entry of
/// WirePlumber's history that is a real mic, plugged in or not. Unplugged
/// at login, it is still the user's choice: restored, WirePlumber falls
/// back by itself and switches to it when it is plugged in, as it would
/// have without HushMic. A stand-in restored instead would stay the
/// default for good (A118 SF-T1). The chain and Live follow the mic that
/// feeds the chain meanwhile. Pure.
pub fn prior_from_history(history: &[String]) -> Option<String> {
    history
        .iter()
        .find(|n| crate::config::can_own_profile(n))
        .cloned()
}

/// WirePlumber's saved default sources ([`parse_wp_source_history`]),
/// from its state file: `$XDG_STATE_HOME/wireplumber/default-nodes`, else
/// the one under `~/.local/state` (a Flatpak's own state dir is not
/// WirePlumber's). Empty when there is none.
pub fn wp_source_history() -> Vec<String> {
    wp_history("default.configured.audio.source")
}

/// [`wp_source_history`] for any of WirePlumber's default keys.
pub fn wp_history(key: &str) -> Vec<String> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
        dirs.push(std::path::PathBuf::from(d));
    }
    if let Some(h) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        dirs.push(std::path::Path::new(&h).join(".local/state"));
    }
    dirs.iter()
        .filter_map(|d| std::fs::read_to_string(d.join("wireplumber/default-nodes")).ok())
        .map(|t| parse_wp_history(&t, key))
        .find(|h| !h.is_empty())
        .unwrap_or_default()
}

/// The mic the capture stream is EXPECTED to be fed by: the pinned mic,
/// else the default source — except when the default is our own output
/// ("Set as default microphone" on), where the chain follows the
/// pre-takeover default (the controller's restore target). `None` = no
/// safe expectation, so no re-pin: a healer must never point the chain at
/// its own output (that is issue #5's silent loop, self-inflicted).
pub fn repin_want(
    active_mic: Option<&str>,
    default_source: Option<String>,
    restore_target: Option<&str>,
) -> Option<String> {
    if let Some(m) = active_mic {
        return Some(m.to_string());
    }
    let d = default_source?;
    let want = if d == "hushmic_source" {
        restore_target?.to_string()
    } else {
        d
    };
    (want != "hushmic_source").then_some(want)
}

/// Corrections that go out at once; after these, [`REPIN_SLOW`].
pub const REPIN_QUICK: u32 = 3;
/// Once [`REPIN_QUICK`] corrections did not hold: one a minute.
pub const REPIN_SLOW: Duration = Duration::from_secs(60);
/// Our own correction takes a moment to relink: a stolen look this soon
/// after it is not a new theft.
pub const REPIN_SETTLE: Duration = Duration::from_secs(2);
/// Healed this long and the fight is over: the next theft heals at once
/// again.
pub const REPIN_HOLDS: Duration = Duration::from_secs(10);
/// Fed by nothing this long: no session manager still linking (a move
/// links in tens of ms), a link that failed to come up.
pub const REPIN_UNLINKED: Duration = Duration::from_secs(1);
/// A re-pin of a stream fed by nothing that has not linked it this long
/// after did not take: its link is stuck.
pub const REPIN_RELINKED: Duration = Duration::from_millis(1500);
/// Chain restarts in a row that did not get the stream linked: after
/// these the healer stops restarting it and says so ([`Repin::gave_up`]).
pub const REPIN_RESTARTS: u32 = 3;
/// A correction is looked at again this long after it: one that left the
/// stream fed by nothing is caught then, not at the next tick.
pub const REPIN_CHECK: Duration = Duration::from_secs(1);

/// What a look at the graph found about the capture stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// Fed by the mic we want.
    Held,
    /// Fed by another source, and a re-pin can be written.
    Stolen,
    /// Not fed by the mic we want, but there is nothing to re-pin to (the
    /// mic or our input is not in the graph): a mic that left, not a thief.
    OutOfReach,
    /// Fed by nothing, though the mic and our input are in the graph and a
    /// re-pin can be written. Moments of it are the session manager
    /// linking; past [`REPIN_UNLINKED`] it is a move whose new link failed
    /// (WirePlumber 0.4: "item deactivated before format set"), which
    /// leaves the stream unlinked until something makes it look again: a
    /// re-pin does.
    Unlinked,
}

/// What the capture healer does after a look at the graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepinStep {
    Leave,
    /// Re-pin now; `notify`: say once that another app keeps re-routing.
    Repin {
        notify: bool,
    },
    /// Restart the chain: a re-pin did not relink a stream fed by nothing
    /// (WirePlumber's link to the mic is stuck, see [`Repin`]).
    Restart,
}

/// The capture healer's memory (issue #5). The first corrections heal at
/// once (one-shot re-routes: a stale saved target, a mixer misclick, the
/// leftovers of a quit EasyEffects). A stream stolen again after
/// [`REPIN_QUICK`] corrections has an adversary that re-asserts its route
/// after each one (EasyEffects' process-all-inputs): fighting it every
/// time would flap the user's mic, so it says so once and retries once a
/// minute, which still heals soon after the adversary exits.
///
/// It counts corrections, not looks: the graph is read on a 5 s tick and
/// also in between (a new engine run, a chain respawn), and a look that
/// catches the moment between our correction and the adversary's next
/// route must not start the count over. Only a stream that stays healed
/// for [`REPIN_HOLDS`] does. A look with nothing to re-pin to (the mic
/// just left) is no fight either: no correction goes out, and the count
/// starts over, so a headset that keeps reconnecting is never taken for
/// an app that keeps re-routing.
///
/// A stream fed by nothing gets one re-pin (WirePlumber 0.4 drops a move
/// whose link failed, and the write makes it link again). Still fed by
/// nothing [`REPIN_RELINKED`] after it, its link is stuck: a move that
/// lands while the stream's adapter is still being set up for the move
/// before ("was started": the link waits for a format that never comes,
/// until the stream goes). No write frees it, the slow retry neither: the
/// chain restarts, once a [`REPIN_SLOW`] at most. A restart is no
/// correction: it counts toward neither the back-off nor the notice; and
/// the back-off does not hold it back: a stream fed by nothing while the
/// slow retry is on restarts at once, with no re-pin first. Restarts
/// that do not get it linked stop after [`REPIN_RESTARTS`] (a link that
/// never forms, whatever the cause, would restart the chain every minute
/// for good): the healer then re-pins once a minute, silently, and says
/// that the microphone does not connect, until the stream holds again.
///
/// These waits are shorter than the tick: [`Repin::look_due`] says when
/// the next look is owed (a correction to check, a wait that runs out),
/// so a stuck mic is restarted 2.5 s after it is seen fed by nothing
/// (under 4 s after a theft whose correction stuck it), not three ticks
/// later.
#[derive(Debug, Default)]
pub struct Repin {
    /// Corrections since the stream last held.
    streak: u32,
    last: Option<std::time::Instant>,
    healed_since: Option<std::time::Instant>,
    /// Since when the looks found the stream fed by nothing.
    unlinked_since: Option<std::time::Instant>,
    /// When a re-pin went out for a stream fed by nothing, while it still
    /// is: the next step is a restart.
    relinked: Option<std::time::Instant>,
    /// The last chain restart for a stuck link.
    restarted: Option<std::time::Instant>,
    /// Chain restarts since the stream last held.
    restarts: u32,
    /// [`REPIN_RESTARTS`] restarts did not help (see [`Repin::gave_up`]).
    gave_up: bool,
    /// When the last correction is due its check ([`REPIN_CHECK`] after
    /// it), until a look comes then.
    check: Option<std::time::Instant>,
    /// The streak saw the stream fed by another source: an app re-routes
    /// it. One made of failed links only is WirePlumber's, and the notice
    /// (which names EasyEffects) would be wrong advice.
    thief: bool,
    notified: bool,
}

impl Repin {
    /// A look at `now`.
    pub fn observe(&mut self, look: Look, now: std::time::Instant) -> RepinStep {
        if self.check.is_some_and(|t| now >= t) {
            self.check = None;
        }
        if look != Look::Unlinked {
            self.unlinked_since = None;
            self.relinked = None;
        }
        match look {
            Look::Held => {
                self.restarts = 0;
                self.gave_up = false;
                let since = *self.healed_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= REPIN_HOLDS {
                    self.streak = 0;
                    self.thief = false;
                }
                return RepinStep::Leave;
            }
            // The mic left: whatever comes back (a headset that reconnects
            // returns under a new serial and looks stolen at first) is a
            // new story, not a fight that goes on. The notice stays said.
            Look::OutOfReach => {
                self.streak = 0;
                self.thief = false;
                self.restarts = 0;
                self.gave_up = false;
                self.healed_since = None;
                return RepinStep::Leave;
            }
            Look::Stolen => {
                self.healed_since = None;
                self.thief = true;
            }
            // A stream that keeps losing its link counts as one that keeps
            // being re-routed: the same armistice.
            Look::Unlinked => {
                self.healed_since = None;
                let since = *self.unlinked_since.get_or_insert(now);
                if now.saturating_duration_since(since) < REPIN_UNLINKED {
                    return RepinStep::Leave;
                }
                if let Some(due) = self.restart_due(since, now) {
                    if now < due {
                        return RepinStep::Leave;
                    }
                    if self.restarts >= REPIN_RESTARTS {
                        // restarts do not help: the slow re-pin, and say so
                        self.gave_up = true;
                    } else if !self.restarted_lately(now) {
                        self.restarts += 1;
                        self.restarted = Some(now);
                        self.relinked = None;
                        // the new chain links from scratch
                        self.unlinked_since = None;
                        return RepinStep::Restart;
                    }
                }
            }
        }
        let since_last = self.last.map(|t| now.saturating_duration_since(t));
        if since_last.is_some_and(|d| d < REPIN_SETTLE) {
            return RepinStep::Leave;
        }
        if self.streak >= REPIN_QUICK && since_last.is_some_and(|d| d < REPIN_SLOW) {
            return RepinStep::Leave;
        }
        self.streak = self.streak.saturating_add(1);
        self.last = Some(now);
        self.check = Some(now + REPIN_CHECK);
        if look == Look::Unlinked {
            self.relinked = Some(now);
        }
        let notify = self.streak >= REPIN_QUICK
            && self.thief
            && !std::mem::replace(&mut self.notified, true);
        RepinStep::Repin { notify }
    }

    /// Something keeps re-routing the stream (the slow retry is on).
    pub fn fighting(&self) -> bool {
        self.streak >= REPIN_QUICK
    }

    /// Another app keeps taking the stream: the slow retry is on, and the
    /// streak saw it fed by another source. A streak of failed links only
    /// (a stream fed by nothing) is WirePlumber's, and naming an app
    /// (EasyEffects) would be wrong advice.
    pub fn rerouted(&self) -> bool {
        self.fighting() && self.thief
    }

    /// The stream is fed by nothing though [`REPIN_RESTARTS`] chain
    /// restarts in a row tried: the microphone does not connect. Until the
    /// stream holds again (or the mic leaves).
    pub fn gave_up(&self) -> bool {
        self.gave_up
    }

    fn restarted_lately(&self, now: std::time::Instant) -> bool {
        self.restarted
            .is_some_and(|t| now.saturating_duration_since(t) < REPIN_SLOW)
    }

    /// Corrections are held back at `now` (the slow retry).
    fn backed_off(&self, now: std::time::Instant) -> bool {
        self.streak >= REPIN_QUICK
            && self
                .last
                .is_some_and(|t| now.saturating_duration_since(t) < REPIN_SLOW)
    }

    /// When a stream fed by nothing since `since` is due a chain restart
    /// rather than a re-pin, as of `now`: [`REPIN_RELINKED`] after a
    /// re-pin of ours that did not relink it, or once [`REPIN_UNLINKED`]
    /// is out while the back-off holds that re-pin back (a mic fed by
    /// nothing is no fight to back off from, and the slow retry would come
    /// a minute late). None: a re-pin is due first.
    fn restart_due(
        &self,
        since: std::time::Instant,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        match self.relinked {
            Some(at) => Some(at + REPIN_RELINKED),
            None => self.backed_off(now).then_some(since + REPIN_UNLINKED),
        }
    }

    /// The look to ask for after a probe at `now`: [`Repin::look_due`],
    /// but only after a look the healer got (`looked`), and never one
    /// already due. A probe that forms no look (the wanted mic's node is
    /// gone, nothing to fall back to) leaves the due where it was, in the
    /// past: asked for again, it would be read again at once, a pw-dump
    /// after another until the mic is back.
    pub fn owed_look(&self, looked: bool, now: std::time::Instant) -> Option<std::time::Instant> {
        self.look_due().filter(|&due| looked && due > now)
    }

    /// When the next look is owed, if before the tick could be: a
    /// correction to check, or a stream fed by nothing whose wait runs
    /// out (a re-pin, or a restart, falls due then). None: the tick will
    /// do (nothing pending, or only the slow retry and the restart a
    /// minute, which the tick serves well enough).
    pub fn look_due(&self) -> Option<std::time::Instant> {
        let wait = self.unlinked_since.and_then(|since| {
            let waited = since + REPIN_UNLINKED;
            match self.restart_due(since, waited) {
                // a restart, or giving up on them, falls due then
                Some(due) => (!self.gave_up
                    && (self.restarts >= REPIN_RESTARTS || !self.restarted_lately(due)))
                .then_some(due),
                // the re-pin, once our last correction settled
                None => Some(self.last.map_or(waited, |t| waited.max(t + REPIN_SETTLE))),
            }
        });
        match (self.check, wait) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// What a look at the graph finds about the capture stream, from what
/// feeds it and whether our input (`input`) and the mic we want (`mic`)
/// are in the graph. None: nothing to judge (our input itself is gone,
/// and the watchdog sees to that).
///
/// The mic we want gone with nothing feeding us is a mic that left (a
/// pinned USB mic unplugged, nothing to fall back to), not a look to skip:
/// skipped, it would leave the healer's state as it was, with a look owed
/// in the past and a "does not connect" that is no longer so (review A83).
pub fn capture_look(feeders: &[String], want: &str, input: bool, mic: bool) -> Option<Look> {
    match (input, mic) {
        (true, true) if feeders.is_empty() => Some(Look::Unlinked),
        (true, false) if feeders.is_empty() => Some(Look::OutOfReach),
        _ if feeders.is_empty() => None,
        _ if !capture_stolen(feeders, want) => Some(Look::Held),
        (true, true) => Some(Look::Stolen),
        _ => Some(Look::OutOfReach),
    }
}

/// Whether the capture stream has been re-routed away from the mic we
/// declared (issue #5: EasyEffects' "process all input streams" adopts
/// every new capture stream onto `easyeffects_source`, silencing the
/// chain — in the reported setup as a closed hushmic->EE->hushmic loop).
/// Only an EXCLUSIVELY-foreign feed counts: an empty list means the
/// session manager is still linking (re-pinning then would race it), and
/// any link from the wanted mic means audio flows.
pub fn capture_stolen(feeders: &[String], want: &str) -> bool {
    !feeders.is_empty() && !feeders.iter().any(|f| f == want)
}

/// Point the routing metadata for node `id` back at the node named
/// `target` (`target.object`) — the key a session GUI or EasyEffects
/// writes, so it cleanly overrides theirs; the session manager relinks
/// within a tick. A tool that re-asserts its route after every correction
/// turns this into a tug-of-war — [`Repin`] is the caller's armistice.
///
/// By name, not by object serial: WirePlumber's restore-stream (0.4.13 to
/// 0.5) saves the target of every such change as the pick of all streams
/// with the stream's key (its media.role, else its app), and it resolves
/// the value as a serial, so a name saves nothing while a serial saved
/// `Input/Audio:<key>:target=hushmic_source` (or the mic, for the chain's
/// own capture) for good. Its policy finds the target by name as well
/// (and a name outlives a mic node that is recreated). WirePlumber 0.4.8
/// reads only `target.node`, as before.
pub fn repin_capture(id: u32, target: &str) -> bool {
    pw_metadata_within(
        &[
            "-n",
            "default",
            &id.to_string(),
            "target.object",
            target,
            "Spa:String",
        ],
        CALL_BUDGET,
    )
    .is_some()
}

/// What in `pw-mon`'s report makes the graph worth a read now: a node or
/// a device added, changed or removed (a microphone plugged in or out, an
/// app's stream coming or going), and a link added. Clients, ports, the
/// other link changes, modules and the rest are left out: each read is a `pw-dump`, a client of its own that
/// `pw-mon` reports as added and removed, so reacting to clients would
/// read the graph again for every read (UX panel round 2, A107 SF-G1).
/// Fed `pw-mon`'s lines one at a time.
#[derive(Debug, Default)]
pub struct GraphEvents {
    /// The record being read: its kind, and its id once seen.
    kind: Option<GraphChange>,
    id: Option<u32>,
    /// What each id seen so far is: a node or a device (true) or not.
    /// A removal's own record may not say.
    watched: std::collections::HashMap<u32, bool>,
    /// The last report that asked for a read was a link added.
    link_added: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GraphChange {
    Added,
    Changed,
    Removed,
}

impl GraphEvents {
    /// Whether the read just asked for was for a link added (and forget
    /// it): such a read is due at once.
    pub fn take_link_added(&mut self) -> bool {
        std::mem::take(&mut self.link_added)
    }

    /// One line of `pw-mon`; true when it completes the report of a node
    /// or device added, changed or removed.
    pub fn feed(&mut self, line: &str) -> bool {
        let kind = match line {
            "added:" => Some(GraphChange::Added),
            "changed:" => Some(GraphChange::Changed),
            "removed:" => Some(GraphChange::Removed),
            _ => None,
        };
        if kind.is_some() {
            self.kind = kind;
            self.id = None;
            return false;
        }
        let Some(k) = self.kind else {
            return false;
        };
        // a record's own fields: one tab in, a changed one marked `*`
        let field = line.trim_start_matches([' ', '*', '\t']);
        if line[..line.len() - field.len()].matches('\t').count() != 1 {
            return false;
        }
        if let Some(v) = field.strip_prefix("id: ") {
            self.id = v.trim().parse().ok();
            // a change or a removal of an object already known
            let known = self.id.and_then(|id| self.watched.get(&id).copied());
            if let (Some(w), true) = (known, k != GraphChange::Added) {
                self.kind = None;
                if k == GraphChange::Removed {
                    self.watched.remove(&self.id.unwrap_or_default());
                }
                return w;
            }
            return false;
        }
        if let Some(v) = field.strip_prefix("type: ") {
            let Some(id) = self.id else {
                return false;
            };
            self.kind = None;
            let w = ["PipeWire:Interface:Node", "PipeWire:Interface:Device"]
                .iter()
                .any(|t| v.starts_with(t));
            // a new link: one may put a stream where none belongs (see
            // `ChainGraph::intruders`), with no node changing; a read
            // makes no link, so this cannot loop
            if k == GraphChange::Added && v.starts_with("PipeWire:Interface:Link") {
                self.watched.insert(id, false);
                self.link_added = true;
                return true;
            }
            if k == GraphChange::Removed {
                self.watched.remove(&id);
            } else {
                self.watched.insert(id, w);
            }
            return w;
        }
        false
    }
}

/// How long after the first line of a burst the poke comes: one per
/// burst, once its objects are in place.
const MONITOR_SETTLE: Duration = Duration::from_millis(300);
/// The same for a link added (one may carry an app's sound into the
/// microphone): no gap since the last poke, a short settle.
const MONITOR_LINK_SETTLE: Duration = Duration::from_millis(50);
/// And at least this long after the poke before: a graph that keeps
/// changing (a stream's node changing state on and off) is read at most
/// once a second.
const MONITOR_GAP: Duration = Duration::from_secs(1);

/// Watch the graph with `pw-mon` and call `poke` (at most once per burst,
/// at most once a second) when a node or device is added, changed or
/// removed ([`GraphEvents`]), so an unplugged microphone is seen in a
/// moment rather than at the next 5 s look (UX panel round 2, A5). Runs
/// for the life of the process on a thread of its own (the child is bound
/// to it by `PR_SET_PDEATHSIG`); a `pw-mon` that ends (PipeWire restarted)
/// is started again after a pause, and without `pw-mon` nothing changes
/// but the speed.
pub fn watch_graph(poke: impl Fn() + Send + 'static) {
    // true: a link added (due at once)
    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    std::thread::spawn(move || {
        let mut last: Option<std::time::Instant> = None;
        while let Ok(link) = rx.recv() {
            // the rest of the burst, and the gap since the last poke; a
            // link added skips the gap
            let now = std::time::Instant::now();
            let mut until = now + MONITOR_SETTLE;
            if let Some(l) = last {
                until = until.max(l + MONITOR_GAP);
            }
            if link {
                until = now + MONITOR_LINK_SETTLE;
            }
            while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(true) => until = until.min(std::time::Instant::now() + MONITOR_LINK_SETTLE),
                    Ok(false) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
            poke();
            last = Some(std::time::Instant::now());
        }
    });
    let _ = std::thread::Builder::new()
        .name("graph-watch".into())
        .spawn(move || loop {
            use std::io::BufRead;
            use std::os::unix::process::CommandExt;
            let mut cmd = std::process::Command::new("pw-mon");
            cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null());
            // SAFETY: prctl is async-signal-safe; nothing else runs here.
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong);
                    Ok(())
                });
            }
            let Ok(mut child) = cmd.spawn() else {
                return;
            };
            let mut events = GraphEvents::default();
            if let Some(out) = child.stdout.take() {
                for line in std::io::BufReader::new(out).lines() {
                    let Ok(line) = line else { break };
                    if events.feed(&line) && tx.send(events.take_link_added()).is_err() {
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                }
            }
            let _ = child.kill();
            let _ = child.wait();
            std::thread::sleep(Duration::from_secs(5));
        });
}

/// Move the app stream `id` onto the node named `target`, as a session
/// GUI does (the same write as [`repin_capture`]). Not on WirePlumber 0.4.8,
/// which reads only `target.node`: a number written there is saved as the
/// target of every stream of the app's role (pw-record's Music among them).
pub fn move_stream(id: u32, target: &str) -> bool {
    repin_capture(id, target)
}

/// Resolve a node name to its numeric PipeWire id via `pw-dump`, for
/// `pw-record --target`. `None` if the probe fails or the node is absent — the
/// caller then falls back to passing the name (accepted by modern pw-cat).
pub fn node_id(name: &str) -> Option<u32> {
    parse_node_id(&pw_dump()?, name)
}

/// The string to pass to `pw-record --target` for a node NAME.
///
/// Modern pw-cat (>= 0.3.64) accepts the NAME and RE-RESOLVES it at connect
/// time, so it is immune to global-id churn: on newer PipeWire an idle source
/// (a USB mic) is suspended and RECREATED with a new id when a capture wakes
/// it, and a numeric id captured a few ms earlier by `pw-dump` is then stale —
/// pw-cat treats `--target` as a hint, doesn't error, and the session manager
/// silently reroutes the stream to the SYSTEM DEFAULT source (which is how a
/// raw-mic recording once ended up capturing hushmic_source). So prefer the name.
///
/// Only legacy pw-cat (< 0.3.64) needs the numeric id — it rejects a name
/// outright ("bad target option") — and there the id does not churn. Does I/O
/// (`pipewire --version`, and `pw-dump` on the legacy branch).
pub fn record_target(name: &str) -> String {
    if supports_target_object() {
        name.to_string()
    } else {
        node_id(name)
            .map(|id| id.to_string())
            .unwrap_or_else(|| name.to_string())
    }
}

/// Extract the node name from a pw-metadata value line: value:'{"name":"X"}'.
pub fn parse_metadata_value(stdout: &str) -> Option<String> {
    // pw-metadata prints: update: id:0 key:'…' value:'{"name":"X"}' type:'…'.
    // The value is single-quoted with no escaping, so slice from after
    // "value:'" to the LAST "' type:" and hand the payload to a real JSON
    // parser — node names containing double quotes/backslashes then round-trip
    // instead of being truncated by naive quote-splitting.
    let after = stdout.split("value:'").nth(1)?;
    let json = match after.rfind("' type:") {
        Some(i) => &after[..i],
        None => after.split('\'').next()?,
    };
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    Some(v.get("name")?.as_str()?.to_string())
}

/// Extract a plain numeric value from a pw-metadata line: value:'1024'.
pub fn parse_metadata_number(stdout: &str) -> Option<u32> {
    let after = stdout.split("value:'").nth(1)?;
    let raw = match after.rfind("' type:") {
        Some(i) => &after[..i],
        None => after.split('\'').next()?,
    };
    raw.trim().parse().ok()
}

/// The system-wide `clock.force-quantum` from the settings metadata — the
/// manual override knob (`pw-metadata -n settings 0 clock.force-quantum`).
/// `None` = unset, zero (unforced), or probe failure.
pub fn settings_force_quantum() -> Option<u32> {
    let out = pw_metadata_within(&["-n", "settings", "0", "clock.force-quantum"], CALL_BUDGET)?;
    parse_metadata_number(&out).filter(|&q| q > 0)
}

/// The graph's `clock.min-quantum` from the settings metadata (what the
/// quantum pin overrides while it is in effect); None = probe failure.
pub fn settings_min_quantum() -> Option<u32> {
    let out = pw_metadata_within(&["-n", "settings", "0", "clock.min-quantum"], CALL_BUDGET)?;
    parse_metadata_number(&out).filter(|&q| q > 0)
}

/// Run `pw-dump`. `None` means the PROBE failed (binary missing, daemon
/// unreachable, non-zero exit, or output that isn't JSON even after the
/// old-pw-dump repair below) — callers must treat that as "unknown", never
/// as "no nodes": tearing down a healthy child on a failed probe is exactly
/// the watchdog misfire this distinction prevents.
///
/// Pub (raw JSON) because the mic test also traces the *link graph*, which
/// `parse_pwdump_nodes` drops.
pub fn pw_dump() -> Option<String> {
    let o = Command::new("pw-dump").output().ok()?;
    if !o.status.success() {
        return None;
    }
    valid_dump(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// How long one PipeWire tool call the daemon makes on its main loop may
/// take. pw-dump, pw-metadata on the "default" metadata and every pw-cli
/// command wait for the session manager; while WirePlumber does not answer
/// they would wait forever, and the loop (mute, push-to-talk, the tray)
/// with them.
pub const CALL_BUDGET: Duration = Duration::from_secs(2);

/// What a bounded tool call came back with.
enum Ran {
    /// It ended: its exit status and stdout.
    Done(std::process::ExitStatus, Vec<u8>),
    TimedOut,
    /// It could not be started or read.
    Failed,
}

/// Run `cmd` (stdout captured, stderr dropped) to its end, or kill it once
/// `budget` has passed.
fn run_within(mut cmd: Command, budget: Duration) -> Ran {
    let Ok(mut child) = cmd.stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
        return Ran::Failed;
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Ran::Failed;
    };
    let (tx, rx) = std::sync::mpsc::channel();
    // read(2) has no timeout: the reader gets its own thread, and killing
    // the child on a timeout ends it with EOF.
    std::thread::spawn(move || {
        let mut raw = Vec::new();
        let ok = stdout.read_to_end(&mut raw).is_ok();
        let _ = tx.send(ok.then_some(raw));
    });
    let raw = match rx.recv_timeout(budget) {
        Ok(raw) => raw,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Ran::TimedOut;
        }
    };
    match (child.wait(), raw) {
        (Ok(status), Some(raw)) => Ran::Done(status, raw),
        _ => Ran::Failed,
    }
}

/// `tool` with `args`, bounded by `budget`: whether it exited 0 and its
/// stdout; None when it did not end in time or could not run.
pub(crate) fn tool_within(tool: &str, args: &[&str], budget: Duration) -> Option<(bool, String)> {
    let mut cmd = Command::new(tool);
    cmd.args(args);
    match run_within(cmd, budget) {
        Ran::Done(status, raw) => {
            Some((status.success(), String::from_utf8_lossy(&raw).into_owned()))
        }
        Ran::TimedOut | Ran::Failed => None,
    }
}

/// `pw-metadata` with `args`, bounded by `budget`: its stdout when it
/// ended successfully.
pub(crate) fn pw_metadata_within(args: &[&str], budget: Duration) -> Option<String> {
    let mut cmd = Command::new("pw-metadata");
    cmd.args(args);
    match run_within(cmd, budget) {
        Ran::Done(status, raw) if status.success() => {
            Some(String::from_utf8_lossy(&raw).into_owned())
        }
        _ => None,
    }
}

/// What a bounded `pw-dump` came back with.
#[derive(Debug, PartialEq, Eq)]
pub enum DumpProbe {
    Dump(String),
    /// No answer within the budget: PipeWire may only be slow.
    TimedOut,
    /// No dump: pw-dump is missing, failed, or printed no JSON.
    Failed,
}

/// [`pw_dump`] with a deadline, for callers on the main loop that must
/// not stall on a wedged daemon: past `budget` the child is killed and the
/// probe reads as failed (`None`, i.e. unknown).
pub fn pw_dump_within(budget: Duration) -> Option<String> {
    match pw_dump_probe(budget) {
        DumpProbe::Dump(d) => Some(d),
        DumpProbe::TimedOut | DumpProbe::Failed => None,
    }
}

/// [`pw_dump_within`], telling a slow answer from a failed one.
pub fn pw_dump_probe(budget: Duration) -> DumpProbe {
    match run_within(Command::new("pw-dump"), budget) {
        Ran::Done(status, raw) if status.success() => {
            match valid_dump(String::from_utf8_lossy(&raw).into_owned()) {
                Some(d) => DumpProbe::Dump(d),
                None => DumpProbe::Failed,
            }
        }
        Ran::TimedOut => DumpProbe::TimedOut,
        _ => DumpProbe::Failed,
    }
}

/// The JSON check (and old-pw-dump repair) shared by the dump probes.
fn valid_dump(raw: String) -> Option<String> {
    // Fast path: valid JSON passes through untouched (IgnoredAny validates
    // without building a tree).
    if serde_json::from_str::<serde::de::IgnoredAny>(&raw).is_ok() {
        return Some(raw);
    }
    if let Some(merged) = merge_dump_batches(&raw) {
        return Some(merged);
    }
    let repaired = repair_keyless_members(&raw);
    if serde_json::from_str::<serde::de::IgnoredAny>(&repaired).is_ok() {
        return Some(repaired);
    }
    merge_dump_batches(&repaired)
}

/// One JSON array from what pw-dump prints when objects come or go while
/// it runs: the graph array plus more arrays, each a batch of changes in
/// its monitor format (an object with `"info": null` is gone, any other
/// replaces or adds the object with that id), before or after the graph
/// (pw-dump 1.0, observed: our own concurrent pw-dump or pw-metadata, a
/// PipeWire client each, ending mid-dump). One parse of the whole output
/// fails on the second array ("trailing characters"), and a dump read as
/// failed made a port write fail and a graph probe count as PipeWire not
/// answering. The batches are applied in order. None unless the output
/// is two or more arrays of objects. Pub for testability.
pub fn merge_dump_batches(raw: &str) -> Option<String> {
    let mut batches = serde_json::Deserializer::from_str(raw).into_iter::<serde_json::Value>();
    // In order of appearance; a removed object leaves an empty slot. The
    // index finds an id's slot without a scan (a dump holds hundreds).
    let mut objects: Vec<Option<serde_json::Value>> = Vec::new();
    let mut slot_of: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    let mut count = 0;
    for batch in batches.by_ref() {
        let serde_json::Value::Array(items) = batch.ok()? else {
            return None;
        };
        count += 1;
        for item in items {
            let id = item.get("id")?.as_u64()?;
            let gone = item.get("info").is_some_and(|i| i.is_null());
            match (slot_of.get(&id).copied(), gone) {
                (Some(i), true) => {
                    objects[i] = None;
                    slot_of.remove(&id);
                }
                (None, true) => {}
                (Some(i), false) => objects[i] = Some(item),
                (None, false) => {
                    slot_of.insert(id, objects.len());
                    objects.push(Some(item));
                }
            }
        }
    }
    (count > 1)
        .then(|| serde_json::Value::Array(objects.into_iter().flatten().collect()).to_string())
}

/// Salvage the invalid JSON old pw-dump emits when the graph contains a node
/// param type it has no name for: `pw-dump` before ~0.3.80 prints such a
/// param as a KEYLESS `[ ]` object member — `"ProcessLatency": [ ], [ ] }` —
/// which no JSON parser accepts. Any client linking a modern libpipewire
/// triggers it (every Flatpak app with native PipeWire nodes attaches the
/// 0.3.79+ `Tag` param; observed live on Debian 12's pw-dump 0.3.65 while
/// the sandboxed HushMic filter-chain ran). Without the repair every
/// consumer's parse fails and the dump reads as an EMPTY graph — see
/// `pw_dump` for why that must instead surface as a failed probe.
///
/// Only called on input that already failed to parse, and only removes empty
/// `[ ]` members whose enclosing container is an OBJECT (keyless members are
/// impossible in valid JSON); empty arrays nested in arrays or sitting as a
/// key's value are legitimate and preserved. String-aware so brackets inside
/// node names can't derail the scan. Pub for testability.
pub fn repair_keyless_members(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    // Container stack: true = object, false = array.
    let mut stack: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'"' => {
                // Copy the whole string literal, honoring escapes.
                let start = i;
                i += 1;
                while i < b.len() {
                    match b[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                let end = i.min(b.len());
                match s.get(start..end) {
                    Some(chunk) => out.push_str(chunk),
                    // An escape overshooting a non-ASCII boundary: not
                    // structurally salvageable — hand back the original so
                    // the caller's re-parse fails and the probe reads as
                    // failed. Never panic under the tray.
                    None => return s.to_string(),
                }
                continue;
            }
            b'{' => stack.push(true),
            b'}' | b']' => {
                stack.pop();
            }
            b'[' => {
                // An empty `[ <ws> ]` directly inside an OBJECT that does
                // NOT follow a `:` is the keyless-member emission (a key's
                // empty-array VALUE follows its colon; a keyless member can
                // only follow `,` or the opening `{`). Drop it together
                // with ONE adjacent comma (the preceding one when present,
                // else the following one for a leading member).
                if stack.last() == Some(&true) {
                    let mut j = i + 1;
                    while j < b.len() && (b[j] as char).is_whitespace() {
                        j += 1;
                    }
                    let prev = out.trim_end().chars().last();
                    if j < b.len() && b[j] == b']' && matches!(prev, Some(',') | Some('{')) {
                        if prev == Some(',') {
                            // Trim back through whitespace and the comma.
                            let keep = out.trim_end().len() - 1;
                            out.truncate(keep);
                        } else {
                            // Leading member: consume a following comma.
                            let mut k = j + 1;
                            while k < b.len() && (b[k] as char).is_whitespace() {
                                k += 1;
                            }
                            if k < b.len() && b[k] == b',' {
                                i = k + 1;
                                continue;
                            }
                        }
                        i = j + 1;
                        continue;
                    }
                }
                stack.push(false);
            }
            _ => {}
        }
        out.push(c as char);
        i += 1;
    }
    out
}

/// One consistent snapshot of every Audio/Source node; `None` = probe failed.
pub fn sources_snapshot() -> Option<Vec<Source>> {
    pw_dump().map(|s| parse_pwdump_nodes(&s))
}

/// The predicate `list_real_sources` applies: everything except our own
/// virtual node and `.monitor` loopbacks.
pub fn filter_real(sources: &[Source]) -> Vec<Source> {
    sources
        .iter()
        .filter(|s| s.name != "hushmic_source" && !s.name.ends_with(".monitor"))
        .cloned()
        .collect()
}

/// List real capture sources, excluding our own `hushmic_source` and any
/// `.monitor` monitor sources. Empty on probe failure.
pub fn list_real_sources() -> Vec<Source> {
    sources_snapshot()
        .map(|v| filter_real(&v))
        .unwrap_or_default()
}

/// The mic to actually pin in the filter-chain, given the saved choice and a
/// source snapshot. A saved mic that no longer matches any live source is
/// dropped (`None` → follow the system default) so a stale or re-enumerated
/// device can't pin a dead `target.object`, which links to nothing and leaves
/// the chain silent. A failed probe (`None` snapshot) keeps the saved mic —
/// unknown is not the same as gone. A mic still re-enumerating (autostart,
/// resume-from-suspend) can be dropped on that single snapshot, and the chain
/// then follows the default until it is re-selected — an accepted tradeoff for
/// auto-healing a permanently-stale saved device. Pure — no I/O.
pub fn resolve_effective_mic(cfg_mic: Option<&str>, snapshot: Option<&[Source]>) -> Option<String> {
    match (cfg_mic, snapshot) {
        (None, _) => None,
        (Some(m), None) => Some(m.to_string()),
        (Some(m), Some(srcs)) => srcs.iter().any(|s| s.name == m).then(|| m.to_string()),
    }
}

/// The source a chain that follows the system default captures from, as
/// far as per-mic profiles go: the snapshot's effective default (the one
/// WirePlumber links streams to), else the configured one. When the
/// CONFIGURED default is our own node, the answer is `fallback` (with
/// "Set as default microphone" on, the pre-takeover device the chain
/// follows) if the snapshot has it, or nobody: the effective key may name any device while our
/// node comes and goes, and which one would be a race. Never a node that
/// cannot own a profile. Pure.
pub fn followed_default(dump: &str, fallback: Option<&str>) -> Option<String> {
    let usable = |d: &String| crate::config::can_own_profile(d);
    // Only a device that is there: an unplugged pre-takeover default is
    // not what the chain captures from (WirePlumber fell back to another
    // one), and naming it would make every tick see a change.
    let fallback = || {
        fallback
            .map(str::to_string)
            .filter(usable)
            .filter(|f| parse_pwdump_nodes(dump).iter().any(|s| &s.name == f))
    };
    let (effective, configured) = parse_default_keys(dump);
    if configured
        .as_deref()
        .is_some_and(|d| d.starts_with("hushmic_"))
    {
        return fallback();
    }
    effective
        .filter(usable)
        .or_else(|| configured.filter(usable))
        .or_else(fallback)
}

/// What a chain started now runs on.
pub struct ChainTarget {
    /// The effective mic (see [`resolve_effective_mic`]); None = follows
    /// the default source.
    pub mic: Option<String>,
    /// The device whose settings apply (see `config::profile_owner`).
    pub profile_device: Option<String>,
    /// The snapshot both came from; None = probe failed.
    pub sources: Option<Vec<Source>>,
}

/// Resolve [`ChainTarget`] from one pw-dump; follow-default as in
/// [`followed_default`], with `restore_target` the pre-takeover default.
/// Without a snapshot, the configured default alone decides.
pub fn resolve_chain_target(cfg_mic: Option<&str>, restore_target: Option<&str>) -> ChainTarget {
    let usable = |d: &String| crate::config::can_own_profile(d);
    let probe = pw_dump_probe(CALL_BUDGET);
    // Slow is not gone, but a session manager that kept pw-dump waiting
    // would keep pw-metadata waiting too: then the saved values decide.
    let silent = probe == DumpProbe::TimedOut;
    let dump = match probe {
        DumpProbe::Dump(d) => Some(d),
        DumpProbe::TimedOut | DumpProbe::Failed => None,
    };
    let sources = dump.as_deref().map(parse_pwdump_nodes);
    let mic = resolve_effective_mic(cfg_mic, sources.as_deref());
    let followed = if mic.is_some() {
        None
    } else {
        let restore = || restore_target.map(str::to_string).filter(usable);
        match dump.as_deref() {
            Some(d) => followed_default(d, restore_target),
            None if silent => restore(),
            None => match get_default_source() {
                Some(c) if c.starts_with("hushmic_") => restore(),
                c => c.filter(usable).or_else(restore),
            },
        }
    };
    let profile_device =
        crate::config::profile_owner(mic.as_deref(), followed.as_deref()).map(str::to_string);
    ChainTarget {
        mic,
        profile_device,
        sources,
    }
}

/// A source's description for display, when the snapshot has it.
pub fn description_of<'a>(sources: &'a [Source], name: &str) -> Option<&'a str> {
    sources
        .iter()
        .find(|s| s.name == name)
        .map(|s| s.description.as_str())
}

/// Whether the running PipeWire honors `target.object` in a filter-chain
/// capture. The key was added in PipeWire 0.3.64; older releases silently
/// ignore it (the capture then follows the system default), so they need the
/// legacy `node.target` to pin a specific mic. A failed probe is treated as
/// modern — old PipeWire is rare and `target.object`-only is the safe default.
///
/// What must be versioned here is the HOST side (its session manager
/// interprets the key). Outside a sandbox `pipewire --version` is the host;
/// inside a Flatpak that binary is the BUNDLED daemon — always modern, even
/// against an old host session — so there the version comes from the daemon's
/// own Core object in a `pw-dump` snapshot instead.
pub fn supports_target_object() -> bool {
    if crate::sandbox::is_flatpak() {
        return match pw_dump_within(CALL_BUDGET)
            .as_deref()
            .and_then(parse_core_version)
        {
            Some(v) => pw_version_at_least(&v, (0, 3, 64)),
            None => true, // probe failed -> same optimistic default as below
        };
    }
    match Command::new("pipewire").arg("--version").output() {
        Ok(o) => pw_version_at_least(&String::from_utf8_lossy(&o.stdout), (0, 3, 64)),
        Err(_) => true,
    }
}

/// The WirePlumber on this system, (major, minor, micro), from
/// `wireplumber --version` ("Linked with libwireplumber 0.4.13"), read
/// once. None when it cannot say: no such binary here (inside a Flatpak the
/// host's is out of reach), or one too old to have the option (0.4.8).
pub fn wireplumber_version() -> Option<(u32, u32, u32)> {
    static V: std::sync::OnceLock<Option<(u32, u32, u32)>> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        if crate::sandbox::is_flatpak() {
            return None;
        }
        let (_, out) = tool_within("wireplumber", &["--version"], CALL_BUDGET)?;
        parse_wireplumber_version(&out)
    })
}

/// The version on the "Linked with libwireplumber" line of
/// `wireplumber --version`. Pure.
pub fn parse_wireplumber_version(out: &str) -> Option<(u32, u32, u32)> {
    let line = out
        .lines()
        .find(|l| l.contains("Linked with libwireplumber"))?;
    let v = line.split_whitespace().last()?;
    let mut parts = v.split('.').map(|p| p.parse::<u32>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

/// Whether WirePlumber saves nothing for a target written name last (see
/// `speakers::retarget_calls`): its restore-stream reads `target.object`
/// (as a serial, so a name saves nothing) from 0.4.13 on; 0.4.8 reads only
/// `target.node` and saves the output's name whatever comes after. Unknown
/// counts as no.
pub fn name_last_unsaved() -> bool {
    wireplumber_version().is_some_and(|v| v >= (0, 4, 13))
}

/// Whether the filter-chain HOST binary supports the report-only latency
/// node (builtin `delay` grew its `latency` config and the graph latency
/// propagation in PipeWire 1.6.0). The BINARY's version matters, not the
/// daemon's — `pipewire -c` is what hosts the graph, and in the flatpak
/// that is the bundled binary this same probe resolves to. Failure is
/// CONSERVATIVE (false): emitting the node on an unknown host risks
/// 0.3.48's missing-builtin chain kill, while omitting it only costs the
/// latency report.
pub fn supports_latency_report() -> bool {
    match Command::new("pipewire").arg("--version").output() {
        // strict, unlike pw_version_at_least's optimistic unparseable
        // default: no recognizable version token means NO node
        Ok(o) => {
            parsed_version_at_least(&String::from_utf8_lossy(&o.stdout), (1, 6, 0)).unwrap_or(false)
        }
        Err(_) => false,
    }
}

/// Like [`pw_version_at_least`] but `None` when no `a.b.c` token parses,
/// for probes whose safe default is pessimistic. Pure.
pub fn parsed_version_at_least(text: &str, min: (u32, u32, u32)) -> Option<bool> {
    for tok in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let parts: Vec<&str> = tok.split('.').collect();
        if parts.len() >= 3 {
            if let (Ok(a), Ok(b), Ok(c)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
            ) {
                return Some((a, b, c) >= min);
            }
        }
    }
    None
}

/// Extract the `rate` (samples) field from `pw-cli enum-params <id>
/// ProcessLatency` output. `None` for absent/zero/unparseable — a chain
/// that reports nothing publishes rate 0. Pure.
pub fn parse_process_latency(text: &str) -> Option<u32> {
    let mut in_rate = false;
    for l in text.lines() {
        if in_rate {
            if let Some(v) = l.trim().strip_prefix("Int ") {
                return v.trim().parse::<u32>().ok().filter(|&n| n > 0);
            }
            in_rate = false;
        }
        if l.contains("ProcessLatency:rate") {
            in_rate = true;
        }
    }
    None
}

/// Live read-back of the latency the RUNNING chain reports to PipeWire —
/// no audio involved, just params. `None` = chain absent, pw-cli failed,
/// or nothing reported (old host or missing declaration).
pub fn chain_reported_latency() -> Option<u32> {
    let id = node_id("hushmic_input")?;
    let out = Command::new("pw-cli")
        .args(["enum-params", &id.to_string(), "ProcessLatency"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_process_latency(&String::from_utf8_lossy(&out.stdout))
}

/// The HOST daemon's version from a `pw-dump` snapshot: the
/// `PipeWire:Interface:Core` object's `info.version`. This is the daemon
/// being talked to, regardless of which pw-* binaries do the talking. Pure.
pub fn parse_core_version(stdout: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stdout).ok()?;
    for o in v.as_array()? {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Core") {
            continue;
        }
        if let Some(ver) = o
            .get("info")
            .and_then(|i| i.get("version"))
            .and_then(|s| s.as_str())
        {
            return Some(ver.to_string());
        }
    }
    None
}

/// Parse the first `X.Y.Z` version triple out of `pipewire --version` output
/// ("Compiled with libpipewire 0.3.48") and compare it to `min`. No triple
/// found → true (assume modern). Pure.
pub fn pw_version_at_least(text: &str, min: (u32, u32, u32)) -> bool {
    for tok in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let parts: Vec<&str> = tok.split('.').collect();
        if parts.len() >= 3 {
            if let (Ok(a), Ok(b), Ok(c)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
            ) {
                return (a, b, c) >= min;
            }
        }
    }
    true
}

/// Whether our virtual mic node `hushmic_source` is currently a live PipeWire
/// source. Used by the watchdog: the host child can linger after a daemon
/// restart while the node itself is gone, so node presence (not child PID) is
/// the real liveness signal. `None` = the probe itself failed (unknown).
/// Retry a definitive-answer probe across transient failures. `pw-dump`
/// exits non-zero (or emits unparseable JSON) under graph churn — several
/// concurrent clients plus node registration at once, which is exactly
/// when a window gets (re)opened — and one transient probe failure
/// must not decline a user-visible action (observed live: tag pipeline
/// 3158's declined window reopen). First `Some` wins; `delay` between
/// attempts; all-`None` after `attempts` stays `None` (unknown is still
/// not a verdict). Pure over the injected probe — unit-tested.
pub fn retry_probe<F: FnMut() -> Option<bool>>(
    mut probe: F,
    attempts: u32,
    delay: std::time::Duration,
) -> Option<bool> {
    for i in 0..attempts {
        if let Some(v) = probe() {
            return Some(v);
        }
        if i + 1 < attempts {
            std::thread::sleep(delay);
        }
    }
    None
}

/// Get the currently configured default source node name (None also when
/// pw-metadata did not answer within [`CALL_BUDGET`]).
pub fn get_default_source() -> Option<String> {
    read_default_source(CALL_BUDGET).flatten()
}

/// The configured default source as read within `budget`: None when
/// pw-metadata gave no answer, Some(None) when the key is unset.
pub fn read_default_source(budget: Duration) -> Option<Option<String>> {
    let out = pw_metadata_within(
        &["-n", "default", "0", "default.configured.audio.source"],
        budget,
    )?;
    Some(parse_metadata_value(&out))
}

/// The flatpak-manifest-installed drop-in that gives the bundled pw-* tools
/// `media.category = Manager` context properties. WirePlumber grants plain
/// flatpak-flagged clients `rx` only and silently discards their metadata
/// writes (`pw-metadata` exits 0, the key never changes — verified on
/// PipeWire 1.6 / WirePlumber 0.5.14); Manager-flagged clients get full
/// `rwxm` (the sanctioned mechanism EasyEffects and pavucontrol use, also
/// verified live). The path must stay in lockstep with
/// packaging/flatpak/io.github.fovty.HushMic.yml.
const FLATPAK_MANAGER_DROPIN: &str = "/app/share/pipewire/client.conf.d/50-hushmic-manager.conf";

/// Whether this process is able to change the system default source.
/// Native installs always can. Inside a Flatpak it works exactly when the
/// Manager drop-in ships next to the bundled tools — a rebuild that strips
/// it would otherwise leave every takeover to fail invisibly, so the whole
/// take-the-default feature (menu entry included) is gated on the actual
/// capability rather than attempted blindly.
pub fn can_set_default() -> bool {
    !crate::sandbox::is_flatpak() || std::path::Path::new(FLATPAK_MANAGER_DROPIN).exists()
}

/// The LADSPA node name the conf renders; the filter-chain exposes every
/// control port of it as `<node>:<port>` in the capture node's `Props`.
const DSP_NODE: &str = "hushmic_dsp";

/// The plugin's attenuation-limit control port, spelled exactly as the
/// conf template's `control = { … }` names it.
pub const ATTN_CONTROL: &str = "Attenuation Limit (dB)";

/// The SPA-JSON argument `pw-cli set-param … Props` takes to change one of
/// the plugin's control ports on a running chain. `value` is the literal
/// token the caller already formatted (`1`, `20.0`).
pub fn param_arg(control: &str, value: &str) -> String {
    format!(r#"{{ params = [ "{DSP_NODE}:{control}" {value} ] }}"#)
}

/// The token the attenuation limit is written as: one decimal, sanitized
/// like `render_conf` does (a non-finite value would be a literal `NaN`
/// PipeWire rejects).
pub fn attn_value(db: f32) -> String {
    format!("{:.1}", attn_sanitized(db))
}

/// The attenuation limit as the chain carries it: clamped to 0..=100 dB, a
/// non-finite value read as 100 (what `render_conf` writes).
pub fn attn_sanitized(db: f32) -> f32 {
    if db.is_finite() {
        db.clamp(0.0, 100.0)
    } else {
        100.0
    }
}

/// Write one control port of the running chain. The controls surface on
/// the `hushmic_input` stream node (verified against a live chain:
/// enum-params shows them there, and a set-param read-back round-trips).
/// Both PipeWire calls wait for the session manager (pw-cli binds every
/// object at start, WirePlumber's metadata among them), so each gives up
/// after `budget`. False = node missing, pw-cli failed or too slow. Off
/// the main loop only (`portwriter`).
///
/// `still_wanted` is asked between the lookup and the write: a chain that
/// was replaced meanwhile has a new `hushmic_input`, and a write meant for
/// the old one must not land on it (true then: nothing to report).
pub fn set_chain_param_within(
    control: &str,
    value: &str,
    budget: Duration,
    still_wanted: &dyn Fn() -> bool,
) -> bool {
    let Some(id) = pw_dump_within(budget).and_then(|d| parse_node_id(&d, "hushmic_input")) else {
        return false;
    };
    if !still_wanted() {
        return true;
    }
    let mut cmd = Command::new("pw-cli");
    cmd.args([
        "set-param",
        &id.to_string(),
        "Props",
        &param_arg(control, value),
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    status_within(&mut cmd, budget).is_some_and(|s| s.success())
}

/// Run `cmd` to its end, or kill it once `budget` has passed (None then,
/// and on a failed spawn).
pub fn status_within(cmd: &mut Command, budget: Duration) -> Option<std::process::ExitStatus> {
    let mut child = cmd.spawn().ok()?;
    let deadline = std::time::Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Set the default source node name via pw-metadata. The write and its
/// read-back share one [`CALL_BUDGET`] (plus a last quarter-second look).
pub fn set_default_source(node_name: &str) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + CALL_BUDGET;
    set_default_source_with(node_name, &|| Some(left(deadline)))
}

/// The write and its read-back, each call bounded by what `budget` gives
/// (None: no time for another call). No answer in time is
/// `ErrorKind::TimedOut` (the write may still land); an answer that is not
/// the value, or a failed write, is `Other`.
pub fn set_default_source_with(
    node_name: &str,
    budget: &dyn Fn() -> Option<Duration>,
) -> std::io::Result<()> {
    write_default_source(node_name, budget)?;
    confirm_default_source(node_name, budget)
}

/// The write alone (the first half of [`set_default_source_with`]): Ok once
/// pw-metadata has sent it and exited 0, which does not mean it applied.
pub fn write_default_source(
    node_name: &str,
    budget: &dyn Fn() -> Option<Duration>,
) -> std::io::Result<()> {
    // serde_json handles escaping; a node name containing `"` or `\` must not
    // produce malformed metadata JSON (a silently failed restore on disable).
    let val = serde_json::json!({ "name": node_name }).to_string();
    let silent = |what: &str| std::io::Error::new(std::io::ErrorKind::TimedOut, what.to_string());
    let Some(b) = budget() else {
        return Err(silent("pw-metadata set did not answer in time"));
    };
    let mut cmd = Command::new("pw-metadata");
    cmd.args([
        "-n",
        "default",
        "0",
        "default.configured.audio.source",
        &val,
        "Spa:String:JSON",
    ]);
    match run_within(cmd, b) {
        Ran::Done(status, _) if status.success() => {}
        Ran::TimedOut => return Err(silent("pw-metadata set did not answer in time")),
        Ran::Done(..) | Ran::Failed => {
            return Err(std::io::Error::other("pw-metadata set failed"));
        }
    }
    Ok(())
}

/// The read-back (the second half of [`set_default_source_with`]): Ok once
/// the key holds `node_name`.
pub fn confirm_default_source(
    node_name: &str,
    budget: &dyn Fn() -> Option<Duration>,
) -> std::io::Result<()> {
    let silent = |what: &str| std::io::Error::new(std::io::ErrorKind::TimedOut, what.to_string());
    // Exit status alone is not proof: the daemon acks metadata writes it has
    // no intention of applying (a permission-restricted client's set is
    // dropped server-side with pw-metadata still exiting 0). Success is the
    // key actually holding the value — callers persist prior-default state
    // on this result, so a false Ok would strand the user's real device.
    // One retried read guards the OTHER false verdict: a transiently failed
    // read-back must not report an applied write as dropped.
    let mut answered = false;
    for attempt in 0..2 {
        let Some(b) = budget() else {
            break;
        };
        if let Some(now) = read_default_source(b) {
            if now.as_deref() == Some(node_name) {
                return Ok(());
            }
            answered = true;
        }
        if attempt == 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    if answered {
        Err(std::io::Error::other(
            "pw-metadata set was not applied (insufficient permissions on the metadata object?)",
        ))
    } else {
        Err(silent("the default could not be read back in time"))
    }
}

/// What is left of `deadline`, and never less than a quarter second (a
/// last look still gets a chance).
fn left(deadline: std::time::Instant) -> Duration {
    deadline
        .saturating_duration_since(std::time::Instant::now())
        .max(Duration::from_millis(250))
}

/// What is left of `deadline` for one more call; None once too little is
/// left to start one.
pub fn remaining(deadline: std::time::Instant) -> Option<Duration> {
    let r = deadline.saturating_duration_since(std::time::Instant::now());
    (r >= Duration::from_millis(50)).then_some(r)
}

/// Delete the `default.configured.audio.source` metadata key entirely.
///
/// Used on teardown when this run set the default but there was no prior
/// configured default to restore: leaving the key pointed at the now-dead
/// `hushmic_source` would strand the system default on a vanished node, so we
/// delete the key instead, returning PipeWire to "no configured default".
/// As with [`set_default_source_with`], no answer in time is
/// `ErrorKind::TimedOut`; a failed delete, or one that was not applied, is
/// `Other`.
pub fn clear_default_source() -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + CALL_BUDGET;
    let silent = |what: &str| std::io::Error::new(std::io::ErrorKind::TimedOut, what.to_string());
    let mut cmd = Command::new("pw-metadata");
    cmd.args([
        "-n",
        "default",
        "-d",
        "0",
        "default.configured.audio.source",
    ]);
    match run_within(cmd, left(deadline)) {
        Ran::Done(status, _) if status.success() => {}
        Ran::TimedOut => return Err(silent("pw-metadata delete did not answer in time")),
        Ran::Done(..) | Ran::Failed => {
            return Err(std::io::Error::other("pw-metadata delete failed"));
        }
    }
    // Same silent-drop hazard as set_default_source, but the goal here is
    // narrower: the key must no longer strand the default on OUR dead node.
    // Absent is success; someone ELSE's value appearing concurrently (a
    // session manager restoring its persisted default the moment ours is
    // deleted) is success too — treating that as failure would loop the
    // teardown against an actor that keeps winning. Only the key still
    // reading `hushmic_source` (after one retried look) proves the delete
    // was dropped; no answer proves nothing either way.
    let mut answered = false;
    for attempt in 0..2 {
        match read_default_source(left(deadline)) {
            Some(v) if v.as_deref() != Some("hushmic_source") => return Ok(()),
            Some(_) => answered = true,
            None => {}
        }
        if attempt == 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    if answered {
        Err(std::io::Error::other(
            "pw-metadata delete was not applied (insufficient permissions on the metadata object?)",
        ))
    } else {
        Err(silent("the default could not be read back in time"))
    }
}
