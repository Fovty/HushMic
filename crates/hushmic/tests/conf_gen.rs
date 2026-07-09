use hushmic::config::Config;
use hushmic::controller::{render_conf, Paths};
use std::path::PathBuf;

#[test]
fn conf_contains_required_fields() {
    let cfg = Config {
        mic: Some("alsa_input.realmic".into()),
        attn_limit: 24.0,
        ..Config::default()
    };
    let paths = Paths {
        plugin_so: PathBuf::from("/usr/lib/ladspa/libdpdfnet_ladspa.so"),
        model_dir: PathBuf::from("/usr/share/hushmic/models"),
        dylib: PathBuf::from("/usr/lib/hushmic/libonnxruntime.so"),
    };
    let c = render_conf(&cfg, &paths);
    assert!(c.contains("label  = \"dpdfnet_mono\""), "label missing");
    assert!(
        c.contains("/usr/lib/ladspa/libdpdfnet_ladspa.so"),
        "plugin path missing"
    );
    assert!(
        c.contains("\"Attenuation Limit (dB)\" = 24"),
        "attn control missing"
    );
    assert!(
        c.contains("target.object  = \"alsa_input.realmic\""),
        "mic pin missing"
    );
    assert!(
        c.contains("media.class      = Audio/Source"),
        "not exposed as a source"
    );
    assert!(c.contains("audio.rate     = 48000"));
    assert!(c.contains("node.name        = \"hushmic_source\""));

    // CRITICAL: `pipewire -c <conf>` needs the core base modules,
    // otherwise it fails with "can't find protocol 'PipeWire:Protocol:Native'".
    // render_conf MUST emit a SELF-CONTAINED config, not a bare filter-chain
    // fragment. Assert the load-bearing base module is present.
    assert!(
        c.contains("libpipewire-module-protocol-native"),
        "self-contained base modules missing (would fail to load standalone)"
    );
}

#[test]
fn conf_escapes_hostile_values() {
    // Device node names come from hardware/user config: quotes or backslashes
    // must neither break the conf nor inject keys into it, and a hand-edited
    // non-finite attn_limit must not render as a literal `NaN` token.
    let cfg = Config {
        mic: Some(r#"evil" } inject = { x"#.into()),
        attn_limit: f32::NAN,
        ..Config::default()
    };
    let paths = Paths {
        plugin_so: PathBuf::from("/usr/lib/ladspa/libdpdfnet_ladspa.so"),
        model_dir: PathBuf::from("/usr/share/hushmic/models"),
        dylib: PathBuf::from("/usr/lib/hushmic/libonnxruntime.so"),
    };
    let c = render_conf(&cfg, &paths);
    assert!(
        c.contains(r#"target.object  = "evil\" } inject = { x""#),
        "quotes must be escaped: {c}"
    );
    assert!(!c.contains("NaN"), "non-finite attn must be clamped: {c}");
}

#[test]
fn conf_omits_target_when_no_mic() {
    // When no specific mic is chosen, there must be no target.object pin so the
    // filter-chain follows the system default capture device.
    let cfg = Config {
        mic: None,
        ..Config::default()
    };
    let paths = Paths {
        plugin_so: PathBuf::from("/usr/lib/ladspa/libdpdfnet_ladspa.so"),
        model_dir: PathBuf::from("/usr/share/hushmic/models"),
        dylib: PathBuf::from("/usr/lib/hushmic/libonnxruntime.so"),
    };
    let c = render_conf(&cfg, &paths);
    assert!(
        !c.contains("target.object"),
        "target.object must be absent when no mic chosen"
    );
}

#[test]
fn pin_decision_respects_existing_restrictions() {
    use hushmic::controller::pin_intersection;
    let all24: Vec<usize> = (0..24).collect();
    let p16: Vec<usize> = (0..16).collect();

    // 13700KF, unrestricted session: pin to the 16 P-threads.
    assert_eq!(pin_intersection(&p16, &all24), Some(p16.clone()));

    // User deliberately taskset hushmic onto the E-cores (allowed = 16-23):
    // the intersection is empty — their placement wins, no pin.
    let ecores: Vec<usize> = (16..24).collect();
    assert_eq!(pin_intersection(&p16, &ecores), None);

    // Straddling cgroup cpuset leaves only ONE P-core allowed (Arrow Lake
    // 265K, AllowedCPUs=0,8-19): pinning the whole host to a single shared
    // cpu is worse than not pinning — degenerate sets are refused.
    let straddle: Vec<usize> = std::iter::once(0).chain(8..20).collect();
    let p8: Vec<usize> = (0..8).collect();
    assert_eq!(pin_intersection(&p8, &straddle), None);

    // Session confined to exactly the P-cores already: nothing to narrow.
    assert_eq!(pin_intersection(&p16, &p16), None);

    // Meteor Lake-U (4 P-threads of 14 cpus): small but non-degenerate.
    let p4: Vec<usize> = (0..4).collect();
    let all14: Vec<usize> = (0..14).collect();
    assert_eq!(pin_intersection(&p4, &all14), Some(p4.clone()));
}

#[test]
fn kernel_cpu_lists_parse() {
    use hushmic::controller::parse_cpu_list;
    assert_eq!(parse_cpu_list("0-15"), (0..=15).collect::<Vec<_>>());
    assert_eq!(parse_cpu_list("0-3,8-11"), vec![0, 1, 2, 3, 8, 9, 10, 11]);
    assert_eq!(parse_cpu_list("7"), vec![7]);
    assert_eq!(parse_cpu_list("0-1, 4"), vec![0, 1, 4]);
    assert_eq!(parse_cpu_list(""), Vec::<usize>::new());
    assert_eq!(parse_cpu_list("garbage"), Vec::<usize>::new());
    assert_eq!(parse_cpu_list("5-2"), Vec::<usize>::new()); // inverted range
}

#[test]
fn prefix_derivation_from_binary_location() {
    use hushmic::controller::prefix_of;
    assert_eq!(
        prefix_of(std::path::Path::new("/usr/local/bin/hushmic")),
        Some(std::path::PathBuf::from("/usr/local"))
    );
    assert_eq!(
        prefix_of(std::path::Path::new("/home/u/.local/bin/hushmic")),
        Some(std::path::PathBuf::from("/home/u/.local"))
    );
    // non-installed layouts must not invent a prefix
    assert_eq!(
        prefix_of(std::path::Path::new("/repo/target/release/hushmic")),
        None
    );
}
