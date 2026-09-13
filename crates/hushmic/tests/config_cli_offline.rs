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
    let (code, _, err) = run(&home, &["config", "set", "enabled", "false"]);
    assert_eq!(code, 1);
    assert!(err.contains("hushmic mode"), "{err}");
    let (code, _, err) = run(&home, &["config", "set", "tray"]);
    assert_eq!(code, 1, "missing value is a usage error");
    assert!(err.contains("usage"), "{err}");
    let (code, out, _) = run(&home, &["config", "set", "tray", "off"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("tray = false"), "{out}");
    // autostart is the one offline key with a side effect: the entry.
    let entry = home.join(".config/autostart/hushmic.desktop");
    let (code, out, err) = run(&home, &["config", "set", "autostart", "true"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.starts_with("autostart = true"), "{out}");
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
