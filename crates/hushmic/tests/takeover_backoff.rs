//! The takeover runs on a worker: the caller never waits for the session
//! manager, a slow box (about 1 s per call) still takes the default, and a
//! takeover that keeps missing is never given up (while HushMic is not the
//! default, apps on the default record the raw mic). After five tries in a
//! row it is tried every 30 s; a session manager that answers again after
//! not answering makes the next try due at once (the count stays), a
//! restart of the source host starts the count over (a chain restart
//! leaves it alone: the default stays on the host); only the user's own
//! pick ends it.
//! A teardown waits for a write being sent, never for its read-back.
//! Stand-in PipeWire tools keep the default in a file; its own test binary.

use hushmic::config::Config;
use hushmic::controller::{Controller, Paths};
use hushmic::takeover::Caps;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

    /// pw-metadata calls on the default so far (reads and writes).
    fn calls(&self) -> usize {
        std::fs::read_to_string(self.root.join("calls"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn writes(&self) -> usize {
        std::fs::read_to_string(self.root.join("calls"))
            .map(|s| s.lines().filter(|l| *l == "w").count())
            .unwrap_or(0)
    }
}

/// How long the test waits for anything that should end on its own: wide,
/// since every call is a shell the worker starts, slow on a loaded runner.
/// Only a failure waits that long.
const PATIENCE: Duration = Duration::from_secs(45);

/// One attempt, waited for.
fn take(c: &mut Controller, cfg: &Config) {
    c.ensure_default_takeover(cfg, true);
    c.finish_takeover(PATIENCE);
}

/// `f` returns while every pw-metadata call is held at a gate: a caller
/// that waited for the session manager would still be there when the gate
/// opens by itself, 30 s on. No clock on the caller (on a loaded runner
/// even waking a thread can take a second).
fn without_waiting(g: &Graph, f: impl FnOnce()) {
    g.flag("gate", true);
    let (done, wait) = mpsc::channel::<()>();
    let gate = g.root.join("gate");
    let opener = std::thread::spawn(move || {
        let waited = wait.recv_timeout(Duration::from_secs(30)).is_err();
        let _ = std::fs::remove_file(gate);
        waited
    });
    f();
    let _ = done.send(());
    assert!(
        !opener.join().unwrap(),
        "the caller waited for the session manager"
    );
}

/// `op` (a teardown) until the default is `want` and the controller has
/// let it go. Each try of a restore gets one pipewire::CALL_BUDGET for its
/// write and read-back, which a loaded runner can overrun (Off and Quit try
/// again while there is no answer, a restart does not); then the product
/// keeps it for the next teardown (disable, Drop, startup) and so does
/// this, twice at most: a restore that only ever lands on a later teardown
/// fails.
fn handed_back(g: &Graph, c: &mut Controller, want: &str, mut op: impl FnMut(&mut Controller)) {
    for _ in 0..2 {
        op(c);
        if g.default_source().as_deref() == Some(want) && !c.default_taken() {
            return;
        }
    }
    panic!(
        "two teardowns and the default is not back to {want} (it is {:?})",
        g.default_source()
    );
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
fn the_takeover_never_holds_the_caller_and_never_gives_up() {
    let root = std::env::temp_dir().join(format!("hm-takeover-backoff-{}", std::process::id()));
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
    // The default lives in `default.json`; every call on it is counted in
    // `calls` (r/w). With `slow` each takes a second; with `hang` a read
    // never answers; with `lose-write` a write is acknowledged and dropped;
    // `hang-after-write` sets `hang` once a write is stored; with `gate`
    // every call waits for it to go.
    script(
        &bin,
        "pw-metadata",
        &format!(
            r#"r={root}
store=$r/default.json
[ "$2" = default ] || exit 0
if [ "$3" = -d ]; then rm -f "$store"; exit 0; fi
[ "$4" = default.configured.audio.source ] || exit 0
while [ -f $r/gate ]; do sleep 0.2; done
[ -f $r/slow ] && sleep 1
if [ $# -ge 5 ]; then
  echo w >> $r/calls
  [ -f $r/lose-write ] && exit 0
  printf '%s' "$5" > "$store"
  [ -f $r/hang-after-write ] && : > $r/hang
  exit 0
fi
echo r >> $r/calls
[ -f $r/hang ] && exec sleep 30
echo 'Found "default" metadata 34'
k=default.configured.audio.source
[ -f "$store" ] && echo "update: id:0 key:'$k' value:'$(cat "$store")' type:'Spa:String:JSON'"
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
    let paths = || Paths {
        plugin_so: root.join("plugin.so"),
        model_dir: root.join("models"),
        dylib: root.join("libonnxruntime.so"),
    };
    // Short caps only for the controller whose calls hang: there each one
    // ends at its cap however loaded the box is. Where calls answer, a
    // loaded CI runner can take longer than a short cap for one that does
    // not hang, and a try would fail that should not (pipeline 7629), so
    // the other controllers keep the real caps. The caps are the worker's,
    // set before its first attempt: a controller keeps the ones it began
    // with.
    let short = Caps {
        call: Duration::from_secs(2),
        attempt: Duration::from_secs(5),
    };
    let g = Graph { root: root.clone() };
    let later = |s: u64| Instant::now() + Duration::from_secs(s);

    // A slow box, the real caps: every call takes a second, and the
    // takeover lands before the backoff (on a loaded runner one call can
    // outlast its cap; the next tick tries again, as here). That the
    // caller waits for none of them is shown below, where a gate would
    // cost these calls their cap.
    g.pick("real_mic");
    g.flag("slow", true);
    let mut c = Controller::new(paths());
    c.enable(&cfg).unwrap();
    let t = Instant::now();
    for _ in 0..5 {
        take(&mut c, &cfg);
        if c.default_landed() {
            break;
        }
    }
    let took = t.elapsed();
    assert!(
        c.default_taken() && c.default_landed(),
        "slow box: not taken"
    );
    assert_eq!(g.default_source().as_deref(), Some("hushmic_source"));
    assert!(took >= Duration::from_secs(2), "the stand-in was not slow");
    // Restored at full speed: a restore's write and read-back share one
    // 2 s budget, which two one-second calls fill.
    g.flag("slow", false);
    handed_back(&g, &mut c, "real_mic", |c| c.disable().unwrap());
    drop(c);

    // WirePlumber stalled: every read hangs. Each try ends at its cap
    // without the caller waiting (it returns with the call still held at
    // the gate); five in a row, then the backoff; once a graph read
    // answers, the takeover is due again at once.
    let mut c = Controller::new(paths());
    c.set_takeover_caps(short);
    c.enable(&cfg).unwrap();
    g.flag("hang", true);
    for _ in 0..5 {
        without_waiting(&g, || c.ensure_default_takeover(&cfg, true));
        c.finish_takeover(PATIENCE);
    }
    assert!(!c.default_taken(), "nothing captured without an answer");
    let calls = g.calls();
    take(&mut c, &cfg);
    assert_eq!(g.calls(), calls, "backing off: no sixth try in a row");
    // The session manager answers again but the metadata still does not:
    // one try at once, and the count kept, so the backoff holds after it.
    c.session_manager_answered();
    take(&mut c, &cfg);
    assert!(g.calls() > calls, "due at once after an answer");
    let calls = g.calls();
    take(&mut c, &cfg);
    assert_eq!(g.calls(), calls, "the count stayed: backing off again");
    g.flag("hang", false);
    // Still the short caps (the worker's): 2 s a call, room enough for a
    // stand-in that answers.
    c.session_manager_answered();
    take(&mut c, &cfg);
    assert!(c.default_taken() && c.default_landed());
    assert_eq!(g.default_source().as_deref(), Some("hushmic_source"));
    handed_back(&g, &mut c, "real_mic", |c| c.disable().unwrap());
    drop(c);

    // The session manager answers and drops every write: five tries, then
    // one every 30 s; its answers do not reset that; a host restart does.
    let mut c = Controller::new(paths());
    c.enable(&cfg).unwrap();
    g.flag("lose-write", true);
    let w0 = g.writes();
    for _ in 0..8 {
        take(&mut c, &cfg);
    }
    assert_eq!(g.writes() - w0, 5, "five tries in a row, then the backoff");
    c.session_manager_answered();
    take(&mut c, &cfg);
    assert_eq!(
        g.writes() - w0,
        5,
        "it answered all along: still backing off"
    );
    c.ensure_default_takeover_at(&cfg, true, later(31));
    c.finish_takeover(PATIENCE);
    assert_eq!(g.writes() - w0, 6, "tried again after 30 s");
    c.ensure_default_takeover_at(&cfg, true, later(5));
    c.finish_takeover(PATIENCE);
    assert_eq!(g.writes() - w0, 6, "and not before the next 30 s");
    g.flag("lose-write", false);
    let w2 = g.writes();
    c.enable(&cfg).unwrap();
    take(&mut c, &cfg);
    assert_eq!(g.writes(), w2, "a chain restart keeps the backoff");
    handed_back(&g, &mut c, "real_mic", |c| c.restart_all(&cfg).unwrap());
    take(&mut c, &cfg);
    assert!(
        c.default_taken() && c.default_landed(),
        "a host restart starts over"
    );
    handed_back(&g, &mut c, "real_mic", |c| c.disable().unwrap());
    drop(c);

    // Backing off, and the user picks a device: that ends it for good.
    let mut c = Controller::new(paths());
    c.enable(&cfg).unwrap();
    g.flag("lose-write", true);
    for _ in 0..5 {
        take(&mut c, &cfg);
    }
    g.flag("lose-write", false);
    g.pick("usb_mic");
    let w1 = g.writes();
    c.ensure_default_takeover_at(&cfg, true, later(31));
    c.finish_takeover(PATIENCE);
    c.ensure_default_takeover_at(&cfg, true, later(62));
    c.finish_takeover(PATIENCE);
    c.session_manager_answered();
    take(&mut c, &cfg);
    assert_eq!(g.writes(), w1, "nothing written over the pick");
    assert!(!c.default_taken());
    c.disable().unwrap();
    assert_eq!(g.default_source().as_deref(), Some("usb_mic"));
    drop(c);

    // WirePlumber stalls right after taking the write: a teardown during
    // the read-back restores the user's default at once.
    g.pick("real_mic");
    g.flag("hang-after-write", true);
    let mut c = Controller::new(paths());
    c.set_takeover_caps(Caps {
        call: Duration::from_secs(20),
        attempt: Duration::from_secs(40),
    });
    c.enable(&cfg).unwrap();
    let c0 = g.calls();
    c.ensure_default_takeover(&cfg, true);
    let until = |n: usize| {
        let deadline = Instant::now() + PATIENCE;
        while g.calls() < c0 + n && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        g.calls() >= c0 + n
    };
    // read, write, then the read-back that hangs
    assert!(until(3), "no read-back ({} calls)", g.calls() - c0);
    g.flag("hang-after-write", false);
    g.flag("hang", false);
    // Waiting for the read-back would be its whole 20 s cap; the teardown
    // itself is the 6 s write wait at most, and a restore.
    let t = Instant::now();
    c.disable().unwrap();
    let held = t.elapsed();
    assert!(
        held < Duration::from_secs(12),
        "the teardown waited {held:?} for the read-back"
    );
    handed_back(&g, &mut c, "real_mic", |c| c.disable().unwrap());
    assert!(!c.default_taken(), "restored and forgotten");
    // The hung read-back ends at its cap, and with it the attempt (which
    // writes nothing more: the default stays the user's).
    let t = Instant::now();
    c.finish_takeover(PATIENCE);
    assert!(
        t.elapsed() < PATIENCE,
        "the attempt never finished ({} calls)",
        g.calls() - c0
    );
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
