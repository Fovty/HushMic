use hushmic::pipewire::{
    parse_core_version, parse_metadata_value, parse_node_id, parse_pwdump_nodes,
    pw_version_at_least, resolve_effective_mic,
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

// --- live mode switching -----------------------------------------------------
#[test]
fn mode_param_arg_is_exact_spa_json() {
    // The precise SPA-JSON pw-cli set-param accepts (verified against a live
    // filter-chain); a syntax slip here fails silently at runtime.
    assert_eq!(
        hushmic::pipewire::mode_param_arg(2),
        r#"{ params = [ "hushmic_dsp:Mode" 2 ] }"#
    );
    assert_eq!(
        hushmic::pipewire::mode_param_arg(0),
        r#"{ params = [ "hushmic_dsp:Mode" 0 ] }"#
    );
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
