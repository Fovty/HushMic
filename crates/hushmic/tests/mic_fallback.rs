//! A mic-recovery fallback through the real `Controller::enable`, with
//! stand-in PipeWire tools: every start follows the default while the
//! fallback holds, whatever the device list says (unreadable, or stale and
//! still listing the vanished mic), until something deliberate ends it.
//! Its own test binary: it points PATH, HOME and the XDG dirs at a scratch
//! directory.

use hushmic::config::Config;
use hushmic::controller::{Controller, Paths};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_fallback_holds_through_restarts_whatever_the_list_says() {
    let root = std::env::temp_dir().join(format!("hm-fallback-{}", std::process::id()));
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for d in ["home", "config", "state", "run", "models"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    // The chain host idles; pw-dump prints the file the test chooses, or
    // fails without one; nothing reaches a real PipeWire.
    script(
        &bin,
        "pipewire",
        r#"[ "$1" = --version ] && { echo "Compiled with libpipewire 1.0.5"; exit 0; }
exec sleep 30"#,
    );
    let dump = root.join("dump.json");
    script(
        &bin,
        "pw-dump",
        &format!("[ -f {0} ] && exec cat {0}; exit 1", dump.display()),
    );
    for tool in ["pw-metadata", "pw-cli", "pw-link", "wpctl"] {
        script(&bin, tool, "exit 1");
    }
    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.display()));
    std::env::set_var("HOME", root.join("home"));
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var("XDG_STATE_HOME", root.join("state"));
    std::env::set_var("XDG_RUNTIME_DIR", root.join("run"));
    std::env::set_var("PIPEWIRE_REMOTE", "hushmic-test-no-daemon");
    for f in [
        "plugin.so",
        "libonnxruntime.so",
        "models/dpdfnet8_48khz_hr.onnx",
    ] {
        std::fs::write(root.join(f), b"").unwrap();
    }
    let paths = || Paths {
        plugin_so: root.join("plugin.so"),
        model_dir: root.join("models"),
        dylib: root.join("libonnxruntime.so"),
    };
    let cfg = Config {
        mic: Some("mic_a".into()),
        set_default: false,
        ..Config::default()
    };

    // No device list: without a fallback the saved mic is kept (unknown is
    // not gone)...
    let mut c = Controller::new(paths());
    c.enable(&cfg).unwrap();
    assert_eq!(c.active_mic(), Some("mic_a"));
    // ...during one, every start follows the default, whatever asked for it.
    c.set_mic_fallback(true);
    c.enable(&cfg).unwrap();
    assert_eq!(
        c.active_mic(),
        None,
        "a failed read re-pinned the vanished mic"
    );
    c.enable(&cfg).unwrap();
    assert_eq!(c.active_mic(), None);

    // A list without the mic keeps the fallback too.
    let node = |name: &str| {
        format!(
            r#"{{ "id": 40, "type": "PipeWire:Interface:Node",
  "info": {{ "props": {{ "node.name": "{name}", "media.class": "Audio/Source" }} }} }}"#
        )
    };
    std::fs::write(&dump, format!("[{}]", node("mic_b"))).unwrap();
    c.enable(&cfg).unwrap();
    assert_eq!(c.active_mic(), None);

    // Even a list that still shows the mic (a read racing its removal, right
    // at the fallback) does not end it: the start stays on the default.
    std::fs::write(&dump, format!("[{}]", node("mic_a"))).unwrap();
    c.set_mic_fallback(true);
    c.enable(&cfg).unwrap();
    assert_eq!(
        c.active_mic(),
        None,
        "a stale list re-pinned the vanished mic"
    );

    // A deliberate turn-on (or a pick, or the return) ends it: the next
    // start is on the saved mic again.
    assert!(c.end_mic_fallback());
    c.enable(&cfg).unwrap();
    assert_eq!(c.active_mic(), Some("mic_a"));

    let _ = c.disable();
    let _ = std::fs::remove_dir_all(&root);
}
