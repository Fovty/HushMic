//! A takeover write whose read-back got no answer may have landed or not,
//! and the user may pick a device before the next try. The retry looks
//! first: the user's pick sticks (the takeover ends without a write), a
//! write that landed after all is confirmed without another one, and only
//! a write that was lost is made again. Stand-in PipeWire tools keep the
//! default in a file; its own test binary.

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

struct Graph {
    root: PathBuf,
}

impl Graph {
    fn flag(&self, name: &str, on: bool) {
        let p = self.root.join(name);
        if on {
            std::fs::write(p, b"").unwrap();
        } else {
            let _ = std::fs::remove_file(p);
        }
    }

    fn default_source(&self) -> Option<String> {
        let raw = std::fs::read_to_string(self.root.join("default.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        Some(v.get("name")?.as_str()?.to_string())
    }

    fn pick(&self, name: &str) {
        std::fs::write(
            self.root.join("default.json"),
            format!(r#"{{"name":"{name}"}}"#),
        )
        .unwrap();
    }

    fn writes(&self) -> usize {
        std::fs::read_to_string(self.root.join("writes"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
}

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

/// A takeover whose write lands but whose read-back gets no answer.
fn unconfirmed_takeover(g: &Graph, c: &mut Controller, cfg: &Config) {
    g.flag("silent-after-write", true);
    c.ensure_default_takeover(cfg, true);
    // The caller is back while the attempt is still out: its read-backs
    // hang until their caps, seconds from here.
    assert!(
        !c.finish_takeover(Duration::ZERO),
        "the attempt held the caller"
    );
    finish(c);
    g.flag("silent-after-write", false);
    g.flag("hang", false);
    assert!(c.default_taken() && !c.default_landed());
    assert_eq!(g.default_source().as_deref(), Some("hushmic_source"));
}

/// One takeover attempt, waited for.
fn take(c: &mut Controller, cfg: &Config) {
    c.ensure_default_takeover(cfg, true);
    finish(c);
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
fn a_retry_looks_before_it_writes_and_the_users_pick_sticks() {
    let root = std::env::temp_dir().join(format!("hm-takeover-retry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let bin = root.join("bin");
    for d in ["bin", "home", "config", "state", "run", "models"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let state = state_dir(&root);
    script(
        &bin,
        "pipewire",
        r#"[ "$1" = --version ] && { echo "Compiled with libpipewire 1.0.5"; exit 0; }
exec sleep 60"#,
    );
    script(&bin, "pw-dump", "echo '[]'");
    // The default lives in `default.json`. A write is counted in `writes`;
    // with `lose-write` it is acknowledged and dropped, with
    // `silent-after-write` it lands and every read after it hangs (`hang`).
    script(
        &bin,
        "pw-metadata",
        &format!(
            r#"r={root}
store=$r/default.json
[ "$2" = default ] || exit 0
if [ "$3" = -d ]; then rm -f "$store"; exit 0; fi
[ "$4" = default.configured.audio.source ] || exit 0
if [ $# -ge 5 ]; then
  echo x >> $r/writes
  [ -f $r/lose-write ] && exit 0
  printf '%s' "$5" > "$store"
  [ -f $r/silent-after-write ] && : > $r/hang
  exit 0
fi
[ -f $r/hang ] && exec sleep 10
echo 'Found "default" metadata 34'
[ -f "$store" ] && echo "update: id:0 key:'default.configured.audio.source' value:'$(cat "$store")' type:'Spa:String:JSON'"
exit 0"#,
            root = root.display(),
        ),
    );
    for tool in ["pw-cli", "pw-link", "wpctl"] {
        script(&bin, tool, "exit 0");
    }
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
    // A hanging read-back ends at its call cap; the attempts that must land
    // run under the same caps (they are the worker's), so they leave room
    // for a stand-in call a starved CI pod answers late (as in
    // takeover_capture).
    let caps = Caps {
        call: Duration::from_secs(3),
        attempt: Duration::from_secs(8),
    };
    let paths = || Paths {
        plugin_so: root.join("plugin.so"),
        model_dir: root.join("models"),
        dylib: root.join("libonnxruntime.so"),
    };
    let g = Graph { root: root.clone() };

    // The user picks another device after the unconfirmed write: the
    // retry leaves it, and so does quitting.
    g.pick("real_mic");
    let mut c = Controller::new(paths());
    c.set_takeover_caps(caps);
    c.enable(&cfg).unwrap();
    unconfirmed_takeover(&g, &mut c, &cfg);
    g.pick("usb_mic");
    let writes = g.writes();
    take(&mut c, &cfg);
    assert!(!c.default_taken(), "the takeover ended");
    assert_eq!(g.default_source().as_deref(), Some("usb_mic"));
    take(&mut c, &cfg);
    assert_eq!(g.writes(), writes, "nothing was written over the pick");
    c.disable().unwrap();
    assert_eq!(g.default_source().as_deref(), Some("usb_mic"));
    drop(c);

    // The unconfirmed write had landed: confirmed, not written again; the
    // user's device comes back on quit.
    g.pick("real_mic");
    let mut c = Controller::new(paths());
    c.set_takeover_caps(caps);
    c.enable(&cfg).unwrap();
    unconfirmed_takeover(&g, &mut c, &cfg);
    let writes = g.writes();
    take(&mut c, &cfg);
    assert!(c.default_taken() && c.default_landed());
    assert_eq!(g.writes(), writes, "a landed write is not made again");
    c.disable().unwrap();
    assert_eq!(g.default_source().as_deref(), Some("real_mic"));
    drop(c);

    // The write was lost (the default is still the one the takeover
    // found): the retry writes again.
    g.pick("real_mic");
    let mut c = Controller::new(paths());
    c.set_takeover_caps(caps);
    c.enable(&cfg).unwrap();
    g.flag("lose-write", true);
    take(&mut c, &cfg);
    g.flag("lose-write", false);
    assert!(c.default_taken() && !c.default_landed());
    assert_eq!(g.default_source().as_deref(), Some("real_mic"));
    take(&mut c, &cfg);
    assert!(c.default_landed());
    assert_eq!(g.default_source().as_deref(), Some("hushmic_source"));
    c.disable().unwrap();
    assert_eq!(g.default_source().as_deref(), Some("real_mic"));
    drop(c);

    if let Some(d) = hushmic::meter::dir_for(Some(&root.join("run"))) {
        if !d.starts_with(&root) {
            let _ = std::fs::remove_dir_all(&d);
        }
    }
    let _ = std::fs::remove_dir_all(&state);
    let _ = std::fs::remove_dir_all(&root);
}
