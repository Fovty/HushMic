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
