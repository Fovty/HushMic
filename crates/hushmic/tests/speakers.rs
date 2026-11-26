//! The speakers cleaner, offline: its conf, what it reads from the graph,
//! and its decisions (start, follow the output, the self-default case,
//! recovery, stop).
#![cfg(feature = "speakers")]

use hushmic::config::{Config, Inference};
use hushmic::controller::Paths;
use hushmic::speakers::conf::{render, ConfSpec};
use hushmic::speakers::graph::{
    own_player_target, parse_consumers, parse_default_sink, parse_metadata_targets, targets_naming,
    Graph,
};
use hushmic::speakers::runtime::{wanted_target, Facts, Runtime, Step, Why, SETTLE, START_GRACE};
use hushmic::speakers::{Delta, Rendered, MODEL, SINK};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn paths() -> Paths {
    Paths {
        plugin_so: PathBuf::from("/usr/lib/ladspa/libdpdfnet_ladspa.so"),
        model_dir: PathBuf::from("/usr/share/hushmic/models"),
        dylib: PathBuf::from("/usr/lib/hushmic/libonnxruntime.so"),
    }
}

fn spec(target: Option<&str>) -> ConfSpec<'_> {
    ConfSpec {
        target,
        attn: 100.0,
        legacy_node_target: false,
        report_latency: false,
    }
}

// --- the conf ----------------------------------------------------------------

#[test]
fn the_conf_matches_its_golden_file() {
    let got = render(&spec(None), &paths());
    let file =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/speakers-chain-follow.conf");
    // HUSHMIC_BLESS=1 rewrites the fixture: only on purpose, and review it.
    if std::env::var_os("HUSHMIC_BLESS").is_some() {
        std::fs::write(&file, &got).unwrap();
    }
    let want = std::fs::read_to_string(&file).unwrap();
    assert_eq!(got, want, "regenerate the fixture only on purpose");
}

#[test]
fn the_conf_is_a_selectable_output_with_its_own_group() {
    let c = render(&spec(Some("alsa_output.usb-dac")), &paths());
    assert!(c.contains("node.name          = \"hushmic_sink\""), "{c}");
    assert!(c.contains("media.class        = Audio/Sink"), "{c}");
    assert!(
        c.contains("node.description   = \"HushMic Speakers\""),
        "{c}"
    );
    assert!(
        c.contains("node.name           = \"hushmic_playback\""),
        "{c}"
    );
    // its own link group on both streams, never the microphone's
    assert_eq!(c.matches("node.link-group").count(), 2, "{c}");
    assert_eq!(c.matches("\"hushmic-speakers\"").count(), 2, "{c}");
    assert!(!c.contains("link-group     = \"hushmic\""), "{c}");
    // a mixer's mute on the internal stream is not restored; the sink's
    // own volume is the user's and is
    assert_eq!(c.matches("state.restore-props = false").count(), 1, "{c}");
    let playback = &c[c.find("playback.props").unwrap()..];
    assert!(playback.contains("state.restore-props = false"));
    assert!(playback.contains("media.name          = \"HushMic Speakers output\""));
    // no application.name (WirePlumber 0.4 key collision)
    assert!(!c.contains("application.name"), "{c}");
    // mono, pinned quantum, passive output, the plugin starts suppressing
    assert_eq!(c.matches("audio.channels").count(), 2, "{c}");
    assert!(c.contains("node.force-quantum = 480"), "{c}");
    assert!(c.contains("node.passive        = true"), "{c}");
    assert!(c.contains("\"Mode\" = 0"), "{c}");
    assert!(
        c.contains("target.object  = \"alsa_output.usb-dac\""),
        "{c}"
    );
    assert!(!c.contains("node.target"), "{c}");
    // the sink is never told to be the default
    assert!(!c.contains("priority.session"), "{c}");
}

#[test]
fn the_conf_follows_the_default_without_a_target() {
    let c = render(&spec(None), &paths());
    assert!(!c.contains("target.object"), "{c}");
    assert!(!c.contains("node.target"), "{c}");
}

#[test]
fn old_pipewire_gets_the_legacy_target_and_new_the_latency() {
    let c = render(
        &ConfSpec {
            legacy_node_target: true,
            ..spec(Some("dac"))
        },
        &paths(),
    );
    assert!(c.contains("node.target     = \"dac\""), "{c}");
    assert!(!c.contains("hushmic_speakers_latency"), "{c}");
    let c = render(
        &ConfSpec {
            report_latency: true,
            ..spec(None)
        },
        &paths(),
    );
    assert!(c.contains("name   = hushmic_speakers_latency"), "{c}");
    assert!(c.contains("\"latency\" = 0.08"), "{c}");
    assert!(c.contains("\"Delay (s)\" = 0.0"), "{c}");
    assert!(
        c.contains(
            r#"{ output = "hushmic_speakers_dsp:Output" input = "hushmic_speakers_latency:In" }"#
        ),
        "{c}"
    );
}

#[test]
fn the_conf_escapes_and_clamps() {
    let c = render(
        &ConfSpec {
            attn: f32::NAN,
            ..spec(Some("we\"ird\\name"))
        },
        &paths(),
    );
    assert!(c.contains(r#"target.object  = "we\"ird\\name""#), "{c}");
    assert!(c.contains("\"Attenuation Limit (dB)\" = 100"), "{c}");
    let c = render(
        &ConfSpec {
            attn: 12.0,
            ..spec(None)
        },
        &paths(),
    );
    assert!(c.contains("\"Attenuation Limit (dB)\" = 12 "), "{c}");
}

#[test]
fn the_model_is_the_quality_default() {
    assert_eq!(MODEL, Config::default().model);
}

// --- the graph ---------------------------------------------------------------

/// A trimmed `pw-dump` for the output side: a real USB DAC sink, the
/// laptop speakers, our virtual `hushmic_sink` (quantum-pinned), the DAC's
/// monitor SOURCE, a real mic, the chain's `hushmic_playback` stream
/// feeding the DAC (two links, FL/FR), and an app stream (Discord) feeding
/// hushmic_sink. The `default` metadata carries the sink keys next to the
/// source keys.
const SINK_DUMP: &str = r#"[
  { "id": 100, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Sink",
      "node.name": "alsa_output.usb-dac", "node.description": "USB DAC" } } },
  { "id": 101, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Source",
      "node.name": "alsa_output.usb-dac.monitor", "node.description": "Monitor of USB DAC" } } },
  { "id": 102, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Source",
      "node.name": "alsa_input.usb-mic", "node.description": "USB Mic" } } },
  { "id": 103, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Sink",
      "node.name": "alsa_output.pci-speakers", "node.nick": "Speakers" } } },
  { "id": 110, "type": "PipeWire:Interface:Node",
    "info": { "state": "running", "props": { "media.class": "Audio/Sink",
      "node.name": "hushmic_sink", "node.description": "HushMic Speakers",
      "node.force-quantum": 480 } } },
  { "id": 120, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Stream/Output/Audio",
      "node.name": "hushmic_playback" } } },
  { "id": 130, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Stream/Output/Audio",
      "node.name": "Discord" } } },
  { "id": 400, "type": "PipeWire:Interface:Link",
    "info": { "output-node-id": 120, "input-node-id": 100 } },
  { "id": 401, "type": "PipeWire:Interface:Link",
    "info": { "output-node-id": 120, "input-node-id": 100 } },
  { "id": 402, "type": "PipeWire:Interface:Link",
    "info": { "output-node-id": 130, "input-node-id": 110 } },
  { "type": "PipeWire:Interface:Metadata", "id": 30,
    "props": { "metadata.name": "default" },
    "metadata": [
      { "subject": 0, "key": "default.configured.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "alsa_output.usb-dac" } },
      { "subject": 0, "key": "default.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "alsa_output.usb-dac" } },
      { "subject": 0, "key": "default.configured.audio.source", "type": "Spa:String:JSON",
        "value": { "name": "stale_configured" } },
      { "subject": 0, "key": "default.audio.source", "type": "Spa:String:JSON",
        "value": { "name": "hushmic_source" } }
    ] }
]"#;

/// Sink keys disagree: the persisted preference is stale, the session
/// manager links to the DAC.
const SINK_META_SPLIT: &str = r#"[
  { "type": "PipeWire:Interface:Metadata", "id": 30,
    "props": { "metadata.name": "default" },
    "metadata": [
      { "subject": 0, "key": "default.configured.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "stale_configured_sink" } },
      { "subject": 0, "key": "default.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "alsa_output.usb-dac" } }
    ] }
]"#;

const SINK_META_CONFIGURED_ONLY: &str = r#"[
  { "type": "PipeWire:Interface:Metadata", "id": 30,
    "props": { "metadata.name": "default" },
    "metadata": [
      { "subject": 0, "key": "default.configured.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "alsa_output.usb-dac" } }
    ] }
]"#;

/// SINK_DUMP with HushMic Speakers made the system default.
fn self_default_dump() -> String {
    SINK_DUMP
        .replace(
            r#""key": "default.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "alsa_output.usb-dac" }"#,
            r#""key": "default.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "hushmic_sink" }"#,
        )
        .replace(
            r#""key": "default.configured.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "alsa_output.usb-dac" }"#,
            r#""key": "default.configured.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "hushmic_sink" }"#,
        )
}

/// The self default while HushMic Speakers is down: the configured key
/// still names it, the session manager links to a real output meanwhile.
fn self_default_sink_down() -> Graph {
    let mut g = Graph::read(&self_default_dump());
    g.default_effective = Some("alsa_output.pci-speakers".into());
    g.sink_up = false;
    g.playback_up = false;
    g
}

#[test]
fn the_graph_lists_real_outputs_only() {
    let g = Graph::read(SINK_DUMP);
    let names: Vec<_> = g.sinks.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["alsa_output.usb-dac", "alsa_output.pci-speakers"]);
    assert_eq!(g.description_of("alsa_output.usb-dac"), Some("USB DAC"));
    assert_eq!(
        g.description_of("alsa_output.pci-speakers"),
        Some("Speakers")
    );
    assert!(g.sink_up && g.playback_up);
    assert_eq!(g.playing_to, ["alsa_output.usb-dac"]);
    assert_eq!(g.fed_by, ["Discord"]);
    assert_eq!(g.sink_state.as_deref(), Some("running"));
    assert_eq!(g.sink_quantum, Some(480));
    assert_eq!(g.default_sink(), Some("alsa_output.usb-dac"));
    assert_eq!(g.real_default(), Some("alsa_output.usb-dac"));
    assert!(!g.default_is_ours());
    // garbage is an empty graph, never a panic
    assert_eq!(Graph::read("not json"), Graph::default());
    assert_eq!(Graph::read("[]"), Graph::default());
}

#[test]
fn the_default_output_prefers_the_effective_key() {
    let s = |v: &str| Some(v.to_string());
    assert_eq!(
        parse_default_sink(SINK_META_SPLIT),
        (s("alsa_output.usb-dac"), s("stale_configured_sink"))
    );
    assert_eq!(
        parse_default_sink(SINK_META_CONFIGURED_ONLY),
        (None, s("alsa_output.usb-dac"))
    );
    assert_eq!(parse_default_sink("[]"), (None, None));
    // the source keys never leak in
    let g = Graph::read(SINK_META_SPLIT);
    assert_eq!(g.default_sink(), Some("alsa_output.usb-dac"));
    let g = Graph::read(SINK_META_CONFIGURED_ONLY);
    assert_eq!(g.default_sink(), Some("alsa_output.usb-dac"));
}

#[test]
fn consumers_are_the_mirror_of_feeders() {
    assert_eq!(
        parse_consumers(SINK_DUMP, "hushmic_playback"),
        ["alsa_output.usb-dac"]
    );
    assert!(parse_consumers(SINK_DUMP, "hushmic_sink").is_empty());
    assert_eq!(parse_consumers(SINK_DUMP, "Discord"), ["hushmic_sink"]);
    assert!(parse_consumers("not json", "hushmic_playback").is_empty());
}

#[test]
fn own_players_target_a_real_output_only_while_the_default_is_ours() {
    // the default is a real output: players play to the default as before
    assert_eq!(own_player_target(&Graph::read(SINK_DUMP)), None);
    // the default is HushMic Speakers: where the chain plays, else the
    // first real output
    let mut g = Graph::read(&self_default_dump());
    assert!(g.default_is_ours());
    assert_eq!(
        own_player_target(&g).as_deref(),
        Some("alsa_output.usb-dac")
    );
    g.playing_to.clear();
    assert_eq!(
        own_player_target(&g).as_deref(),
        Some("alsa_output.usb-dac")
    );
    g.sinks.remove(0);
    assert_eq!(
        own_player_target(&g).as_deref(),
        Some("alsa_output.pci-speakers")
    );
    // a configured default of ours that the session manager did not take
    // (the chain is off) is not ours
    let configured_only = SINK_META_SPLIT.replace("stale_configured_sink", SINK);
    assert_eq!(own_player_target(&Graph::read(&configured_only)), None);
}

#[test]
fn own_players_start_on_a_named_output_while_speakers_runs() {
    use hushmic::speakers::graph::own_player_pick;
    // HushMic Speakers runs, the default is a real output: that output,
    // named, so no target saved for the player's media.role sends it on
    let g = Graph::read(SINK_DUMP);
    assert!(g.sink_up && !g.default_is_ours());
    assert_eq!(own_player_pick(&g).as_deref(), Some("alsa_output.usb-dac"));
    // the default is HushMic Speakers: a real output, as before
    let ours = Graph::read(&self_default_dump());
    assert_eq!(own_player_pick(&ours), own_player_target(&ours));
    assert_eq!(
        own_player_pick(&ours).as_deref(),
        Some("alsa_output.usb-dac")
    );
    // no HushMic Speakers: the default, as any player
    let mut off = g.clone();
    off.sink_up = false;
    assert_eq!(own_player_pick(&off), None);
    // a default that is gone names nothing
    let mut gone = g.clone();
    gone.sinks.retain(|s| s.name != "alsa_output.usb-dac");
    assert_eq!(own_player_pick(&gone), None);
}

// --- which output ------------------------------------------------------------

#[test]
fn the_chosen_output_is_pinned_and_the_default_followed() {
    let g = Graph::read(SINK_DUMP);
    let s = |v: &str| Some(v.to_string());
    assert_eq!(
        wanted_target(Some("alsa_output.pci-speakers"), Some(&g), None),
        s("alsa_output.pci-speakers")
    );
    assert_eq!(wanted_target(None, Some(&g), None), None);
    // unplugged: the pin stays (the session manager plays on the default
    // meanwhile and returns to it)
    assert_eq!(wanted_target(Some("gone"), Some(&g), None), s("gone"));
    // no snapshot: the saved output stands
    assert_eq!(wanted_target(Some("x"), None, None), s("x"));
    assert_eq!(wanted_target(None, None, None), None);
    // the sink itself is never a target
    assert_eq!(wanted_target(Some(SINK), Some(&g), None), None);
}

#[test]
fn with_hushmic_speakers_as_the_default_the_chain_plays_to_the_last_real_one() {
    let g = Graph::read(&self_default_dump());
    let s = |v: &str| Some(v.to_string());
    assert_eq!(
        wanted_target(None, Some(&g), Some("alsa_output.pci-speakers")),
        s("alsa_output.pci-speakers")
    );
    // the last real default is gone too: the first real output
    assert_eq!(
        wanted_target(None, Some(&g), Some("gone")),
        s("alsa_output.usb-dac")
    );
    assert_eq!(
        wanted_target(None, Some(&g), None),
        s("alsa_output.usb-dac")
    );
    // a chosen output that is there stays chosen
    assert_eq!(
        wanted_target(
            Some("alsa_output.pci-speakers"),
            Some(&g),
            Some("alsa_output.usb-dac")
        ),
        s("alsa_output.pci-speakers")
    );
    // a chosen output that is gone would fall back into the sink: the
    // last real default instead
    assert_eq!(
        wanted_target(Some("gone"), Some(&g), Some("alsa_output.pci-speakers")),
        s("alsa_output.pci-speakers")
    );
    // no real output at all: nothing to pin
    let mut empty = g.clone();
    empty.sinks.clear();
    assert_eq!(wanted_target(None, Some(&empty), None), None);
}

// --- decisions ---------------------------------------------------------------

fn facts<'a>(
    on: bool,
    output: Option<&'a str>,
    graph: Option<&'a Graph>,
    alive: bool,
    now: Instant,
) -> Facts<'a> {
    Facts {
        on,
        output,
        graph,
        alive,
        now,
    }
}

#[test]
fn it_starts_when_on_and_stops_when_off() {
    let t0 = Instant::now();
    let g = Graph::read(SINK_DUMP);
    let mut rt = Runtime::new();
    assert_eq!(
        rt.observe(&facts(false, None, Some(&g), false, t0)),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(true, None, Some(&g), false, t0)),
        Step::Start(None)
    );
    rt.started(None, true, t0);
    assert_eq!(rt.running_target(), Some(None));
    assert_eq!(
        rt.observe(&facts(true, None, Some(&g), true, t0)),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(false, None, Some(&g), true, t0)),
        Step::Stop
    );
    assert_eq!(rt.running_target(), None);
    assert_eq!(
        rt.observe(&facts(false, None, Some(&g), false, t0)),
        Step::Nothing
    );
}

#[test]
fn a_chain_that_dies_is_started_again_after_a_backoff() {
    let t0 = Instant::now();
    let g = Graph::read(SINK_DUMP);
    let mut rt = Runtime::new();
    rt.started(None, true, t0);
    let at = |s: u64| t0 + Duration::from_secs(s);
    // it died: the first retry waits a moment
    assert_eq!(
        rt.observe(&facts(true, None, Some(&g), false, at(10))),
        Step::Nothing
    );
    assert_eq!(rt.failures(), 1);
    assert_eq!(
        rt.observe(&facts(true, None, Some(&g), false, at(11))),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(true, None, Some(&g), false, at(12))),
        Step::Start(None)
    );
    // a start that fails waits longer
    rt.started(None, false, at(12));
    assert_eq!(rt.failures(), 2);
    assert_eq!(
        rt.observe(&facts(true, None, Some(&g), false, at(15))),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(true, None, Some(&g), false, at(16))),
        Step::Start(None)
    );
    rt.started(None, false, at(16));
    assert!(rt.failing());
    // a working chain whose sink shows up clears the count
    rt.started(None, true, at(100));
    assert_eq!(
        rt.observe(&facts(true, None, Some(&g), true, at(101))),
        Step::Nothing
    );
    assert_eq!(rt.failures(), 0);
    assert!(!rt.failing());
    // a deliberate change starts over without waiting
    rt.started(None, false, at(200));
    assert_eq!(rt.reconfigure(true, None, Some(&g)), Step::Start(None));
}

#[test]
fn a_sink_that_never_appears_or_leaves_is_restarted() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let up = Graph::read(SINK_DUMP);
    let mut down = up.clone();
    down.sink_up = false;
    down.playback_up = false;
    let mut rt = Runtime::new();
    rt.started(None, true, t0);
    // within the start grace a missing sink is only starting
    assert_eq!(
        rt.observe(&facts(true, None, Some(&down), true, at(1000))),
        Step::Nothing
    );
    let grace = START_GRACE.as_millis() as u64;
    assert_eq!(
        rt.observe(&facts(true, None, Some(&down), true, at(grace + 100))),
        Step::Nothing
    );
    // missing for long enough after the grace: a failed start, restarted
    assert_eq!(
        rt.observe(&facts(true, None, Some(&down), true, at(grace + 3200))),
        Step::Restart {
            target: None,
            why: Why::SinkGone
        }
    );
    assert_eq!(rt.failures(), 1);
    // this one comes up, then PipeWire restarts under it: no failure
    rt.started(None, true, at(20_000));
    assert_eq!(
        rt.observe(&facts(true, None, Some(&up), true, at(21_000))),
        Step::Nothing
    );
    assert_eq!(rt.failures(), 0);
    assert_eq!(
        rt.observe(&facts(true, None, Some(&down), true, at(40_000))),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(true, None, Some(&down), true, at(43_100))),
        Step::Restart {
            target: None,
            why: Why::SinkGone
        }
    );
    assert_eq!(rt.failures(), 0);
    // a snapshot that failed says nothing about the sink
    rt.started(None, true, at(50_000));
    assert_eq!(
        rt.observe(&facts(true, None, None, true, at(70_000))),
        Step::Nothing
    );
}

#[test]
fn the_self_default_moves_the_chain_to_the_last_real_default_once_settled() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let normal = Graph::read(SINK_DUMP);
    let ours = Graph::read(&self_default_dump());
    let mut rt = Runtime::new();
    assert_eq!(
        rt.observe(&facts(true, None, Some(&normal), false, t0)),
        Step::Start(None)
    );
    rt.started(None, true, t0);
    assert_eq!(rt.last_real_default(), Some("alsa_output.usb-dac"));
    assert_eq!(
        rt.observe(&facts(true, None, Some(&normal), true, at(1000))),
        Step::Nothing
    );
    // the user makes HushMic Speakers the system output
    assert_eq!(
        rt.observe(&facts(true, None, Some(&ours), true, at(2000))),
        Step::Nothing
    );
    let settle = SETTLE.as_millis() as u64;
    assert_eq!(
        rt.observe(&facts(true, None, Some(&ours), true, at(2000 + settle))),
        Step::Restart {
            target: Some("alsa_output.usb-dac".into()),
            why: Why::Output
        }
    );
    rt.started(Some("alsa_output.usb-dac".into()), true, at(4000));
    // the default stays ours: the pin holds, nothing more
    assert_eq!(
        rt.observe(&facts(true, None, Some(&ours), true, at(5000))),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(true, None, Some(&ours), true, at(9000))),
        Step::Nothing
    );
    // the user picks a real default again: back to following it
    assert_eq!(
        rt.observe(&facts(true, None, Some(&normal), true, at(10_000))),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(true, None, Some(&normal), true, at(10_000 + settle))),
        Step::Restart {
            target: None,
            why: Why::Output
        }
    );
}

#[test]
fn a_chain_restarted_under_the_self_default_comes_back_on_the_real_default() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let normal = Graph::read(SINK_DUMP);
    let down = self_default_sink_down();
    let mut rt = Runtime::new();
    rt.observe(&facts(true, None, Some(&normal), false, t0));
    rt.started(None, true, t0);
    rt.started(Some("alsa_output.usb-dac".into()), true, at(5));
    // the chain dies; the session manager's fallback while the sink is
    // down is neither followed nor remembered as the user's default
    assert_eq!(
        rt.observe(&facts(true, None, Some(&down), false, at(10))),
        Step::Nothing
    );
    assert_eq!(rt.last_real_default(), Some("alsa_output.usb-dac"));
    assert_eq!(
        rt.observe(&facts(true, None, Some(&down), false, at(12))),
        Step::Start(Some("alsa_output.usb-dac".into()))
    );
}

#[test]
fn a_flicker_shorter_than_the_settle_time_restarts_nothing() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let normal = Graph::read(SINK_DUMP);
    let ours = Graph::read(&self_default_dump());
    let mut rt = Runtime::new();
    rt.started(None, true, t0);
    assert_eq!(
        rt.observe(&facts(true, None, Some(&normal), true, at(500))),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(true, None, Some(&ours), true, at(1000))),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(true, None, Some(&normal), true, at(1400))),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(true, None, Some(&ours), true, at(3000))),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(true, None, Some(&normal), true, at(9000))),
        Step::Nothing
    );
}

#[test]
fn an_unplugged_output_keeps_the_chain_unless_that_would_loop() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let dac = Some("alsa_output.usb-dac");
    let mut unplugged = Graph::read(SINK_DUMP);
    unplugged.sinks.remove(0);
    unplugged.default_effective = Some("alsa_output.pci-speakers".into());
    let mut rt = Runtime::new();
    assert_eq!(
        rt.observe(&facts(true, dac, Some(&Graph::read(SINK_DUMP)), false, t0)),
        Step::Start(dac.map(str::to_string))
    );
    rt.started(dac.map(str::to_string), true, t0);
    // unplugged, default real: no restart (the session manager plays on
    // the default and comes back)
    for ms in [1000, 3000, 9000] {
        assert_eq!(
            rt.observe(&facts(true, dac, Some(&unplugged), true, at(ms))),
            Step::Nothing
        );
    }
    // unplugged while HushMic Speakers is the default: the chain moves to
    // the last real default rather than loop
    let mut looping = unplugged.clone();
    looping.default_effective = Some(SINK.into());
    looping.default_configured = Some(SINK.into());
    assert_eq!(
        rt.observe(&facts(true, dac, Some(&looping), true, at(10_000))),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(true, dac, Some(&looping), true, at(12_000))),
        Step::Restart {
            target: Some("alsa_output.pci-speakers".into()),
            why: Why::Output
        }
    );
    rt.started(Some("alsa_output.pci-speakers".into()), true, at(12_000));
    // plugged back in: back to the chosen output
    let back = Graph::read(&self_default_dump());
    assert_eq!(
        rt.observe(&facts(true, dac, Some(&back), true, at(20_000))),
        Step::Nothing
    );
    assert_eq!(
        rt.observe(&facts(true, dac, Some(&back), true, at(22_000))),
        Step::Restart {
            target: dac.map(str::to_string),
            why: Why::Output
        }
    );
}

#[test]
fn a_settings_change_applies_at_once() {
    let g = Graph::read(SINK_DUMP);
    let mut rt = Runtime::new();
    let t0 = Instant::now();
    assert_eq!(rt.reconfigure(true, None, Some(&g)), Step::Start(None));
    rt.started(None, true, t0);
    assert_eq!(rt.reconfigure(true, None, Some(&g)), Step::Nothing);
    assert_eq!(
        rt.reconfigure(true, Some("alsa_output.pci-speakers"), Some(&g)),
        Step::Restart {
            target: Some("alsa_output.pci-speakers".into()),
            why: Why::Output
        }
    );
    rt.started(Some("alsa_output.pci-speakers".into()), true, t0);
    assert_eq!(rt.reconfigure(false, None, Some(&g)), Step::Stop);
    assert_eq!(
        rt.reconfigure(false, None, Some(&g)),
        Step::Nothing,
        "nothing runs"
    );
}

#[test]
fn only_the_strength_is_live() {
    let r = Rendered {
        target: None,
        attn: 100.0,
        inference: Inference::Auto,
    };
    assert_eq!(Delta::of(&r, &r.clone()), Delta::Unchanged);
    assert_eq!(
        Delta::of(
            &r,
            &Rendered {
                attn: 12.0,
                ..r.clone()
            }
        ),
        Delta::Attn(12.0)
    );
    for other in [
        Rendered {
            target: Some("dac".into()),
            ..r.clone()
        },
        Rendered {
            inference: Inference::Onnx,
            ..r.clone()
        },
    ] {
        assert_eq!(Delta::of(&r, &other), Delta::Restart);
    }
}

#[test]
fn the_switch_needs_hushmic_on_too() {
    use hushmic::speakers::wanted_on;
    let mut cfg = Config::default();
    assert!(!wanted_on(&cfg), "off until switched on");
    cfg.speakers_mut().enabled = true;
    assert!(wanted_on(&cfg));
    cfg.enabled = false;
    assert!(!wanted_on(&cfg), "HushMic turned off stops both chains");
}

#[test]
fn a_run_that_starts_on_the_self_default_takes_the_last_real_one_from_history() {
    // WirePlumber's default-nodes keeps the outputs configured before
    let text = "[default-nodes]\n\
                default.configured.audio.sink=hushmic_sink\n\
                default.configured.audio.sink.0=alsa_output.pci-speakers\n\
                default.configured.audio.sink.1=alsa_output.usb-dac\n\
                default.configured.audio.source=alsa_input.usb-mic\n";
    assert_eq!(
        hushmic::pipewire::parse_wp_history(text, "default.configured.audio.sink"),
        [
            "hushmic_sink",
            "alsa_output.pci-speakers",
            "alsa_output.usb-dac"
        ]
    );
    // the source history is untouched by the sink keys
    assert_eq!(
        hushmic::pipewire::parse_wp_source_history(text),
        ["alsa_input.usb-mic"]
    );
    let g = Graph::read(&self_default_dump());
    let mut rt = Runtime::new();
    rt.seed_last_real_default(Some("alsa_output.pci-speakers".into()));
    assert_eq!(
        rt.observe(&facts(true, None, Some(&g), false, Instant::now())),
        Step::Start(Some("alsa_output.pci-speakers".into()))
    );
    // a seed never replaces what this run saw, nor names the sink itself
    rt.seed_last_real_default(Some("alsa_output.usb-dac".into()));
    assert_eq!(rt.last_real_default(), Some("alsa_output.pci-speakers"));
    let mut fresh = Runtime::new();
    fresh.seed_last_real_default(Some(SINK.into()));
    assert_eq!(fresh.last_real_default(), None);
}

/// Against a live session (a private one): `HUSHMIC_LIVE_PLAYER_TARGET`
/// names the output HushMic's own players must target now, or `-` for
/// none. Run by hand with `--ignored`.
#[test]
#[ignore = "needs a live PipeWire session"]
fn own_players_target_live() {
    let want = std::env::var("HUSHMIC_LIVE_PLAYER_TARGET").expect("set the expected target");
    let got = hushmic::speakers::graph::own_player_target_now();
    assert_eq!(got.as_deref().unwrap_or("-"), want);
}

#[test]
fn the_speakers_command_says_its_state_output_and_strength() {
    use hushmic::speakers::{output_warning, render_outputs, render_show};
    let mut cfg = Config::default();
    assert_eq!(
        render_show(&cfg, None),
        "speakers: off\noutput: default (the system output)\nstrength: maximum (100 dB)\n"
    );
    cfg.speakers_mut().enabled = true;
    cfg.speakers_mut().attn_limit = 12.0;
    cfg.speakers_mut().output = Some("alsa_output.usb-dac".into());
    assert_eq!(
        render_show(&cfg, None),
        "speakers: on (applies when HushMic starts)\noutput: alsa_output.usb-dac\nstrength: medium (12 dB)\n"
    );
    cfg.speakers_mut().attn_limit = 17.5;
    assert!(render_show(&cfg, None).ends_with("strength: 17.5 dB\n"));
    let g = Graph::read(SINK_DUMP);
    let out = render_outputs(&g.sinks, Some("alsa_output.usb-dac"), false);
    assert_eq!(
        out,
        "* alsa_output.usb-dac\tUSB DAC\n  alsa_output.pci-speakers\tSpeakers\n"
    );
    let v: serde_json::Value = serde_json::from_str(&render_outputs(&g.sinks, None, true)).unwrap();
    assert_eq!(v[0]["name"], "alsa_output.usb-dac");
    assert_eq!(v[0]["selected"], false);
    assert_eq!(render_outputs(&[], None, false), "(no outputs found)\n");
    assert!(output_warning("gone", Some(&g)).is_some());
    assert!(output_warning("alsa_output.usb-dac", Some(&g)).is_none());
    assert!(output_warning("default", Some(&g)).is_none());
    assert!(output_warning("gone", None).is_none());
}

// --- the doctor ----------------------------------------------------------------

fn healthy_report() -> hushmic::speakers::doctor::Report {
    hushmic::speakers::doctor::Report {
        enabled: true,
        output: None,
        attn_limit: 100.0,
        hushmic_on: true,
        graph: Some(Graph::read(SINK_DUMP)),
        latency_reported: Some(3840),
        latency_supported: true,
        controls: vec![],
        failures: 0,
        error: None,
        left: vec![],
        worker_rt: Some(Ok("9 (data loop RR 20, 1 below)".into())),
        engine_cost: Some(("quality".into(), 0.12)),
        inference: vec![],
        log_tail: Some("[dpdfnet-ladspa] engine: quality (cost 0.12)\n".into()),
    }
}

#[test]
fn an_app_a_restart_left_behind_is_named_once() {
    use hushmic::speakers::doctor::render;
    let mut r = healthy_report();
    r.left = vec!["Firefox".into()];
    let lines = render(&r, true);
    let want =
        "  Firefox left HushMic Speakers when it restarted. Pick HushMic Speakers for it again.";
    assert_eq!(
        problems(&lines)
            .iter()
            .filter(|p| p.as_str() == want)
            .count(),
        1,
        "{}",
        text(&lines)
    );
    // read back from the running instance's status
    let status = r#"{"speakers": {"failures": 0, "error": null, "left": ["Firefox"]}}"#;
    let cfg = Config::default();
    let got = hushmic::speakers::doctor::collect(&cfg, None, Some(status));
    assert_eq!(got.left, ["Firefox"]);
}

fn problems(lines: &[(hushmic::speakers::doctor::Kind, String)]) -> Vec<String> {
    lines
        .iter()
        .filter(|(k, _)| *k == hushmic::speakers::doctor::Kind::Problem)
        .map(|(_, s)| s.clone())
        .collect()
}

fn text(lines: &[(hushmic::speakers::doctor::Kind, String)]) -> String {
    lines.iter().map(|(_, s)| format!("{s}\n")).collect()
}

#[test]
fn the_doctor_says_off_in_one_line() {
    use hushmic::speakers::doctor::{render, Report};
    let lines = render(&Report::default(), true);
    assert_eq!(text(&lines), "speakers: off\n");
    assert!(problems(&lines).is_empty());
}

#[test]
fn a_healthy_speakers_chain_is_facts_only() {
    use hushmic::speakers::doctor::render;
    let lines = render(&healthy_report(), true);
    let t = text(&lines);
    assert!(problems(&lines).is_empty(), "{t}");
    for has in [
        "speakers: on\n",
        "  output: default (the system output)\n",
        "  strength: 100 dB\n",
        "  outputs: 2 (USB DAC, Speakers)\n",
        "  default output: alsa_output.usb-dac\n",
        "  hushmic_sink present: yes\n",
        "  plays to: alsa_output.usb-dac\n",
        "  playing through it: Discord\n",
        "  state: running (the output it plays to runs at 480 samples per quantum meanwhile)\n",
        "  quantum pin: 480\n",
        "  reported to PipeWire: yes (3840)\n",
        "  engine worker: realtime priority 9 (data loop RR 20, 1 below)\n",
        "  engine: quality (cost 0.12)\n",
        "  speakers log (tail):\n    [dpdfnet-ladspa] engine: quality (cost 0.12)\n",
    ] {
        assert!(t.contains(has), "missing {has:?} in\n{t}");
    }
    assert!(!t.contains("own players"), "{t}");
}

#[test]
fn the_doctor_flags_a_missing_sink_a_lost_output_and_a_muted_stream() {
    use hushmic::speakers::doctor::render;
    let mut r = healthy_report();
    r.graph.as_mut().unwrap().sink_up = false;
    let lines = render(&r, true);
    assert_eq!(problems(&lines), ["  hushmic_sink present: no"]);
    // not judged while no HushMic runs, or HushMic is off
    assert!(problems(&render(&r, false)).is_empty());
    r.hushmic_on = false;
    let lines = render(&r, true);
    assert!(problems(&lines).is_empty());
    assert!(text(&lines).starts_with("speakers: on (stopped while HushMic is off)\n"));

    let mut r = healthy_report();
    r.graph.as_mut().unwrap().playing_to.clear();
    assert!(problems(&render(&r, true))[0].starts_with("  plays to: nothing"));

    let mut r = healthy_report();
    r.controls = vec![hushmic::pipewire::StreamControls {
        name: "hushmic_playback".into(),
        muted: true,
        volume: 1.0,
    }];
    assert_eq!(
        problems(&render(&r, true)),
        ["  hushmic_playback is muted in a mixer: HushMic Speakers is silent"]
    );

    let mut r = healthy_report();
    r.error = Some("model file not found at /x".into());
    assert_eq!(
        problems(&render(&r, true)),
        ["  could not start: model file not found at /x"]
    );
}

#[test]
fn the_doctor_judges_the_pin_and_the_latency_like_the_microphones() {
    use hushmic::speakers::doctor::render;
    let mut r = healthy_report();
    r.graph.as_mut().unwrap().sink_quantum = Some(1024);
    r.latency_reported = Some(2880);
    let p = problems(&render(&r, true));
    assert_eq!(p.len(), 2, "{p:?}");
    assert!(p[0].contains("quantum pin: 1024"));
    assert!(p[1].contains("mismatch (2880, declared 3840)"));
    // playing, yet nothing declared: the declaration is not in effect
    let mut r = healthy_report();
    assert_eq!(
        r.graph.as_ref().unwrap().sink_state.as_deref(),
        Some("running")
    );
    for nothing in [None, Some(0)] {
        r.latency_reported = nothing;
        let lines = render(&r, true);
        assert_eq!(
            problems(&lines),
            ["  reported to PipeWire: missing (declaration not in effect)"],
            "{}",
            text(&lines)
        );
        assert_eq!(hushmic::diagnostics::exit_code(problems(&lines).len()), 1);
        // with HushMic off or not running nothing is judged
        assert!(problems(&render(&r, false)).is_empty());
    }
    // nothing played through it yet: not declared yet, a fact
    r.graph.as_mut().unwrap().sink_state = Some("suspended".into());
    r.latency_reported = None;
    let lines = render(&r, true);
    assert!(problems(&lines).is_empty());
    assert!(text(&lines).contains("  reported to PipeWire: not yet"));
    // an old PipeWire cannot declare it: a fact, even while it plays
    r.graph.as_mut().unwrap().sink_state = Some("running".into());
    r.latency_supported = false;
    assert!(text(&render(&r, true)).contains("  reported to PipeWire: no (PipeWire 1.6+ required"));
}

#[test]
fn the_doctor_counts_no_problem_while_nothing_plays_through_the_speakers() {
    // No app plays: the sink is suspended and nothing is declared yet.
    // The whole report (mic + speakers) stays clean and exits 0.
    use hushmic::speakers::doctor::render;
    for st in ["idle", "suspended"] {
        let mut r = healthy_report();
        r.graph.as_mut().unwrap().sink_state = Some(st.into());
        r.graph.as_mut().unwrap().fed_by.clear();
        r.latency_reported = None;
        let lines = render(&r, true);
        assert!(problems(&lines).is_empty(), "{}", text(&lines));
        assert!(
            text(&lines).contains(
                "  reported to PipeWire: not yet (declared once sound plays through it)\n"
            ),
            "{}",
            text(&lines)
        );
        assert_eq!(hushmic::diagnostics::exit_code(problems(&lines).len()), 0);
    }
}

#[test]
fn the_doctor_explains_the_self_default_and_an_unplugged_output() {
    use hushmic::speakers::doctor::render;
    let mut r = healthy_report();
    r.graph = Some(Graph::read(&self_default_dump()));
    let t = text(&render(&r, true));
    assert!(
        t.contains("  default output: HushMic Speakers (everything heard is cleaned; it plays to the last real default)\n"),
        "{t}"
    );
    assert!(
        t.contains("  HushMic's own players play to: alsa_output.usb-dac\n"),
        "{t}"
    );
    // a chosen output unplugged: with HushMic Speakers as the system
    // output, the stand-in is the last real default
    r.output = Some("alsa_output.headset".into());
    let t = text(&render(&r, true));
    assert!(
        t.contains("  output: alsa_output.headset (not plugged in: it plays to the last real default meanwhile)\n"),
        "{t}"
    );
    assert!(t.contains("it plays to the last real default)\n"), "{t}");
    // a chosen output that is there: that is where it plays, not the
    // last real default
    r.output = Some("alsa_output.usb-dac".into());
    let t = text(&render(&r, true));
    assert!(
        t.contains("  output: USB DAC (alsa_output.usb-dac)\n"),
        "{t}"
    );
    assert!(
        t.contains("  default output: HushMic Speakers (everything heard is cleaned; it plays to the chosen output)\n"),
        "{t}"
    );
    assert!(!t.contains("last real default"), "{t}");
    // with a real system output, unplugged means the system output
    let mut r = healthy_report();
    r.output = Some("alsa_output.headset".into());
    let t = text(&render(&r, true));
    assert!(
        t.contains("  output: alsa_output.headset (not plugged in: it plays to the system output meanwhile)\n"),
        "{t}"
    );
}

// --- the notification --------------------------------------------------------------

#[test]
fn it_tells_once_when_failing_and_once_when_back() {
    use hushmic::speakers::{Tell, Teller};
    let mut t = Teller::default();
    // restarts that work say nothing
    assert_eq!(t.tell(true, false, false), None);
    assert_eq!(t.tell(true, false, true), None);
    // several failed starts: once
    assert_eq!(t.tell(true, true, false), Some(Tell::Failing));
    assert_eq!(t.tell(true, true, false), None);
    // a start that runs but whose sink is not seen yet: not back yet
    assert_eq!(t.tell(true, false, false), None);
    assert_eq!(t.tell(true, false, true), Some(Tell::Recovered));
    assert_eq!(t.tell(true, false, true), None);
    // switched off while the failing one shows: withdrawn, quietly
    assert_eq!(t.tell(true, true, false), Some(Tell::Failing));
    assert_eq!(t.tell(false, true, false), Some(Tell::Withdraw));
    assert_eq!(t.tell(false, false, false), None);
    assert_eq!(t.tell(true, false, true), None);
}

// --- the Speakers page's snapshot ---------------------------------------------

/// SINK_DUMP with Discord playing (its stream running).
fn discord_playing() -> String {
    SINK_DUMP.replace(
        r#""info": { "props": { "media.class": "Stream/Output/Audio",
      "node.name": "Discord" } } }"#,
        r#""info": { "state": "running", "props": { "media.class": "Stream/Output/Audio",
      "node.name": "Discord", "application.name": "Discord" } } }"#,
    )
}

fn speakers_cfg(enabled: bool, output: Option<&str>, attn_limit: f32) -> Config {
    Config {
        speakers: Some(hushmic::config::SpeakersConfig {
            enabled,
            output: output.map(str::to_string),
            attn_limit,
        }),
        ..Config::default()
    }
}

#[test]
fn the_apps_playing_through_it_are_running_output_streams_not_ours() {
    use hushmic::apps::{playing, Names};
    use hushmic::pipewire::parse_writers;
    let names = Names::with_dirs(vec![]);
    let w = parse_writers(&discord_playing(), SINK);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].node_name, "Discord");
    let apps = playing(&w, &|_| false, &names);
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].name.as_deref(), Some("Discord"));
    // ours, or corked: nobody
    assert!(playing(&w, &|_| true, &names).is_empty());
    assert!(playing(&parse_writers(SINK_DUMP, SINK), &|_| false, &names).is_empty());
    // what the chain itself plays is not a writer of the sink
    assert!(parse_writers(SINK_DUMP, "alsa_output.usb-dac")
        .iter()
        .all(|r| r.node_name == "hushmic_playback"));
}

#[test]
fn the_page_sees_the_outputs_the_default_and_the_saved_settings() {
    use hushmic::appstate::{Phase, Strength};
    let names = hushmic::apps::Names::with_dirs(vec![]);
    let mut sp = hushmic::speakers::Speakers::new(paths());
    let cfg = Config::default();
    sp.look(&cfg, Some(SINK_DUMP));
    let s = sp.app_state(&cfg, &names);
    assert!(!s.enabled);
    assert_eq!(s.phase, Phase::Off);
    let listed: Vec<(&str, &str, bool)> = s
        .outputs
        .iter()
        .map(|o| (o.name.as_str(), o.description.as_str(), o.present))
        .collect();
    assert_eq!(
        listed,
        [
            ("alsa_output.usb-dac", "USB DAC", true),
            ("alsa_output.pci-speakers", "Speakers", true)
        ]
    );
    assert_eq!(s.system_default.as_deref(), Some("alsa_output.usb-dac"));
    assert!(!s.is_default);
    assert_eq!(s.playing_on, None);
    assert_eq!((s.strength, s.attn_limit), (Strength::Maximum, 100.0));
    assert!(!s.held && !s.failing && s.error.is_none() && s.playing.is_empty());

    // a chosen output never seen: listed last, not plugged in
    let cfg = speakers_cfg(false, Some("alsa_output.headset"), 12.0);
    let s = sp.app_state(&cfg, &names);
    assert_eq!(s.strength, Strength::Medium);
    assert_eq!(s.output.as_deref(), Some("alsa_output.headset"));
    let last = s.outputs.last().unwrap();
    assert_eq!(
        (last.name.as_str(), last.description.as_str(), last.present),
        ("alsa_output.headset", "alsa_output.headset", false)
    );

    // one seen and then unplugged keeps its place and its name
    let cfg = speakers_cfg(false, Some("alsa_output.usb-dac"), 7.0);
    let unplugged = SINK_DUMP.replace(
        "\"node.name\": \"alsa_output.usb-dac\", \"node.description\": \"USB DAC\"",
        "\"node.name\": \"gone\"",
    );
    sp.look(&cfg, Some(&unplugged));
    let s = sp.app_state(&cfg, &names);
    assert_eq!(s.strength, Strength::Custom);
    let first = &s.outputs[0];
    assert_eq!(
        (
            first.name.as_str(),
            first.description.as_str(),
            first.present
        ),
        ("alsa_output.usb-dac", "USB DAC", false)
    );
}

#[test]
fn under_the_self_default_system_default_is_the_last_real_one() {
    let names = hushmic::apps::Names::with_dirs(vec![]);
    let mut sp = hushmic::speakers::Speakers::new(paths());
    let cfg = Config::default();
    sp.look(&cfg, Some(SINK_DUMP));
    sp.look(&cfg, Some(&self_default_dump()));
    let s = sp.app_state(&cfg, &names);
    assert!(s.is_default);
    assert_eq!(s.system_default.as_deref(), Some("alsa_output.usb-dac"));
    assert!(s.outputs.iter().all(|o| o.name != SINK));
}

#[test]
fn hold_and_retry_need_a_chain_that_is_wanted() {
    let mut sp = hushmic::speakers::Speakers::new(paths());
    // no chain: nothing to hold, and the hold is not remembered for later
    assert!(!sp.set_bypass(true));
    assert!(!sp.bypassed());
    // off: nothing to retry
    assert!(!sp.retry(&Config::default()));
}

#[test]
fn a_hold_is_a_lease_that_runs_out_without_renewal() {
    use hushmic::speakers::{HoldLease, HOLD_LEASE};
    let t0 = Instant::now();
    let mut l = HoldLease::default();
    // nothing held: nothing runs out
    assert!(!l.expired(t0 + Duration::from_secs(60)));
    l.renew(t0);
    assert!(!l.expired(t0 + HOLD_LEASE - Duration::from_millis(1)));
    assert!(l.expired(t0 + HOLD_LEASE));
    // the window renews every second: the hold lasts as long as it asks
    let mut t = t0;
    for _ in 0..10 {
        t += Duration::from_secs(1);
        assert!(!l.expired(t), "renewed a second ago");
        l.renew(t);
    }
    // it stops asking (it died, or the latch is gone): the hold ends
    assert!(!l.expired(t + Duration::from_secs(2)));
    assert!(l.expired(t + Duration::from_millis(2500)));
    // hold off: ended at once, nothing left to run out
    l.end();
    assert_eq!(l.until, None);
    assert!(!l.expired(t + Duration::from_secs(60)));
    // a hold on nothing has no lease and no wake
    let mut sp = hushmic::speakers::Speakers::new(paths());
    assert!(!sp.set_bypass(true));
    assert_eq!(sp.hold_until(), None);
    assert!(!sp.expire_hold(t0 + Duration::from_secs(60)));
}

// --- the apps stay on the sink ------------------------------------------------------

/// SINK_DUMP with serials, and Discord's stream linked into the sink or not.
fn with_serials(linked: bool) -> String {
    let d = SINK_DUMP.replace(
        r#""node.name": "Discord" }"#,
        r#""node.name": "Discord", "object.serial": 931 }"#,
    );
    let d = d.replace(
        r#""node.name": "hushmic_playback" }"#,
        r#""node.name": "hushmic_playback", "object.serial": 920 }"#,
    );
    if linked {
        d
    } else {
        // moved to the DAC while the sink was gone
        d.replace(
            r#""info": { "output-node-id": 130, "input-node-id": 110 }"#,
            r#""info": { "output-node-id": 130, "input-node-id": 100 }"#,
        )
    }
}

#[test]
fn an_output_change_moves_the_playback_stream_by_metadata() {
    use hushmic::speakers::retarget_calls;
    let call = |k: &str, v: &str, t: &str| -> Vec<String> {
        ["-n", "default", "--", "120", k, v, t]
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    // a name for current session managers, the id for WirePlumber 0.4.8;
    // the name last, so restore-stream saves no target for the stream's
    // media.role (it reads a target.object value as a serial)
    assert_eq!(
        retarget_calls(120, Some("alsa_output.usb-dac"), Some(100)),
        [
            call("target.node", "100", "Spa:Id"),
            call("target.object", "alsa_output.usb-dac", "Spa:String")
        ]
    );
    assert_eq!(
        retarget_calls(120, Some("alsa_output.usb-dac"), None),
        [call("target.object", "alsa_output.usb-dac", "Spa:String")]
    );
    // the system default: no target (and "-1" must not read as an option)
    assert_eq!(
        retarget_calls(120, None, None),
        [
            call("target.node", "-1", "Spa:Id"),
            call("target.object", "-1", "Spa:Id")
        ]
    );
    let g = Graph::read(SINK_DUMP);
    assert_eq!(g.playback_id, Some(120));
    assert_eq!(g.node_id("alsa_output.usb-dac"), Some(100));
    assert_eq!(g.node_id(SINK), Some(110));
    assert_eq!(g.node_id("alsa_output.usb-dac.monitor"), None);
}

#[test]
fn a_retarget_landed_where_the_playback_plays() {
    use hushmic::speakers::retarget_landed;
    let g = Graph::read(SINK_DUMP);
    assert!(retarget_landed(Some("alsa_output.usb-dac"), &g));
    assert!(!retarget_landed(Some("alsa_output.pci-speakers"), &g));
    // the default is the DAC
    assert!(retarget_landed(None, &g));
    // not linked at all (passive, nothing playing on old PipeWire): not wrong
    let mut idle = g.clone();
    idle.playing_to.clear();
    assert!(retarget_landed(Some("alsa_output.pci-speakers"), &idle));
}

#[test]
fn after_a_restart_the_streams_that_left_the_sink_are_sent_back() {
    use hushmic::speakers::graph::parse_out_streams;
    use hushmic::speakers::graph::OutStream;
    use hushmic::speakers::to_reattach;
    let dac = ["alsa_output.usb-dac"];
    // HushMic's own playback stream is never one of them; each says what
    // it plays into
    assert_eq!(
        parse_out_streams(&with_serials(true)),
        [OutStream {
            id: 130,
            serial: 931,
            to: vec![SINK.to_string()],
            own_target: None,
        }]
    );
    let on = Graph::read(&with_serials(true));
    assert_eq!(on.writers[0].serial, Some(931));
    // still on the sink: nothing to do
    assert!(to_reattach(&[931], &on, &dac).is_empty());
    // moved off it to the default output: back it goes, by its node id
    let off = Graph::read(&with_serials(false));
    assert_eq!(off.out_streams[0].to, dac);
    assert_eq!(to_reattach(&[931], &off, &dac), [130]);
    // a stream that never played into it stays where it is
    assert!(to_reattach(&[555], &off, &dac).is_empty());
    // moved by the user to another output meanwhile: it stays there too
    let elsewhere = Graph::read(&with_serials(true).replace(
        r#""info": { "output-node-id": 130, "input-node-id": 110 }"#,
        r#""info": { "output-node-id": 130, "input-node-id": 103 }"#,
    ));
    assert_eq!(elsewhere.out_streams[0].to, ["alsa_output.pci-speakers"]);
    assert!(to_reattach(&[931], &elsewhere, &dac).is_empty());
    // linked nowhere (paused, or between moves): it comes back
    let unlinked = Graph::read(&with_serials(true).replace(
        r#"{ "id": 402, "type": "PipeWire:Interface:Link",
    "info": { "output-node-id": 130, "input-node-id": 110 } },"#,
        "",
    ));
    assert!(unlinked.out_streams[0].to.is_empty());
    assert_eq!(to_reattach(&[931], &unlinked, &dac), [130]);
    // one that asks for an output feeding the microphone is that mic's
    // sound: never sent to HushMic Speakers, by name or by node id
    let mut feed = off.clone();
    feed.feeders = vec!["alsa_output.usb-dac".into()];
    feed.out_streams[0].own_target = Some("alsa_output.usb-dac".into());
    assert!(to_reattach(&[931], &feed, &dac).is_empty());
    let id = feed.node_id("alsa_output.usb-dac").unwrap();
    feed.out_streams[0].own_target = Some(id.to_string());
    assert!(to_reattach(&[931], &feed, &dac).is_empty());
    // its own target elsewhere: back it goes as before
    feed.out_streams[0].own_target = Some("alsa_output.pci-speakers".into());
    assert_eq!(to_reattach(&[931], &feed, &dac), [130]);
}

#[test]
fn a_retarget_keeps_the_run() {
    let now = Instant::now();
    let mut rt = Runtime::new();
    let g = Graph::read(SINK_DUMP);
    assert_eq!(rt.reconfigure(true, None, Some(&g)), Step::Start(None));
    rt.started(None, true, now);
    rt.observe(&facts(true, None, Some(&g), true, now));
    assert!(rt.seen_up());
    // Play to: the owner re-targets the running chain
    assert_eq!(
        rt.reconfigure(true, Some("alsa_output.pci-speakers"), Some(&g)),
        Step::Restart {
            target: Some("alsa_output.pci-speakers".into()),
            why: Why::Output
        }
    );
    rt.retargeted(Some("alsa_output.pci-speakers".into()));
    assert_eq!(rt.running_target(), Some(Some("alsa_output.pci-speakers")));
    assert!(rt.seen_up(), "the same run goes on");
    assert_eq!(
        rt.reconfigure(true, Some("alsa_output.pci-speakers"), Some(&g)),
        Step::Nothing
    );
}

#[test]
fn only_what_hushmic_wrote_for_a_gone_stream_is_cleared() {
    use hushmic::speakers::our_targets;
    let wrote: Vec<(String, String)> = vec![
        ("target.object".into(), "hushmic_sink".into()),
        ("target.node".into(), "110".into()),
    ];
    let listing = "Found \"default\" metadata 34\n\
        update: id:57 key:'target.object' value:'hushmic_sink' type:'Spa:String'\n\
        update: id:57 key:'target.node' value:'110' type:'Spa:Id'\n\
        update: id:571 key:'target.object' value:'hushmic_sink' type:'Spa:String'\n";
    assert_eq!(
        our_targets(listing, 57, &wrote),
        ["target.object", "target.node"]
    );
    // a newer node with that id and a target of its own keeps it
    let theirs =
        "update: id:57 key:'target.object' value:'alsa_output.usb-dac' type:'Spa:String'\n\
        update: id:57 key:'target.node' value:'100' type:'Spa:Id'\n";
    assert!(our_targets(theirs, 57, &wrote).is_empty());
    // cleared already (PipeWire did it with the node)
    assert!(our_targets("Found \"default\" metadata 34\n", 57, &wrote).is_empty());
}

// --- which outputs: hardware first, never the microphone's own --------------------

/// A virtual microphone (a loopback whose sink side is `vmic_sink` and whose
/// source side HushMic captures), a loopback output whose source side only
/// a recorder records, an EasyEffects-like virtual sink, a null sink, and
/// two sound cards listed after them.
fn mixed_dump(default_sink: &str, capture_linked: bool) -> String {
    let link = if capture_linked {
        r#"{ "id": 500, "type": "PipeWire:Interface:Link",
    "info": { "output-node-id": 11, "input-node-id": 90 } },"#
    } else {
        ""
    };
    format!(
        r#"[
  {{ "id": 10, "type": "PipeWire:Interface:Node",
    "info": {{ "props": {{ "media.class": "Audio/Sink", "node.name": "vmic_sink",
      "node.link-group": "loopback-1-18" }} }} }},
  {{ "id": 11, "type": "PipeWire:Interface:Node",
    "info": {{ "props": {{ "media.class": "Audio/Source", "node.name": "vmic",
      "node.link-group": "loopback-1-18" }} }} }},
  {{ "id": 12, "type": "PipeWire:Interface:Node",
    "info": {{ "props": {{ "media.class": "Audio/Sink", "node.name": "loop_out",
      "node.link-group": "loopback-2-19" }} }} }},
  {{ "id": 13, "type": "PipeWire:Interface:Node",
    "info": {{ "props": {{ "media.class": "Audio/Source", "node.name": "loop_out_mon",
      "node.link-group": "loopback-2-19" }} }} }},
  {{ "id": 14, "type": "PipeWire:Interface:Node",
    "info": {{ "props": {{ "media.class": "Audio/Sink", "node.name": "easyeffects_sink",
      "node.virtual": true, "node.description": "Easy Effects Sink" }} }} }},
  {{ "id": 15, "type": "PipeWire:Interface:Node",
    "info": {{ "props": {{ "media.class": "Audio/Sink", "node.name": "null",
      "factory.name": "support.null-audio-sink" }} }} }},
  {{ "id": 20, "type": "PipeWire:Interface:Node",
    "info": {{ "props": {{ "media.class": "Audio/Sink", "node.name": "raop_sink.kitchen",
      "node.description": "Kitchen" }} }} }},
  {{ "id": 21, "type": "PipeWire:Interface:Node",
    "info": {{ "props": {{ "media.class": "Audio/Sink", "node.name": "alsa_output.usb-dac",
      "device.api": "alsa", "node.description": "USB DAC" }} }} }},
  {{ "id": 22, "type": "PipeWire:Interface:Node",
    "info": {{ "props": {{ "media.class": "Audio/Sink",
      "node.name": "bluez_output.AA_BB.1", "node.description": "Headphones" }} }} }},
  {{ "id": 90, "type": "PipeWire:Interface:Node",
    "info": {{ "props": {{ "media.class": "Stream/Input/Audio", "node.name": "hushmic_capture" }} }} }},
  {link}
  {{ "type": "PipeWire:Interface:Metadata", "id": 30,
    "props": {{ "metadata.name": "default" }},
    "metadata": [
      {{ "subject": 0, "key": "default.audio.sink", "type": "Spa:String:JSON",
        "value": {{ "name": "{default_sink}" }} }}
    ] }}
]"#
    )
}

#[test]
fn outputs_are_hardware_first_and_never_the_microphones_own() {
    let names = |g: &Graph| g.sinks.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    let g = Graph::read(&mixed_dump("alsa_output.usb-dac", true));
    // the sound cards first, a network speaker next, the virtual ones last;
    // the virtual mic's sink not at all
    assert_eq!(
        names(&g),
        [
            "alsa_output.usb-dac",
            "bluez_output.AA_BB.1",
            "raop_sink.kitchen",
            "loop_out",
            "easyeffects_sink",
            "null"
        ]
    );
    assert_eq!(g.feeders, ["vmic_sink"]);
    assert_eq!(g.virtual_sinks, ["loop_out", "easyeffects_sink", "null"]);
    // offered: no virtual one, unless it is the default or the pick
    let offered = |g: &Graph, chosen, last| {
        g.offered(chosen, last)
            .into_iter()
            .map(|s| s.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        offered(&g, None, None),
        [
            "alsa_output.usb-dac",
            "bluez_output.AA_BB.1",
            "raop_sink.kitchen"
        ]
    );
    assert_eq!(
        offered(&g, Some("null"), None),
        [
            "alsa_output.usb-dac",
            "bluez_output.AA_BB.1",
            "raop_sink.kitchen",
            "null"
        ]
    );
    let ee = Graph::read(&mixed_dump("easyeffects_sink", true));
    assert!(offered(&ee, None, None).contains(&"easyeffects_sink".to_string()));
    // the last real default while HushMic Speakers is the default
    assert!(offered(&g, None, Some("loop_out")).contains(&"loop_out".to_string()));
    // the fallback is the first sound card
    let mine = Graph::read(&mixed_dump(SINK, true));
    assert_eq!(
        wanted_target(None, Some(&mine), None).as_deref(),
        Some("alsa_output.usb-dac")
    );
    // the microphone's own sink: never played to, picked or the default
    assert!(g.is_feeder("vmic_sink") && !g.has_sink("vmic_sink"));
    assert_eq!(wanted_target(Some("vmic_sink"), Some(&g), None), None);
    let into_mic = Graph::read(&mixed_dump("vmic_sink", true));
    assert_eq!(
        wanted_target(None, Some(&into_mic), None).as_deref(),
        Some("alsa_output.usb-dac")
    );
    assert!(hushmic::speakers::output_warning("vmic_sink", Some(&g))
        .is_some_and(|w| w.contains("microphone")));
    // a picked virtual output is played to
    assert_eq!(
        wanted_target(Some("loop_out"), Some(&g), None).as_deref(),
        Some("loop_out")
    );
    // with no link to show it (the mic chain idle), the configured
    // microphone or the default source still names it
    let idle = Graph::read(&mixed_dump("alsa_output.usb-dac", false));
    assert!(idle.feeders.is_empty());
    assert_eq!(
        Graph::read_for(&mixed_dump("alsa_output.usb-dac", false), Some("vmic")).feeders,
        ["vmic_sink"]
    );
    let by_default = mixed_dump("alsa_output.usb-dac", false).replace(
        r#""value": { "name": "alsa_output.usb-dac" } }"#,
        r#""value": { "name": "alsa_output.usb-dac" } },
      { "subject": 0, "key": "default.audio.source", "type": "Spa:String:JSON",
        "value": { "name": "vmic" } }"#,
    );
    assert_eq!(Graph::read(&by_default).feeders, ["vmic_sink"]);
    // a recorder on loop_out's other side is not HushMic: loop_out stays
    assert!(g.has_sink("loop_out"));
    // nothing else left: the session manager linked the playback into the
    // microphone's own sink. It counts as playing nowhere (and the chain
    // is muted while it lasts)
    let mut stuck = g.clone();
    stuck.playing_to = vec!["vmic_sink".into()];
    assert!(stuck.plays_into_mic());
    assert_eq!(stuck.playing_on(), None);
    let mut fine = g.clone();
    fine.playing_to = vec!["alsa_output.usb-dac".into()];
    assert!(!fine.plays_into_mic());
    assert_eq!(fine.playing_on(), Some("alsa_output.usb-dac"));
    {
        use hushmic::speakers::doctor::render;
        let mut r = healthy_report();
        let mut up = stuck.clone();
        up.sink_up = true;
        r.graph = Some(up);
        let lines = render(&r, true);
        assert!(
            problems(&lines).iter().any(|p| p.contains(
                "plays to: vmic_sink (it feeds the microphone HushMic cleans: muted until another output is there)"
            )),
            "{}",
            text(&lines)
        );
    }
    // PipeWire 0.3.48: every pw-loopback process names its group alike;
    // the client that made the halves tells the loopbacks apart
    let old_pw = mixed_dump("alsa_output.usb-dac", true)
        .replace("loopback-2-19", "loopback-18")
        .replace("loopback-1-18", "loopback-18")
        .replace(
            r#""node.name": "vmic_sink","#,
            r#""node.name": "vmic_sink", "client.id": 40,"#,
        )
        .replace(
            r#""node.name": "vmic","#,
            r#""node.name": "vmic", "client.id": 40,"#,
        )
        .replace(
            r#""node.name": "loop_out","#,
            r#""node.name": "loop_out", "client.id": 41,"#,
        )
        .replace(
            r#""node.name": "loop_out_mon","#,
            r#""node.name": "loop_out_mon", "client.id": 41,"#,
        );
    let g = Graph::read(&old_pw);
    assert_eq!(g.feeders, ["vmic_sink"], "{old_pw}");
    assert!(g.has_sink("loop_out"));
}

// --- HushMic's own players follow a default change ---------------------------------

#[test]
fn own_players_move_when_the_default_changes_under_them() {
    use hushmic::speakers::graph::parse_own_players;
    use hushmic::speakers::own_player_moves;
    // Mic check's replay, started while the DAC was the default, linked
    // into it; then into HushMic Speakers once that became the default
    let player = |into: u32| {
        format!(
            r#"{{ "id": 140, "type": "PipeWire:Interface:Node",
    "info": {{ "props": {{ "media.class": "Stream/Output/Audio",
      "node.name": "hushmic-miccheck", "object.serial": 950 }} }} }},
  {{ "id": 403, "type": "PipeWire:Interface:Link",
    "info": {{ "output-node-id": 140, "input-node-id": {into} }} }},
  {{ "id": 400, "type": "PipeWire:Interface:Link","#
        )
    };
    let on_dac = SINK_DUMP.replacen(
        r#"{ "id": 400, "type": "PipeWire:Interface:Link","#,
        &player(100),
        1,
    );
    let g = Graph::read(&on_dac);
    assert_eq!(parse_own_players(&on_dac).len(), 1);
    assert_eq!(g.own_players[0].to, ["alsa_output.usb-dac"]);
    // never counted among the apps
    assert!(g.out_streams.iter().all(|s| s.serial != 950));
    // where it should be: nothing to do
    assert!(own_player_moves(&g, &[]).is_empty());
    // HushMic Speakers became the default and it followed: off it, to the
    // output the chain plays on
    let ours = self_default_dump().replacen(
        r#"{ "id": 400, "type": "PipeWire:Interface:Link","#,
        &player(110),
        1,
    );
    let g = Graph::read(&ours);
    assert!(g.default_is_ours(), "{g:?}");
    let moves = own_player_moves(&g, &[]);
    assert_eq!(moves, [(140, 950, Some("alsa_output.usb-dac".to_string()))]);
    // asked once per target
    assert!(own_player_moves(&g, &[(950, Some("alsa_output.usb-dac".into()))]).is_empty());
    // started with a target while HushMic Speakers was the default, and the
    // default is a real output again (the speakers): back to the default
    let back = on_dac
        .replace(
            r#""input-node-id": 100 } },
  { "id": 400"#,
            r#""input-node-id": 103 } },
  { "id": 400"#,
        )
        .replace(
            r#""value": { "name": "alsa_output.usb-dac" } }"#,
            r#""value": { "name": "alsa_output.pci-speakers" } }"#,
        );
    let g = Graph::read(&back);
    assert_eq!(g.own_players[0].to, ["alsa_output.pci-speakers"], "{back}");
    assert!(own_player_moves(&g, &[]).is_empty());
    let g = Graph::read(&on_dac.replace(
        r#""value": { "name": "alsa_output.usb-dac" } }"#,
        r#""value": { "name": "alsa_output.pci-speakers" } }"#,
    ));
    assert_eq!(own_player_moves(&g, &[]), [(140, 950, None)]);
}

#[test]
fn a_snapshot_asked_before_a_restart_decides_nothing_for_its_apps() {
    use hushmic::speakers::reattach_plan;
    let dac = ["alsa_output.usb-dac"];
    // A127: an app moved in by a mixer (numbers only, deleted at the stop),
    // then a restart HushMic makes itself (the inference engine changed).
    // The look's pw-dump was asked for before the stop: the old sink, the
    // app still on it. Taken as current it finds nothing to bring back,
    // and the app stays on the default output for good.
    let before = Graph::read(&with_serials(true));
    assert_eq!(
        reattach_plan(&[931], &before, &dac, true, true),
        Some((vec![], vec![]))
    );
    // so it decides nothing, and the streams wait for the next look
    assert_eq!(reattach_plan(&[931], &before, &dac, true, false), None);
    // which sees the app moved to the default output and the new sink up:
    // back it goes, held there (WirePlumber 0.4.13 on)
    let after = Graph::read(&with_serials(false));
    assert_eq!(
        reattach_plan(&[931], &after, &dac, true, true),
        Some((vec![(130, true)], vec![]))
    );
    // where HushMic may not hold it (0.4.8): left there, and named
    assert_eq!(
        reattach_plan(&[931], &after, &dac, false, true),
        Some((vec![], vec![931]))
    );
    // a new sink not up yet: wait as well
    let mut down = after.clone();
    down.sink_up = false;
    assert_eq!(reattach_plan(&[931], &down, &dac, true, true), None);
}

// --- a send-back leaves nothing WirePlumber would store --------------------------

#[test]
fn a_send_back_lets_go_of_its_metadata_once_the_stream_stays_on_its_own() {
    use hushmic::speakers::{holds_without_keys, release_landed, send_back_plan};
    let dac = ["alsa_output.usb-dac"];
    // Discord, linked to `linked_to`, with or without a target of its own
    let with = |props: &str, linked_to: u32| {
        with_serials(true)
            .replace(
                r#""node.name": "Discord", "object.serial": 931 }"#,
                &format!(r#""node.name": "Discord", "object.serial": 931{props} }}"#),
            )
            .replace(
                r#""info": { "output-node-id": 130, "input-node-id": 110 }"#,
                &format!(r#""info": {{ "output-node-id": 130, "input-node-id": {linked_to} }}"#),
            )
    };
    // it picked HushMic Speakers itself by name (pw-play --target
    // hushmic_sink, a PulseAudio client): it stays without HushMic's
    // metadata, so it is sent back, and once on the sink the metadata goes
    let own = Graph::read(&with(r#", "target.object": "hushmic_sink""#, 100));
    assert_eq!(own.out_streams[0].own_target.as_deref(), Some(SINK));
    assert!(holds_without_keys(&own.out_streams[0], &own));
    assert_eq!(
        send_back_plan(&[931], &own, &dac, false),
        (vec![(130, false)], vec![])
    );
    assert_eq!(
        send_back_plan(&[931], &own, &dac, true),
        (vec![(130, false)], vec![])
    );
    let landed = Graph::read(&with(r#", "target.object": "hushmic_sink""#, 110));
    assert_eq!(release_landed(&landed, false), [931]);
    // not back yet: it keeps them until it is (at most LAND_LIMIT), or
    // the chain stops
    assert!(release_landed(&own, false).is_empty());
    assert_eq!(release_landed(&own, true), [931]);
    // the node id the sink has now: the same
    let by_id = Graph::read(&with(r#", "node.target": "110""#, 100));
    assert!(holds_without_keys(&by_id.out_streams[0], &by_id));
    // a number of a sink that is gone (an old pw-cat by id, a new one by
    // serial): only metadata kept by HushMic could hold it. Where
    // WirePlumber saves nothing for keys written name last (0.4.13 on) it
    // is sent back and held; elsewhere it is left on the default output,
    // and named
    let stale = Graph::read(&with(r#", "node.target": "77""#, 100));
    assert!(!holds_without_keys(&stale.out_streams[0], &stale));
    assert_eq!(
        send_back_plan(&[931], &stale, &dac, false),
        (vec![], vec![931])
    );
    assert_eq!(
        send_back_plan(&[931], &stale, &dac, true),
        (vec![(130, true)], vec![])
    );
    let by_serial = Graph::read(&with(r#", "target.object": "4711""#, 100));
    assert_eq!(
        send_back_plan(&[931], &by_serial, &dac, false),
        (vec![], vec![931])
    );
    assert_eq!(
        send_back_plan(&[931], &by_serial, &dac, true),
        (vec![(130, true)], vec![])
    );
    // moved there by a mixer (no target of its own, pactl and the desktops'
    // pickers write numbers): the same
    let mixed = Graph::read(&with("", 100));
    assert_eq!(
        send_back_plan(&[931], &mixed, &dac, false),
        (vec![], vec![931])
    );
    assert_eq!(
        send_back_plan(&[931], &mixed, &dac, true),
        (vec![(130, true)], vec![])
    );
    // moved to another output by the user meanwhile: it stays there either way
    let away = Graph::read(&with("", 103));
    assert_eq!(send_back_plan(&[931], &away, &dac, true), (vec![], vec![]));
    // with HushMic Speakers as the default, the default keeps any of them
    let ours = |dump: String| {
        dump.replace(
            r#""key": "default.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "alsa_output.usb-dac" }"#,
            r#""key": "default.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "hushmic_sink" }"#,
        )
        .replace(
            r#""key": "default.configured.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "alsa_output.usb-dac" }"#,
            r#""key": "default.configured.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "hushmic_sink" }"#,
        )
    };
    let g = Graph::read(&ours(with(r#", "node.target": "77""#, 100)));
    assert!(g.default_is_ours());
    assert!(holds_without_keys(&g.out_streams[0], &g));
    assert_eq!(
        send_back_plan(&[931], &g, &dac, false),
        (vec![(130, false)], vec![])
    );
}

#[test]
fn wireplumber_says_its_version_from_0_4_13_on() {
    use hushmic::pipewire::parse_wireplumber_version;
    let out =
        "wireplumber\nCompiled with libwireplumber 0.4.13\nLinked with libwireplumber 0.4.13\n";
    assert_eq!(parse_wireplumber_version(out), Some((0, 4, 13)));
    assert_eq!(
        parse_wireplumber_version("Linked with libwireplumber 0.5.19"),
        Some((0, 5, 19))
    );
    // 0.4.8 has no --version (it says so on stderr): unknown
    assert_eq!(parse_wireplumber_version(""), None);
    assert_eq!(parse_wireplumber_version("Unknown option --version"), None);
}

#[test]
fn targets_a_killed_hushmic_left_are_taken_over() {
    use hushmic::speakers::orphan_targets;
    // Discord with HushMic's name-last keys in the default metadata
    let keyed = |object: &str| {
        with_serials(false).replace(
            r#""metadata": ["#,
            &format!(
                r#""metadata": [
      {{ "subject": 130, "key": "target.node", "type": "Spa:Id", "value": "77" }},
      {{ "subject": 130, "key": "target.object", "type": "Spa:String", "value": "{object}" }},"#
            ),
        )
    };
    let g = Graph::read(&keyed(SINK));
    assert_eq!(g.target_of(130, "target.object"), Some(SINK));
    assert_eq!(g.target_of(130, "target.node"), Some("77"));
    assert_eq!(
        orphan_targets(&g, &[]),
        [(
            130,
            931,
            vec![
                ("target.node".to_string(), "77".to_string()),
                ("target.object".to_string(), SINK.to_string())
            ]
        )]
    );
    // known already, or a mixer's numbers: not HushMic's to take
    assert!(orphan_targets(&g, &[931]).is_empty());
    assert!(orphan_targets(&Graph::read(&keyed("4711")), &[]).is_empty());
}

#[test]
fn targets_naming_the_sink_by_number_are_found() {
    // a mixer's numbers (pactl: id and serial), WirePlumber 0.4.8's bare
    // id, HushMic's own name, and an other output's id
    let keyed = with_serials(false).replace(
        r#""metadata": ["#,
        r#""metadata": [
      { "subject": 130, "key": "target.node", "type": "Spa:Id", "value": "110" },
      { "subject": 130, "key": "target.object", "type": "Spa:Id", "value": "4711" },
      { "subject": 131, "key": "target.node", "type": "Spa:Id", "value": "110" },
      { "subject": 132, "key": "target.object", "type": "Spa:String", "value": "hushmic_sink" },
      { "subject": 133, "key": "target.node", "type": "Spa:Id", "value": "100" },
      { "subject": 134, "key": "target.object", "type": "Spa:Id", "value": "110" },
      { "subject": 135, "key": "target.object", "type": "Spa:Id", "value": "4711" },"#,
    );
    let g = Graph::read(&keyed);
    let mut found = g.targets_naming(110, Some(4711), false);
    found.sort();
    let t = |s: u32, k: &str, v: &str| (s, k.to_string(), v.to_string());
    assert_eq!(
        found,
        vec![
            t(130, "target.node", "110"),
            t(130, "target.object", "4711"),
            t(131, "target.node", "110"),
        ]
    );
    // a target.object equal to the sink's id is some node's serial, not
    // the sink's, and one alone (135) is never acted on; without a serial
    // only target.node counts
    assert_eq!(g.targets_naming(110, None, false).len(), 2);
    assert!(g.targets_naming(999, Some(1), false).is_empty());
    // where WirePlumber goes by the serial (0.4.13 on): the mixer's pair
    // stays, it keeps WirePlumber's saved pick for the app; the bare id
    // goes. Without the sink's serial known, a pair cannot be told
    assert_eq!(
        g.targets_naming(110, Some(4711), true),
        vec![t(131, "target.node", "110")]
    );
    assert_eq!(g.targets_naming(110, None, true).len(), 2);
    // a target.object beside it that is not the sink's serial: not a pair
    let other = Graph::read(&keyed.replace(
        r#"{ "subject": 130, "key": "target.object", "type": "Spa:Id", "value": "4711" },"#,
        r#"{ "subject": 130, "key": "target.object", "type": "Spa:Id", "value": "4712" },"#,
    ));
    assert_eq!(
        other.targets_naming(110, Some(4711), true),
        vec![t(130, "target.node", "110"), t(131, "target.node", "110")]
    );
}

#[test]
fn a_mixer_moved_app_is_sent_back_the_mixer_way() {
    use hushmic::speakers::{mixer_pairs, pair_calls};
    // what pactl writes: the id, then the serial (WirePlumber 0.4.13 on
    // saves the sink as the app's pick from it; a name last, or the keys
    // deleted, take that pick away)
    let call = |k: &str, v: &str| -> Vec<String> {
        ["-n", "default", "--", "130", k, v, "Spa:Id"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    assert_eq!(
        pair_calls(130, 110, 4711),
        [call("target.node", "110"), call("target.object", "4711")]
    );
    // the streams a mixer moved in: id and serial of the sink on them
    let with_md = |md: &str| {
        Graph::read(
            &with_serials(true)
                .replace(r#""metadata": ["#, &format!(r#""metadata": [{md}"#))
                .replace(
                    r#""node.name": "hushmic_sink""#,
                    r#""node.name": "hushmic_sink", "object.serial": 4711"#,
                ),
        )
    };
    let pair = r#"
      { "subject": 130, "key": "target.node", "type": "Spa:Id", "value": "110" },
      { "subject": 130, "key": "target.object", "type": "Spa:Id", "value": "4711" },"#;
    let g = with_md(pair);
    assert_eq!(g.sink_serial, Some(4711));
    assert_eq!(mixer_pairs(&g), [931]);
    // HushMic's own name-last keys, a bare id, a stale serial: not a mixer's pick
    for md in [
        r#"
      { "subject": 130, "key": "target.node", "type": "Spa:Id", "value": "110" },
      { "subject": 130, "key": "target.object", "type": "Spa:String", "value": "hushmic_sink" },"#,
        r#"
      { "subject": 130, "key": "target.node", "type": "Spa:Id", "value": "110" },"#,
        r#"
      { "subject": 130, "key": "target.node", "type": "Spa:Id", "value": "110" },
      { "subject": 130, "key": "target.object", "type": "Spa:Id", "value": "4700" },"#,
    ] {
        assert!(mixer_pairs(&with_md(md)).is_empty(), "{md}");
    }
    // after a restart: the pair of the sink that is gone, left in place,
    // still says a mixer moved it in; HushMic's own name, a bare id do not
    use hushmic::speakers::carries_pair;
    let stale = with_md(
        r#"
      { "subject": 130, "key": "target.node", "type": "Spa:Id", "value": "77" },
      { "subject": 130, "key": "target.object", "type": "Spa:Id", "value": "4700" },"#,
    );
    assert!(carries_pair(&stale, 130));
    assert!(!carries_pair(&stale, 131));
    assert!(!carries_pair(&g.clone(), 999));
    for md in [
        r#"
      { "subject": 130, "key": "target.node", "type": "Spa:Id", "value": "77" },
      { "subject": 130, "key": "target.object", "type": "Spa:String", "value": "hushmic_sink" },"#,
        r#"
      { "subject": 130, "key": "target.node", "type": "Spa:Id", "value": "77" },"#,
        r#"
      { "subject": 130, "key": "target.node", "type": "Spa:Id", "value": "-1" },
      { "subject": 130, "key": "target.object", "type": "Spa:Id", "value": "-1" },"#,
    ] {
        assert!(!carries_pair(&with_md(md), 130), "{md}");
    }
}

#[test]
fn metadata_listing_targets_are_read_as_from_a_dump() {
    // `pw-metadata -n default`, read fresh when the chain stops
    let listing = "Found \"default\" metadata 31
update: id:0 key:'default.audio.sink' value:'{ \"name\": \"x\" }' type:'Spa:String:JSON'
update: id:130 key:'target.node' value:'110' type:'Spa:Id'
update: id:130 key:'target.object' value:'4711' type:'Spa:Id'
update: id:132 key:'target.object' value:'hushmic_sink' type:'Spa:String'
update: id:133 key:'media.role' value:'Music' type:''
";
    let t = |s: u32, k: &str, v: &str| (s, k.to_string(), v.to_string());
    let targets = parse_metadata_targets(listing);
    assert_eq!(
        targets,
        vec![
            t(130, "target.node", "110"),
            t(130, "target.object", "4711"),
            t(132, "target.object", "hushmic_sink"),
        ]
    );
    assert_eq!(targets_naming(&targets, 110, Some(4711), false).len(), 2);
    assert!(targets_naming(&targets, 110, Some(4711), true).is_empty());
    assert!(parse_metadata_targets("").is_empty());
}
