//! Each chain gets a control file, and a live mode or strength change
//! lands in it at once even while every PipeWire tool hangs (WirePlumber
//! silent): the port write that follows runs off the caller's thread, and
//! one that fails is reported with its chain. Stand-in PipeWire tools; its
//! own test binary: it points PATH, HOME and the XDG dirs at a scratch
//! directory.

use hushmic::config::Config;
use hushmic::controller::{Applied, Controller, Paths, RunMode};
use hushmic_shm::control::{ControlReader, MODE_MUTE, MODE_PROCESS};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

mod graph;

fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn mode_and_strength_reach_the_control_file_while_pipewire_hangs() {
    let root = std::env::temp_dir().join(format!("hm-control-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let bin = root.join("bin");
    for d in ["bin", "home", "config", "state", "run", "models"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
    // The chain host records its environment (whole: written aside, then
    // moved in place) and idles.
    let env_file = root.join("host-env");
    script(
        &bin,
        "pipewire",
        &format!(
            r#"[ "$1" = --version ] && {{ echo "Compiled with libpipewire 1.0.5"; exit 0; }}
{}
env > {}.new && mv {}.new {}
exec sleep 60"#,
            graph::HOST_IDLES,
            env_file.display(),
            env_file.display(),
            env_file.display()
        ),
    );
    // PipeWire's tools answer until `stall` exists (WirePlumber silent:
    // they hang), and fail while `broken` exists.
    let stall = root.join("stall");
    let broken = root.join("broken");
    // A call that hangs records its pid first.
    let hung = root.join("hung");
    let gate = format!(
        "[ -f {} ] && {{ echo $$ >> {}; exec sleep 30; }}; [ -f {} ] && exit 1",
        stall.display(),
        hung.display(),
        broken.display()
    );
    script(&bin, "pw-dump", &format!("{gate}; echo '[]'"));
    for tool in ["pw-cli", "pw-metadata", "pw-link", "wpctl"] {
        script(&bin, tool, &format!("{gate}; exit 0"));
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
    let cfg = Config {
        set_default: false,
        attn_limit: 30.0,
        ..Config::default()
    };
    let mut c = Controller::new(Paths {
        plugin_so: root.join("plugin.so"),
        model_dir: root.join("models"),
        dylib: root.join("libonnxruntime.so"),
    });
    let (fail_tx, fail_rx) = std::sync::mpsc::channel::<u64>();
    let fail_tx = std::sync::Mutex::new(fail_tx);
    c.on_port_failure(Box::new(move |g| {
        let _ = fail_tx.lock().unwrap().send(g);
    }));
    c.enable(&cfg).unwrap();
    let generation = c.chain_generation().unwrap();

    // The file is there before the host starts, with what the conf says,
    // and the host is told where.
    // Where the daemon keeps chain files for this runtime dir (a private
    // /dev/shm directory when the scratch dir is on disk).
    let chain_dir = hushmic::meter::dir_for(Some(&root.join("run"))).unwrap();
    let path = chain_dir.join(format!("control-{generation}"));
    // Waited for, not timed: the host starts late on a loaded runner.
    let deadline = Instant::now() + Duration::from_secs(45);
    while !env_file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let env = std::fs::read_to_string(&env_file).unwrap();
    assert!(
        env.lines()
            .any(|l| l == format!("HUSHMIC_CONTROL={}", path.display())),
        "{env}"
    );
    assert!(!c.control_honoured(), "no plugin mapped it yet");
    let plugin = ControlReader::open(&path).unwrap();
    assert!(c.control_honoured());
    assert_eq!(
        (plugin.mode(), plugin.attn_db()),
        (Some(MODE_PROCESS), Some(30.0))
    );

    // WirePlumber goes silent: every PipeWire tool hangs.
    std::fs::write(&stall, b"").unwrap();
    c.set_mode_state(RunMode::Mute);
    let t = Instant::now();
    assert!(c.write_mode_live());
    assert_eq!(plugin.mode(), Some(MODE_MUTE));
    let cfg2 = Config {
        attn_limit: 12.0,
        ..cfg.clone()
    };
    assert_eq!(c.apply_settings(&cfg2).unwrap(), Applied::Live);
    assert_eq!(plugin.attn_db(), Some(12.0));
    // Whatever PipeWire call they started still hangs: the writes did not
    // wait for one to give up (the cap only catches one that waits for
    // good; a loaded runner is slow, not stuck).
    let alive = std::fs::read_to_string(&hung)
        .unwrap_or_default()
        .lines()
        .filter_map(|p| p.trim().parse::<i32>().ok())
        .all(|pid| unsafe { libc::kill(pid, 0) } == 0);
    assert!(alive, "a hung PipeWire call ended before the writes did");
    assert!(
        t.elapsed() < Duration::from_secs(10),
        "the writes waited for PipeWire: {:?}",
        t.elapsed()
    );

    // PipeWire back but the port write fails (an older plugin would miss
    // it): the failure names this chain.
    std::fs::remove_file(&stall).unwrap();
    std::fs::write(&broken, b"").unwrap();
    c.set_mode_state(RunMode::Bypass);
    assert!(c.write_mode_live());
    let failed = fail_rx.recv_timeout(Duration::from_secs(40)).unwrap();
    assert_eq!(failed, generation);

    // Gone with the chain.
    c.disable().unwrap();
    assert!(!path.exists());
    assert_eq!(c.chain_generation(), None);
    drop(plugin);

    // Turned on again while muted: the new chain's file opens muted, so
    // its first block is silent before any live write could reach it.
    std::fs::remove_file(&broken).unwrap();
    let _ = std::fs::remove_file(&env_file);
    c.set_mode_state(RunMode::Mute);
    c.enable(&cfg).unwrap();
    let generation = c.chain_generation().unwrap();
    let path = chain_dir.join(format!("control-{generation}"));
    let plugin = ControlReader::open(&path).unwrap();
    assert_eq!(
        (plugin.mode(), plugin.attn_db()),
        (Some(MODE_MUTE), Some(30.0))
    );
    c.disable().unwrap();
    drop(plugin);
    drop(c);
    if !chain_dir.starts_with(&root) {
        let _ = std::fs::remove_dir_all(&chain_dir);
    }
    let _ = std::fs::remove_dir_all(&root);
}
