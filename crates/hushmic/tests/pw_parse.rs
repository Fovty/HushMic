use hushmic::pipewire::{
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
