//! After a reboot that ended the session with HushMic still the configured
//! default (WirePlumber never saved the quit's restore), the takeover finds
//! `hushmic_source` already the default and no saved prior mic. It takes
//! the user's mic from WirePlumber's own history instead (UX panel A26), so
//! quitting hands it back, also when it was unplugged at login (A118
//! SF-T1). Stand-in PipeWire tools keep the default in a file; its own test
//! binary.

use hushmic::config::Config;
use hushmic::controller::{Controller, Paths};
use hushmic::takeover::Caps;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn default_source(store: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(store).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(v.get("name")?.as_str()?.to_string())
}

/// Only the computer's own mic: the user's is unplugged at login.
const DUMP_UNPLUGGED: &str = r#"[
  { "id": 40, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Source", "node.name": "qemu_mic",
      "node.description": "Built-in Audio" } } }
]"#;

/// The user's mic plugged back in.
const DUMP: &str = r#"[
  { "id": 40, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Source", "node.name": "qemu_mic",
      "node.description": "Built-in Audio" } } },
  { "id": 41, "type": "PipeWire:Interface:Node",
    "info": { "props": { "media.class": "Audio/Source", "node.name": "virt_mic",
      "node.description": "Laptop Microphone" } } }
]"#;

/// How long the test waits for an attempt to come back. An attempt has no
/// fixed length: its caps bound the PipeWire calls, not starting them, nor
/// the write of the crash breadcrumb between them, which waits for the
/// disk. So this is no statement about an attempt, only wide: a failure
/// alone waits that long.
const PATIENCE: Duration = Duration::from_secs(120);

/// The attempt that is out, waited for and applied.
fn finish(c: &mut Controller) {
    assert!(c.finish_takeover(PATIENCE), "the attempt did not come back");
}

/// The state dir, where an attempt writes the crash breadcrumb and waits
/// for it to be on the disk: in memory when the box has /dev/shm, so that
/// wait is never for a disk other jobs are writing to (it counts against
/// the attempt's cap, and a busy disk can hold it for longer than that).
fn state_dir(root: &Path) -> PathBuf {
    let shm = Path::new("/dev/shm").join(root.file_name().unwrap());
    let _ = std::fs::remove_dir_all(&shm);
    if std::fs::create_dir_all(&shm).is_ok() {
        shm
    } else {
        root.join("state")
    }
}

#[test]
fn a_reboot_with_the_default_on_hushmic_restores_the_mic_from_wireplumbers_history() {
    // A118 SF-T1: the newest mic of the history is unplugged at login (a
    // laptop started without its headset), plugged in later; quitting
    // then hands it back, never the computer's mic that stood in
    let root = std::env::temp_dir().join(format!("hm-reboot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let bin = root.join("bin");
    for d in ["bin", "home", "config", "state", "run", "models"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let state = state_dir(&root);
    for d in ["hushmic", "wireplumber"] {
        std::fs::create_dir_all(state.join(d)).unwrap();
    }
    script(
        &bin,
        "pipewire",
        r#"[ "$1" = --version ] && { echo "Compiled with libpipewire 1.0.5"; exit 0; }
exec sleep 60"#,
    );
    std::fs::write(root.join("dump.json"), DUMP_UNPLUGGED).unwrap();
    script(
        &bin,
        "pw-dump",
        &format!("cat {}", root.join("dump.json").display()),
    );
    let store = root.join("default.json");
    std::fs::write(&store, r#"{"name":"hushmic_source"}"#).unwrap();
    script(
        &bin,
        "pw-metadata",
        &format!(
            r#"store={store}
[ "$2" = default ] || exit 0
if [ "$3" = -d ]; then rm -f "$store"; exit 0; fi
[ "$4" = default.configured.audio.source ] || exit 0
if [ $# -ge 5 ]; then printf '%s' "$5" > "$store"; exit 0; fi
echo 'Found "default" metadata 34'
[ -f "$store" ] && echo "update: id:0 key:'default.configured.audio.source' value:'$(cat "$store")' type:'Spa:String:JSON'"
exit 0"#,
            store = store.display(),
        ),
    );
    for tool in ["pw-cli", "pw-link", "wpctl"] {
        script(&bin, tool, "exit 0");
    }
    // the session ended on HushMic: the breadcrumb is empty, WirePlumber
    // kept the user's mic one step back, the computer's own before it
    std::fs::write(state.join("hushmic/prior-default"), "").unwrap();
    std::fs::write(
        state.join("wireplumber/default-nodes"),
        "[default-nodes]\n\
         default.configured.audio.source=hushmic_source\n\
         default.configured.audio.source.0=hushmic_source\n\
         default.configured.audio.source.1=virt_mic\n\
         default.configured.audio.source.2=qemu_mic\n",
    )
    .unwrap();
    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.display()));
    std::env::set_var("HOME", root.join("home"));
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var("XDG_STATE_HOME", &state);
    std::env::set_var("XDG_RUNTIME_DIR", root.join("run"));
    std::env::set_var("PIPEWIRE_REMOTE", "hushmic-test-no-daemon");
    for f in [
        "plugin.so",
        "libonnxruntime.so",
        "models/dpdfnet8_48khz_hr.onnx",
    ] {
        std::fs::write(root.join(f), b"").unwrap();
    }
    let cfg = Config {
        set_default: true,
        ..Config::default()
    };
    let mut c = Controller::new(Paths {
        plugin_so: root.join("plugin.so"),
        model_dir: root.join("models"),
        dylib: root.join("libonnxruntime.so"),
    });
    // Nothing hangs here: room for a stand-in call a starved CI pod
    // answers late (as in takeover_capture).
    c.set_takeover_caps(Caps {
        call: Duration::from_secs(3),
        attempt: Duration::from_secs(8),
    });
    c.enable(&cfg).unwrap();
    c.ensure_default_takeover(&cfg, true);
    finish(&mut c);
    assert!(c.default_taken());
    // the newest real mic of the history, though it is not plugged in
    assert_eq!(c.prior_default(), Some("virt_mic"));
    assert_eq!(
        std::fs::read_to_string(state.join("hushmic/prior-default")).unwrap(),
        "virt_mic"
    );
    assert_eq!(default_source(&store).as_deref(), Some("hushmic_source"));
    // plugged back in; quitting hands the user's mic back, not the stand-in
    std::fs::write(root.join("dump.json"), DUMP).unwrap();
    c.disable().unwrap();
    assert_eq!(default_source(&store).as_deref(), Some("virt_mic"));

    drop(c);
    if let Some(d) = hushmic::meter::dir_for(Some(&root.join("run"))) {
        if !d.starts_with(&root) {
            let _ = std::fs::remove_dir_all(&d);
        }
    }
    let _ = std::fs::remove_dir_all(&state);
    let _ = std::fs::remove_dir_all(&root);
}
