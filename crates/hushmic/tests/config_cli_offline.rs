//! `hushmic config …` with no daemon: the file is read and written through
//! the same validation the daemon applies. Drives the real binary with a
//! private HOME/XDG tree, so no socket, no PipeWire, no real config.

use std::path::Path;
use std::process::Command;

fn run(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_hushmic"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("HUSHMIC_MODEL_DIR", home.join("models"))
        .env_remove("APPIMAGE")
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn config_set_and_get_work_on_the_file_without_a_daemon() {
    let home = std::env::temp_dir().join(format!("hushmic-offline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("run")).unwrap();
    std::fs::create_dir_all(home.join("models")).unwrap();
    std::fs::write(home.join("models/dpdfnet2_48khz_hr.onnx"), b"x").unwrap();

    let (code, out, err) = run(&home, &["config", "set", "attn_limit", "strong"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(
        out.trim(),
        "attn_limit = 24 (saved; applies when HushMic starts)"
    );
    let cfg_path = home.join(".config/hushmic/config.toml");
    let file = std::fs::read_to_string(&cfg_path).expect("config written under XDG_CONFIG_HOME");
    assert!(file.contains("attn_limit = 24.0"), "{file}");
    assert!(
        !file.contains("tray"),
        "defaults stay out of the file: {file}"
    );

    let (code, out, _) = run(&home, &["config", "get", "attn_limit"]);
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "24");
    let (code, out, _) = run(&home, &["config"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("attn_limit = 24\n") && out.contains("mic = default\n"),
        "{out}"
    );
    let (code, out, _) = run(&home, &["config", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["attn_limit"], 24.0);

    let (code, _, err) = run(&home, &["config", "set", "model", "nope"]);
    assert_eq!(code, 1);
    assert!(err.contains("nope.onnx"), "{err}");
    let (code, out, err) = run(&home, &["config", "set", "model", "dpdfnet2_48khz_hr"]);
    assert_eq!(code, 0, "{out}{err}");
    // the window's word for it, said back as typed (F13); the id is what
    // is written
    let (code, out, err) = run(&home, &["config", "set", "model", "light"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.starts_with("model = light"), "{out}");
    let (_, out, _) = run(&home, &["config", "get", "model"]);
    assert_eq!(out.trim(), "dpdfnet2_48khz_hr");
    let (code, _, err) = run(&home, &["config", "set", "model", "quality"]);
    assert_eq!(code, 1);
    assert!(
        err.starts_with("the quality model is not installed"),
        "{err}"
    );
    // out of range is refused, the file unchanged
    let (code, _, err) = run(&home, &["config", "set", "attn_limit", "9999"]);
    assert_eq!(code, 1);
    assert!(err.contains("must be 0-100"), "{err}");
    let (_, out, _) = run(&home, &["config", "get", "attn_limit"]);
    assert_eq!(out.trim(), "24");
    // the app's bookkeeping is not listed
    let (_, out, _) = run(&home, &["config"]);
    assert!(
        !out.contains("close_notice_sent") && !out.lines().any(|l| l.starts_with("enabled =")),
        "{out}"
    );
    let (code, _, err) = run(&home, &["config", "set", "enabled", "false"]);
    assert_eq!(code, 1);
    assert!(err.contains("hushmic mode"), "{err}");
    let (code, _, err) = run(&home, &["config", "set", "tray"]);
    assert_eq!(code, 1, "missing value is a usage error");
    assert!(err.contains("usage"), "{err}");
    let (code, out, _) = run(&home, &["config", "set", "tray", "off"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("tray = false"), "{out}");
    // inference: auto | onnx; the native engines are not settings.
    let (code, _, err) = run(&home, &["config", "set", "inference", "native"]);
    assert_eq!(code, 1);
    assert!(err.contains("auto or onnx"), "{err}");
    let (code, out, err) = run(&home, &["config", "set", "inference", "onnx"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.starts_with("inference = onnx"), "{out}");
    let text = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(text.contains("inference = \"onnx\""), "{text}");
    let (code, out, _) = run(&home, &["config", "get", "inference"]);
    assert_eq!((code, out.trim()), (0, "onnx"));
    let (code, _, _) = run(&home, &["config", "set", "inference", "auto"]);
    assert_eq!(code, 0);
    let text = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(!text.contains("inference"), "auto is not written: {text}");
    // autostart is the one offline key with a side effect: the entry.
    let entry = home.join(".config/autostart/hushmic.desktop");
    let (code, out, err) = run(&home, &["config", "set", "autostart", "true"]);
    assert_eq!(code, 0, "{out}{err}");
    // says what was done: the entry is written now
    assert!(
        out.starts_with("autostart = true (saved; HushMic starts when you log in)"),
        "{out}"
    );
    let text = std::fs::read_to_string(&entry).expect("autostart entry written");
    assert!(text.contains("--tray"), "{text}");
    let (code, _, _) = run(&home, &["config", "set", "autostart", "false"]);
    assert_eq!(code, 0);
    assert!(!entry.exists(), "entry removed");

    let (code, out, _) = run(&home, &["config", "path"]);
    assert_eq!(code, 0);
    assert_eq!(out.trim(), cfg_path.display().to_string());

    // an unrelated control verb still reports "not running"
    let (code, _, err) = run(&home, &["status"]);
    assert_eq!(code, 2, "{err}");
    let _ = std::fs::remove_dir_all(&home);
}

/// F13 / A106 N-T1: with a microphone of its own settings, `set` saves
/// model and strength for it; `--defaults` sets the defaults and leaves
/// the microphone's own alone. One parenthesis says both (N-C1); no value
/// after --defaults is a usage error (N-C2).
#[test]
fn config_set_defaults_leaves_a_microphones_own_alone() {
    let home =
        std::env::temp_dir().join(format!("hushmic-offline-defaults-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("run")).unwrap();
    std::fs::create_dir_all(home.join(".config/hushmic")).unwrap();
    std::fs::create_dir_all(home.join("models")).unwrap();
    std::fs::write(home.join("models/dpdfnet2_48khz_hr.onnx"), b"x").unwrap();
    let cfg_path = home.join(".config/hushmic/config.toml");
    std::fs::write(
        &cfg_path,
        "mic = \"usbmic\"\nattn_limit = 30.0\n\n[mic_prefs.usbmic]\n\
         model = \"dpdfnet2_48khz_hr\"\nattn_limit = 24.0\n",
    )
    .unwrap();
    let (code, out, err) = run(&home, &["config", "set", "attn_limit", "18"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(
        out.trim(),
        "attn_limit = 18 (saved for usbmic; the defaults stay 30; applies when HushMic starts)"
    );
    let (code, out, err) = run(&home, &["config", "set", "--defaults", "attn_limit", "12"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(
        out.trim(),
        "attn_limit = 12 (the defaults; microphones with their own settings keep theirs; \
         saved; applies when HushMic starts)"
    );
    let text = std::fs::read_to_string(&cfg_path).unwrap();
    let cfg: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(cfg["attn_limit"].as_float(), Some(12.0), "{text}");
    assert_eq!(
        cfg["mic_prefs"]["usbmic"]["attn_limit"].as_float(),
        Some(18.0),
        "the microphone keeps its own: {text}"
    );
    let (code, _, err) = run(&home, &["config", "set", "--defaults", "attn_limit"]);
    assert_eq!(code, 1);
    assert!(err.contains("usage: hushmic config"), "{err}");
    assert!(!err.contains("unknown key"), "{err}");
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(feature = "speakers")]
#[test]
fn the_speakers_command_works_on_the_file_without_a_daemon() {
    let home = std::env::temp_dir().join(format!("hushmic-offline-spk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("run")).unwrap();
    std::fs::create_dir_all(home.join("models")).unwrap();
    let (code, out, err) = run(&home, &["speakers"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(
        out,
        "speakers: off\noutput: default (the system output)\nstrength: maximum (100 dB)\n"
    );
    // nothing written by looking
    assert!(!home.join(".config/hushmic/config.toml").exists());
    for (args, says) in [
        (&["speakers", "on"][..], "speakers.enabled = true"),
        (
            &["speakers", "strength", "gentle"][..],
            "speakers.attn_limit = 6",
        ),
        (
            &["speakers", "output", "alsa_output.dac"][..],
            "speakers.output = alsa_output.dac",
        ),
    ] {
        let (code, out, err) = run(&home, args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        assert_eq!(
            out.trim(),
            format!("{says} (saved; applies when HushMic starts)"),
            "{args:?}"
        );
    }
    let text = std::fs::read_to_string(home.join(".config/hushmic/config.toml")).unwrap();
    let cfg: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(cfg["speakers"]["enabled"].as_bool(), Some(true), "{text}");
    assert_eq!(
        cfg["speakers"]["attn_limit"].as_float(),
        Some(6.0),
        "{text}"
    );
    assert_eq!(
        cfg["speakers"]["output"].as_str(),
        Some("alsa_output.dac"),
        "{text}"
    );
    // the microphone's settings are not touched
    assert!(
        cfg.get("attn_limit")
            .is_some_and(|v| v.as_float() == Some(100.0)),
        "{text}"
    );
    let (_, out, _) = run(&home, &["speakers"]);
    assert_eq!(
        out,
        "speakers: on (applies when HushMic starts)\noutput: alsa_output.dac\nstrength: gentle (6 dB)\n"
    );
    let (code, _, err) = run(&home, &["speakers", "loud"]);
    assert_eq!(code, 1);
    assert!(err.starts_with("usage: hushmic speakers\n"), "{err}");
    let (code, _, err) = run(&home, &["speakers", "strength", "300"]);
    assert_eq!(code, 1);
    assert!(err.contains("speakers.attn_limit must be 0-100"), "{err}");
    let _ = std::fs::remove_dir_all(&home);
}

/// A build without HushMic Speakers has no command, key or help for it,
/// reads a config that has its section without a word, and leaves the
/// section in the file.
#[cfg(not(feature = "speakers"))]
#[test]
fn a_build_without_speakers_has_no_command_or_key_for_it() {
    let home = std::env::temp_dir().join(format!("hushmic-offline-nospk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("run")).unwrap();
    std::fs::create_dir_all(home.join("models")).unwrap();
    let cfg_path = home.join(".config/hushmic/config.toml");
    std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    let section = "[speakers]\nenabled = true\noutput = \"alsa_output.dac\"\nattn_limit = 6.0\n";
    std::fs::write(&cfg_path, format!("attn_limit = 100.0\n\n{section}")).unwrap();

    let (code, out, err) = run(&home, &["config"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(err.is_empty(), "{err}");
    assert!(!out.contains("speakers"), "{out}");
    let (code, out, err) = run(&home, &["config", "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(!out.contains("speakers"), "{out}");
    for args in [
        &["config", "get", "speakers.enabled"][..],
        &["config", "set", "speakers.enabled", "false"],
    ] {
        let (code, out, err) = run(&home, args);
        assert_eq!(code, 1, "{args:?}: {out}{err}");
        assert!(err.contains("unknown key 'speakers.enabled'"), "{err}");
        assert!(!err.contains("speakers.output"), "{err}");
    }
    for args in [
        &["speakers"][..],
        &["speakers", "on"],
        &["speakers", "--help"],
        &["help", "speakers"],
        &["devices", "outputs"],
    ] {
        let (code, out, err) = run(&home, args);
        assert_ne!(code, 0, "{args:?}: {out}{err}");
        assert!(
            !out.contains("peakers") && !err.contains("HushMic Speakers"),
            "{args:?}: {out}{err}"
        );
    }
    for args in [
        &["--help"][..],
        &["config", "--help"],
        &["devices", "--help"],
    ] {
        let (code, out, err) = run(&home, args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        assert!(
            !out.contains("peakers") && !out.contains("outputs"),
            "{args:?}: {out}"
        );
    }
    // a write of another key leaves the section as it was
    let (code, out, err) = run(&home, &["config", "set", "attn_limit", "strong"]);
    assert_eq!(code, 0, "{out}{err}");
    let text = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(
        text.contains("attn_limit = 24.0\n") && text.ends_with(section),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
