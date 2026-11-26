//! A live mode change whose port write does not land restarts the chain
//! only where its plugin has run and still ignores the control file. The
//! plugin maps the file when it first starts (its reader word is unset
//! until then), so an idle chain must not restart for a write the session
//! manager did not take: it gets the value from the file once it starts.
//! The real binary (`--headless`) against stand-in PipeWire tools whose
//! `pw-cli set-param` always fails; its own test binary.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod graph;

fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Env {
    root: PathBuf,
}

impl Env {
    /// A scratch home whose graph shows the chain in `state`.
    fn new(tag: &str, state: &str) -> Env {
        let root = std::env::temp_dir().join(format!("hm-port-miss-{tag}-{}", std::process::id()));
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
        // The chain host counts its starts and idles; it never maps the
        // control file, like a plugin that has not started.
        script(
            &bin,
            "pipewire",
            &format!(
                r#"[ "$1" = --version ] && {{ echo "Compiled with libpipewire 1.0.5"; exit 0; }}
{}
echo x >> {}
exec sleep 120"#,
                graph::HOST_IDLES,
                root.join("chain-starts").display()
            ),
        );
        std::fs::write(
            root.join("dump.json"),
            format!("[ {} ]", graph::chain_objects(state)),
        )
        .unwrap();
        script(
            &bin,
            "pw-dump",
            &format!("exec cat {}", root.join("dump.json").display()),
        );
        // Every port write fails; anything else pw-cli is asked succeeds.
        script(&bin, "pw-cli", r#"[ "$1" = set-param ] && exit 1; exit 0"#);
        for tool in ["pw-metadata", "wpctl"] {
            script(&bin, tool, "exit 0");
        }
        script(&bin, "pw-link", &graph::pw_link(None));
        Env { root }
    }

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

    fn chain_starts(&self) -> usize {
        std::fs::read_to_string(self.root.join("chain-starts"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    /// Start the daemon and wait until it answers and its chain is past
    /// the spawn's grace.
    fn daemon(&self) -> Child {
        let daemon = self
            .command(&["--headless"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let up = Instant::now();
        while !self.command(&["status"]).output().unwrap().status.success() {
            assert!(
                up.elapsed() < Duration::from_secs(20),
                "the daemon never answered"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(self.chain_starts(), 1, "one chain");
        std::thread::sleep(Duration::from_secs(4));
        daemon
    }

    /// Stop the daemon; its log. Removes the scratch directories.
    fn finish(self, mut daemon: Child) -> String {
        unsafe {
            libc::kill(daemon.id() as libc::pid_t, libc::SIGTERM);
        }
        let end = Instant::now();
        while daemon.try_wait().unwrap().is_none() && end.elapsed() < Duration::from_secs(15) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = daemon.kill();
        let out = daemon.wait_with_output().unwrap();
        if let Some(d) = hushmic::meter::dir_for(Some(&self.root.join("run"))) {
            if !d.starts_with(&self.root) {
                let _ = std::fs::remove_dir_all(&d);
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
        String::from_utf8_lossy(&out.stderr).into_owned()
    }
}

const RESTARTED: &str = "did not reach the chain";

#[test]
fn an_idle_chain_keeps_running_when_a_port_write_fails() {
    let env = Env::new("idle", "suspended");
    let daemon = env.daemon();
    let out = env.command(&["mode", "mute"]).output().unwrap();
    let reply = String::from_utf8_lossy(&out.stdout).into_owned();
    // Two ticks: the owed write is looked at again and still waits.
    std::thread::sleep(Duration::from_secs(12));
    let starts = env.chain_starts();
    let log = env.finish(daemon);
    assert!(
        reply.contains("mute"),
        "mute answered {reply:?}; log:\n{log}"
    );
    assert_eq!(starts, 1, "the idle chain was restarted; log:\n{log}");
    assert!(!log.contains(RESTARTED), "log:\n{log}");
}

#[test]
fn a_running_chain_that_ignores_its_control_file_restarts() {
    let env = Env::new("running", "running");
    let daemon = env.daemon();
    let out = env.command(&["mode", "mute"]).output().unwrap();
    let reply = String::from_utf8_lossy(&out.stdout).into_owned();
    // The graph shows the chain running while the reader word stays unset:
    // the ports were the only way in. A restart renders the mute instead.
    // A wide deadline, polled: on a loaded CI runner the restart was
    // decided and logged but not yet counted after 20 s (pipeline 7629).
    let t = Instant::now();
    while env.chain_starts() < 2 && t.elapsed() < Duration::from_secs(45) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let starts = env.chain_starts();
    let log = env.finish(daemon);
    assert!(
        reply.contains("mute"),
        "mute answered {reply:?}; log:\n{log}"
    );
    assert!(starts >= 2, "no restart; log:\n{log}");
    assert!(log.contains(RESTARTED), "log:\n{log}");
}
