//! A teardown that comes while an attempt writes the crash breadcrumb.
//! The breadcrumb waits for the disk, for as long as the disk likes, and
//! the write of the default comes after it. Off, Quit or the setting
//! turned off in that wait: the attempt, once the disk answers, sends no
//! write and takes its breadcrumb away again, so the default is never left
//! on a `hushmic_source` that is gone. A breadcrumb that was there before
//! the attempt (a crashed run's) stays. A breadcrumb that used up the
//! attempt's time sends no write either; the next try does.
//! And a teardown that comes during the read-back of a sent write deals
//! with that write itself: what the read-back finds afterwards changes
//! nothing any more.
//! Off and Quit leave no `hushmic_source` behind, so their restore of the
//! user's default is tried again while PipeWire does not answer, three
//! times in all; where the node stays or a new one follows (the setting
//! turned off, a restart) it gets the one try.
//! Stand-in PipeWire tools keep the default in a file; its own test binary.

use hushmic::config::Config;
use hushmic::controller::{Controller, Paths};
use hushmic::takeover::Caps;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// How long the test waits for anything that should end on its own: wide,
/// since every call is a shell the worker starts, slow on a loaded runner.
/// Only a failure waits that long.
const PATIENCE: Duration = Duration::from_secs(120);

/// The stand-in session. It lives in the process environment, so the
/// tests take turns on it: each builds its own and removes it again.
struct Bed {
    root: PathBuf,
    state: PathBuf,
    _turn: MutexGuard<'static, ()>,
}

impl Bed {
    fn paths(&self) -> Paths {
        Paths {
            plugin_so: self.root.join("plugin.so"),
            model_dir: self.root.join("models"),
            dylib: self.root.join("libonnxruntime.so"),
        }
    }

    fn default_source(&self) -> Option<String> {
        let raw = std::fs::read_to_string(self.root.join("default.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        Some(v.get("name")?.as_str()?.to_string())
    }

    /// Every value written to the default so far, in order.
    fn writes(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("writes"))
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn breadcrumb(&self) -> PathBuf {
        self.state.join("hushmic").join("prior-default")
    }

    /// The next `n` writes get no answer.
    fn hang_writes(&self, n: usize) {
        std::fs::write(self.root.join("hang-writes"), n.to_string()).unwrap();
    }

    /// Writes that got no answer so far.
    fn hung(&self) -> usize {
        self.writes().iter().filter(|w| *w == "hung").count()
    }
}

impl Drop for Bed {
    fn drop(&mut self) {
        if let Some(d) = hushmic::meter::dir_for(Some(&self.root.join("run"))) {
            if !d.starts_with(&self.root) {
                let _ = std::fs::remove_dir_all(&d);
            }
        }
        let _ = std::fs::remove_dir_all(&self.state);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The state dir, where an attempt writes the crash breadcrumb: in memory
/// when the box has /dev/shm, so the only wait for it is the one a test
/// puts there.
fn state_dir(root: &Path) -> PathBuf {
    let shm = Path::new("/dev/shm").join(root.file_name().unwrap());
    let _ = std::fs::remove_dir_all(&shm);
    if std::fs::create_dir_all(&shm).is_ok() {
        shm
    } else {
        root.join("state")
    }
}

/// A session whose default is `real_mic`, nothing written yet.
fn bed() -> Bed {
    static TURN: Mutex<()> = Mutex::new(());
    let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!("hm-takeover-teardown-{}", std::process::id()));
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
    // The default lives in `default.json`; every value written to it is
    // kept in `writes`. With `hold-read-back`, the read that follows the
    // next write waits (as `holding`) until `go` is there. `hang-writes`
    // holds a number: that many writes get no answer (each `hung` in
    // `writes`).
    std::fs::write(root.join("default.json"), r#"{"name":"real_mic"}"#).unwrap();
    script(
        &bin,
        "pw-metadata",
        &format!(
            r#"r={root}
store=$r/default.json
[ "$2" = default ] || exit 0
if [ "$3" = -d ]; then echo deleted >> $r/writes; rm -f "$store"; exit 0; fi
[ "$4" = default.configured.audio.source ] || exit 0
if [ $# -ge 5 ]; then
  n=$(cat $r/hang-writes 2>/dev/null || echo 0)
  if [ "$n" -gt 0 ]; then
    echo $((n - 1)) > $r/hang-writes
    echo hung >> $r/writes
    exec sleep 30
  fi
  echo "$5" >> $r/writes
  printf '%s' "$5" > "$store"
  [ -f $r/hold-read-back ] && mv $r/hold-read-back $r/hold-next-read
  exit 0
fi
if [ -f $r/hold-next-read ]; then
  mv $r/hold-next-read $r/holding
  while [ ! -f $r/go ]; do sleep 0.1; done
fi
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
    Bed {
        root,
        state,
        _turn: turn,
    }
}

fn wanted() -> Config {
    Config {
        set_default: true,
        ..Config::default()
    }
}

/// The breadcrumb of `c`'s first attempt, held on its way to the disk for
/// as long as the test likes (a disk that is slow to sync); later ones go
/// straight through.
struct Held {
    reached: mpsc::Receiver<()>,
    let_go: mpsc::Sender<()>,
    ended: mpsc::Receiver<()>,
}

impl Held {
    fn on(c: &mut Controller) -> Held {
        let (reach, reached) = mpsc::channel::<()>();
        let (let_go, go) = mpsc::channel::<()>();
        let (end, ended) = mpsc::channel::<()>();
        let (reach, go) = (Mutex::new(reach), Mutex::new(go));
        c.before_breadcrumb(Arc::new(move || {
            let _ = reach.lock().unwrap().send(());
            let _ = go.lock().unwrap().recv();
        }));
        let end = Mutex::new(end);
        c.on_takeover_done(Box::new(move || {
            let _ = end.lock().unwrap().send(());
        }));
        Held {
            reached,
            let_go,
            ended,
        }
    }

    /// The attempt is at its breadcrumb, and stays there.
    fn wait_reached(&self) {
        self.reached
            .recv_timeout(PATIENCE)
            .expect("the attempt never got to its breadcrumb");
    }

    /// The disk answers; returns once the attempt has ended.
    fn release(self) {
        drop(self.let_go);
        self.ended
            .recv_timeout(PATIENCE)
            .expect("the attempt did not end");
    }
}

/// An attempt whose caps a held breadcrumb cannot use up: what it does
/// after the hold is then the same however long the teardown took.
const ROOMY: Caps = Caps {
    call: hushmic::takeover::CALL_CAP,
    attempt: Duration::from_secs(600),
};

/// The attempt that is out, waited for and applied.
fn finish(c: &mut Controller) {
    assert!(c.finish_takeover(PATIENCE), "the attempt did not come back");
}

/// `op` (a teardown) until the default is the user's again and the
/// controller has let it go. A restore's calls are bounded, which a loaded
/// runner can overrun; then the product keeps it for the next teardown and
/// so does this, twice at most.
fn handed_back(b: &Bed, c: &mut Controller, mut op: impl FnMut(&mut Controller)) {
    for _ in 0..2 {
        op(c);
        if b.default_source().as_deref() == Some("real_mic") && !c.default_taken() {
            return;
        }
    }
    panic!(
        "two teardowns and the default is not back (it is {:?})",
        b.default_source()
    );
}

/// Nothing of the held attempt is left: no write went out, the default is
/// the user's, the breadcrumb is gone.
fn untouched(b: &Bed, when: &str) {
    assert_eq!(b.writes(), Vec::<String>::new(), "written after {when}");
    assert_eq!(b.default_source().as_deref(), Some("real_mic"));
    assert!(
        !b.breadcrumb().exists(),
        "the breadcrumb stayed after {when}"
    );
}

#[test]
fn off_while_the_breadcrumb_waits_for_the_disk_sends_no_write_afterwards() {
    let b = &bed();
    let cfg = wanted();
    let mut c = Controller::new(b.paths());
    c.set_takeover_caps(ROOMY);
    let held = Held::on(&mut c);
    c.enable(&cfg).unwrap();
    c.ensure_default_takeover(&cfg, true);
    held.wait_reached();
    // Off: the teardown has nothing to restore, and does not wait for the
    // disk either.
    c.disable().unwrap();
    assert!(!c.default_taken());
    held.release();
    untouched(b, "Off");
    // On again: taken as ever, and handed back.
    c.takeover_finished();
    c.enable(&cfg).unwrap();
    c.ensure_default_takeover(&cfg, true);
    finish(&mut c);
    assert!(c.default_taken() && c.default_landed());
    assert_eq!(b.default_source().as_deref(), Some("hushmic_source"));
    assert_eq!(
        std::fs::read_to_string(b.breadcrumb()).unwrap().trim(),
        "real_mic"
    );
    handed_back(b, &mut c, |c| c.disable().unwrap());
    assert!(!b.breadcrumb().exists());
}

#[test]
fn quit_while_the_breadcrumb_waits_for_the_disk_sends_no_write_afterwards() {
    let b = &bed();
    let cfg = wanted();
    let mut c = Controller::new(b.paths());
    c.set_takeover_caps(ROOMY);
    let held = Held::on(&mut c);
    c.enable(&cfg).unwrap();
    c.ensure_default_takeover(&cfg, true);
    held.wait_reached();
    // Quit: the controller goes, the worker's thread is still at the disk
    // (the process ending would only stop it earlier).
    drop(c);
    held.release();
    untouched(b, "Quit");
}

#[test]
fn the_setting_turned_off_while_the_breadcrumb_waits_sends_no_write_afterwards() {
    let b = &bed();
    let cfg = wanted();
    let mut c = Controller::new(b.paths());
    c.set_takeover_caps(ROOMY);
    let held = Held::on(&mut c);
    c.enable(&cfg).unwrap();
    let host = c.host_generation();
    c.ensure_default_takeover(&cfg, true);
    held.wait_reached();
    // "Set as default microphone" off: the host stays, the takeover ends.
    let off = Config {
        set_default: false,
        ..Config::default()
    };
    c.enable(&off).unwrap();
    assert_eq!(c.host_generation(), host, "the host was restarted");
    held.release();
    c.takeover_finished();
    assert!(!c.default_taken());
    untouched(b, "the setting was turned off");
    drop(c);
    untouched(b, "the quit that followed");
}

#[test]
fn the_setting_turned_off_during_the_read_back_stays_handed_back() {
    let b = &bed();
    let cfg = wanted();
    let mut c = Controller::new(b.paths());
    std::fs::write(b.root.join("hold-read-back"), b"").unwrap();
    c.enable(&cfg).unwrap();
    c.ensure_default_takeover(&cfg, true);
    let holding = b.root.join("holding");
    let deadline = Instant::now() + PATIENCE;
    while !holding.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(holding.exists(), "no read-back ({:?})", b.writes());
    // "Set as default microphone" off, the write sent and not confirmed:
    // the host stays, the user's default is written back.
    let off = Config {
        set_default: false,
        ..Config::default()
    };
    handed_back(b, &mut c, |c| c.enable(&off).unwrap());
    // The read-back answers, and finds the user's default.
    std::fs::write(b.root.join("go"), b"").unwrap();
    finish(&mut c);
    assert!(
        !c.default_taken(),
        "the read-back's outcome undid the hand-back"
    );
    assert_eq!(b.default_source().as_deref(), Some("real_mic"));
    assert!(!b.breadcrumb().exists());
}

#[test]
fn a_crashed_runs_breadcrumb_outlives_an_attempt_that_is_told_not_to_write() {
    let b = &bed();
    let cfg = wanted();
    // A run that died holding the default: it still names us, and the
    // breadcrumb is the one copy of the user's device.
    std::fs::write(b.root.join("default.json"), r#"{"name":"hushmic_source"}"#).unwrap();
    std::fs::create_dir_all(b.breadcrumb().parent().unwrap()).unwrap();
    std::fs::write(b.breadcrumb(), "usb_mic").unwrap();
    let mut c = Controller::new(b.paths());
    c.set_takeover_caps(ROOMY);
    let held = Held::on(&mut c);
    c.enable(&cfg).unwrap();
    c.ensure_default_takeover(&cfg, true);
    held.wait_reached();
    c.disable().unwrap();
    held.release();
    assert_eq!(b.writes(), Vec::<String>::new(), "written after Off");
    assert_eq!(
        std::fs::read_to_string(b.breadcrumb()).unwrap().trim(),
        "usb_mic",
        "the crashed run's breadcrumb"
    );
}

#[test]
fn a_breadcrumb_that_used_up_the_attempts_time_sends_no_write_and_the_next_try_does() {
    let b = &bed();
    let cfg = wanted();
    let mut c = Controller::new(b.paths());
    // The real cap for a call (the read before the breadcrumb is a shell
    // on a runner that may be loaded), and an attempt just longer.
    let caps = Caps {
        call: hushmic::takeover::CALL_CAP,
        attempt: hushmic::takeover::CALL_CAP + Duration::from_secs(1),
    };
    c.set_takeover_caps(caps);
    // The first breadcrumb is held from when the attempt reaches it for a
    // whole attempt's time: whenever the attempt began, its time is up.
    let first = AtomicBool::new(true);
    c.before_breadcrumb(Arc::new(move || {
        if first.swap(false, Ordering::SeqCst) {
            let until = Instant::now() + caps.attempt;
            while Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }));
    c.enable(&cfg).unwrap();
    c.ensure_default_takeover(&cfg, true);
    finish(&mut c);
    assert_eq!(b.writes(), Vec::<String>::new(), "written after its time");
    assert!(
        c.default_taken() && !c.default_landed(),
        "recorded, and not landed"
    );
    assert_eq!(
        std::fs::read_to_string(b.breadcrumb()).unwrap().trim(),
        "real_mic"
    );
    // The next tick's try finds the breadcrumb there and writes.
    c.ensure_default_takeover(&cfg, true);
    finish(&mut c);
    assert!(c.default_taken() && c.default_landed());
    assert_eq!(b.default_source().as_deref(), Some("hushmic_source"));
    handed_back(b, &mut c, |c| c.disable().unwrap());
    assert!(!b.breadcrumb().exists());
}

/// A controller that holds the default, landed.
fn taken(b: &Bed, cfg: &Config) -> Controller {
    let mut c = Controller::new(b.paths());
    c.enable(cfg).unwrap();
    for _ in 0..5 {
        c.ensure_default_takeover(cfg, true);
        finish(&mut c);
        if c.default_landed() {
            break;
        }
    }
    assert!(c.default_taken() && c.default_landed(), "not taken");
    assert_eq!(b.default_source().as_deref(), Some("hushmic_source"));
    c
}

#[test]
fn off_tries_the_restore_again_while_pipewire_does_not_answer() {
    let b = &bed();
    let mut c = taken(b, &wanted());
    b.hang_writes(2);
    handed_back(b, &mut c, |c| c.disable().unwrap());
    assert_eq!(b.hung(), 2);
    assert!(!b.breadcrumb().exists());
}

#[test]
fn quit_tries_the_restore_again_while_pipewire_does_not_answer() {
    let b = &bed();
    let c = taken(b, &wanted());
    // One write without an answer: the quit has two more tries, as many
    // as a teardown that can be repeated gets elsewhere in these tests.
    b.hang_writes(1);
    drop(c);
    assert_eq!(b.hung(), 1);
    assert_eq!(b.default_source().as_deref(), Some("real_mic"));
    assert!(!b.breadcrumb().exists());
}

#[test]
fn off_gives_up_after_three_tries_and_keeps_what_it_recorded() {
    let b = &bed();
    let mut c = taken(b, &wanted());
    b.hang_writes(9);
    c.disable().unwrap();
    assert_eq!(b.hung(), 3, "three tries, no fourth");
    assert!(c.default_taken(), "still to be handed back");
    assert_eq!(
        std::fs::read_to_string(b.breadcrumb()).unwrap().trim(),
        "real_mic"
    );
    // PipeWire answers again: the next stop puts it back.
    b.hang_writes(0);
    handed_back(b, &mut c, |c| c.disable().unwrap());
    assert!(!b.breadcrumb().exists());
}

#[test]
fn the_setting_turned_off_and_a_restart_give_the_restore_one_try() {
    let b = &bed();
    let cfg = wanted();
    let mut c = taken(b, &cfg);
    // "Set as default microphone" off: the host and hushmic_source stay.
    let off = Config {
        set_default: false,
        ..Config::default()
    };
    b.hang_writes(9);
    c.enable(&off).unwrap();
    assert_eq!(b.hung(), 1, "the host stays: one try");
    assert!(c.default_taken(), "still to be handed back");
    // A restart of everything: a new hushmic_source follows. One try
    // before the old host goes, one before the new one starts.
    c.restart_all(&cfg).unwrap();
    assert_eq!(b.hung(), 3, "a restart: one try each time");
    assert!(c.default_taken());
    b.hang_writes(0);
    handed_back(b, &mut c, |c| c.disable().unwrap());
    assert!(!b.breadcrumb().exists());
}
