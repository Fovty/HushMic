use hushmic::config::Config;
use hushmic::controller::{render_conf, Paths, RunMode, LATENCY_SAMPLES};
use std::path::PathBuf;

fn test_paths() -> Paths {
    Paths {
        plugin_so: PathBuf::from("/usr/lib/ladspa/libdpdfnet_ladspa.so"),
        model_dir: PathBuf::from("/usr/share/hushmic/models"),
        dylib: PathBuf::from("/usr/lib/hushmic/libonnxruntime.so"),
    }
}

#[test]
fn latency_node_rendered_only_when_supported() {
    let cfg = Config::default();
    // Gate ON (PipeWire >= 1.6): the report-only delay node + link appear,
    // with the latency in seconds derived from the samples constant.
    let on = render_conf(&cfg, &test_paths(), false, RunMode::Suppress, true);
    assert!(on.contains("name   = hushmic_latency"), "{on}");
    assert!(on.contains("label  = delay"), "{on}");
    assert!(
        on.contains("\"latency\" = 0.08"),
        "latency must render as seconds: {on}"
    );
    assert!(
        on.contains("\"Delay (s)\" = 0.0"),
        "the node must never actually delay audio: {on}"
    );
    assert!(
        on.contains(r#"{ output = "hushmic_dsp:Output" input = "hushmic_latency:In" }"#),
        "explicit link so the graph stays deterministic: {on}"
    );
    // Gate OFF (< 1.6, e.g. 0.3.48 has no delay builtin at all): the conf
    // must stay byte-identical to the single-node graph of today.
    let off = render_conf(&cfg, &test_paths(), false, RunMode::Suppress, false);
    assert!(!off.contains("hushmic_latency"), "{off}");
    assert!(!off.contains("links"), "{off}");
}

#[test]
fn latency_constant_and_rendered_seconds_agree() {
    // 3840 samples @ 48 kHz = 0.08 s; the render derives one from the
    // other so they cannot drift apart. The 3840 = engine 2400 (pinned
    // against the MEASURED DSP by the hushmic-denoiser latency tests) +
    // 1440 async output lead (PLUGIN_LATENCY_SAMPLES in dpdfnet-ladspa,
    // pinned end-to-end by its asset-gated latency test).
    assert_eq!(LATENCY_SAMPLES, 3840);
    let secs = LATENCY_SAMPLES as f64 / 48_000.0;
    let on = render_conf(
        &Config::default(),
        &test_paths(),
        false,
        RunMode::Suppress,
        true,
    );
    assert!(on.contains(&format!("\"latency\" = {secs}")), "{on}");
}

/// The free idle (docs/superpowers/idle-mode-design.md): with nothing
/// recording from HushMic, PipeWire must not schedule the chain at all.
/// That rests on the capture stream being passive (its link to the mic
/// does not make the chain runnable) and on nothing forcing the chain to
/// run without consumers. Measured on PipeWire 0.3.65, 1.0.5 and 1.6.9:
/// 0 % CPU idle; a chain without `node.passive` would run whenever the
/// mic does. Every rendering variant must keep both.
#[test]
fn every_conf_lets_pipewire_idle_the_chain() {
    let mics = [None, Some("alsa_input.realmic".to_string())];
    let modes = [RunMode::Suppress, RunMode::Bypass, RunMode::Mute];
    for mic in &mics {
        for legacy in [false, true] {
            for report_latency in [false, true] {
                for mode in modes {
                    let cfg = Config {
                        mic: mic.clone(),
                        ..Config::default()
                    };
                    let c = render_conf(&cfg, &test_paths(), legacy, mode, report_latency);
                    let capture = c
                        .split("capture.props = {")
                        .nth(1)
                        .and_then(|rest| rest.split("playback.props").next())
                        .expect("capture.props block");
                    assert!(
                        capture.contains("node.passive   = true"),
                        "capture stream must stay passive: {c}"
                    );
                    let playback = c.split("playback.props = {").nth(1).expect("playback");
                    // A passive source would not run for its consumers.
                    assert!(
                        !playback.contains("node.passive"),
                        "the source itself must not be passive: {c}"
                    );
                    // Each of these forces the chain to run, or joins it to
                    // a scheduling group that a mic consumer keeps running.
                    for forbidden in [
                        "node.always-process",
                        "node.want-driver",
                        "node.suspend-on-idle",
                        "node.pause-on-idle",
                        "node.link-group",
                        "node.group",
                    ] {
                        assert!(
                            !c.contains(forbidden),
                            "{forbidden} changes when PipeWire schedules the chain: {c}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn conf_renders_the_run_mode() {
    let cfg = Config::default();
    let paths = Paths {
        plugin_so: PathBuf::from("/usr/lib/ladspa/libdpdfnet_ladspa.so"),
        model_dir: PathBuf::from("/usr/share/hushmic/models"),
        dylib: PathBuf::from("/usr/lib/hushmic/libonnxruntime.so"),
    };
    let c = render_conf(&cfg, &paths, false, RunMode::Suppress, false);
    assert!(c.contains("\"Mode\" = 0"), "suppress renders Mode 0: {c}");
    // A chain spawned while Mute is selected must be BORN muted (no
    // unmuted window between spawn and a later set-param).
    let c = render_conf(&cfg, &paths, false, RunMode::Mute, false);
    assert!(c.contains("\"Mode\" = 2"), "mute renders Mode 2: {c}");
    let c = render_conf(&cfg, &paths, false, RunMode::Bypass, false);
    assert!(c.contains("\"Mode\" = 1"), "bypass renders Mode 1: {c}");
}

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
    let c = render_conf(&cfg, &paths, false, RunMode::Suppress, false);
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

    // Issue #10: the source node must pin the graph quantum — with the
    // async DSP the pin's job is to make the quantum KNOWN and SMALL so
    // the plugin's output margin (and the declared latency) is exact.
    assert!(
        c.contains("node.force-quantum = 480"),
        "quantum pin missing from playback props"
    );

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
    let c = render_conf(&cfg, &paths, false, RunMode::Suppress, false);
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
    let c = render_conf(&cfg, &paths, false, RunMode::Suppress, false);
    assert!(
        !c.contains("target.object"),
        "target.object must be absent when no mic chosen"
    );
}

#[test]
fn legacy_node_target_only_on_old_pipewire() {
    let cfg = Config {
        mic: Some("alsa_input.realmic".into()),
        ..Config::default()
    };
    let paths = Paths {
        plugin_so: PathBuf::from("/usr/lib/ladspa/libdpdfnet_ladspa.so"),
        model_dir: PathBuf::from("/usr/share/hushmic/models"),
        dylib: PathBuf::from("/usr/lib/hushmic/libonnxruntime.so"),
    };
    // Modern PipeWire: target.object only, byte-for-byte as before.
    let modern = render_conf(&cfg, &paths, false, RunMode::Suppress, false);
    assert!(modern.contains("target.object  = \"alsa_input.realmic\""));
    assert!(
        !modern.contains("node.target"),
        "node.target must NOT appear on modern PipeWire"
    );
    // Old PipeWire (< 0.3.64): also emit the legacy node.target so the mic pins.
    let legacy = render_conf(&cfg, &paths, true, RunMode::Suppress, false);
    assert!(legacy.contains("target.object  = \"alsa_input.realmic\""));
    assert!(
        legacy.contains("node.target     = \"alsa_input.realmic\""),
        "node.target must be emitted on old PipeWire: {legacy}"
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

// --- Controller::active_mic() lifecycle (mic recovery) ----------------------
// The recovery state machine keys off which mic the RUNNING chain was
// rendered with. Safe to test without spawning anything: enable() preflights
// the assets first, so nonexistent paths fail it before any side effect.
#[test]
fn active_mic_none_initially_and_after_failed_enable_and_disable() {
    use hushmic::controller::Controller;
    let paths = Paths {
        plugin_so: PathBuf::from("/nonexistent/plugin.so"),
        model_dir: PathBuf::from("/nonexistent/models"),
        dylib: PathBuf::from("/nonexistent/libonnxruntime.so"),
    };
    let mut c = Controller::new(paths);
    assert_eq!(c.active_mic(), None);
    // A failed enable must not leave a phantom active mic behind — the
    // recovery machine would judge switches against a chain that isn't up.
    let cfg = Config {
        mic: Some("some-mic".into()),
        set_default: false,
        ..Config::default()
    };
    assert!(
        c.enable(&cfg).is_err(),
        "preflight must fail on missing assets"
    );
    assert_eq!(c.active_mic(), None);
    let _ = c.disable();
    assert_eq!(c.active_mic(), None);
}

// --- per-mic prefs reach the rendered conf ----------------------------------
// enable() renders from an adjusted Config carrying effective_settings():
// a pinned mic with a saved profile must produce a conf with THAT profile's
// attenuation, not the globals.
#[test]
fn per_mic_profile_drives_the_rendered_conf() {
    use hushmic::config::MicPrefs;
    let mut cfg = Config {
        mic: Some("alsa_input.rode".into()),
        attn_limit: 100.0,
        ..Config::default()
    };
    cfg.mic_prefs.insert(
        "alsa_input.rode".into(),
        MicPrefs {
            model: "dpdfnet2_48khz_hr".into(),
            attn_limit: 24.0,
        },
    );
    let p = cfg.active_profile(cfg.mic.as_deref(), None);
    let adjusted = Config {
        model: p.model,
        attn_limit: p.attn_limit,
        ..cfg.clone()
    };
    let paths = Paths {
        plugin_so: PathBuf::from("/usr/lib/ladspa/libdpdfnet_ladspa.so"),
        model_dir: PathBuf::from("/usr/share/hushmic/models"),
        dylib: PathBuf::from("/usr/lib/hushmic/libonnxruntime.so"),
    };
    let conf = render_conf(&adjusted, &paths, false, RunMode::Suppress, false);
    assert!(
        conf.contains("\"Attenuation Limit (dB)\" = 24"),
        "profile attn missing:\n{conf}"
    );
    assert!(conf.contains("alsa_input.rode"), "{conf}");
}

#[test]
fn every_restart_during_a_mic_fallback_follows_the_default() {
    use hushmic::controller::restart_config;
    use hushmic::pipewire::resolve_effective_mic;
    let cfg = Config {
        mic: Some("virt_mic".into()),
        ..Config::default()
    };
    // a respawn (or profile or settings restart) during the fallback, with
    // no device list: still the default, never the vanished mic
    let during = restart_config(&cfg, true);
    assert_eq!(during.mic, None);
    assert_eq!(resolve_effective_mic(during.mic.as_deref(), None), None);
    // the plain config would have pinned it again
    assert_eq!(
        resolve_effective_mic(cfg.mic.as_deref(), None).as_deref(),
        Some("virt_mic")
    );
    // everything else as saved
    assert_eq!(during.model, cfg.model);
    assert_eq!(during.attn_limit, cfg.attn_limit);
    assert_eq!(during.set_default, cfg.set_default);
    // outside a fallback (after the return, or a new pick): as saved
    assert_eq!(restart_config(&cfg, false).mic, cfg.mic);
}

#[test]
fn picking_the_same_mic_again_ends_a_recovery_fallback() {
    use hushmic::controller::{restart_config, settings_restart};
    let cfg = Config {
        mic: Some("mic_a".into()),
        ..Config::default()
    };
    // `config set mic mic_a` while the fallback is latched: the value does
    // not change, but the pick ends the fallback and restarts onto mic_a
    assert!(settings_restart(false, true));
    assert_eq!(restart_config(&cfg, false).mic.as_deref(), Some("mic_a"));
    // nothing changed and no fallback: no restart
    assert!(!settings_restart(false, false));
    // a value change restarts as before
    assert!(settings_restart(true, false));
    // the controller's latch: set by the fallback, ended by a pick once
    let mut c = hushmic::controller::Controller::new(test_paths());
    assert!(!c.end_mic_fallback());
    c.set_mic_fallback(true);
    assert!(
        c.end_mic_fallback(),
        "a latched fallback ends with the pick"
    );
    assert!(!c.end_mic_fallback(), "and only once");
}
