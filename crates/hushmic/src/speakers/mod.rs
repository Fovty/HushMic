//! The speakers cleaner: a selectable virtual output, "HushMic Speakers"
//! (`hushmic_sink`), whose sound reaches a real output cleaned. A second
//! filter-chain process next to the microphone's, sharing only the spawn
//! path ([`crate::controller::spawn_chain`]); none of the microphone's
//! host, link, takeover or healer machinery applies.
//!
//! HushMic never makes the sink the default output and never moves
//! streams. When the user makes it the default, the chain plays to the
//! last real default instead (it would otherwise loop into itself), and
//! HushMic's own players target a real output ([`graph::own_player_target`]).
//! Mute, bypass, shortcuts and the tray never touch it; HushMic turned off
//! and quit stop it. Its engine worker runs one realtime step below the
//! microphone's, so the microphone wins under load.

pub mod conf;
pub mod doctor;
pub mod graph;
pub mod runtime;

use crate::config::{Config, Inference};
use crate::controller::{spawn_chain, ChainControl, Paths, SpawnSpec};
use crate::diagnostics::Chain;
use graph::{own_player_target, Graph};
use runtime::{Facts, Runtime, Step};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

/// The virtual output apps pick ("HushMic Speakers").
pub const SINK: &str = "hushmic_sink";
/// The chain's stream toward the real output.
pub const PLAYBACK: &str = "hushmic_playback";
/// Both nodes' `node.link-group`: its own, not the microphone's.
pub const LINK_GROUP: &str = "hushmic-speakers";
/// The fixed model: quality, with the light model as the plugin's
/// automatic fallback under load.
pub const MODEL: &str = "dpdfnet8_48khz_hr";
/// How many realtime steps the speakers worker asks for below the
/// microphone's (`HUSHMIC_WORKER_RT_BELOW`).
pub const RT_BELOW: &str = "1";

/// Where the speakers conf is written.
pub fn conf_path() -> PathBuf {
    crate::controller::state_dir().join("filter-chain-speakers.conf")
}

/// Whether the cleaner should run under `cfg`: HushMic on and the cleaner
/// switched on.
pub fn wanted_on(cfg: &Config) -> bool {
    cfg.enabled && cfg.speakers.as_ref().is_some_and(|s| s.enabled)
}

/// What the cleaner is brought to on a settings change: everything in the
/// config that reaches it.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub on: bool,
    pub speakers: crate::config::SpeakersConfig,
    pub inference: Inference,
}

impl Settings {
    pub fn of(cfg: &Config) -> Settings {
        Settings {
            on: wanted_on(cfg),
            speakers: cfg.speakers(),
            inference: cfg.inference,
        }
    }
}

/// What `status` reports about the cleaner (its `speakers` block).
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// The saved switch.
    pub enabled: bool,
    /// It should run: the switch, and HushMic on.
    pub on: bool,
    /// The chain process is alive.
    pub running: bool,
    /// The saved output (None: the system default).
    pub output_configured: Option<String>,
    /// The output the running chain was pinned to (None: it follows the
    /// default, or none runs).
    pub output_target: Option<String>,
    /// Where its sound goes now (what `hushmic_playback` is linked into),
    /// and that output's name.
    pub output_active: Option<String>,
    pub output_active_name: Option<String>,
    /// The user made HushMic Speakers the system default output.
    pub system_default: bool,
    pub attn_limit: f32,
    pub model: &'static str,
    /// The engine tier its chain reports, and what it runs the live model on.
    pub engine: Option<crate::diagnostics::EngineTier>,
    pub inference: Option<crate::diagnostics::Inference>,
    /// `hushmic_sink` in the last graph read (None: no read yet).
    pub node_present: Option<bool>,
    pub node_state: Option<String>,
    /// The streams playing into it (node names).
    pub playing: Vec<String>,
    /// "Hold to hear the original" is held.
    pub bypass: bool,
    /// Failed starts in a row, whether that counts as failing, and why the
    /// last one failed.
    pub failures: u32,
    pub failing: bool,
    pub error: Option<String>,
    /// Apps a restart left on another output (see `Speakers::left`).
    pub left: Vec<String>,
}

impl Status {
    /// `status`'s line, e.g. `on, playing to Speakers (2 streams)`.
    pub fn words(&self) -> String {
        if !self.enabled {
            return "off".into();
        }
        if !self.on {
            return "off (HushMic is off)".into();
        }
        if !self.running {
            return match (&self.error, self.failing) {
                (Some(e), _) => format!("not running ({e})"),
                (None, true) => "not running (it keeps stopping)".into(),
                (None, false) => "starting".into(),
            };
        }
        if self.node_present == Some(false) {
            return "running, HushMic Speakers missing from PipeWire".into();
        }
        let to = self
            .output_active_name
            .as_deref()
            .or(self.output_active.as_deref());
        let mut w = match to {
            Some(t) => format!("on, playing to {t}"),
            None => "on, not linked to an output".into(),
        };
        match (&self.output_configured, &self.output_active) {
            (Some(c), Some(a)) if c != a => w.push_str(&format!(" (chosen: {c}, not plugged in)")),
            (None, _) if self.system_default => {
                w.push_str(" (HushMic Speakers is the system output; playing to the last one)")
            }
            _ => {}
        }
        match self.playing.len() {
            0 => w.push_str(", nothing playing through it"),
            1 => w.push_str(", 1 stream"),
            n => w.push_str(&format!(", {n} streams")),
        }
        if self.bypass {
            w.push_str(", bypass held");
        }
        w
    }

    /// `status --json`'s `speakers` block.
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "on": self.on,
            "running": self.running,
            "output": {
                "configured": self.output_configured,
                "target": self.output_target,
                "active": self.output_active,
                "active_name": self.output_active_name,
                "system_default": self.system_default,
            },
            "attn_limit": self.attn_limit,
            "model": self.model,
            "engine": self.engine.filter(|_| self.running).map(crate::diagnostics::EngineTier::word),
            "inference": self.inference.as_ref().filter(|_| self.running).map(|i| serde_json::json!({
                "model": i.model,
                "engine": i.engine,
                "reason": i.reason,
            })),
            "chain": {
                "node_present": self.node_present.filter(|_| self.running),
                "node_state": self.node_state.as_ref().filter(|_| self.running),
                "idle": self.node_state.as_deref()
                    .filter(|_| self.running)
                    .map(crate::pipewire::state_is_idle),
            },
            "playing": self.playing,
            "bypass": self.bypass,
            "failures": self.failures,
            "error": self.error,
            "left": self.left,
        })
    }
}

/// A strength in dB the way the CLI and the doctor say it: `12 dB`, `7.5 dB`.
pub(crate) fn db_words(attn_limit: f32) -> String {
    if attn_limit.fract() == 0.0 {
        format!("{} dB", attn_limit as i64)
    } else {
        format!("{attn_limit} dB")
    }
}

/// `hushmic speakers`'s answer: its state, where it plays, its strength.
/// `status`: from the running HushMic; None renders the saved settings
/// (no HushMic running).
pub fn render_show(cfg: &Config, status: Option<&Status>) -> String {
    let sp = cfg.speakers();
    let state = match status {
        Some(s) => s.words(),
        None if sp.enabled => "on (applies when HushMic starts)".into(),
        None => "off".into(),
    };
    let db = db_words(sp.attn_limit);
    let step = crate::appstate::Strength::from_attn(sp.attn_limit);
    let strength = match step.attn_limit() {
        Some(_) => format!("{} ({db})", format!("{step:?}").to_lowercase()),
        None => db,
    };
    format!(
        "speakers: {state}\noutput: {}\nstrength: {strength}\n",
        sp.output
            .as_deref()
            .unwrap_or("default (the system output)")
    )
}

/// `hushmic devices outputs`: the real outputs, the chosen one marked.
pub fn render_outputs(
    outputs: &[crate::pipewire::Source],
    configured: Option<&str>,
    json: bool,
) -> String {
    if json {
        return serde_json::Value::Array(
            outputs
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name,
                        "description": s.description,
                        "selected": configured == Some(s.name.as_str()),
                    })
                })
                .collect(),
        )
        .to_string();
    }
    if outputs.is_empty() {
        return "(no outputs found)\n".to_string();
    }
    outputs
        .iter()
        .map(|s| {
            let mark = if configured == Some(s.name.as_str()) {
                "*"
            } else {
                " "
            };
            format!("{mark} {}\t{}\n", s.name, s.description)
        })
        .collect()
}

/// After `speakers output NAME`: a word when NAME is not an output there
/// now (`outputs`: None when PipeWire could not be asked). Saved all the
/// same: an output unplugged now is played to when it comes back.
pub fn output_warning(name: &str, graph: Option<&Graph>) -> Option<String> {
    let g = graph?;
    if g.is_feeder(name) {
        return Some(format!(
            "warning: {name} feeds the microphone HushMic cleans, so HushMic Speakers \
             does not play to it (it plays to the system output)"
        ));
    }
    if name == "default" || g.has_sink(name) {
        return None;
    }
    Some(format!(
        "warning: {name} is not an output now (see hushmic devices outputs); until it is, \
         HushMic Speakers plays to the system output"
    ))
}

/// What a running chain was started with.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    /// The output it is pinned to (None: it follows the default).
    pub target: Option<String>,
    pub attn: f32,
    pub inference: Inference,
}

/// How a settings change reaches the chain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Delta {
    Unchanged,
    /// Only the strength: written live through the control file.
    Attn(f32),
    /// Anything the conf or spawn carries.
    Restart,
}

impl Delta {
    /// `running` = what the live chain carries, `wanted` = the same for
    /// the new settings. Pure.
    pub fn of(running: &Rendered, wanted: &Rendered) -> Delta {
        if running.target != wanted.target || running.inference != wanted.inference {
            Delta::Restart
        } else if running.attn != wanted.attn {
            Delta::Attn(wanted.attn)
        } else {
            Delta::Unchanged
        }
    }
}

/// A running speakers chain.
struct Running {
    child: Child,
    tee: Option<std::thread::JoinHandle<()>>,
    meter: Option<PathBuf>,
    control: Option<ChainControl>,
    generation: u64,
    rendered: Rendered,
}

/// Owns the speakers chain: decides with [`Runtime`] and carries it out.
/// Main thread only (the spawn's parent-death signal is thread-scoped).
pub struct Speakers {
    paths: Paths,
    running: Option<Running>,
    runtime: Runtime,
    /// The last snapshot looked at.
    graph: Option<Graph>,
    /// "Hold to hear the original": the chain passes its input through.
    bypass: bool,
    /// The chain is muted through its control file: its playback landed
    /// on an output that feeds the microphone (N-S1: never played to).
    gagged: bool,
    /// Until when the hold lasts without a renewal.
    lease: HoldLease,
    /// Why the last start failed.
    last_error: Option<String>,
    /// The failing notification.
    teller: Teller,
    /// Every real output seen this run: where it was last listed and its
    /// description (a chosen output that is unplugged keeps both).
    seen_outputs: std::collections::HashMap<String, (usize, String)>,
    /// The streams playing into the sink at the last look that saw it
    /// running (object.serial): what a restart, or a start after a switch
    /// off or HushMic off, brings back.
    last_writers: Vec<u64>,
    /// The same streams as read then (what the notice for one left behind
    /// names).
    last_writer_refs: Vec<crate::pipewire::Reader>,
    /// Of `last_writers`, the ones a mixer moved in by numbers (the sink's
    /// id and serial, as pactl and the desktops' pickers write them):
    /// WirePlumber keeps that as the app's pick (see [`mixer_pairs`]).
    last_paired: Vec<u64>,
    /// After a restart: the streams to bring back onto the new sink, and
    /// since when.
    reattach: Option<(Vec<u64>, Instant)>,
    /// A re-target to verify: where the playback stream should now play,
    /// and since when.
    retarget_check: Option<(Option<String>, Instant)>,
    /// The streams HushMic moved by metadata: the ones sent back to the
    /// sink (cleared once back there, or after LAND_LIMIT if they never get
    /// there; one HushMic holds keeps them while it runs) and its own
    /// players (cleared once gone). Every one is cleared when the chain
    /// stops.
    attached: Vec<Attached>,
    /// Apps a restart left on another output: only kept metadata could
    /// hold them on the new sink (their own target is a number of the sink
    /// that is gone, or a mixer put them there), and this WirePlumber would
    /// save it (see [`hold_allowed`]). Named in the Speakers page and the
    /// doctor until they end or play into the sink again.
    left: Vec<crate::pipewire::Reader>,
    /// The streams carrying HushMic's targets from before (a HushMic that
    /// was killed) were looked for since the chain started.
    adopted: bool,
    /// The sink's node id and serial while it is up: once it is gone, a
    /// routing target naming it by number points at nothing, and the next
    /// node given that id (HushMic's own capture among them) would get the
    /// stream on WirePlumber 0.4. Such targets are deleted then.
    sink_numbers: Option<(u32, Option<u64>)>,
    /// The running chain's playback stream as re-targeted: its node id and
    /// what was written for it (cleared when the chain stops).
    playback_written: Option<(u32, Written)>,
    /// HushMic's own players moved, by serial, and where to (None: the
    /// system default): each is asked once per target.
    own_moves: Vec<(u64, Option<String>)>,
    /// When the last look was.
    last_look: Option<Instant>,
}

/// How long a re-targeted playback stream gets to move before the chain is
/// restarted onto the output instead.
const RETARGET_GRACE: Duration = Duration::from_secs(3);

/// How long the streams of a restarted chain are waited for.
const REATTACH_LIMIT: Duration = Duration::from_secs(30);

/// How long a stream sent back keeps HushMic's metadata at most when it
/// does not land on the sink: then it goes anyway.
const LAND_LIMIT: Duration = Duration::from_secs(5);

/// A stream HushMic moved by metadata: its node id and serial, what was
/// written for it, when, whether it is one of HushMic's own players, and
/// whether HushMic holds it on the sink (its keys stay while HushMic runs).
#[derive(Debug, Clone)]
struct Attached {
    id: u32,
    serial: u64,
    written: Written,
    at: Instant,
    own: bool,
    held: bool,
    /// It has been seen on the sink since.
    landed: bool,
    /// Sent back by the sink's id and serial, as the mixer that moved it in
    /// did (see [`pair_calls`]): the pair stays when the chain stops, as
    /// WirePlumber's pick for the app stands on it.
    pair: bool,
}

/// The `pw-metadata` calls that move a stream (node `id`) to the node
/// `target` (with its node id when known), or with None back to the system
/// default output. A stream's target in the session manager's metadata wins
/// over the one its properties carry, and "-1" means "none: follow the
/// default". `target.object` (a name) is what WirePlumber 0.4.13+ and 0.5
/// read first; `target.node` (an id) is all 0.4.8 reads.
///
/// `target.object` comes last: WirePlumber's restore-stream (0.4.13 to
/// 0.5) saves the target of each change as the pick of every stream with
/// the same key (its media.role, which pw-cat sets to Music, else its
/// app), and it resolves a `target.object` value as an object serial, so
/// a name saves nothing. Written last, it leaves nothing saved; `target.node`
/// last would save the output for all of them.
pub fn retarget_calls(id: u32, target: Option<&str>, target_id: Option<u32>) -> Vec<Vec<String>> {
    // "--": "-1" is a value, not an option
    let call = |key: &str, value: &str, kind: &str| -> Vec<String> {
        ["-n", "default", "--", &id.to_string(), key, value, kind]
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    match (target, target_id) {
        (Some(t), Some(n)) => vec![
            call("target.node", &n.to_string(), "Spa:Id"),
            call("target.object", t, "Spa:String"),
        ],
        (Some(t), None) => vec![call("target.object", t, "Spa:String")],
        (None, _) => vec![
            call("target.node", "-1", "Spa:Id"),
            call("target.object", "-1", "Spa:Id"),
        ],
    }
}

/// The `pw-metadata` calls that move a stream (node `id`) onto the sink by
/// its node id `sink_id` and serial `sink_serial`, the serial last: what a
/// mixer (pactl, pavucontrol, the desktops' pickers) writes. WirePlumber
/// 0.4.13 and newer save that as the pick of every stream with the app's
/// key, as they did for the mixer's own move; written name last (as
/// [`retarget_calls`] does) or deleted, that pick is gone. For a stream a
/// mixer moved in, sent back after a restart. Pure.
pub fn pair_calls(id: u32, sink_id: u32, sink_serial: u64) -> Vec<Vec<String>> {
    let call = |key: &str, value: &str| -> Vec<String> {
        ["-n", "default", "--", &id.to_string(), key, value, "Spa:Id"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    vec![
        call("target.node", &sink_id.to_string()),
        call("target.object", &sink_serial.to_string()),
    ]
}

/// The app streams of `g` a mixer moved onto the sink: their metadata
/// names it by its node id and its serial (by serial: the ones WirePlumber
/// keeps as the app's pick). Pure.
pub fn mixer_pairs(g: &Graph) -> Vec<u64> {
    let (Some(id), Some(serial)) = (g.node_id(SINK), g.sink_serial) else {
        return Vec::new();
    };
    let (id, serial) = (id.to_string(), serial.to_string());
    g.out_streams
        .iter()
        .filter(|s| {
            g.target_of(s.id, "target.node") == Some(id.as_str())
                && g.target_of(s.id, "target.object") == Some(serial.as_str())
        })
        .map(|s| s.serial)
        .collect()
}

/// Whether the stream `id` carries a mixer's id + serial pair (both
/// numbers): after a restart, the numbers of the sink that is gone, which
/// HushMic left in place (see `forget_numbers`). Pure.
pub fn carries_pair(g: &Graph, id: u32) -> bool {
    let num = |k: &str| {
        g.target_of(id, k)
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
    };
    num("target.node") && num("target.object")
}

/// Run `calls` ([`retarget_calls`] or [`pair_calls`]): the (key, value)
/// pairs written, None when the `target.object` one was not taken.
fn write_calls(calls: Vec<Vec<String>>) -> Option<Written> {
    let mut written = Vec::new();
    let mut decided = false;
    for args in calls {
        let refs: Vec<&str> = args.iter().map(|a| a.as_str()).collect();
        if crate::pipewire::pw_metadata_within(&refs, crate::pipewire::CALL_BUDGET).is_some() {
            decided |= args[4] == "target.object";
            written.push((args[4].clone(), args[5].clone()));
        }
    }
    decided.then_some(written)
}

/// Run [`retarget_calls`]: the (key, value) pairs written, None when the
/// `target.object` one (the one that decides on current session managers)
/// was not taken.
fn move_stream(id: u32, target: Option<&str>, target_id: Option<u32>) -> Option<Written> {
    write_calls(retarget_calls(id, target, target_id))
}

/// Metadata keys HushMic wrote for one node, with their values.
pub type Written = Vec<(String, String)>;

/// The keys of `id` in a `pw-metadata -n default -- ID` listing that still
/// hold what HushMic wrote (`written`): the ones to delete once that node
/// is gone. PipeWire 1.x drops a node's metadata with the node; older ones
/// keep it for the next node given the same id.
pub fn our_targets(listing: &str, id: u32, written: &[(String, String)]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for line in listing.lines() {
        if !line.contains(&format!("id:{id} ")) {
            continue;
        }
        for (key, value) in written {
            if line.contains(&format!("key:'{key}'"))
                && line.contains(&format!("value:'{value}'"))
                && !keys.contains(key)
            {
                keys.push(key.clone());
            }
        }
    }
    keys
}

fn clear_our_targets(id: u32, written: &[(String, String)]) {
    let budget = crate::pipewire::CALL_BUDGET;
    let id_s = id.to_string();
    let Some(listing) =
        crate::pipewire::pw_metadata_within(&["-n", "default", "--", &id_s], budget)
    else {
        return;
    };
    for key in our_targets(&listing, id, written) {
        let _ = crate::pipewire::pw_metadata_within(
            &["-n", "default", "-d", "--", &id_s, &key],
            budget,
        );
    }
}

/// Delete the routing targets that name the sink, gone or going (node
/// `id`, serial `serial`), by number (see `Speakers::sink_numbers`), as
/// `targets` has them. Where WirePlumber goes by the serial (0.4.13 on,
/// [`hold_allowed`]) a mixer's id + serial pair stays: the serial is never
/// given again, and the pair keeps WirePlumber's saved pick of HushMic
/// Speakers for the app. A bare id goes everywhere.
fn forget_numbers(targets: &[(u32, String, String)], id: u32, serial: Option<u64>) {
    let stale = graph::targets_naming(targets, id, serial, hold_allowed());
    for (subject, key, value) in &stale {
        clear_our_targets(*subject, &[(key.clone(), value.clone())]);
    }
    if !stale.is_empty() {
        eprintln!(
            "[hushmic] {} stream(s) named HushMic Speakers by its node number: that target is \
             gone with it (the number goes to another node next)",
            stale
                .iter()
                .map(|t| t.0)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        );
    }
}

/// The streams (by serial) whose send-back metadata goes now: each one
/// on the sink again (only a stream that stays there without it is sent
/// back, see [`holds_without_keys`]), and with `at_stop` every one. Pure.
pub fn release_landed(g: &Graph, at_stop: bool) -> Vec<u64> {
    g.out_streams
        .iter()
        .filter(|s| at_stop || s.on_sink())
        .map(|s| s.serial)
        .collect()
}

/// Whether a stream sent back to HushMic Speakers stays there once
/// HushMic's metadata is gone: its own target names the sink (by name, or
/// by the node id it has now), or the system default is HushMic Speakers.
/// One whose own target is a number of a sink that is gone could only be
/// held there by metadata HushMic keeps, and WirePlumber would save that
/// as the pick of every stream with its key. Pure.
pub fn holds_without_keys(s: &graph::OutStream, g: &Graph) -> bool {
    s.targets_sink(g.node_id(SINK)) || g.default_is_ours()
}

/// A [`send_back_plan`]: (node ids to send back, each with whether HushMic
/// holds it there; serials left where they are).
pub type SendBack = (Vec<(u32, bool)>, Vec<u64>);

/// The streams of [`to_reattach`] split: (node ids to send back, each with
/// whether HushMic must hold it there with its keys; serials left where
/// they are). A stream that stays without keys ([`holds_without_keys`]) is
/// sent back and let go once there. One that does not (its own target is a
/// number of the sink that is gone, or a mixer moved it in by numbers) is
/// sent back and held when `hold` (this WirePlumber saves nothing for keys
/// written name last, see [`hold_allowed`]), and left where it is
/// otherwise. Pure.
pub fn send_back_plan(carried: &[u64], g: &Graph, fallback: &[&str], hold: bool) -> SendBack {
    let ids = to_reattach(carried, g, fallback);
    let mut back = Vec::new();
    let mut stays = Vec::new();
    for s in g.out_streams.iter().filter(|s| ids.contains(&s.id)) {
        if holds_without_keys(s, g) {
            back.push((s.id, false));
        } else if hold {
            back.push((s.id, true));
        } else {
            stays.push(s.serial);
        }
    }
    (back, stays)
}

/// What a look does for the streams of a restarted chain (`carried`, see
/// [`send_back_plan`]): None to wait for a later look, while the new sink is
/// not up or the snapshot is not `current` (asked for before the chain
/// started: it shows the old sink with the apps still on it, and taken as
/// it is it would bring nothing back, A127). Pure.
pub fn reattach_plan(
    carried: &[u64],
    g: &Graph,
    fallback: &[&str],
    hold: bool,
    current: bool,
) -> Option<SendBack> {
    (current && g.sink_up).then(|| send_back_plan(carried, g, fallback, hold))
}

/// Whether HushMic may hold streams on its sink with kept keys: only where
/// WirePlumber saves nothing for keys written name last (0.4.13 and newer,
/// known from `wireplumber --version`); on 0.4.8, or when it cannot be
/// told, it would save the sink as the pick of every stream of the role.
pub fn hold_allowed() -> bool {
    crate::pipewire::name_last_unsaved()
}

/// The app streams that carry HushMic's target (`target.object` =
/// `hushmic_sink`, the name only HushMic writes; mixers write numbers)
/// without HushMic knowing them: left by a HushMic that did not stop
/// cleanly. (node id, serial, the keys and values found.) One that asks
/// for an output feeding the microphone is left out. Pure.
pub fn orphan_targets(g: &Graph, known: &[u64]) -> Vec<(u32, u64, Written)> {
    g.out_streams
        .iter()
        .filter(|s| !known.contains(&s.serial))
        .filter(|s| g.target_of(s.id, "target.object") == Some(SINK))
        .filter(|s| !s.own_target.as_deref().is_some_and(|t| g.names_feeder(t)))
        .map(|s| {
            let written = ["target.node", "target.object"]
                .iter()
                .filter_map(|k| g.target_of(s.id, k).map(|v| (k.to_string(), v.to_string())))
                .collect();
            (s.id, s.serial, written)
        })
        .collect()
}

/// Add the stream `serial` (as last read, from `refs`) to the apps left
/// behind, once.
fn note_left(
    left: &mut Vec<crate::pipewire::Reader>,
    refs: &[crate::pipewire::Reader],
    serial: u64,
) {
    if left.iter().any(|r| r.serial == Some(serial)) {
        return;
    }
    if let Some(r) = refs.iter().find(|r| r.serial == Some(serial)) {
        left.push(r.clone());
    }
}

/// A name for an app left behind, as the status and the doctor say it.
fn left_name(r: &crate::pipewire::Reader) -> String {
    r.app_name
        .clone()
        .or_else(|| r.binary.clone())
        .unwrap_or_else(|| r.node_name.clone())
}

/// The streams that played into the old sink (`carried`, by serial) and
/// play elsewhere now: the node ids to send back to HushMic Speakers. The
/// session manager moved them to the default output while the sink was
/// gone, and an app that had picked HushMic Speakers itself (its own
/// `target.object`) is from then on left following the default
/// (PulseAudio's rule, which WirePlumber keeps). Only a stream on the
/// default output (`fallback`: the real default now, or the last one while
/// HushMic Speakers is the default) or on none comes back: one the user
/// moved to another output in the meantime stays there. A stream that asks
/// for an output feeding the microphone HushMic cleans is that
/// microphone's sound and never goes to HushMic Speakers, however it got
/// there before: the microphone would go silent.
pub fn to_reattach(carried: &[u64], g: &Graph, fallback: &[&str]) -> Vec<u32> {
    let on_sink: Vec<u64> = g.writers.iter().filter_map(|w| w.serial).collect();
    g.out_streams
        .iter()
        .filter(|s| carried.contains(&s.serial) && !on_sink.contains(&s.serial))
        .filter(|s| s.to.iter().all(|t| fallback.contains(&t.as_str())))
        .filter(|s| !s.own_target.as_deref().is_some_and(|t| g.names_feeder(t)))
        .map(|s| s.id)
        .collect()
}

/// HushMic's own players that play where they should not: (node id,
/// serial, where to: None = the system default). Where they should is
/// [`own_player_target`]: a real output while the default is HushMic
/// Speakers, else the default. One not linked yet is left alone, and so is
/// one already asked to go there (`asked`, by serial). Pure.
pub fn own_player_moves(
    g: &Graph,
    asked: &[(u64, Option<String>)],
) -> Vec<(u32, u64, Option<String>)> {
    let want = own_player_target(g);
    let real_default = g.real_default();
    g.own_players
        .iter()
        .filter(|p| {
            let right = match (&want, real_default) {
                _ if p.to.is_empty() => true,
                (Some(t), _) => p.to.iter().all(|x| x == t),
                (None, Some(d)) => p.to.iter().all(|x| x == d),
                (None, None) => !p.to.iter().any(|x| x == SINK),
            };
            !right && !asked.contains(&(p.serial, want.clone()))
        })
        .map(|p| (p.id, p.serial, want.clone()))
        .collect()
}

/// Whether the playback stream plays where a re-target sent it: on
/// `target`, or with none on the system default output. Not linked at all
/// (a passive stream with nothing playing, on old PipeWire) is not wrong.
pub fn retarget_landed(target: Option<&str>, g: &Graph) -> bool {
    if g.playing_to.is_empty() {
        return true;
    }
    match target.or_else(|| g.real_default()) {
        Some(t) => g.playing_to.iter().any(|p| p == t),
        None => true,
    }
}

/// How long a hold lasts without a renewal (the window renews it every
/// second while the button is held or latched).
pub const HOLD_LEASE: Duration = Duration::from_millis(2500);

/// A hold's lease: when it runs out unless renewed.
#[derive(Debug, Default)]
pub struct HoldLease {
    pub until: Option<Instant>,
}

impl HoldLease {
    pub fn renew(&mut self, now: Instant) {
        self.until = Some(now + HOLD_LEASE);
    }

    pub fn end(&mut self) {
        self.until = None;
    }

    /// Ran out by `now` (none held: not expired).
    pub fn expired(&self, now: Instant) -> bool {
        self.until.is_some_and(|t| now >= t)
    }
}

/// A notification about HushMic Speakers (its own slot: a microphone
/// notice never replaces it, and it never replaces one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tell {
    /// Several starts in a row failed.
    Failing,
    /// Running again after that.
    Recovered,
    /// Switched off (or HushMic turned off) while the failing one shows.
    Withdraw,
}

/// When to tell: once when it starts failing, once when it is back, and
/// nothing about restarts that work (an output change, a recovered sink).
#[derive(Debug, Default)]
pub struct Teller {
    told: bool,
}

impl Teller {
    /// `failing`: several starts in a row failed; `healthy`: the chain
    /// runs and its sink is in the graph.
    pub fn tell(&mut self, on: bool, failing: bool, healthy: bool) -> Option<Tell> {
        if !on {
            return std::mem::take(&mut self.told).then_some(Tell::Withdraw);
        }
        if failing {
            return (!std::mem::replace(&mut self.told, true)).then_some(Tell::Failing);
        }
        if healthy && self.told {
            self.told = false;
            return Some(Tell::Recovered);
        }
        None
    }
}

impl Speakers {
    pub fn new(paths: Paths) -> Speakers {
        Speakers {
            paths,
            running: None,
            runtime: Runtime::new(),
            graph: None,
            bypass: false,
            gagged: false,
            lease: HoldLease::default(),
            last_error: None,
            teller: Teller::default(),
            seen_outputs: Default::default(),
            last_writers: Vec::new(),
            last_paired: Vec::new(),
            last_writer_refs: Vec::new(),
            reattach: None,
            retarget_check: None,
            attached: Vec::new(),
            left: Vec::new(),
            adopted: false,
            sink_numbers: None,
            own_moves: Vec::new(),
            playback_written: None,
            last_look: None,
        }
    }

    /// One look at the graph (each `pw-dump` the main loop reads; None
    /// when PipeWire did not answer): start, follow the output, recover,
    /// or stop. The dump is taken as read now; see [`Speakers::look_asked`].
    pub fn look(&mut self, cfg: &Config, dump: Option<&str>) {
        let now = crate::diagnostics::engine_generation_of(Chain::Speakers);
        self.look_asked(cfg, dump, now);
    }

    /// [`Speakers::look`] with a dump asked for under the speakers chain
    /// generation `asked`. One asked before the running chain started (a
    /// restart HushMic made itself meanwhile: a setting, a stall, Play to
    /// falling back) still shows the old sink with the apps on it, and the
    /// metadata HushMic deleted at the stop: it brings nothing back, takes
    /// over no one and lists no one, or the apps the restart moved off
    /// would be taken for still there and never come back (A127). The next
    /// look, a second later while streams are to come back, decides.
    pub fn look_asked(&mut self, cfg: &Config, dump: Option<&str>, asked: u64) {
        let current = asked == crate::diagnostics::engine_generation_of(Chain::Speakers);
        if let Some(d) = dump {
            let g = Graph::read_for(d, cfg.mic.as_deref());
            // the sink gone or given new numbers: the old ones go (kept
            // past a stop, for targets written after it)
            match (g.node_id(SINK), self.sink_numbers) {
                (Some(id), Some((old, serial))) if old != id => {
                    forget_numbers(&g.targets, old, serial);
                    self.sink_numbers = Some((id, g.sink_serial));
                }
                (Some(id), _) => self.sink_numbers = Some((id, g.sink_serial)),
                (None, Some((id, serial))) => {
                    self.sink_numbers = None;
                    forget_numbers(&g.targets, id, serial);
                }
                (None, None) => {}
            }
            for (i, o) in g.sinks.iter().enumerate() {
                self.seen_outputs
                    .insert(o.name.clone(), (i, o.description.clone()));
            }
            self.graph = Some(g);
        }
        let alive = self.is_running();
        let graph = dump.and(self.graph.as_ref());
        let sp = cfg.speakers();
        let step = self.runtime.observe(&Facts {
            on: wanted_on(cfg),
            output: sp.output.as_deref(),
            graph,
            alive,
            now: Instant::now(),
        });
        self.carry_out(step, cfg);
        if dump.is_some() {
            self.follow_streams(cfg, current);
        }
        self.last_look = Some(Instant::now());
        self.tell(cfg);
    }

    /// When the owner should look again without waiting for its next tick:
    /// a retry due after a failure, streams to bring back to a restarted
    /// chain's sink (a look a second), a re-target to verify.
    pub fn wake_at(&self) -> Option<Instant> {
        let after_look = self.last_look.map(|t| t + Duration::from_secs(1));
        [
            self.runtime.next_attempt(),
            self.reattach.as_ref().and(after_look),
            self.retarget_check
                .as_ref()
                .map(|(_, since)| *since + RETARGET_GRACE),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// After a look: bring back the streams a restart moved off the sink,
    /// check a re-target landed, and note who plays into the sink.
    fn follow_streams(&mut self, cfg: &Config, current: bool) {
        self.guard_the_mic();
        let now = Instant::now();
        let Some(g) = self.graph.as_ref() else {
            return;
        };
        let running = self.running.is_some();
        if let Some((carried, since)) = &self.reattach {
            let fallback: Vec<&str> = [g.real_default(), self.runtime.last_real_default()]
                .into_iter()
                .flatten()
                .collect();
            if !running || now.saturating_duration_since(*since) > REATTACH_LIMIT {
                self.reattach = None;
            } else if let Some((ids, stays)) =
                reattach_plan(carried, g, &fallback, hold_allowed(), current)
            {
                let sink_id = g.node_id(SINK);
                for (id, held) in &ids {
                    let serial = g
                        .out_streams
                        .iter()
                        .find(|st| st.id == *id)
                        .map(|st| st.serial);
                    // moved in by a mixer: back the same way, so WirePlumber
                    // keeps the app's pick (only where it may be held)
                    let pair = match (*held, serial, sink_id, g.sink_serial) {
                        (true, Some(s), Some(n), Some(sn))
                            if self.last_paired.contains(&s) || carries_pair(g, *id) =>
                        {
                            Some(pair_calls(*id, n, sn))
                        }
                        _ => None,
                    };
                    let paired = pair.is_some();
                    let written = match pair {
                        Some(calls) => write_calls(calls),
                        None => move_stream(*id, Some(SINK), sink_id),
                    };
                    if let Some(written) = written {
                        if let Some(serial) = serial {
                            self.attached.push(Attached {
                                id: *id,
                                serial,
                                written,
                                at: now,
                                own: false,
                                held: *held,
                                landed: false,
                                pair: paired,
                            });
                        }
                    } else {
                        eprintln!("[hushmic] could not send stream {id} back to HushMic Speakers");
                    }
                }
                if !ids.is_empty() {
                    eprintln!(
                        "[hushmic] {} stream(s) sent back to HushMic Speakers",
                        ids.len()
                    );
                }
                for serial in &stays {
                    note_left(&mut self.left, &self.last_writer_refs, *serial);
                }
                if !stays.is_empty() {
                    eprintln!(
                        "[hushmic] {} stream(s) left on the default output: only a target \
                         kept for them could hold them on HushMic Speakers, and this \
                         WirePlumber would save it",
                        stays.len()
                    );
                }
                self.reattach = None;
            }
        }
        // A stream sent back that has ended: the metadata written for it
        // goes (PipeWire before 0.3.6x keeps it for the next node that
        // gets the id), but only what is still HushMic's own.
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.attached)
            .into_iter()
            .partition(|a| {
                !g.out_streams
                    .iter()
                    .chain(&g.own_players)
                    .any(|s| s.serial == a.serial)
            });
        self.attached = kept;
        for a in gone {
            clear_our_targets(a.id, &a.written);
        }
        // Targets of HushMic's on streams it does not know: a HushMic that
        // was killed held them. WirePlumber has moved them back by those
        // keys; they are HushMic's again (cleared on stop, as any other).
        if current && running && g.sink_up && !self.adopted {
            self.adopted = true;
            let known: Vec<u64> = self.attached.iter().map(|a| a.serial).collect();
            let hold = hold_allowed();
            for (id, serial, written) in orphan_targets(g, &known) {
                let held = hold
                    && g.out_streams
                        .iter()
                        .find(|s| s.serial == serial)
                        .is_some_and(|s| !holds_without_keys(s, g));
                eprintln!(
                    "[hushmic] stream {id} carries a target of an earlier HushMic: taken over"
                );
                self.attached.push(Attached {
                    id,
                    serial,
                    written,
                    at: now,
                    own: false,
                    held,
                    landed: false,
                    pair: false,
                });
            }
        }
        // A stream sent back has HushMic's metadata deleted as soon as it
        // is on the sink again if it stays there without it, and after
        // LAND_LIMIT if it never gets there. One HushMic holds keeps it
        // while HushMic runs: written name last, this WirePlumber saves
        // nothing for it (see `hold_allowed`).
        // A held stream off the sink after it got there was moved away on
        // purpose (a mixer's keys replaced HushMic's): it is let go.
        let release = release_landed(g, false);
        for a in self.attached.iter_mut() {
            a.landed |= release.contains(&a.serial);
        }
        let (done, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.attached)
            .into_iter()
            .partition(|a| {
                let on = release.contains(&a.serial);
                !a.own
                    && ((on && !a.held)
                        || (!on && a.landed)
                        || (!on && now.saturating_duration_since(a.at) >= LAND_LIMIT))
            });
        self.attached = kept;
        for a in done {
            clear_our_targets(a.id, &a.written);
            if !a.landed {
                eprintln!(
                    "[hushmic] stream {} did not land on HushMic Speakers; left where it is",
                    a.id
                );
                note_left(&mut self.left, &self.last_writer_refs, a.serial);
            }
        }
        // the notice lasts while the app plays elsewhere
        self.left.retain(|r| {
            g.out_streams
                .iter()
                .any(|s| Some(s.serial) == r.serial && !s.on_sink())
        });
        // HushMic's own players pick their output when they start; one
        // that started before the default changed is moved: off HushMic
        // Speakers (it would be cleaned a second time) when that became
        // the default, back to the default when it no longer is.
        self.own_moves
            .retain(|(serial, _)| g.own_players.iter().any(|p| p.serial == *serial));
        for (id, serial, want) in own_player_moves(g, &self.own_moves) {
            let target_id = want.as_deref().and_then(|t| g.node_id(t));
            match move_stream(id, want.as_deref(), target_id) {
                Some(written) => {
                    self.attached.push(Attached {
                        id,
                        serial,
                        written,
                        at: now,
                        own: true,
                        held: false,
                        landed: false,
                        pair: false,
                    });
                    eprintln!(
                        "[hushmic] own player {id} moved to {}",
                        want.as_deref().unwrap_or("the system default output")
                    );
                }
                None => eprintln!("[hushmic] could not move own player {id}"),
            }
            // only the latest ask counts
            self.own_moves.retain(|(s, _)| *s != serial);
            self.own_moves.push((serial, want));
        }
        // while streams are being brought back, the old list stands
        if current && self.reattach.is_none() && running && g.sink_up {
            self.last_writers = g.writers.iter().filter_map(|w| w.serial).collect();
            self.last_paired = mixer_pairs(g);
            self.last_writer_refs = g.writers.clone();
        }
        if let Some((target, since)) = self.retarget_check.clone() {
            if !running {
                self.retarget_check = None;
            } else if now.saturating_duration_since(since) >= RETARGET_GRACE {
                self.retarget_check = None;
                if !retarget_landed(target.as_deref(), g) {
                    eprintln!(
                        "[hushmic] the speakers output did not follow the re-target; restarting the chain"
                    );
                    self.carry_out(
                        Step::Restart {
                            target,
                            why: runtime::Why::Settings,
                        },
                        cfg,
                    );
                }
            }
        }
    }

    /// The settings changed (the switch, the output, the strength, HushMic
    /// turned on or off): the smallest change that brings the chain there.
    pub fn apply(&mut self, cfg: &Config) {
        self.apply_settings(cfg);
        self.tell(cfg);
    }

    fn apply_settings(&mut self, cfg: &Config) {
        let sp = cfg.speakers();
        let on = wanted_on(cfg);
        if on && self.running.is_none() && self.graph.is_none() {
            // The first start (at launch, or the switch turned on before
            // any look): one bounded read, so it starts on the right
            // output instead of restarting onto it at the first look.
            self.graph = crate::pipewire::pw_dump_within(crate::pipewire::CALL_BUDGET)
                .map(|d| Graph::read_for(&d, cfg.mic.as_deref()));
            let history = crate::pipewire::wp_history("default.configured.audio.sink");
            let present = |n: &&String| self.graph.as_ref().is_none_or(|g| g.has_sink(n));
            self.runtime.seed_last_real_default(
                history.iter().filter(|n| *n != SINK).find(present).cloned(),
            );
        }
        let step = self
            .runtime
            .reconfigure(on, sp.output.as_deref(), self.graph.as_ref());
        if step != Step::Nothing {
            self.carry_out(step, cfg);
            return;
        }
        if !on {
            return;
        }
        let Some(r) = &self.running else {
            return;
        };
        let wanted = Rendered {
            target: r.rendered.target.clone(),
            attn: crate::pipewire::attn_sanitized(sp.attn_limit),
            inference: cfg.inference,
        };
        match Delta::of(&r.rendered, &wanted) {
            Delta::Unchanged => {}
            Delta::Attn(db) => {
                if !self.write_attn(db) {
                    self.carry_out(
                        Step::Restart {
                            target: wanted.target,
                            why: runtime::Why::Settings,
                        },
                        cfg,
                    );
                }
            }
            Delta::Restart => self.carry_out(
                Step::Restart {
                    target: wanted.target,
                    why: runtime::Why::Settings,
                },
                cfg,
            ),
        }
    }

    /// Stop the chain (quit; also what `apply` does when it is switched off).
    pub fn stop(&mut self) {
        if self.running.is_some() {
            self.stop_chain();
            eprintln!("[hushmic] speakers chain stopped");
        }
        self.runtime.stopped();
        self.bypass = false;
    }

    /// "Hold to hear the original": pass the input through while `on`.
    /// A hold is a lease: each `on` starts or renews it, and it ends by
    /// itself when no renewal came for [`HOLD_LEASE`] ([`Speakers::expire_hold`]),
    /// so a window that died or stopped asking cannot leave the original
    /// playing. False when no chain runs or it has no control file.
    pub fn set_bypass(&mut self, on: bool) -> bool {
        let now = Instant::now();
        let Some(c) = self.running.as_ref().and_then(|r| r.control.as_ref()) else {
            self.bypass = false;
            self.lease.end();
            return false;
        };
        if on && self.bypass {
            // a renewal: the original keeps playing
            self.lease.renew(now);
            return true;
        }
        // while it is muted for the microphone's sake the hold is kept
        // and applies once that ends
        let ok = self.gagged || c.writer.set_mode(if on { 1 } else { 0 });
        self.bypass = on && ok;
        if self.bypass {
            self.lease.renew(now);
        } else {
            self.lease.end();
        }
        ok
    }

    /// With nothing else left, the session manager may link the playback
    /// into an output that feeds the microphone HushMic cleans (a virtual
    /// microphone's sink): the cleaned far side would go into the call.
    /// The chain is muted through its control file while it is linked
    /// there, and gets its mode back once it plays elsewhere.
    fn guard_the_mic(&mut self) {
        let Some(g) = self.graph.as_ref() else {
            return;
        };
        let into_mic = g.plays_into_mic();
        if into_mic == self.gagged {
            return;
        }
        let Some(c) = self.running.as_ref().and_then(|r| r.control.as_ref()) else {
            return;
        };
        let mode = if into_mic {
            2
        } else if self.bypass {
            1
        } else {
            0
        };
        if c.writer.set_mode(mode) {
            self.gagged = into_mic;
            if into_mic {
                eprintln!(
                    "[hushmic] speakers: muted, its only output ({}) feeds the microphone",
                    g.playing_to.join(", ")
                );
            } else {
                eprintln!("[hushmic] speakers: playing again");
            }
        }
    }

    /// The chain is muted because its only output feeds the microphone.
    pub fn muted_for_mic(&self) -> bool {
        self.gagged && self.running.is_some()
    }

    /// End a hold whose lease ran out. True when it ended one now.
    pub fn expire_hold(&mut self, now: Instant) -> bool {
        if !self.bypass || !self.lease.expired(now) {
            return false;
        }
        self.set_bypass(false);
        eprintln!("[hushmic] speakers hold ended: not renewed");
        true
    }

    /// When the running hold's lease runs out.
    pub fn hold_until(&self) -> Option<Instant> {
        self.lease.until.filter(|_| self.bypass)
    }

    /// "Try again": a chain that should run but does not starts now
    /// instead of at the next backoff step. False when none is down.
    pub fn retry(&mut self, cfg: &Config) -> bool {
        if !wanted_on(cfg) || self.is_running() {
            return false;
        }
        self.runtime.retry_now();
        let wanted = runtime::wanted_target(
            cfg.speakers().output.as_deref(),
            self.graph.as_ref(),
            self.runtime.last_real_default(),
        );
        self.carry_out(Step::Start(wanted), cfg);
        self.tell(cfg);
        true
    }

    /// The Speakers page's part of the window's snapshot.
    pub fn app_state(
        &mut self,
        cfg: &Config,
        names: &crate::apps::Names,
    ) -> crate::appstate::SpeakersState {
        use crate::appstate::{OutputDevice, Phase, SpeakersState, Strength};
        use crate::diagnostics::{engine_generation_of, engine_tier_of};
        let sp = cfg.speakers();
        let on = wanted_on(cfg);
        let running = self.is_running();
        let g = self.graph.as_ref();
        let phase = if !on {
            Phase::Off
        } else if !running {
            if self.runtime.failures() > 0 || self.last_error.is_some() {
                Phase::Down
            } else {
                Phase::Starting
            }
        } else if g.is_some_and(|g| g.sink_up && g.playback_up) {
            Phase::Ready
        } else if self.runtime.seen_up() {
            Phase::Reconnecting
        } else {
            Phase::Starting
        };
        let is_default = g.is_some_and(Graph::user_default_is_ours);
        let system_default = if is_default {
            self.runtime.last_real_default().map(str::to_string)
        } else {
            g.and_then(Graph::real_default).map(str::to_string)
        };
        let playing_on = if running {
            match g {
                // into the microphone's own sink only: it plays nowhere
                Some(g) if g.plays_into_mic() => g.playing_on().map(str::to_string),
                _ => g
                    .and_then(|g| g.playing_to.first().cloned())
                    .or_else(|| self.rendered().and_then(|r| r.target.clone()))
                    .or_else(|| system_default.clone()),
            }
        } else {
            None
        };
        let last_default = self.runtime.last_real_default();
        let mut outputs: Vec<OutputDevice> = g
            .map(|g| {
                g.offered(sp.output.as_deref(), last_default)
                    .into_iter()
                    .map(|o| OutputDevice {
                        name: o.name,
                        description: o.description,
                        present: true,
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(chosen) = sp.output.as_deref().filter(|o| *o != SINK) {
            if !outputs.iter().any(|o| o.name == chosen) {
                let (at, description) = match self.seen_outputs.get(chosen) {
                    Some((i, d)) => ((*i).min(outputs.len()), d.clone()),
                    None => (outputs.len(), chosen.to_string()),
                };
                outputs.insert(
                    at,
                    OutputDevice {
                        name: chosen.to_string(),
                        description,
                        present: false,
                    },
                );
            }
        }
        let playing = match g.filter(|_| running) {
            Some(g) => crate::apps::playing(&g.writers, &crate::apps::is_ours, names),
            None => Vec::new(),
        };
        let mut left: Vec<crate::appstate::AppRef> = Vec::new();
        for r in self.left.iter().filter(|_| on) {
            let a = crate::apps::app_ref(r, names);
            if !left.iter().any(|l| l.id == a.id) {
                left.push(a);
            }
        }
        SpeakersState {
            enabled: sp.enabled,
            phase,
            failing: on && self.runtime.failing(),
            error: self.last_error.clone().filter(|_| on),
            output: sp.output.clone(),
            playing_on,
            system_default,
            outputs,
            strength: Strength::from_attn(sp.attn_limit),
            attn_limit: sp.attn_limit,
            held: self.bypassed(),
            is_default,
            playing,
            left,
            tier: engine_tier_of(Chain::Speakers).filter(|_| phase == Phase::Ready),
            generation: engine_generation_of(Chain::Speakers),
        }
    }

    /// Whether the bypass is held.
    pub fn bypassed(&self) -> bool {
        self.bypass && self.running.is_some()
    }

    /// The chain process is alive (reaps it on exit).
    pub fn is_running(&mut self) -> bool {
        match self.running.as_mut() {
            Some(r) => matches!(r.child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// What the running chain was started with.
    pub fn rendered(&self) -> Option<&Rendered> {
        self.running.as_ref().map(|r| &r.rendered)
    }

    /// What `status` reports (see [`Status`]).
    pub fn status(&mut self, cfg: &Config) -> Status {
        use crate::diagnostics::EngineTier;
        let sp = cfg.speakers();
        let running = self.is_running();
        let g = self.graph.as_ref();
        let output_active = g
            .filter(|_| running)
            .and_then(|g| g.playing_on().map(str::to_string));
        let engine = running
            .then(|| crate::diagnostics::engine_tier_of(Chain::Speakers))
            .flatten();
        let live_model = match engine {
            Some(EngineTier::Passthrough) => None,
            Some(EngineTier::Light) => Some(crate::controller::LIGHT_MODEL),
            _ => Some(MODEL),
        };
        let (failures, failing) = self.failures();
        Status {
            enabled: sp.enabled,
            on: wanted_on(cfg),
            running,
            output_configured: sp.output.clone(),
            output_target: self.rendered().and_then(|r| r.target.clone()),
            output_active_name: output_active
                .as_deref()
                .and_then(|a| g.and_then(|g| g.description_of(a)))
                .map(str::to_string),
            output_active,
            system_default: g.is_some_and(Graph::user_default_is_ours),
            attn_limit: self
                .rendered()
                .map(|r| r.attn)
                .unwrap_or_else(|| crate::pipewire::attn_sanitized(sp.attn_limit)),
            model: MODEL,
            engine,
            inference: live_model
                .filter(|_| running)
                .and_then(|m| crate::diagnostics::inference_for_of(Chain::Speakers, m)),
            node_present: g.map(|g| g.sink_up),
            node_state: g.and_then(|g| g.sink_state.clone()),
            playing: g
                .filter(|_| running)
                .map(|g| g.fed_by.clone())
                .unwrap_or_default(),
            bypass: self.bypassed(),
            failures,
            failing,
            error: self.last_error.clone(),
            left: if wanted_on(cfg) {
                let mut names: Vec<String> = self.left.iter().map(left_name).collect();
                names.dedup();
                names
            } else {
                Vec::new()
            },
        }
    }

    /// The chain stepped down for want of CPU while something plays
    /// through it: files wait then, as for the microphone.
    pub fn stepped_down(&mut self) -> bool {
        use crate::diagnostics::EngineTier;
        self.is_running()
            && self
                .graph
                .as_ref()
                .is_some_and(|g| g.sink_state.as_deref() == Some("running"))
            && matches!(
                crate::diagnostics::engine_tier_of(Chain::Speakers),
                Some(EngineTier::Light | EngineTier::Passthrough)
            )
    }

    /// The running chain's engine meter file.
    pub fn meter_path(&self) -> Option<&Path> {
        self.running.as_ref().and_then(|r| r.meter.as_deref())
    }

    /// The running chain's engine generation.
    pub fn generation(&self) -> Option<u64> {
        self.running.as_ref().map(|r| r.generation)
    }

    /// The last snapshot looked at.
    pub fn graph(&self) -> Option<&Graph> {
        self.graph.as_ref()
    }

    /// Failed starts in a row, and whether that counts as failing.
    pub fn failures(&self) -> (u32, bool) {
        (self.runtime.failures(), self.runtime.failing())
    }

    /// Why the last start failed (None after one that worked).
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// The last real system default output seen.
    pub fn last_real_default(&self) -> Option<&str> {
        self.runtime.last_real_default()
    }

    /// Notify on the edges [`Teller`] picks.
    fn tell(&mut self, cfg: &Config) {
        let healthy = self.running.is_some() && self.graph.as_ref().is_some_and(|g| g.sink_up);
        let tell = self
            .teller
            .tell(wanted_on(cfg), self.runtime.failing(), healthy);
        let slot = crate::notify::Slot::Speakers;
        match tell {
            None => {}
            Some(Tell::Failing) => {
                let body = match &self.last_error {
                    Some(e) => e.clone(),
                    None => crate::tr!("notify-speakers-failed-body"),
                };
                crate::notify::send(
                    slot,
                    "dialog-error",
                    &crate::tr!("notify-speakers-failed-summary"),
                    &body,
                );
            }
            Some(Tell::Recovered) => crate::notify::send_transient(
                slot,
                "audio-volume-high",
                &crate::tr!("notify-speakers-running-again-summary"),
                &crate::tr!("notify-speakers-running-again-body"),
            ),
            Some(Tell::Withdraw) => crate::notify::close(slot),
        }
    }

    fn carry_out(&mut self, step: Step, cfg: &Config) {
        match step {
            Step::Nothing => {}
            Step::Stop => {
                self.stop_chain();
                self.bypass = false;
                // Switched off (or HushMic off): the apps go where the
                // session manager sends them meanwhile. The ones that
                // played into the sink are kept, so a start brings back
                // those still on the default output.
                self.reattach = None;
                eprintln!("[hushmic] speakers chain stopped");
            }
            Step::Start(target) => self.start(target, cfg),
            Step::Restart { target, why } => {
                if why == runtime::Why::Output && self.retarget(target.clone()) {
                    return;
                }
                if why == runtime::Why::Output {
                    eprintln!("[hushmic] speakers chain: output changed");
                }
                self.stop_chain();
                self.start(target, cfg);
            }
        }
    }

    /// Move the running chain's playback stream to `target` (None: the
    /// system default output) without restarting it, so its sink, and every
    /// app playing into it, stays. False when it could not be asked.
    fn retarget(&mut self, target: Option<String>) -> bool {
        let Some(id) = self.graph.as_ref().and_then(|g| g.playback_id) else {
            return false;
        };
        if self.running.is_none() {
            return false;
        }
        let target_id = target
            .as_deref()
            .and_then(|t| self.graph.as_ref().and_then(|g| g.node_id(t)));
        let Some(written) = move_stream(id, target.as_deref(), target_id) else {
            return false;
        };
        self.playback_written = Some((id, written));
        match &target {
            Some(t) => eprintln!("[hushmic] speakers output changed: playing to {t}"),
            None => {
                eprintln!("[hushmic] speakers output changed: following the system default output")
            }
        }
        if let Some(r) = self.running.as_mut() {
            r.rendered.target = target.clone();
        }
        self.runtime.retargeted(target.clone());
        self.retarget_check = Some((target, Instant::now()));
        true
    }

    fn start(&mut self, target: Option<String>, cfg: &Config) {
        // the streams the last chain had come back onto this one's sink
        self.retarget_check = None;
        // the old chain's playback node is gone with it
        if let Some(g) = self.graph.as_mut() {
            g.playback_id = None;
        }
        if !self.last_writers.is_empty() {
            self.reattach = Some((self.last_writers.clone(), Instant::now()));
        }
        let attn = crate::pipewire::attn_sanitized(cfg.speakers().attn_limit);
        let result = self.spawn(target.as_deref(), attn, cfg.inference);
        let ok = result.is_ok();
        match result {
            Ok(r) => {
                match &target {
                    Some(t) => eprintln!("[hushmic] speakers chain started, playing to {t}"),
                    None => eprintln!(
                        "[hushmic] speakers chain started, following the system default output"
                    ),
                }
                self.running = Some(r);
                self.adopted = false;
                self.last_error = None;
            }
            Err(e) => {
                eprintln!("[hushmic] the speakers chain could not start: {e}");
                self.last_error = Some(e.to_string());
            }
        }
        self.runtime.started(target, ok, Instant::now());
    }

    fn spawn(
        &mut self,
        target: Option<&str>,
        attn: f32,
        inference: Inference,
    ) -> std::io::Result<Running> {
        let model = self.paths.model_dir.join(format!("{MODEL}.onnx"));
        for (what, p) in [
            ("LADSPA plugin", &self.paths.plugin_so),
            ("model file", &model),
            ("ONNX Runtime library", &self.paths.dylib),
        ] {
            if !p.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("{what} not found at {}", p.display()),
                ));
            }
        }
        let text = conf::render(
            &conf::ConfSpec {
                target,
                attn,
                legacy_node_target: !crate::pipewire::supports_target_object(),
                report_latency: crate::pipewire::supports_latency_report(),
            },
            &self.paths,
        );
        let path = conf_path();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&path, text)?;
        let spawned = spawn_chain(
            &self.paths,
            &SpawnSpec {
                chain: Chain::Speakers,
                conf: &path,
                model: &model,
                model_id: MODEL,
                inference,
                mode: if self.bypass { 1 } else { 0 },
                attn_db: attn,
                env: &[("HUSHMIC_WORKER_RT_BELOW", RT_BELOW)],
            },
        )?;
        Ok(Running {
            child: spawned.child,
            tee: spawned.tee,
            meter: spawned.meter,
            control: spawned.control,
            generation: spawned.generation,
            rendered: Rendered {
                target: target.map(str::to_string),
                attn,
                inference,
            },
        })
    }

    fn write_attn(&mut self, db: f32) -> bool {
        let Some(r) = self.running.as_mut() else {
            return false;
        };
        let Some(c) = &r.control else {
            return false;
        };
        if !c.writer.set_attn_db(db) {
            return false;
        }
        r.rendered.attn = db;
        eprintln!(
            "[hushmic] speakers strength set to {} dB",
            crate::pipewire::attn_value(db)
        );
        true
    }

    /// Stop and reap the chain (SIGTERM first, see
    /// [`crate::controller::terminate`]), remove its files, retire its
    /// generation.
    fn stop_chain(&mut self) {
        // a hold ends with the chain it was held on
        self.bypass = false;
        self.gagged = false;
        self.lease.end();
        let Some(mut r) = self.running.take() else {
            return;
        };
        // every target HushMic wrote goes before the sink does: the ones
        // sent back not looked at since, and its own players' moves; but a
        // mixer's id + serial pair HushMic put back stays, as the mixer's
        // own does (see `forget_numbers`)
        for a in std::mem::take(&mut self.attached) {
            if !a.pair {
                clear_our_targets(a.id, &a.written);
            }
        }
        // and a mixer's, naming the sink by number: they would point at
        // whatever node gets its id next. Read now, not from the last look;
        // the numbers stay for the next look, for one written meanwhile.
        if let Some((id, serial)) = self.sink_numbers {
            if let Some(listing) = crate::pipewire::pw_metadata_within(
                &["-n", "default"],
                crate::pipewire::CALL_BUDGET,
            ) {
                forget_numbers(&graph::parse_metadata_targets(&listing), id, serial);
            }
        }
        self.own_moves.clear();
        crate::controller::stop_process(&mut r.child, "speakers chain");
        // its playback stream's re-target dies with it
        if let Some((id, written)) = self.playback_written.take() {
            clear_our_targets(id, &written);
        }
        if let Some(m) = r.meter.take() {
            crate::meter::remove(&m);
        }
        if let Some(c) = r.control.take() {
            c.writer.unlink_if_at(&c.path);
        }
        // A tee that outlives the wait below reports under a retired
        // generation, which nobody reads.
        crate::diagnostics::new_engine_generation_of(Chain::Speakers);
        if let Some(t) = r.tee.take() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !t.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
    }
}

impl Drop for Speakers {
    fn drop(&mut self) {
        self.stop_chain();
    }
}
