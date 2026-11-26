//! Review SF-W1: the window sends its actions one after another, so a
//! setting answered only once it is on disk would hold a Mute behind a slow
//! disk. The window's `config set` is answered once applied; the command
//! line's (`--written`) once written, so it can report a failure. The real
//! binary (`--headless`) against stand-in PipeWire tools; the write is held
//! by making config.toml a FIFO (the writer's read of it blocks until it is
//! opened for writing). Its own test binary.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn command(root: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_hushmic"));
    c.args(args)
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        )
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("PIPEWIRE_REMOTE", "hushmic-test-no-daemon")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
        .env("HUSHMIC_PLUGIN_SO", root.join("plugin.so"))
        .env("HUSHMIC_MODEL_DIR", root.join("models"))
        .env("ORT_DYLIB_PATH", root.join("libonnxruntime.so"));
    c
}

/// One request on the control socket, as the window's link sends it; the
/// answer's first line and how long it took.
fn ask(sock: &Path, line: &str) -> (String, Duration) {
    let t = Instant::now();
    let mut c = UnixStream::connect(sock).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
    c.write_all(format!("{line}\n").as_bytes()).unwrap();
    let _ = c.shutdown(std::net::Shutdown::Write);
    let mut out = String::new();
    let _ = BufReader::new(c).read_line(&mut out);
    (out, t.elapsed())
}

#[test]
fn a_window_mute_never_waits_behind_a_settings_write() {
    let root = std::env::temp_dir().join(format!("hm-settings-ans-{}", std::process::id()));
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
    let config = root.join("config/hushmic/config.toml");
    std::fs::write(
        &config,
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
    let node = r#"{ "id": 7, "type": "PipeWire:Interface:Node", "info": { "props": {
        "media.class": "Audio/Source", "node.name": "hushmic_source" } } }"#;
    std::fs::write(root.join("dump.json"), format!("[ {node} ]")).unwrap();
    script(
        &bin,
        "pw-dump",
        &format!("exec cat {}", root.join("dump.json").display()),
    );
    for tool in ["pw-cli", "pw-metadata", "pw-link", "wpctl"] {
        script(&bin, tool, "exit 0");
    }
    let sock = root.join("run/hushmic-control.sock");
    let mut daemon = command(&root, &["--headless"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let up = Instant::now();
    loop {
        if command(&root, &["status"])
            .output()
            .unwrap()
            .status
            .success()
        {
            break;
        }
        assert!(
            up.elapsed() < Duration::from_secs(20),
            "the daemon never answered"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    // From here every write of config.toml waits until we open the FIFO.
    std::fs::remove_file(&config).unwrap();
    let c = std::ffi::CString::new(config.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);

    // the window: a strength change, then Mute
    let (set, set_took) = ask(&sock, "config set attn_limit 33");
    let (mute, mute_took) = ask(&sock, "mode mute");
    // the command line: waits for the write
    let held_sock = sock.clone();
    let cli = std::thread::spawn(move || ask(&held_sock, "config set --written attn_limit 34"));
    std::thread::sleep(Duration::from_millis(600));
    let cli_waited = !cli.is_finished();
    // the disk lets go
    drop(
        std::fs::OpenOptions::new()
            .write(true)
            .open(&config)
            .unwrap(),
    );
    let (cli_set, _) = cli.join().unwrap();
    let written = std::fs::read_to_string(&config).unwrap_or_default();

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
    let chain_dir = hushmic::meter::dir_for(Some(&root.join("run"))).unwrap();
    if !chain_dir.starts_with(&root) {
        let _ = std::fs::remove_dir_all(&chain_dir);
    }
    let _ = std::fs::remove_dir_all(&root);

    assert!(set.starts_with("ok"), "{set:?}; log:\n{log}");
    assert!(
        set_took < Duration::from_secs(2),
        "the window's set took {set_took:?}"
    );
    assert!(mute.starts_with("ok"), "{mute:?}; log:\n{log}");
    // a loaded runner is slow, not 10 s slow: the write is held for good
    assert!(
        mute_took < Duration::from_millis(250),
        "mute took {mute_took:?}; log:\n{log}"
    );
    assert!(
        cli_waited,
        "the command line's set did not wait for its write"
    );
    assert!(cli_set.starts_with("ok"), "{cli_set:?}; log:\n{log}");
    assert!(written.contains("attn_limit = 34"), "{written}");
    let _: PathBuf = root;
}
