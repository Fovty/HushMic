//! `hushmic --enable-once` takes the system default even when the chain's
//! node shows up later than enable()'s bounded wait (a busy machine): that
//! mode has no watchdog tick to finish the takeover, so it must finish it
//! itself once the node is confirmed. Stand-in PipeWire tools: pw-dump
//! lists `hushmic_source` only once the test says so, and pw-metadata
//! keeps the default source in a file. Its own test binary: it runs the
//! real executable against a scratch HOME, XDG dirs and PATH.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod graph;

fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn node(name: &str) -> String {
    format!(
        r#"{{ "id": 1, "type": "PipeWire:Interface:Node", "info": {{ "props": {{
            "media.class": "Audio/Source", "node.name": "{name}",
            "node.description": "{name}" }} }} }}"#
    )
}

/// The default source's name in the store, if any.
fn default_source(store: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(store).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(v.get("name")?.as_str()?.to_string())
}

fn wait_for(store: &Path, want: &str, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if default_source(store).as_deref() == Some(want) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn a_late_node_still_gets_the_default() {
    let root = std::env::temp_dir().join(format!("hm-enable-once-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let bin = root.join("bin");
    for d in ["bin", "home", "config/hushmic", "state", "run", "models"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
    for f in [
        "plugin.so",
        "libonnxruntime.so",
        "models/dpdfnet8_48khz_hr.onnx",
    ] {
        std::fs::write(root.join(f), b"").unwrap();
    }
    std::fs::write(
        root.join("config/hushmic/config.toml"),
        "enabled = true\nset_default = true\nautostart = false\n",
    )
    .unwrap();

    // Both hosts idle; their nodes and ports appear when `node-up` exists.
    script(
        &bin,
        "pipewire",
        r#"[ "$1" = --version ] && { echo "Compiled with libpipewire 1.0.5"; exit 0; }
exec sleep 60"#,
    );
    let mic = node("builtin");
    std::fs::write(root.join("before.json"), format!("[ {mic} ]")).unwrap();
    std::fs::write(
        root.join("after.json"),
        format!("[ {mic}, {} ]", graph::chain_objects("idle")),
    )
    .unwrap();
    let up = root.join("node-up");
    script(
        &bin,
        "pw-dump",
        &format!(
            "[ -f {up} ] && exec cat {after}; exec cat {before}",
            up = up.display(),
            after = root.join("after.json").display(),
            before = root.join("before.json").display(),
        ),
    );
    // The default source lives in `store`: read, set, delete.
    let store = root.join("default.json");
    std::fs::write(&store, r#"{"name":"builtin"}"#).unwrap();
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
    for tool in ["pw-cli", "wpctl"] {
        script(&bin, tool, "exit 1");
    }
    script(&bin, "pw-link", &graph::pw_link(Some(&up)));

    let child = Command::new(env!("CARGO_BIN_EXE_hushmic"))
        .arg("--enable-once")
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("PIPEWIRE_REMOTE", "hushmic-test-no-daemon")
        .env("HUSHMIC_PLUGIN_SO", root.join("plugin.so"))
        .env("HUSHMIC_MODEL_DIR", root.join("models"))
        .env("ORT_DYLIB_PATH", root.join("libonnxruntime.so"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Later than enable()'s 2 s wait, well inside the link's 8 s.
    std::thread::sleep(Duration::from_secs(3));
    std::fs::write(&up, b"").unwrap();

    let taken = wait_for(&store, "hushmic_source", Duration::from_secs(8));
    // SIGTERM: the teardown hands the default back.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let restored = wait_for(&store, "builtin", Duration::from_secs(10));
    let out = child.wait_with_output().unwrap();
    let log = String::from_utf8_lossy(&out.stderr);
    // Chain files outside the scratch dir (a disk-backed one): ours too.
    if let Some(d) = hushmic::meter::dir_for(Some(&root.join("run"))) {
        if !d.starts_with(&root) {
            let _ = std::fs::remove_dir_all(&d);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(taken, "the default was never taken; log:\n{log}");
    assert!(
        restored,
        "quitting did not restore the default; log:\n{log}"
    );
}
