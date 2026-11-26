use hushmic::pipewire;
use hushmic::pipewire::{
    chain_pins_quantum, merge_dump_batches, parse_core_version, parse_metadata_number,
    parse_metadata_value, parse_node_id, parse_pwdump_nodes, pw_version_at_least,
    resolve_effective_mic,
};

/// A trimmed `pw-dump` array: a Device (not a Node), our virtual source, a real
/// RODE capture source, a Sink (not a source), a sink monitor source, and a
/// nick-only source. Mirrors the real shape captured on PipeWire 1.0.5.
const PWDUMP: &str = r#"[
  { "id": 10, "type": "PipeWire:Interface:Device",
    "info": { "props": { "device.name": "alsa_card.pci-0000_00_1f.3" } } },
  { "id": 39, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Source",
      "node.name": "hushmic_source", "node.description": "hushmic Microphone" } } },
  { "id": 46, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Source",
      "node.name": "alsa_input.usb-RODE_Microphones_RODE_NT-USB-00.analog-stereo",
      "node.description": "RODE NT-USB Analog Stereo" } } },
  { "id": 52, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Sink",
      "node.name": "alsa_output.pci-0000_00_1f.3.analog-stereo", "node.description": "Speakers" } } },
  { "id": 55, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Source",
      "node.name": "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
      "node.description": "Monitor of Speakers" } } },
  { "id": 60, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Source",
      "node.name": "alsa_input.usb-Webcam", "node.nick": "Webcam Mic" } } }
]"#;

#[test]
fn quantum_pin_detection_covers_all_prop_shapes() {
    // The shared fixture has hushmic_source (the host's, always pinned)
    // but no chain: None (chain down).
    assert_eq!(chain_pins_quantum(PWDUMP), None);
    assert_eq!(chain_pins_quantum("[]"), None);
    assert_eq!(chain_pins_quantum("not json"), None);
    let output = |pin: &str| {
        format!(
            r#"[{{ "id": 38, "type": "PipeWire:Interface:Node",
                 "info": {{ "props": {{ "media.class": "Audio/Source",
                   "node.name": "hushmic_source", "node.force-quantum": 480 }} }} }},
               {{ "id": 39, "type": "PipeWire:Interface:Node",
                 "info": {{ "props": {{ "media.class": "Stream/Output/Audio",
                   "node.name": "hushmic_dsp_out"{pin} }} }} }}]"#
        )
    };
    // An unpinned chain reads as Some(0), whatever the host's pin.
    assert_eq!(chain_pins_quantum(&output("")), Some(0));
    // Pinned, numeric and string prop shapes; 0 counts as unforced.
    let with = |v: &str| output(&format!(r#", "node.force-quantum": {v}"#));
    assert_eq!(chain_pins_quantum(&with("1024")), Some(1024));
    assert_eq!(chain_pins_quantum(&with("\"1024\"")), Some(1024));
    assert_eq!(chain_pins_quantum(&with("0")), Some(0));
}

#[test]
fn metadata_number_parses_the_pw_metadata_line_format() {
    let out = "Found \"settings\" metadata 31\nupdate: id:0 key:'clock.force-quantum' value:'1024' type:''\n";
    assert_eq!(parse_metadata_number(out), Some(1024));
    // Key absent: pw-metadata prints only the header line.
    assert_eq!(
        parse_metadata_number("Found \"settings\" metadata 31\n"),
        None
    );
    assert_eq!(parse_metadata_number(""), None);
}

#[test]
fn parses_pwdump_audio_sources_only() {
    let s = parse_pwdump_nodes(PWDUMP);
    let names: Vec<_> = s.iter().map(|x| x.name.as_str()).collect();
    // All four Audio/Source nodes are returned (Device + the Audio/Sink excluded).
    assert_eq!(s.len(), 4, "got {:?}", s);
    assert!(names.contains(&"hushmic_source"));
    assert!(names.contains(&"alsa_input.usb-RODE_Microphones_RODE_NT-USB-00.analog-stereo"));
    assert!(names.contains(&"alsa_output.pci-0000_00_1f.3.analog-stereo.monitor"));
    // The Audio/Sink "Speakers" must NOT appear as a source.
    assert!(!names.contains(&"alsa_output.pci-0000_00_1f.3.analog-stereo"));
}

#[test]
fn friendly_description_with_nick_fallback() {
    let s = parse_pwdump_nodes(PWDUMP);
    let rode = s
        .iter()
        .find(|x| x.name.contains("RODE"))
        .expect("RODE source");
    assert_eq!(rode.description, "RODE NT-USB Analog Stereo");
    // node.nick is used when node.description is absent.
    let webcam = s
        .iter()
        .find(|x| x.name == "alsa_input.usb-Webcam")
        .expect("webcam source");
    assert_eq!(webcam.description, "Webcam Mic");
}

#[test]
fn real_source_filter_excludes_hushmic_and_monitor() {
    // Same predicate list_real_sources() applies after parsing.
    let real: Vec<_> = parse_pwdump_nodes(PWDUMP)
        .into_iter()
        .filter(|s| s.name != "hushmic_source" && !s.name.ends_with(".monitor"))
        .collect();
    let names: Vec<_> = real.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(real.len(), 2, "got {:?}", real);
    assert!(names.contains(&"alsa_input.usb-RODE_Microphones_RODE_NT-USB-00.analog-stereo"));
    assert!(names.contains(&"alsa_input.usb-Webcam"));
    assert!(!names.contains(&"hushmic_source"));
}

#[test]
fn hushmic_source_presence_detected() {
    // Present in the full dump...
    assert!(parse_pwdump_nodes(PWDUMP)
        .iter()
        .any(|s| s.name == "hushmic_source"));
    // ...absent when the node is gone (watchdog must then re-instantiate).
    let without = r#"[
      { "id": 46, "type": "PipeWire:Interface:Node",
        "info": { "props": { "media.class": "Audio/Source",
          "node.name": "alsa_input.usb-RODE", "node.description": "RODE" } } }
    ]"#;
    assert!(!parse_pwdump_nodes(without)
        .iter()
        .any(|s| s.name == "hushmic_source"));
}

#[test]
fn resolve_effective_mic_drops_stale_and_keeps_on_probe_failure() {
    let srcs = parse_pwdump_nodes(PWDUMP);
    let rode = "alsa_input.usb-RODE_Microphones_RODE_NT-USB-00.analog-stereo";
    // A saved mic that is a live source is kept.
    assert_eq!(
        resolve_effective_mic(Some(rode), Some(&srcs)).as_deref(),
        Some(rode)
    );
    // A saved mic that no longer matches any live source is dropped (follow default).
    assert_eq!(
        resolve_effective_mic(Some("alsa_input.gone"), Some(&srcs)),
        None
    );
    // No saved mic stays None (already following the default).
    assert_eq!(resolve_effective_mic(None, Some(&srcs)), None);
    // Probe failure keeps the saved mic — unknown is not gone.
    assert_eq!(
        resolve_effective_mic(Some("alsa_input.gone"), None).as_deref(),
        Some("alsa_input.gone")
    );
}

#[test]
fn pw_version_parse_and_compare() {
    let v048 = "pipewire\nCompiled with libpipewire 0.3.48\nLinked with libpipewire 0.3.48";
    assert!(!pw_version_at_least(v048, (0, 3, 64)), "0.3.48 < 0.3.64");
    assert!(
        pw_version_at_least("libpipewire 0.3.64", (0, 3, 64)),
        "0.3.64 == min"
    );
    assert!(
        pw_version_at_least("Compiled with libpipewire 1.6.7", (0, 3, 64)),
        "1.6.7 >= min"
    );
    // No parseable triple -> assume modern.
    assert!(pw_version_at_least("no version", (0, 3, 64)));
}

#[test]
fn node_id_resolves_name_to_numeric_target() {
    // pw-record --target needs a NUMERIC id on old pw-cat; resolve by node.name.
    assert_eq!(parse_node_id(PWDUMP, "hushmic_source"), Some(39));
    assert_eq!(
        parse_node_id(
            PWDUMP,
            "alsa_input.usb-RODE_Microphones_RODE_NT-USB-00.analog-stereo"
        ),
        Some(46)
    );
    // Not source-only: any Node resolves by name (the Sink id 52 too), so the
    // A/B recorder can target whatever node it was handed.
    assert_eq!(
        parse_node_id(PWDUMP, "alsa_output.pci-0000_00_1f.3.analog-stereo"),
        Some(52)
    );
    // A name no node carries is None so the caller falls back to the name.
    assert_eq!(parse_node_id(PWDUMP, "alsa_input.does-not-exist"), None);
    // A Device (not a Node) must never be matched by a node-name lookup.
    assert_eq!(parse_node_id(PWDUMP, "alsa_card.pci-0000_00_1f.3"), None);
    // Garbage/empty input is safe.
    assert_eq!(parse_node_id("", "hushmic_source"), None);
    assert_eq!(parse_node_id("not json", "hushmic_source"), None);
    assert_eq!(parse_node_id("{}", "hushmic_source"), None);
}

#[test]
fn empty_or_garbage_pwdump_is_safe() {
    assert!(parse_pwdump_nodes("").is_empty());
    assert!(parse_pwdump_nodes("not json").is_empty());
    assert!(parse_pwdump_nodes("[]").is_empty());
}

#[test]
fn extracts_metadata_name() {
    let out = r#"Found "default" metadata
update: id:0 key:'default.configured.audio.source' value:'{"name":"alsa_input.usb-RODE"}' type:'Spa:String:JSON'"#;
    assert_eq!(
        parse_metadata_value(out).as_deref(),
        Some("alsa_input.usb-RODE")
    );
}

#[test]
fn extracts_the_daemon_version_from_the_core_object() {
    // The Core object is the DAEMON being talked to — inside a Flatpak this
    // is the only truthful version source (`pipewire --version` there
    // reports the bundled binary, not the host).
    let dump = r#"[
      { "id": 0, "type": "PipeWire:Interface:Core", "version": 4,
        "info": { "cookie": 1, "user-name": "u", "host-name": "h",
                  "version": "0.3.48", "name": "pipewire-0", "props": {} } },
      { "id": 31, "type": "PipeWire:Interface:Node",
        "info": { "props": { "media.class": "Audio/Source", "node.name": "m" } } }
    ]"#;
    assert_eq!(parse_core_version(dump).as_deref(), Some("0.3.48"));
    // and the parsed triple feeds the same comparator the probe uses
    assert!(!pw_version_at_least("0.3.48", (0, 3, 64)));
    assert!(pw_version_at_least("1.6.2", (0, 3, 64)));
    // garbage/missing core: unknown, caller stays optimistic
    assert_eq!(parse_core_version("[]"), None);
    assert_eq!(parse_core_version("not json"), None);
}

#[test]
fn repairs_old_pwdump_keyless_param_members() {
    use hushmic::pipewire::repair_keyless_members;
    // Shape captured from a REAL pw-dump 0.3.65 run on Debian 12 while a
    // modern (libpipewire 1.4.9, sandboxed) client's node was in the graph:
    // params the old pw-dump has no name for come out as a KEYLESS `[ ]`
    // object member — invalid JSON.
    let broken = r#"[
      { "id": 62, "type": "PipeWire:Interface:Node",
        "info": {
          "params": {
            "ProcessLatency": [
            ],
            [ ]
          },
          "props": { "media.class": "Audio/Source", "node.name": "hushmic_source" }
        } }
    ]"#;
    assert!(serde_json::from_str::<serde_json::Value>(broken).is_err());
    let repaired = repair_keyless_members(broken);
    assert!(
        serde_json::from_str::<serde_json::Value>(&repaired).is_ok(),
        "repaired output must parse: {repaired}"
    );
    // The graph content survives the repair — this is exactly what keeps the
    // watchdog from reading a healthy chain as "node gone".
    assert!(parse_pwdump_nodes(&repaired)
        .iter()
        .any(|s| s.name == "hushmic_source"));
}

#[test]
fn repair_handles_leading_and_multiple_keyless_members() {
    use hushmic::pipewire::repair_keyless_members;
    for broken in [
        r#"{ [ ], "a": 1 }"#,              // leading member: trailing comma consumed
        r#"{ "a": 1, [ ], [ ] }"#,         // several in a row
        r#"{ "a": { "b": [ 1 ], [ ] } }"#, // nested object
        r#"{ [ ] }"#,                      // sole member
    ] {
        assert!(serde_json::from_str::<serde_json::Value>(broken).is_err());
        let repaired = repair_keyless_members(broken);
        assert!(
            serde_json::from_str::<serde_json::Value>(&repaired).is_ok(),
            "should repair {broken:?} but got {repaired:?}"
        );
    }
}

#[test]
fn repair_preserves_legitimate_json() {
    use hushmic::pipewire::repair_keyless_members;
    // Empty arrays as VALUES or as ARRAY elements are valid and must pass
    // through byte-identical — including brackets/braces inside strings and
    // escaped quotes, which must not derail the container tracking.
    for valid in [
        r#"{ "a": [], "b": [ [], [] ] }"#,
        r#"{ "name": "we[ird{ , ]", "x": [ ] }"#,
        r#"{ "q": "a\"b[", "arr": [ { "y": [] } ] }"#,
        r#"[ [], { "z": [] } ]"#,
    ] {
        assert_eq!(
            repair_keyless_members(valid),
            valid,
            "must not touch {valid:?}"
        );
    }
}

#[test]
fn repair_leaves_nonempty_keyless_members_alone() {
    use hushmic::pipewire::repair_keyless_members;
    // The salvage removes only EMPTY keyless `[ ]` members — the one shape
    // old pw-dump emits. A NON-empty keyless member is a different (unknown)
    // corruption: it must pass through unrepaired so the caller's re-parse
    // still fails and the dump reads as a failed probe, never as a mangled
    // graph.
    let broken = r#"{ "a": 1, [ 1, 2 ] }"#;
    let repaired = repair_keyless_members(broken);
    assert_eq!(
        repaired, broken,
        "non-empty keyless member must not be touched"
    );
    assert!(serde_json::from_str::<serde_json::Value>(&repaired).is_err());
}

// --- retry_probe: transient probe failures must not become verdicts --------
#[test]
fn retry_probe_returns_first_definitive_answer_without_extra_calls() {
    let mut calls = 0;
    let got = hushmic::pipewire::retry_probe(
        || {
            calls += 1;
            Some(true)
        },
        3,
        std::time::Duration::ZERO,
    );
    assert_eq!(got, Some(true));
    assert_eq!(calls, 1);
}

#[test]
fn retry_probe_retries_past_transient_failures() {
    let mut calls = 0;
    let got = hushmic::pipewire::retry_probe(
        || {
            calls += 1;
            if calls < 3 {
                None
            } else {
                Some(false)
            }
        },
        3,
        std::time::Duration::ZERO,
    );
    assert_eq!(got, Some(false));
    assert_eq!(calls, 3);
}

#[test]
fn retry_probe_gives_up_after_attempts_and_stays_unknown() {
    let mut calls = 0;
    let got = hushmic::pipewire::retry_probe(
        || {
            calls += 1;
            None
        },
        3,
        std::time::Duration::ZERO,
    );
    assert_eq!(got, None);
    assert_eq!(calls, 3);
}

// --- live control writes (mode, strength) -----------------------------------
#[test]
fn param_arg_is_exact_spa_json() {
    use hushmic::pipewire::{attn_value, param_arg, ATTN_CONTROL};
    // The precise SPA-JSON pw-cli set-param accepts (verified against a live
    // filter-chain); a syntax slip here fails silently at runtime.
    assert_eq!(
        param_arg("Mode", "2"),
        r#"{ params = [ "hushmic_dsp:Mode" 2 ] }"#
    );
    assert_eq!(
        param_arg("Mode", "0"),
        r#"{ params = [ "hushmic_dsp:Mode" 0 ] }"#
    );
    // The strength control: the port name with its spaces and parentheses
    // inside the quotes, the value with one decimal (verified live:
    // enum-params reads back `Float 20.000000`, node ids unchanged).
    assert_eq!(
        param_arg(ATTN_CONTROL, &attn_value(20.0)),
        r#"{ params = [ "hushmic_dsp:Attenuation Limit (dB)" 20.0 ] }"#
    );
}

#[test]
fn attn_value_is_one_decimal_and_sanitized() {
    use hushmic::pipewire::attn_value;
    assert_eq!(attn_value(100.0), "100.0");
    assert_eq!(attn_value(24.0), "24.0");
    assert_eq!(attn_value(12.26), "12.3");
    // Out-of-range and non-finite values are clamped like render_conf
    // does — a literal `NaN`/`inf` token is rejected by PipeWire.
    assert_eq!(attn_value(-5.0), "0.0");
    assert_eq!(attn_value(250.0), "100.0");
    assert_eq!(attn_value(f32::NAN), "100.0");
    assert_eq!(attn_value(f32::INFINITY), "100.0");
}

// --- latency read-back -------------------------------------------------------
#[test]
fn parse_process_latency_from_real_pwcli_output() {
    // Verbatim pw-cli enum-params output from a live PipeWire 1.6.8 chain
    // with the report-only latency node.
    let real = r#"  Object: size 80, type Spa:Pod:Object:Param:ProcessLatency (262156), id Spa:Enum:ParamId:ProcessLatency (16)
    Prop: key Spa:Pod:Object:Param:ProcessLatency:quantum (1), flags 00000000
      Float 0.000000
    Prop: key Spa:Pod:Object:Param:ProcessLatency:rate (2), flags 00000000
      Int 2880
    Prop: key Spa:Pod:Object:Param:ProcessLatency:ns (3), flags 00000000
      Long 0
"#;
    assert_eq!(hushmic::pipewire::parse_process_latency(real), Some(2880));
    // an unreported chain publishes rate 0 -> treated as "not reported"
    let zero = real.replace("Int 2880", "Int 0");
    assert_eq!(hushmic::pipewire::parse_process_latency(&zero), None);
    assert_eq!(hushmic::pipewire::parse_process_latency(""), None);
    assert_eq!(
        hushmic::pipewire::parse_process_latency("garbage\nno params here"),
        None
    );
}

#[test]
fn strict_version_probe_is_pessimistic_on_junk() {
    use hushmic::pipewire::parsed_version_at_least;
    assert_eq!(
        parsed_version_at_least("pipewire\nCompiled with libpipewire 1.6.8", (1, 6, 0)),
        Some(true)
    );
    assert_eq!(
        parsed_version_at_least("libpipewire 1.4.9", (1, 6, 0)),
        Some(false)
    );
    assert_eq!(parsed_version_at_least("0.3.48", (1, 6, 0)), Some(false));
    assert_eq!(parsed_version_at_least("no version here", (1, 6, 0)), None);
    assert_eq!(parsed_version_at_least("", (1, 6, 0)), None);
}

// --- capture-stream self-heal (issue #5: EasyEffects re-routes the chain) ---

/// Minimal pw-dump shape: nodes of several classes plus the links wiring
/// the mic (42) and a foreign virtual source (77) into hushmic_input (90).
const LINK_DUMP: &str = r#"[
  { "type": "PipeWire:Interface:Node", "id": 42,
    "info": { "props": { "node.name": "alsa_input.usb-mic", "object.serial": 542,
                         "media.class": "Audio/Source" } } },
  { "type": "PipeWire:Interface:Node", "id": 77,
    "info": { "props": { "node.name": "easyeffects_source", "object.serial": 577,
                         "media.class": "Audio/Source/Virtual" } } },
  { "type": "PipeWire:Interface:Node", "id": 90,
    "info": { "props": { "node.name": "hushmic_input", "object.serial": 590,
                         "media.class": "Stream/Input/Audio" } } },
  { "type": "PipeWire:Interface:Link", "id": 300,
    "info": { "output-node-id": 77, "input-node-id": 90 } },
  { "type": "PipeWire:Interface:Link", "id": 301,
    "info": { "output-node-id": 77, "input-node-id": 90 } },
  { "type": "PipeWire:Interface:Link", "id": 302,
    "info": { "output-node-id": 42, "input-node-id": 91 } }
]"#;

#[test]
fn feeders_come_from_the_link_graph_across_all_node_classes() {
    use hushmic::pipewire::parse_feeders;
    // Two links from the same node (stereo) report the feeder once; the
    // feeder may be ANY class (a virtual source, a sink monitor), so the
    // node map must not be limited to Audio/Source.
    assert_eq!(
        parse_feeders(LINK_DUMP, "hushmic_input"),
        ["easyeffects_source"]
    );
    // No links into an unknown node.
    assert!(parse_feeders(LINK_DUMP, "hushmic_source").is_empty());
    // Garbage stays empty, never panics.
    assert!(parse_feeders("not json", "hushmic_input").is_empty());
}

#[test]
fn node_serial_resolves_by_name() {
    use hushmic::pipewire::parse_node_serial;
    assert_eq!(
        parse_node_serial(LINK_DUMP, "alsa_input.usb-mic"),
        Some(542)
    );
    assert_eq!(parse_node_serial(LINK_DUMP, "absent"), None);
    assert_eq!(parse_node_serial("not json", "x"), None);
}

#[test]
fn capture_stolen_only_when_fed_exclusively_by_foreign_nodes() {
    use hushmic::pipewire::capture_stolen;
    let s = String::from;
    // The theft case (issue #5): linked, but not to the wanted mic.
    assert!(capture_stolen(
        &[s("easyeffects_source")],
        "alsa_input.usb-mic"
    ));
    // Healthy: the wanted mic feeds us (alone or alongside anything else).
    assert!(!capture_stolen(
        &[s("alsa_input.usb-mic")],
        "alsa_input.usb-mic"
    ));
    assert!(!capture_stolen(
        &[s("easyeffects_source"), s("alsa_input.usb-mic")],
        "alsa_input.usb-mic"
    ));
    // No links yet (startup, WirePlumber still linking): not a theft —
    // re-pinning during link setup would race the session manager.
    assert!(!capture_stolen(&[], "alsa_input.usb-mic"));
}

const META_DUMP: &str = r#"[
  { "type": "PipeWire:Interface:Metadata", "id": 30,
    "props": { "metadata.name": "default" },
    "metadata": [
      { "subject": 0, "key": "default.audio.sink", "type": "Spa:String:JSON",
        "value": { "name": "alsa_output.hdmi" } },
      { "subject": 0, "key": "default.configured.audio.source", "type": "Spa:String:JSON",
        "value": { "name": "stale_configured" } },
      { "subject": 0, "key": "default.audio.source", "type": "Spa:String:JSON",
        "value": { "name": "hushmic_source" } }
    ] },
  { "type": "PipeWire:Interface:Metadata", "id": 31,
    "props": { "metadata.name": "route-settings" }, "metadata": [] }
]"#;

const META_DUMP_CONFIGURED_ONLY: &str = r#"[
  { "type": "PipeWire:Interface:Metadata", "id": 30,
    "props": { "metadata.name": "default" },
    "metadata": [
      { "subject": 0, "key": "default.configured.audio.source", "type": "Spa:String:JSON",
        "value": { "name": "alsa_input.usb-mic" } }
    ] }
]"#;

#[test]
fn default_source_parses_from_the_dump_metadata() {
    use hushmic::pipewire::parse_default_source;
    // The EFFECTIVE default (what the session manager links streams to)
    // wins; the configured key is only a fallback — live dumps can carry
    // either one alone.
    assert_eq!(
        parse_default_source(META_DUMP),
        Some("hushmic_source".to_string())
    );
    assert_eq!(
        parse_default_source(META_DUMP_CONFIGURED_ONLY),
        Some("alsa_input.usb-mic".to_string())
    );
    assert_eq!(parse_default_source("[]"), None);
    assert_eq!(parse_default_source("not json"), None);
}

/// "Set as default microphone" active: our node is both keys.
const META_DUMP_TAKEN: &str = r#"[
  { "id": 46, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Source",
      "node.name": "alsa_input.rode", "node.description": "RODE NT-USB" } } },
  { "type": "PipeWire:Interface:Metadata", "id": 30,
    "props": { "metadata.name": "default" },
    "metadata": [
      { "subject": 0, "key": "default.configured.audio.source", "type": "Spa:String:JSON",
        "value": { "name": "hushmic_source" } },
      { "subject": 0, "key": "default.audio.source", "type": "Spa:String:JSON",
        "value": { "name": "hushmic_source" } }
    ] }
]"#;

/// Our node configured as the default, but the effective key still names
/// another device (our node restarting): which one is a race.
const META_DUMP_TAKEN_RACING: &str = r#"[
  { "id": 46, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Source",
      "node.name": "alsa_input.rode", "node.description": "RODE NT-USB" } } },
  { "type": "PipeWire:Interface:Metadata", "id": 30,
    "props": { "metadata.name": "default" },
    "metadata": [
      { "subject": 0, "key": "default.configured.audio.source", "type": "Spa:String:JSON",
        "value": { "name": "hushmic_source" } },
      { "subject": 0, "key": "default.audio.source", "type": "Spa:String:JSON",
        "value": { "name": "alsa_input.whatever" } }
    ] }
]"#;

#[test]
fn profile_default_is_a_real_device_or_the_fallback() {
    use hushmic::pipewire::followed_default;
    let s = |v: &str| Some(v.to_string());
    // A real device as the default: that device.
    assert_eq!(
        followed_default(META_DUMP_CONFIGURED_ONLY, Some("x")),
        s("alsa_input.usb-mic")
    );
    // Effective key on our node, configured on a device: the device.
    assert_eq!(followed_default(META_DUMP, None), s("stale_configured"));
    // Our node configured: the pre-takeover device the chain follows, or
    // nobody (the defaults) — never whatever the effective key says.
    for dump in [META_DUMP_TAKEN, META_DUMP_TAKEN_RACING] {
        assert_eq!(
            followed_default(dump, Some("alsa_input.rode")),
            s("alsa_input.rode")
        );
        assert_eq!(followed_default(dump, None), None);
        assert_eq!(followed_default(dump, Some("hushmic_source")), None);
    }
    // The pre-takeover device unplugged (a docked mic, undocked): not
    // what the chain captures from — nobody, never the absent device.
    let undocked = META_DUMP_TAKEN.replace("alsa_input.rode", "alsa_input.other");
    assert_eq!(followed_default(&undocked, Some("alsa_input.rode")), None);
    // No default at all: the fallback when it is there; no dump: nothing.
    let no_default = r#"[
  { "id": 46, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Source", "node.name": "a" } } }
]"#;
    assert_eq!(followed_default(no_default, s("a").as_deref()), s("a"));
    assert_eq!(followed_default("[]", s("a").as_deref()), None);
    assert_eq!(followed_default("not json", None), None);
}

#[test]
fn repin_expectation_never_resolves_to_our_own_node() {
    use hushmic::pipewire::repin_want;
    let s = |v: &str| Some(v.to_string());
    // Pinned mic: the pin is the expectation.
    assert_eq!(
        repin_want(Some("alsa_input.usb-mic"), s("hushmic_source"), Some("x")),
        s("alsa_input.usb-mic")
    );
    // Follow-default: the default is the expectation…
    assert_eq!(
        repin_want(None, s("alsa_input.usb-mic"), None),
        s("alsa_input.usb-mic")
    );
    // …EXCEPT when the default is our own output ("Set as default" on):
    // the chain follows the pre-takeover default, which the controller
    // remembers as its restore target.
    assert_eq!(
        repin_want(None, s("hushmic_source"), Some("alsa_input.usb-mic")),
        s("alsa_input.usb-mic")
    );
    // No restore target known: no expectation, no re-pin — never heal a
    // chain toward its own output.
    assert_eq!(repin_want(None, s("hushmic_source"), None), None);
    assert_eq!(repin_want(None, None, None), None);
    // A restore target that is itself stale-hushmic (crash artifacts) is
    // equally unusable.
    assert_eq!(
        repin_want(None, s("hushmic_source"), Some("hushmic_source")),
        None
    );
}

use hushmic::pipewire::Look;

/// The healer against the looks it gets: `looks` are (seconds, look);
/// the re-pins it makes and how many of them carry the notice.
fn healer_sees(looks: &[(f64, Look)]) -> (Vec<f64>, usize) {
    use hushmic::pipewire::{Repin, RepinStep};
    let t0 = std::time::Instant::now();
    let mut r = Repin::default();
    let (mut at, mut notices) = (Vec::new(), 0);
    for &(s, look) in looks {
        let now = t0 + std::time::Duration::from_secs_f64(s);
        if let RepinStep::Repin { notify } = r.observe(look, now) {
            at.push(s);
            notices += usize::from(notify);
        }
    }
    (at, notices)
}

/// [`healer_sees`] with (seconds, stolen).
fn healer(looks: &[(f64, bool)]) -> (Vec<f64>, usize) {
    let looks: Vec<_> = looks
        .iter()
        .map(|&(s, stolen)| (s, if stolen { Look::Stolen } else { Look::Held }))
        .collect();
    healer_sees(&looks)
}

#[test]
fn repin_heals_a_one_shot_theft_at_once_every_time() {
    // A theft, healed, then held: the next theft (much later) heals at
    // once again, without the notice.
    let mut looks = vec![(0.0, true)];
    looks.extend((1..=6).map(|k| (k as f64 * 5.0, false)));
    looks.push((40.0, true));
    looks.extend((1..=6).map(|k| (40.0 + k as f64 * 5.0, false)));
    looks.push((80.0, true));
    assert_eq!(healer(&looks), (vec![0.0, 40.0, 80.0], 0));
}

#[test]
fn repin_does_not_count_its_own_correction_settling() {
    // Looked at again within moments of a correction (a new engine run
    // asks for a look at once): still stolen there is our own write not
    // applied yet, not a new theft.
    let looks = [(0.0, true), (0.3, true), (1.0, true), (1.8, true)];
    assert_eq!(healer(&looks), (vec![0.0], 0));
}

#[test]
fn repin_backs_off_against_an_adversary_however_often_it_looks() {
    // An adversary re-routes every 0.5 s for 20 s; the healer looks at
    // the 5 s tick and in between (a probe every 0.25 s here), so many
    // looks fall between its correction and the adversary's next route
    // and see the stream healed. They must not start the count over:
    // three corrections, one notice, then the slow retry.
    let mut looks = Vec::new();
    for k in 0..80 {
        let s = k as f64 * 0.25;
        looks.push((s, k % 2 == 0));
    }
    let (at, notices) = healer(&looks);
    assert_eq!(at.len(), 3, "{at:?}");
    assert_eq!(notices, 1);
    // The tick alone sees it stolen every time: the same.
    let ticks: Vec<_> = (0..5).map(|k| (k as f64 * 5.0, true)).collect();
    let (at, notices) = healer(&ticks);
    assert_eq!((at.len(), notices), (3, 1), "{at:?}");
    // Still stolen a minute after the last correction: one more, gently.
    let mut long = looks.clone();
    long.extend((0..20).map(|k| (20.0 + k as f64 * 5.0, true)));
    let (at, notices) = healer(&long);
    assert_eq!(notices, 1, "said once");
    assert!(at.len() == 4 && at[3] - at[2] >= 60.0, "{at:?}");
}

#[test]
fn repin_counts_nothing_while_the_mic_is_out_of_reach() {
    // The mic we want left the graph: the stream is not fed by it, but
    // nothing can be re-pinned. However long that lasts, no correction
    // goes out and no "keeps re-routing" notice.
    let mut looks: Vec<_> = (0..40)
        .map(|k| (k as f64 * 0.5, Look::OutOfReach))
        .collect();
    assert_eq!(healer_sees(&looks), (vec![], 0));
    // Back, and stolen by a real adversary: the full three, one notice.
    looks.extend((0..20).map(|k| (20.0 + k as f64 * 2.5, Look::Stolen)));
    let (at, notices) = healer_sees(&looks);
    assert_eq!((at.len(), notices), (3, 1), "{at:?}");
}

#[test]
fn repin_takes_a_headset_that_keeps_reconnecting_for_no_thief() {
    // A Bluetooth headset drops and comes back under a new serial, so each
    // return looks stolen once and is re-pinned; it never holds 10 s in
    // between. Every return is a new story: no notice, and no fight shown.
    use hushmic::pipewire::Repin;
    let t0 = std::time::Instant::now();
    let mut r = Repin::default();
    let mut looks = Vec::new();
    for k in 0..5 {
        let s = k as f64 * 8.0;
        looks.extend([
            (s, Look::Stolen),
            (s + 2.5, Look::Held),
            (s + 5.0, Look::OutOfReach),
        ]);
    }
    let (at, notices) = healer_sees(&looks);
    assert_eq!((at.len(), notices), (5, 0), "{at:?}");
    for &(s, look) in &looks {
        r.observe(look, t0 + std::time::Duration::from_secs_f64(s));
        assert!(!r.fighting(), "at {s} s");
    }
    // An adversary that then starts is still called out.
    looks.extend((0..10).map(|k| (50.0 + k as f64 * 2.5, Look::Stolen)));
    assert_eq!(healer_sees(&looks).1, 1);
}

#[test]
fn repin_relinks_a_stream_left_unlinked() {
    // A move whose new link failed (WirePlumber 0.4) leaves the capture
    // stream fed by nothing: moments of it are the session manager
    // linking, but past REPIN_UNLINKED a re-pin goes out, as for a theft.
    let looks: Vec<_> = (0..4).map(|k| (k as f64 * 0.5, Look::Unlinked)).collect();
    assert_eq!(healer_sees(&looks), (vec![1.0], 0));
    // Linked again before that: nothing to do, and the wait starts over.
    let looks = [
        (0.0, Look::Unlinked),
        (0.8, Look::Unlinked),
        (0.9, Look::Held),
        (1.0, Look::Unlinked),
        (1.8, Look::Unlinked),
    ];
    assert_eq!(healer_sees(&looks), (vec![], 0));
    // Links that keep failing get the same back-off (three corrections,
    // then the slow retry), but no "another app keeps re-routing" notice:
    // no app was seen.
    let looks: Vec<_> = (0..40).map(|k| (k as f64 * 0.5, Look::Unlinked)).collect();
    let (at, notices) = healer_sees(&looks);
    assert_eq!((at.len(), notices), (3, 0), "{at:?}");
    // A thief whose re-routes leave the stream unlinked in between is an
    // adversary: the notice goes out.
    let mut looks = vec![(0.0, Look::Stolen)];
    looks.extend((1..40).map(|k| (k as f64 * 0.5, Look::Unlinked)));
    let (at, notices) = healer_sees(&looks);
    assert_eq!((at.len(), notices), (3, 1), "{at:?}");
}

/// The healer's steps against the looks it gets: (seconds, "repin",
/// "notice" or "restart").
fn healer_does(looks: &[(f64, Look)]) -> Vec<(f64, &'static str)> {
    use hushmic::pipewire::{Repin, RepinStep};
    let t0 = std::time::Instant::now();
    let mut r = Repin::default();
    let mut out = Vec::new();
    for &(s, look) in looks {
        match r.observe(look, t0 + std::time::Duration::from_secs_f64(s)) {
            RepinStep::Leave => {}
            RepinStep::Repin { notify: false } => out.push((s, "repin")),
            RepinStep::Repin { notify: true } => out.push((s, "notice")),
            RepinStep::Restart => out.push((s, "restart")),
        }
    }
    out
}

#[test]
fn repin_restarts_the_chain_when_a_relink_does_not_take() {
    // FULL_E2E 7912 (Mint, WirePlumber 0.4): a theft healed at once; the
    // move back landed while the stream's adapter was still set up for
    // the theft ("was started"), so its link to the mic waited for a
    // format for good. Fed by nothing on every 5 s look after it: one
    // re-pin (the 7772 relink), then, still nothing, a chain restart. No
    // notice: no app keeps re-routing it.
    let mut looks = vec![(0.0, Look::Stolen)];
    looks.extend((0..4).map(|k| (4.5 + k as f64 * 5.0, Look::Unlinked)));
    assert_eq!(
        healer_does(&looks),
        vec![(0.0, "repin"), (9.5, "repin"), (14.5, "restart")]
    );
    // The restarted chain links: nothing more.
    looks.truncate(4);
    looks.extend((0..6).map(|k| (20.0 + k as f64 * 5.0, Look::Held)));
    assert_eq!(healer_does(&looks).len(), 3);
    // A relink that takes (7772: the failed link was dropped) needs no
    // restart.
    let looks = [
        (0.0, Look::Unlinked),
        (3.0, Look::Unlinked),
        (4.0, Look::Held),
        (10.0, Look::Held),
    ];
    assert_eq!(healer_does(&looks), vec![(3.0, "repin")]);
    // A later episode of its own (another failed move, minutes on) is the
    // ordinary WirePlumber 0.4 case again: a re-pin first, not a restart.
    let mut later = looks.to_vec();
    later.extend([(40.0, Look::Unlinked), (41.0, Look::Unlinked)]);
    assert_eq!(healer_does(&later), vec![(3.0, "repin"), (41.0, "repin")]);
    // Stuck for good (the restarted chain no better): a restart a minute
    // at most, and the back-off and its notice as before.
    let looks: Vec<_> = (0..40).map(|k| (k as f64 * 5.0, Look::Unlinked)).collect();
    let steps = healer_does(&looks);
    let restarts: Vec<f64> = steps
        .iter()
        .filter(|(_, what)| *what == "restart")
        .map(|(s, _)| *s)
        .collect();
    assert!(restarts.len() >= 2, "{steps:?}");
    assert!(
        restarts.windows(2).all(|w| w[1] - w[0] >= 60.0),
        "{steps:?}"
    );
    assert!(steps.iter().all(|(_, what)| *what != "notice"), "{steps:?}");
}

/// The daemon around the healer: a look on the 5 s tick (the first at
/// `phase`), at the `extra` times (a new engine run), whenever
/// [`Repin::look_due`] owes one, and 1.5 s after a chain restart (the
/// early tick); a probe answers in 50 ms. `graph(t, corrected,
/// restarted)` is what a look at `t` finds, given when the healer last
/// re-pinned and restarted; None: no look (a chain starting, not linked
/// yet). The steps it takes, (seconds, "repin", "notice", "restart" or
/// "gave up": the moment [`Repin::gave_up`] turns on).
///
/// Like the daemon, it asks for an owed look after every probe, one that
/// formed no look too ([`Repin::owed_look`]), and once per due (the
/// daemon's `healer_look`); and it holds the healer to never owing a look
/// it has just had. Either would be a probe loop: it ends in the panic
/// below.
fn healer_loop(
    phase: f64,
    until: f64,
    extra: &[f64],
    graph: impl Fn(f64, Option<f64>, Option<f64>) -> Option<Look>,
) -> Vec<(f64, &'static str)> {
    use hushmic::pipewire::{Repin, RepinStep};
    let t0 = std::time::Instant::now();
    let secs = |at: std::time::Instant| at.saturating_duration_since(t0).as_secs_f64();
    let mut r = Repin::default();
    let mut steps = Vec::new();
    let (mut corrected, mut restarted) = (None, None);
    let mut owed: Vec<f64> = extra.to_vec();
    let mut tick = phase;
    let mut t: f64 = 0.0;
    // the look asked for (the daemon's healer_look), and when it comes
    let mut asked: Option<std::time::Instant> = None;
    let mut pending: Option<(std::time::Instant, f64)> = None;
    let mut saw_stolen = false;
    for _ in 0..10_000 {
        let next = owed.iter().copied().fold(tick, f64::min);
        t = match pending {
            Some((d, at)) if at < next => {
                pending = None;
                if asked == Some(d) {
                    asked = None;
                }
                at.max(t)
            }
            _ if next == tick => {
                tick += 5.0;
                next
            }
            _ => {
                owed.retain(|&o| o != next);
                next
            }
        };
        if t >= until {
            return steps;
        }
        let now = t0 + std::time::Duration::from_secs_f64(t);
        let probed = graph(t, corrected, restarted);
        // the daemon asks after every probe that reached the healer
        let ask = |r: &Repin, asked: &mut Option<std::time::Instant>, pending: &mut Option<_>| {
            if let Some(d) = r.owed_look(probed.is_some(), now) {
                if *asked != Some(d) {
                    *asked = Some(d);
                    *pending = Some((d, secs(d) + 0.05));
                }
            }
        };
        let Some(look) = probed else {
            ask(&r, &mut asked, &mut pending);
            continue;
        };
        saw_stolen |= look == Look::Stolen;
        let stuck_before = r.gave_up();
        let step = r.observe(look, now);
        // no app is blamed for a stream no app took (review A83, SF-MS1)
        assert!(
            saw_stolen || !r.rerouted(),
            "rerouted at {t} with no theft: {steps:?}"
        );
        if r.gave_up() && !stuck_before {
            steps.push((t, "gave up"));
        }
        assert!(
            r.look_due().is_none_or(|d| d > now),
            "owes the look it just had at {t}"
        );
        match step {
            RepinStep::Leave => {}
            RepinStep::Repin { notify } => {
                corrected = Some(t);
                steps.push((t, if notify { "notice" } else { "repin" }));
            }
            RepinStep::Restart => {
                restarted = Some(t);
                owed.push(t + 1.5);
                steps.push((t, "restart"));
            }
        }
        ask(&r, &mut asked, &mut pending);
    }
    panic!("no end of looks: {steps:?}");
}

/// A chain restarted at `r` links its new stream a second later (VM 102:
/// fed by the mic within 1 s of "restarting the chain").
fn restarted_links(t: f64, r: f64) -> Option<Look> {
    (t >= r + 1.0).then_some(Look::Held)
}

#[test]
fn repin_heals_a_stuck_link_within_seconds_whatever_the_tick() {
    // 7912 in the daemon: a theft at 0, seen at once (a new engine run
    // asks for a look); our re-pin collides with the theft's move and the
    // link sticks. The healer checks its correction, relinks once and
    // restarts the chain 3.8 s after the theft at most, wherever the 5 s
    // tick falls: the mic is fed again within 5 s, not 15. One restart,
    // no notice, and then nothing more to do.
    let stuck = |t: f64, corrected: Option<f64>, restarted: Option<f64>| match restarted {
        Some(r) => restarted_links(t, r),
        None if corrected.is_some() => Some(Look::Unlinked),
        None => Some(Look::Stolen),
    };
    for k in 0..20 {
        let phase = k as f64 * 0.25;
        let steps = healer_loop(phase, 60.0, &[0.1], stuck);
        let restarts: Vec<f64> = steps
            .iter()
            .filter(|(_, w)| *w == "restart")
            .map(|(t, _)| *t)
            .collect();
        assert!(
            restarts.len() == 1 && restarts[0] <= 3.8,
            "phase {phase}: {steps:?}"
        );
        assert_eq!(steps.last().map(|s| s.1), Some("restart"), "{steps:?}");
        assert_eq!(steps.len(), 3, "a theft, a relink, a restart: {steps:?}");
    }
    // A link stuck by moves not ours (two writes racing, the stuck
    // battery): seen at the next tick, restarted 2.7 s after it at most.
    for k in 0..20 {
        let phase = k as f64 * 0.25;
        let steps = healer_loop(phase, 60.0, &[], |t, _, r| match r {
            Some(r) => restarted_links(t, r),
            None => Some(Look::Unlinked),
        });
        let first = steps.iter().find(|(_, w)| *w == "restart");
        assert!(
            first.is_some_and(|(t, _)| *t <= phase + 2.7),
            "phase {phase}: {steps:?}"
        );
        assert_eq!(steps.len(), 2, "{steps:?}");
    }
    // Thefts 8 s apart trip the back-off, and the third correction sticks
    // the link: the stream fed by nothing is restarted 1 s after it is
    // seen so, not a minute later by the slow retry's relink.
    let thefts = [0.0, 8.0, 16.0];
    for k in 0..20 {
        let phase = k as f64 * 0.25;
        let steps = healer_loop(phase, 120.0, &[0.1, 8.1, 16.1], |t, c, r| {
            let Some(&theft) = thefts.iter().rev().find(|&&th| th <= t) else {
                return Some(Look::Held);
            };
            match (c, r) {
                (_, Some(r)) => restarted_links(t, r),
                (Some(c), None) if c >= theft => Some(if theft == 16.0 {
                    Look::Unlinked
                } else {
                    Look::Held
                }),
                _ => Some(Look::Stolen),
            }
        });
        let what: Vec<&str> = steps.iter().map(|s| s.1).collect();
        assert_eq!(what, ["repin", "repin", "notice", "restart"], "{steps:?}");
        assert!(steps[3].0 <= 18.3, "phase {phase}: {steps:?}");
    }
    // A relink that takes (7772: linked 0.3 s after the write) restarts
    // nothing; a healthy stream owes no looks.
    let steps = healer_loop(0.0, 60.0, &[], |t, c, _| match c {
        Some(c) if t >= c + 0.3 => Some(Look::Held),
        _ => Some(Look::Unlinked),
    });
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!((steps[0].0 - 1.05).abs() < 1e-6, "{steps:?}");
    let r = hushmic::pipewire::Repin::default();
    assert_eq!(r.look_due(), None);
    let steps = healer_loop(0.0, 60.0, &[], |_, _, _| Some(Look::Held));
    assert!(steps.is_empty());
}

#[test]
fn capture_look_takes_a_mic_gone_for_one_that_left() {
    use hushmic::pipewire::capture_look;
    let none: [String; 0] = [];
    let mic = ["usb_mic".to_string()];
    let other = ["easyeffects_source".to_string()];
    // fed by nothing with the mic there: a link that failed
    assert_eq!(
        capture_look(&none, "usb_mic", true, true),
        Some(Look::Unlinked)
    );
    // fed by nothing, the mic gone (unplugged, nothing to fall back to):
    // a mic that left, so the healer forgets the story (review A83)
    assert_eq!(
        capture_look(&none, "usb_mic", true, false),
        Some(Look::OutOfReach)
    );
    // our input gone: nothing to judge
    assert_eq!(capture_look(&none, "usb_mic", false, true), None);
    assert_eq!(capture_look(&none, "usb_mic", false, false), None);
    assert_eq!(capture_look(&mic, "usb_mic", true, true), Some(Look::Held));
    assert_eq!(
        capture_look(&other, "usb_mic", true, true),
        Some(Look::Stolen)
    );
    assert_eq!(
        capture_look(&other, "usb_mic", true, false),
        Some(Look::OutOfReach)
    );
}

#[test]
fn repin_forgets_a_mic_that_does_not_connect_once_it_leaves() {
    // Given up on, then the mic is unplugged: Live says it is not
    // connected (plug it back in), not that it does not connect, and no
    // look is owed while it is out.
    use hushmic::pipewire::Repin;
    let t0 = std::time::Instant::now();
    let at = |s: f64| t0 + std::time::Duration::from_secs_f64(s);
    let mut r = Repin::default();
    let mut s = 0.0;
    while !r.gave_up() {
        assert!(s < 600.0, "never gave up");
        r.observe(Look::Unlinked, at(s));
        s += 0.5;
    }
    r.observe(
        hushmic::pipewire::capture_look(&[], "usb_mic", true, false).unwrap(),
        at(s),
    );
    assert!(!r.gave_up());
    assert!(r.owed_look(true, at(s)).is_none_or(|d| d > at(s)));
}

#[test]
fn repin_owes_no_look_a_probe_could_not_form() {
    // Review A82 (SF-HL1): seen fed by nothing, then the wanted mic's node
    // goes (a pinned USB mic unplugged, nothing to fall back to): the
    // probes form no look, the wait's due stays in the past, and asking
    // for it again would read the graph again at once, back to back,
    // until the mic is back. Nothing is owed after such a probe: the tick
    // looks on, and the mic back and linked is simply held.
    use hushmic::pipewire::Repin;
    let gone = |t: f64, _: Option<f64>, _: Option<f64>| match t {
        t if t < 0.5 => Some(Look::Unlinked),
        t if t < 200.0 => None,
        _ => Some(Look::Held),
    };
    for k in 0..4 {
        let steps = healer_loop(k as f64 * 0.1, 300.0, &[0.3], gone);
        assert!(steps.is_empty(), "{steps:?}");
    }
    let t0 = std::time::Instant::now();
    let at = |s: f64| t0 + std::time::Duration::from_secs_f64(s);
    let mut r = Repin::default();
    r.observe(Look::Unlinked, at(0.0));
    assert_eq!(r.owed_look(true, at(0.0)), Some(at(1.0)));
    assert_eq!(r.owed_look(false, at(0.5)), None, "no look formed");
    assert_eq!(r.owed_look(true, at(1.05)), None, "already due");
}

#[test]
fn repin_stops_restarting_a_link_that_never_forms() {
    // The mic is there and our input too, but nothing links them, before
    // or after any restart (whatever the cause). Three restarts, then no
    // more: the healer says so (Live's Status, a notification) and re-pins
    // once a minute, silently.
    let never = |t: f64, _: Option<f64>, r: Option<f64>| match r {
        Some(r) if t < r + 3.0 => None,
        _ => Some(Look::Unlinked),
    };
    let steps = healer_loop(0.0, 900.0, &[], never);
    let restarts: Vec<f64> = steps
        .iter()
        .filter(|(_, w)| *w == "restart")
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(restarts.len(), 3, "{steps:?}");
    let gave_up: Vec<f64> = steps
        .iter()
        .filter(|(_, w)| *w == "gave up")
        .map(|(t, _)| *t)
        .collect();
    // said once, a moment after the third restart's chain is up
    assert_eq!(gave_up.len(), 1, "{steps:?}");
    assert!(
        gave_up[0] > restarts[2] && gave_up[0] <= restarts[2] + 10.0,
        "{steps:?}"
    );
    let after: Vec<f64> = steps
        .iter()
        .filter(|(t, w)| *t > gave_up[0] && *w == "repin")
        .map(|(t, _)| *t)
        .collect();
    assert!(after.len() >= 10, "{steps:?}");
    assert!(after.windows(2).all(|w| w[1] - w[0] >= 60.0), "{steps:?}");
    assert!(steps.iter().all(|(_, w)| *w != "notice"), "{steps:?}");
    // A heal (another mic chosen, say) ends it, and a later stuck link is
    // restarted again: the count starts over.
    let healed_at = 400.0;
    let steps = healer_loop(0.0, 900.0, &[], |t, c, r| {
        if (healed_at..healed_at + 60.0).contains(&t) {
            return Some(Look::Held);
        }
        never(t, c, r)
    });
    let late = steps
        .iter()
        .filter(|(t, w)| *t > healed_at && *w == "restart")
        .count();
    assert!((1..=3).contains(&late), "{steps:?}");
    assert_eq!(
        steps.iter().filter(|(_, w)| *w == "gave up").count(),
        2,
        "{steps:?}"
    );
    // The mic leaving ends it too (review A83, N-MS2): back, and still
    // not linking, it is restarted again, from a fresh count.
    let steps = healer_loop(0.0, 900.0, &[], |t, c, r| {
        if (200.0..260.0).contains(&t) {
            return Some(Look::OutOfReach);
        }
        never(t, c, r)
    });
    let back = steps
        .iter()
        .filter(|(t, w)| *t > 260.0 && *w == "restart")
        .count();
    assert!((1..=3).contains(&back), "{steps:?}");
    assert_eq!(
        steps.iter().filter(|(_, w)| *w == "gave up").count(),
        2,
        "{steps:?}"
    );
    let mut r = hushmic::pipewire::Repin::default();
    let t0 = std::time::Instant::now();
    r.observe(Look::Held, t0);
    assert!(!r.gave_up());
    // Its re-pins back off like a fight, but no app is named (the sim
    // holds every look to that): it is WirePlumber's, not EasyEffects'.
    for k in 0..40 {
        r.observe(
            Look::Unlinked,
            t0 + std::time::Duration::from_secs_f64(k as f64 * 0.5),
        );
    }
    assert!(r.fighting() && !r.rerouted());
}

#[test]
fn repin_starts_over_once_the_stream_held() {
    use hushmic::pipewire::{Repin, RepinStep};
    let t0 = std::time::Instant::now();
    let at = |s: u64| t0 + std::time::Duration::from_secs(s);
    let mut r = Repin::default();
    for s in [0, 5, 10] {
        assert!(matches!(
            r.observe(Look::Stolen, at(s)),
            RepinStep::Repin { .. }
        ));
    }
    assert!(r.fighting());
    assert!(r.rerouted(), "a thief: Live names the app");
    assert_eq!(
        r.observe(Look::Stolen, at(15)),
        RepinStep::Leave,
        "slow now"
    );
    // Healed for 5 s is not yet over; for 10 s it is.
    r.observe(Look::Held, at(20));
    r.observe(Look::Held, at(25));
    assert!(r.fighting());
    r.observe(Look::Held, at(30));
    assert!(!r.fighting());
    // A new theft heals at once, and the notice is not said again.
    assert_eq!(
        r.observe(Look::Stolen, at(31)),
        RepinStep::Repin { notify: false }
    );
    // A stolen look resets the healed time: healed 6 s, stolen, healed 6 s
    // is not 10 s held.
    let mut r = Repin::default();
    for s in [0, 5, 10] {
        r.observe(Look::Stolen, at(s));
    }
    r.observe(Look::Held, at(20));
    r.observe(Look::Held, at(26));
    r.observe(Look::Stolen, at(27));
    r.observe(Look::Held, at(28));
    r.observe(Look::Held, at(34));
    assert!(r.fighting());
}

#[test]
fn chain_node_state_comes_from_the_node_info() {
    use hushmic::pipewire::{node_state, state_is_idle};
    // Shape captured from pw-dump on PipeWire 1.6.9 with nothing recording
    // from HushMic: the capture stream idles, the source is suspended.
    let dump = r#"[
      { "id": 46, "type": "PipeWire:Interface:Node",
        "info": { "state": "idle", "props": { "node.name": "hushmic_input" } } },
      { "id": 47, "type": "PipeWire:Interface:Node",
        "info": { "state": "suspended",
          "props": { "media.class": "Audio/Source", "node.name": "hushmic_source" } } },
      { "id": 48, "type": "PipeWire:Interface:Port",
        "info": { "props": { "node.name": "hushmic_source" } } }
    ]"#;
    assert_eq!(
        node_state(dump, "hushmic_source").as_deref(),
        Some("suspended")
    );
    assert_eq!(node_state(dump, "hushmic_input").as_deref(), Some("idle"));
    // No such node, a node without a state, garbage: unknown.
    assert_eq!(node_state(dump, "absent"), None);
    assert_eq!(node_state(PWDUMP, "hushmic_source"), None);
    assert_eq!(node_state("not json", "hushmic_source"), None);

    assert!(state_is_idle("idle"));
    assert!(state_is_idle("suspended"));
    assert!(!state_is_idle("running"));
    assert!(!state_is_idle("creating"));
    assert!(!state_is_idle("error"));
}

/// The readers of a node: an app recording HushMic, one recording the raw
/// mic directly (pinned by target.object), a pavucontrol peak meter, a
/// loopback stage, our own capture stream, and a corked tab.
const READERS_DUMP: &str = r#"[
  { "id": 39, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Audio/Source", "node.name": "hushmic_source" } } },
  { "id": 46, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Audio/Source", "node.name": "alsa_input.rode" } } },
  { "id": 70, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Stream/Input/Audio", "node.name": "Firefox",
      "application.name": "Firefox", "application.process.binary": "firefox",
      "application.process.id": 4242 } } },
  { "id": 71, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Stream/Input/Audio", "node.name": "zoom-capture",
      "application.name": "Zoom Workplace", "application.process.binary": "zoom",
      "pipewire.access.portal.app_id": "us.zoom.Zoom",
      "target.object": "alsa_input.rode" } } },
  { "id": 72, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Stream/Input/Audio", "node.name": "peak detect",
      "application.name": "Volume Control", "stream.monitor": "true" } } },
  { "id": 73, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Stream/Input/Audio", "node.name": "input.loopback-1",
      "node.link-group": "loopback-1", "node.virtual": true } } },
  { "id": 74, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Stream/Input/Audio", "node.name": "hushmic_input" } } },
  { "id": 75, "type": "PipeWire:Interface:Node", "info": { "state": "idle",
    "props": { "media.class": "Stream/Input/Audio", "node.name": "tab",
      "application.name": "Google Chrome", "application.process.id": "77" } } },
  { "id": 80, "type": "PipeWire:Interface:Link",
    "info": { "output-node-id": 39, "input-node-id": 70 } },
  { "id": 81, "type": "PipeWire:Interface:Link",
    "info": { "output-node-id": 39, "input-node-id": 70 } },
  { "id": 82, "type": "PipeWire:Interface:Link",
    "info": { "output-node-id": 46, "input-node-id": 71 } },
  { "id": 83, "type": "PipeWire:Interface:Link",
    "info": { "output-node-id": 46, "input-node-id": 72 } },
  { "id": 84, "type": "PipeWire:Interface:Link",
    "info": { "output-node-id": 46, "input-node-id": 73 } },
  { "id": 85, "type": "PipeWire:Interface:Link",
    "info": { "output-node-id": 46, "input-node-id": 74 } },
  { "id": 86, "type": "PipeWire:Interface:Link",
    "info": { "output-node-id": 39, "input-node-id": 75 } }
]"#;

#[test]
fn readers_of_a_node_carry_what_the_apps_say() {
    use hushmic::pipewire::parse_readers;
    let hm = parse_readers(READERS_DUMP, "hushmic_source");
    assert_eq!(hm.len(), 2, "one entry per reader, not per link: {hm:?}");
    let ff = &hm[0];
    assert_eq!(ff.app_name.as_deref(), Some("Firefox"));
    assert_eq!(ff.binary.as_deref(), Some("firefox"));
    assert_eq!(ff.pid, Some(4242));
    assert_eq!(ff.state, "running");
    assert_eq!(hm[1].pid, Some(77), "a pid given as a string parses");
    assert_eq!(hm[1].state, "idle");

    let raw = parse_readers(READERS_DUMP, "alsa_input.rode");
    let by = |n: &str| raw.iter().find(|r| r.node_name == n).unwrap();
    assert_eq!(
        by("zoom-capture").target.as_deref(),
        Some("alsa_input.rode")
    );
    assert_eq!(
        by("zoom-capture").flatpak_id.as_deref(),
        Some("us.zoom.Zoom")
    );
    assert!(by("peak detect").monitor);
    assert!(by("input.loopback-1").stage);
    assert!(!by("zoom-capture").stage && !by("zoom-capture").monitor);

    // What the daemon makes of it (no desktop entries to consult).
    let names = hushmic::apps::Names::with_dirs(vec![]);
    let users = hushmic::apps::using(&hm, &|_| false, &names);
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].name.as_deref(), Some("Firefox"));
    let direct = hushmic::apps::using(&raw, &|_| false, &names);
    assert_eq!(direct.len(), 1, "only Zoom: {direct:?}");
    assert_eq!(direct[0].id, "us.zoom.Zoom");

    assert!(parse_readers("not json", "x").is_empty());
    assert!(parse_readers(READERS_DUMP, "nobody").is_empty());

    // Off remembers the streams by node, and knows them again (UX panel A1)
    assert_eq!(hm[0].id, 70);
    let users = hushmic::apps::app_streams(&hm, &|_| false);
    assert_eq!(
        users.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![70, 75],
        "the corked Chrome tab too"
    );
    let all = hushmic::pipewire::parse_capture_streams(READERS_DUMP);
    assert_eq!(all.len(), 6, "every capture stream, linked or not");
    let ff = all.iter().find(|s| s.id == 70).unwrap();
    assert_eq!((ff.node_name.as_str(), ff.pid), ("Firefox", Some(4242)));
    assert_eq!(all.iter().find(|s| s.id == 75).unwrap().pid, Some(77));
    assert!(hushmic::pipewire::parse_capture_streams("not json").is_empty());
    // every stream Off remembers is known again in the next read: a
    // stream's process on its client only too (A107 SF-A1)
    for u in &users {
        assert!(all.iter().any(|s| u.same(s)), "{u:?}");
    }
}

/// A107 SF-A1: a native PipeWire stream (pw-record, a PipeWire app) has
/// its process on its client, not on its node; On must still know it
/// again, by the client's process or by its serial.
#[test]
fn a_native_streams_process_and_serial_are_read() {
    let dump = r#"[
      {"id": 31, "type": "PipeWire:Interface:Client",
       "info": {"props": {"application.name": "pw-record", "application.process.id": 3100}}},
      {"id": 60, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Stream/Input/Audio", "node.name": "pw-record",
                          "client.id": 31, "object.serial": "812"}}}
    ]"#;
    let s = hushmic::pipewire::parse_capture_streams(dump);
    assert_eq!(s.len(), 1);
    assert_eq!(
        (s[0].id, s[0].pid, s[0].serial),
        (60, Some(3100), Some(812))
    );
}

/// What a reader says when the node itself does not: app properties on
/// its client (a Flatpak's portal app id), a target as old PipeWire writes
/// it (`node.target`), and targets the session manager keeps in the
/// `default` metadata (a stream moved in pavucontrol).
const READERS_ELSEWHERE_DUMP: &str = r#"[
  { "id": 46, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Audio/Source", "node.name": "alsa_input.rode" } } },
  { "id": 95, "type": "PipeWire:Interface:Client", "info": {
    "props": { "application.name": "Zoom Workplace", "application.process.binary": "zoom",
      "application.process.id": 5150, "pipewire.access.portal.app_id": "us.zoom.Zoom" } } },
  { "id": 90, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Stream/Input/Audio", "node.name": "zoom-capture",
      "client.id": 95 } } },
  { "id": 91, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Stream/Input/Audio", "node.name": "old",
      "application.name": "Old App", "node.target": "alsa_input.rode" } } },
  { "id": 92, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Stream/Input/Audio", "node.name": "moved",
      "application.name": "Discord" } } },
  { "id": 93, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Stream/Input/Audio", "node.name": "by-id",
      "application.name": "OBS" } } },
  { "id": 94, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Stream/Input/Audio", "node.name": "plasma meter",
      "application.name": "Plasma", "application.id": "org.kde.plasma-pa" } } },
  { "id": 96, "type": "PipeWire:Interface:Node", "info": { "state": "running",
    "props": { "media.class": "Stream/Input/Audio", "node.name": "follower",
      "application.name": "Firefox", "target.object": "-1" } } },
  { "id": 30, "type": "PipeWire:Interface:Metadata", "props": { "metadata.name": "default" },
    "metadata": [
      { "subject": 0, "key": "default.audio.source", "type": "Spa:String:JSON",
        "value": { "name": "alsa_input.rode" } },
      { "subject": 92, "key": "target.object", "type": null, "value": "alsa_input.rode" },
      { "subject": 93, "key": "target.node", "type": null, "value": 46 } ] },
  { "id": 80, "type": "PipeWire:Interface:Link", "info": { "output-node-id": 46, "input-node-id": 90 } },
  { "id": 81, "type": "PipeWire:Interface:Link", "info": { "output-node-id": 46, "input-node-id": 91 } },
  { "id": 82, "type": "PipeWire:Interface:Link", "info": { "output-node-id": 46, "input-node-id": 92 } },
  { "id": 83, "type": "PipeWire:Interface:Link", "info": { "output-node-id": 46, "input-node-id": 93 } },
  { "id": 84, "type": "PipeWire:Interface:Link", "info": { "output-node-id": 46, "input-node-id": 94 } },
  { "id": 85, "type": "PipeWire:Interface:Link", "info": { "output-node-id": 46, "input-node-id": 96 } }
]"#;

#[test]
fn readers_learn_from_their_client_and_the_default_metadata() {
    use hushmic::pipewire::parse_readers;
    let raw = parse_readers(READERS_ELSEWHERE_DUMP, "alsa_input.rode");
    let by = |n: &str| raw.iter().find(|r| r.node_name == n).unwrap();
    let zoom = by("zoom-capture");
    assert_eq!(zoom.flatpak_id.as_deref(), Some("us.zoom.Zoom"));
    assert_eq!(zoom.app_name.as_deref(), Some("Zoom Workplace"));
    assert_eq!(zoom.pid, Some(5150));
    assert_eq!(zoom.target, None, "follows the default");
    assert_eq!(by("old").target.as_deref(), Some("alsa_input.rode"));
    assert_eq!(by("moved").target.as_deref(), Some("alsa_input.rode"));
    assert_eq!(by("by-id").target.as_deref(), Some("46"));
    assert_eq!(by("follower").target, None, "-1 is no target");
    assert_eq!(
        by("plasma meter").app_id.as_deref(),
        Some("org.kde.plasma-pa")
    );

    // The notice: the meter is no app; only Zoom and Firefox follow the
    // default (the switch would move them).
    let names = hushmic::apps::Names::with_dirs(vec![]);
    let mut w = hushmic::apps::DirectWatch::default();
    let show = |_: &str| false;
    let none = |_: &hushmic::pipewire::Reader| false;
    let t0 = std::time::Instant::now();
    w.observe_at(&raw, true, &show, &none, &names, t0);
    let direct = w.observe_at(
        &raw,
        true,
        &show,
        &none,
        &names,
        t0 + hushmic::apps::DIRECT_AFTER,
    );
    let follows: Vec<(&str, bool)> = direct
        .iter()
        .map(|d| (d.app.name.as_deref().unwrap_or("?"), d.follows_default))
        .collect();
    assert_eq!(
        follows,
        vec![
            ("Zoom Workplace", true),
            ("Old App", false),
            ("Discord", false),
            ("OBS", false),
            ("Firefox", true),
        ]
    );
}

#[test]
fn a_dump_printed_while_objects_come_and_go_reads_as_one_graph() {
    // pw-dump 1.0 prints the graph and then, for objects removed or changed
    // while it ran (a concurrent pw-dump or pw-metadata is a PipeWire client
    // that ends mid-dump), more arrays in its monitor format, also before
    // the graph. Shape captured on PipeWire 1.0.5 with clients churning.
    let raw = format!(
        "[\n  {{\n    \"id\": 44,\n    \"info\": null\n  }}\n]\n{PWDUMP}\n[\n  {{\n    \"id\": 52,\n    \"info\": null\n  }}\n]\n[\n  {{\n    \"id\": 99,\n    \"info\": null\n  }}\n]\n"
    );
    assert!(serde_json::from_str::<serde_json::Value>(&raw).is_err());
    let merged = merge_dump_batches(&raw).expect("batches merge");
    let nodes = parse_pwdump_nodes(&merged);
    assert!(nodes.iter().any(|s| s.name == "hushmic_source"));
    assert_eq!(parse_node_id(&merged, "hushmic_source"), Some(39));
    // The node removed after the graph was printed is gone.
    let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
    assert!(!v
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o.get("id").and_then(|i| i.as_u64()) == Some(52)));
    // An object that changed replaces its old copy, in place.
    let changed = format!(
        "{PWDUMP}\n[ {{ \"id\": 46, \"type\": \"PipeWire:Interface:Node\", \"info\": {{ \"props\": {{ \"media.class\": \"Audio/Source\", \"node.name\": \"usb_mic_renamed\" }} }} }} ]"
    );
    let merged = merge_dump_batches(&changed).unwrap();
    assert_eq!(parse_node_id(&merged, "usb_mic_renamed"), Some(46));
    assert_eq!(
        parse_node_id(
            &merged,
            "alsa_input.usb-RODE_Microphones_RODE_NT-USB-00.analog-stereo"
        ),
        None
    );
    // One array, or something that is not arrays of objects: not ours.
    assert_eq!(merge_dump_batches(PWDUMP), None);
    assert_eq!(merge_dump_batches("[ ] { }"), None);
    assert_eq!(merge_dump_batches("[ { \"id\": 1 } ] [ 3 ]"), None);
}

fn graph_dump(links: &[(u64, u64, u64, bool)], host: bool) -> String {
    let node = |id: u64, name: &str| {
        format!(
            r#"{{ "id": {id}, "type": "PipeWire:Interface:Node", "info": {{ "props": {{ "node.name": "{name}" }} }} }}"#
        )
    };
    let mut v = vec![
        node(1, "hushmic_input"),
        node(2, "hushmic_dsp_out"),
        node(9, "speakers"),
    ];
    if host {
        v.push(node(3, "hushmic_feed"));
        v.push(node(4, "hushmic_source"));
    }
    for (id, from, to, passive) in links {
        v.push(format!(
            r#"{{ "id": {id}, "type": "PipeWire:Interface:Link", "info": {{ "output-node-id": {from}, "input-node-id": {to}, "props": {{ "link.passive": {passive} }} }} }}"#
        ));
    }
    format!("[{}]", v.join(","))
}

#[test]
fn the_chain_is_up_only_through_one_passive_link_into_the_host() {
    let g = pipewire::chain_graph(&graph_dump(&[(10, 2, 3, true)], true));
    assert!(g.chain() && g.host() && g.linked && g.up());
    assert!(g.foreign.is_empty());
    // 0.3.x prints the flag as a string
    let s = graph_dump(&[(10, 2, 3, true)], true).replace("true }", "\"true\" }");
    assert!(pipewire::chain_graph(&s).linked);

    // not passive: it would keep the mic open; it goes and is made again
    let g = pipewire::chain_graph(&graph_dump(&[(10, 2, 3, false)], true));
    assert!(!g.linked && !g.up());
    assert_eq!(g.foreign, vec![10]);

    // ours plus a second one, and a route of the output to the speakers
    let g = pipewire::chain_graph(&graph_dump(
        &[(10, 2, 3, true), (11, 2, 3, true), (12, 2, 9, false)],
        true,
    ));
    assert!(g.up());
    assert_eq!(g.foreign, vec![11, 12]);

    // the host gone: the chain is there, nothing is up
    let g = pipewire::chain_graph(&graph_dump(&[], false));
    assert!(g.chain() && !g.host() && !g.up());
    assert_eq!(
        pipewire::chain_graph("not json"),
        pipewire::ChainGraph::default()
    );
}

#[test]
fn links_are_read_as_from_and_into_nodes() {
    let listing = "  43 hushmic_dsp_out:output_MONO
  48   |->   44 hushmic_feed:input_MONO
  60 Firefox:output_FL
  61   |->   41 hushmic_input:input_MONO
  62   |->   70 Built-in Audio Analog Stereo:playback_FL
  44 hushmic_feed:input_MONO
  48   |<-   43 hushmic_dsp_out:output_MONO
";
    assert_eq!(
        hushmic::link::links_between(listing),
        vec![
            (48, "hushmic_dsp_out".into(), "hushmic_feed".into()),
            (61, "Firefox".into(), "hushmic_input".into()),
            (62, "Firefox".into(), "Built-in Audio Analog Stereo".into()),
        ]
    );
}

#[test]
fn a_stream_linked_where_none_belongs_is_an_intruder() {
    use hushmic::pipewire::{Cut, GuardStep, Intruder, IntruderGuard};
    let node = |id: u64, name: &str, class: &str, extra: &str| {
        format!(
            r#"{{ "id": {id}, "type": "PipeWire:Interface:Node", "info": {{ "props": {{ "node.name": "{name}", "media.class": "{class}"{extra} }} }} }}"#
        )
    };
    let link = |id: u64, from: u64, to: u64| {
        format!(
            r#"{{ "id": {id}, "type": "PipeWire:Interface:Link", "info": {{ "output-node-id": {from}, "input-node-id": {to}, "props": {{ "link.passive": true }} }} }}"#
        )
    };
    let dump = |links: &[String]| {
        let mut v = vec![
            node(1, "hushmic_input", "Stream/Input/Audio", ""),
            node(2, "hushmic_dsp_out", "Stream/Output/Audio", ""),
            node(3, "hushmic_feed", "Stream/Input/Audio", ""),
            node(4, "hushmic_source", "Audio/Source/Virtual", ""),
            node(5, "virt_mic", "Audio/Source", ""),
            node(6, "hushmic_playback", "Stream/Output/Audio", ""),
            node(7, "alsa_output.dac", "Audio/Sink", ""),
            node(
                8,
                "firefox",
                "Stream/Output/Audio",
                r#", "application.name": "Firefox", "object.serial": 881"#,
            ),
            node(9, "obs", "Stream/Input/Audio", r#", "object.serial": 991"#),
            link(20, 2, 3),
            r#"{ "id": 30, "type": "PipeWire:Interface:Client", "info": { "props": { "application.name": "WirePlumber" } } }"#.to_string(),
        ];
        v.extend(links.iter().cloned());
        format!("[{}]", v.join(","))
    };
    // the mic into the capture, the playback into the output, an app on
    // HushMic's source: all as it should be
    let g = pipewire::chain_graph(&dump(&[link(21, 5, 1), link(22, 6, 7), link(23, 4, 9)]));
    assert!(g.up() && g.intruders.is_empty() && g.foreign.is_empty());
    // a playback into the capture: an app's sound would go into the call
    let firefox = Intruder {
        id: 8,
        serial: Some(881),
        name: "Firefox".into(),
    };
    let cut = |link: u32, into: &str, by_wp: bool| Cut {
        link,
        from: "firefox".into(),
        into: into.into(),
        stream: 8,
        by_wp,
    };
    // WirePlumber's link (it followed a target), one made by hand
    let by_wp = link(24, 8, 1).replace(r#""link.passive": true"#, r#""client.id": 30"#);
    let g = pipewire::chain_graph(&dump(&[link(21, 5, 1), by_wp.clone()]));
    assert_eq!(g.intruders, vec![firefox.clone()]);
    assert_eq!(g.cuts, vec![cut(24, "hushmic_input", true)]);
    assert!(g.foreign.is_empty());
    // into the source host's feed: the same; the chain's own link stays
    let g = pipewire::chain_graph(&dump(&[link(27, 8, 3)]));
    assert!(g.linked);
    assert_eq!(g.intruders, vec![firefox.clone()]);
    assert_eq!(g.cuts, vec![cut(27, "hushmic_feed", false)]);
    // WirePlumber's link is left to the move once (cut under it, the
    // stream would be linked nowhere on 0.4); a hand-made one goes at once
    let mut guard = IntruderGuard::default();
    let both = [
        cut(24, "hushmic_input", true),
        cut(27, "hushmic_feed", false),
    ];
    assert_eq!(
        guard.to_cut(&both, &[8]),
        vec![(27, "firefox".into(), "hushmic_feed".into())]
    );
    assert_eq!(
        guard.to_cut(&both[..1], &[8]),
        vec![(24, "firefox".into(), "hushmic_input".into())]
    );
    assert_eq!(guard.to_cut(&both[..1], &[]).len(), 1);
    // a recorder or a mixer's peak meter on the capture or on HushMic
    // Speakers' playback takes sound, gives none: left alone
    let g = pipewire::chain_graph(&dump(&[link(25, 1, 9), link(26, 6, 9)]));
    assert!(g.intruders.is_empty() && g.cuts.is_empty() && g.foreign.is_empty());
    assert!(g.streams.contains(&(8, Some(881))) && g.streams.contains(&(9, Some(991))));

    // across looks: moved at once; the same intrusion seen again before
    // the move lands is not a second one, three looks in a row are
    let mut guard = IntruderGuard::default();
    let there = [(8, Some(881))];
    let one = std::slice::from_ref(&firefox);
    assert_eq!(guard.observe(one, &there).len(), 1);
    assert_eq!(guard.observe(one, &there), vec![]);
    assert!(guard.kept_off().is_empty());
    assert_eq!(guard.observe(one, &there).len(), 2);
    assert_eq!(guard.kept_off(), vec!["Firefox".to_string()]);
    // back after a look without it: moved again and kept off, once; then
    // only its link goes; forgotten once it ends
    let mut guard = IntruderGuard::default();
    let there = [(8, Some(881))];
    let moved = GuardStep::Move {
        id: 8,
        name: "Firefox".into(),
    };
    let one = std::slice::from_ref(&firefox);
    assert_eq!(guard.observe(one, &there), vec![moved.clone()]);
    assert!(guard.kept_off().is_empty());
    assert_eq!(guard.observe(&[], &there), vec![]);
    assert_eq!(
        guard.observe(one, &there),
        vec![
            moved,
            GuardStep::KeptOff {
                name: "Firefox".into()
            }
        ]
    );
    assert_eq!(guard.kept_off(), vec!["Firefox".to_string()]);
    assert_eq!(guard.observe(one, &there), vec![]);
    // ended; another stream given its id is a new one
    assert_eq!(guard.observe(&[], &[]), vec![]);
    assert!(guard.kept_off().is_empty());
    let other = Intruder {
        serial: Some(900),
        ..firefox
    };
    assert_eq!(guard.observe(&[other], &[(8, Some(900))]).len(), 1);
}

#[test]
fn stream_controls_read_mute_and_the_mixer_volume() {
    let dump = r#"[
      { "id": 1, "type": "PipeWire:Interface:Node", "info": { "props": { "node.name": "hushmic_dsp_out" },
        "params": { "Props": [ { "volume": 1.0, "mute": true, "channelVolumes": [ 0.064 ] }, { "params": [] } ] } } },
      { "id": 2, "type": "PipeWire:Interface:Node", "info": { "props": { "node.name": "hushmic_input" },
        "params": { "Props": [ { "volume": 1.0, "mute": false, "channelVolumes": [ 1.0 ] } ] } } },
      { "id": 3, "type": "PipeWire:Interface:Node", "info": { "props": { "node.name": "speakers" },
        "params": { "Props": [ { "mute": true } ] } } }
    ]"#;
    let c = pipewire::stream_controls(dump);
    assert_eq!(c.len(), 2, "{c:?}");
    assert_eq!(c[0].name, "hushmic_input");
    assert!(!c[0].muted && (c[0].volume - 1.0).abs() < 1e-6);
    assert_eq!(c[1].name, "hushmic_dsp_out");
    assert!(c[1].muted);
    assert!((c[1].volume - 0.4).abs() < 1e-3, "{}", c[1].volume);
    assert!(pipewire::stream_controls("[]").is_empty());
}

/// UX panel round 2, A5 and A107 SF-G1: what in `pw-mon`'s report makes
/// the graph worth a read now: nodes and devices, and a link added, never
/// the clients our own reads are.
#[test]
fn the_monitor_pokes_on_nodes_devices_and_new_links_only() {
    use hushmic::pipewire::GraphEvents;
    let feed =
        |g: &mut GraphEvents, text: &str| -> usize { text.lines().filter(|l| g.feed(l)).count() };
    let mut g = GraphEvents::default();
    // a pw-dump's own client, added and removed: nothing
    let client = "added:\n\tid: 70\n\tpermissions: rwxm\n\ttype: PipeWire:Interface:Client (version 3)\n\tproperties:\n\t\tapplication.name = \"pw-dump\"\n";
    assert_eq!(feed(&mut g, client), 0);
    assert_eq!(feed(&mut g, "removed:\n\tid: 70\n"), 0);
    // its core, its ports, a module: nothing
    for t in ["Core (version 4)", "Port (version 3)", "Module (version 3)"] {
        let r = format!("added:\n\tid: 71\n\ttype: PipeWire:Interface:{t}\n");
        assert_eq!(feed(&mut g, &r), 0, "{t}");
    }
    // a link added: once (it may put a stream into HushMic's capture);
    // its changes and its removal: nothing
    let link = "added:\n\tid: 72\n\ttype: PipeWire:Interface:Link (version 3)\n";
    assert_eq!(feed(&mut g, link), 1);
    assert!(g.take_link_added() && !g.take_link_added());
    assert_eq!(
        feed(&mut g, "changed:\n\tid: 72\n*\tstate: \"active\"\n"),
        0
    );
    assert_eq!(feed(&mut g, "removed:\n\tid: 72\n"), 0);
    // a microphone's node added: once, its properties never again
    let node = "added:\n\tid: 80\n\tpermissions: r-xm-\n\ttype: PipeWire:Interface:Node (version 3)\n\tproperties:\n\t\tid: 3\n\t\ttype: x\n";
    assert_eq!(feed(&mut g, node), 1);
    // changed (fields marked *) and removed, by its id
    assert_eq!(
        feed(&mut g, "changed:\n\tid: 80\n*\tstate: \"running\"\n"),
        1
    );
    assert_eq!(feed(&mut g, "removed:\n\tid: 80\n"), 1);
    // a device, and a removal whose own record names the type
    assert_eq!(
        feed(
            &mut g,
            "added:\n\tid: 90\n\ttype: PipeWire:Interface:Device (version 3)\n"
        ),
        1
    );
    assert_eq!(
        feed(
            &mut g,
            "removed:\n\tid: 91\n\ttype: PipeWire:Interface:Node (version 3)\n"
        ),
        1
    );
    // a line inside a property list that reads like a record start
    assert_eq!(feed(&mut g, "\t\t*\tnode.name = \"added:\"\n"), 0);
}

/// WirePlumber 0.4.17's `default-nodes` after a session that ended with
/// HushMic as the default (UX panel A26: the restore never got saved).
const WP_DEFAULT_NODES: &str = "[default-nodes]
default.configured.audio.sink=alsa_output.pci-0000_00_1b.0.analog-stereo
default.configured.audio.sink.0=alsa_output.pci-0000_00_1b.0.analog-stereo
default.configured.audio.source=hushmic_source
default.configured.audio.source.0=hushmic_source
default.configured.audio.source.2=alsa_input.usb-old_headset
default.configured.audio.source.1=virt_mic
default.configured.audio.source.10=alsa_input.pci.analog-stereo.monitor
default.configured.audio.source.x=junk
default.configured.audio.source.3=Weird\\smic\\\\name
";

#[test]
fn wireplumbers_source_history_reads_newest_first() {
    assert_eq!(
        pipewire::parse_wp_source_history(WP_DEFAULT_NODES),
        vec![
            "hushmic_source",
            "virt_mic",
            "alsa_input.usb-old_headset",
            "Weird mic\\name",
            "alsa_input.pci.analog-stereo.monitor",
        ]
    );
    // 0.5: the configured one first, `.0` is the one before
    let v05 = "[default-nodes]\ndefault.configured.audio.source=hushmic_source\n\
               default.configured.audio.source.0=laptop_mic\n";
    assert_eq!(
        pipewire::parse_wp_source_history(v05),
        vec!["hushmic_source", "laptop_mic"]
    );
    assert!(pipewire::parse_wp_source_history("").is_empty());
}

#[test]
fn the_prior_mic_is_the_newest_real_one_plugged_in_or_not() {
    // ours is skipped, so are monitors; whether it is plugged in does not
    // count (A118 SF-T1: a stand-in restored stays the default for good)
    let history = pipewire::parse_wp_source_history(WP_DEFAULT_NODES);
    assert_eq!(
        pipewire::prior_from_history(&history).as_deref(),
        Some("virt_mic")
    );
    let only_ours = ["hushmic_source".to_string(), "x.monitor".to_string()];
    assert_eq!(pipewire::prior_from_history(&only_ours), None);
    assert_eq!(pipewire::prior_from_history(&[]), None);
}
