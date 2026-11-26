//! While the session manager does not answer, every PipeWire tool the
//! daemon could call hangs. The daemon's loop must not: a mute lands in the
//! chain's control file and is answered at once, `status` answers, and the
//! tick keeps exactly one graph read out instead of piling them up. The
//! real binary (`--headless`) against stand-in PipeWire tools that hang
//! while a flag file exists; its own test binary.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Env {
    root: PathBuf,
}

impl Env {
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_hushmic"));
        c.args(args)
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("PIPEWIRE_REMOTE", "hushmic-test-no-daemon")
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
            .env("HUSHMIC_PLUGIN_SO", self.root.join("plugin.so"))
            .env("HUSHMIC_MODEL_DIR", self.root.join("models"))
            .env("ORT_DYLIB_PATH", self.root.join("libonnxruntime.so"));
        c
    }

    /// Run the CLI; its output and how long it took.
    fn cli(&self, args: &[&str]) -> (Output, Duration) {
        let t = Instant::now();
        let out = self.command(args).output().unwrap();
        (out, t.elapsed())
    }
}

/// The mode word in the newest control file in `dir`.
fn control_mode(dir: &Path) -> Option<u32> {
    let newest = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("control-"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())?;
    let raw = std::fs::read(newest.path()).ok()?;
    Some(u32::from_le_bytes(raw[16..20].try_into().ok()?))
}

/// How long an answer may take while every tool hangs.
const CAP: Duration = Duration::from_secs(10);

#[test]
fn the_loop_answers_while_pipewire_hangs() {
    let root = std::env::temp_dir().join(format!("hm-loop-stall-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
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
        "enabled = true\nset_default = false\nautostart = false\nnotifications = false\n",
    )
    .unwrap();
    let bin = root.join("bin");
    script(
        &bin,
        "pipewire",
        r#"[ "$1" = --version ] && { echo "Compiled with libpipewire 1.0.5"; exit 0; }
exec sleep 120"#,
    );
    // Every tool waits while `stall` exists; pw-dump counts its starts.
    let stall = root.join("stall");
    let starts = root.join("dump-starts");
    let wait = format!("while [ -f {} ]; do sleep 0.1; done", stall.display());
    let node = r#"{ "id": 7, "type": "PipeWire:Interface:Node", "info": { "props": {
        "media.class": "Audio/Source", "node.name": "hushmic_source" } } }"#;
    std::fs::write(root.join("dump.json"), format!("[ {node} ]")).unwrap();
    script(
        &bin,
        "pw-dump",
        &format!(
            "echo $$ >> {}; {wait}; exec cat {}",
            starts.display(),
            root.join("dump.json").display()
        ),
    );
    for tool in ["pw-cli", "pw-metadata", "pw-link", "wpctl"] {
        script(&bin, tool, &format!("{wait}; exit 0"));
    }
    let env = Env { root: root.clone() };
    // Where the daemon keeps chain files for this runtime dir.
    let chain_dir = hushmic::meter::dir_for(Some(&root.join("run"))).unwrap();

    let mut daemon = env
        .command(&["--headless"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let up = Instant::now();
    loop {
        let (out, _) = env.cli(&["status"]);
        if out.status.success() {
            break;
        }
        assert!(
            up.elapsed() < Duration::from_secs(20),
            "the daemon never answered"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(control_mode(&chain_dir), Some(0), "processing");

    // The session manager goes silent; let a tick send a graph read out.
    std::fs::write(&stall, b"").unwrap();
    std::thread::sleep(Duration::from_secs(6));
    let dumps_before = std::fs::read_to_string(&starts).unwrap().lines().count();

    let (out, took) = env.cli(&["mode", "mute"]);
    let mute_reply = String::from_utf8_lossy(&out.stdout).into_owned();
    let mute_took = took;
    let mode_after = control_mode(&chain_dir);
    let (out, status_took) = env.cli(&["status"]);
    let status_ok = out.status.success();
    // The graph read the tick sent out is still hanging: the answers came
    // while the session manager was silent, not once a call gave up.
    let read_out = std::fs::read_to_string(&starts)
        .unwrap()
        .lines()
        .nth(dumps_before.saturating_sub(1))
        .and_then(|p| p.trim().parse::<i32>().ok())
        .is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0);
    // Another tick or two while the read is still out.
    std::thread::sleep(Duration::from_secs(6));
    let dumps_after = std::fs::read_to_string(&starts).unwrap().lines().count();

    std::fs::remove_file(&stall).unwrap();
    unsafe {
        libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM);
    }
    let end = Instant::now();
    while daemon.try_wait().unwrap().is_none() && end.elapsed() < Duration::from_secs(15) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = daemon.kill();
    let out = daemon.wait_with_output().unwrap();
    let log = String::from_utf8_lossy(&out.stderr).into_owned();
    if !chain_dir.starts_with(&root) {
        let _ = std::fs::remove_dir_all(&chain_dir);
    }
    let _ = std::fs::remove_dir_all(&root);

    // Answered at all while every tool hangs is the point; the cap only
    // catches a loop that waits (a loaded runner is slow, not stuck).
    assert!(read_out, "the stalled graph read had ended; log:\n{log}");
    assert!(
        mute_reply.contains("mute") && mute_took < CAP,
        "mute answered {mute_reply:?} after {mute_took:?}; log:\n{log}"
    );
    assert_eq!(
        mode_after,
        Some(2),
        "the control file says mute; log:\n{log}"
    );
    assert!(
        status_ok && status_took < CAP,
        "status took {status_took:?}; log:\n{log}"
    );
    assert!(
        dumps_after <= dumps_before + 1,
        "graph reads piled up: {dumps_before} -> {dumps_after}; log:\n{log}"
    );
}
