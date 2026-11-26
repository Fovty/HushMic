//! Mic check's backend against a fake plugin in this process: the tap
//! handshake as the real plugin does it, without PipeWire. Covers what the
//! env-gated e2e cannot pin in CI: the backend lets go of the microphone
//! when the page stops reading (the window is not drawing) and starts
//! again on the next read; a mapped tap that never moves falls back
//! instead of listening forever; a keep-alive that cannot run is not
//! started in a loop, and one that runs stays through a chain restart; a
//! pw-cat that cannot play from a pipe switches to
//! the restart player at once; a playback that makes no progress fails
//! with a warning.
//!
//! PipeWire is pointed at a socket that does not exist: nothing here
//! records anything. `pw-cat`, `pw-play` and `pw-record` are small scripts
//! first on PATH; the tests take turns (they share the scripts' mode
//! file). The keep-alive recorder exits at once, as without PipeWire,
//! unless the mode is "hold".
//!
//! The runtime files (meter, tap rings) live on tmpfs, as under a real
//! XDG_RUNTIME_DIR: a fault on a file mapping on a disk filesystem can
//! wait for the disk on a busy runner (a backend frozen for seconds failed
//! these tests in CI). Waits are deadlines, not sleeps, and what must not
//! happen too early is checked as a lower bound.

use hushmic::miccheck::{
    Blocker, Command, Engine, EngineInfo, MicCheck, Phase, PlaybackKind, Setup, View, Warning,
    LOOK_TIMEOUT,
};
use hushmic_shm::meter::MeterWriter;
use hushmic_shm::tap::{self, TapCursor, TapRing};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

const LAG: u32 = 2400;
/// Deadline (s) for what the backend does within a second or two: only a
/// stalled runner comes near it.
const WAIT: u64 = 20;

/// The test's own directories: scripts under the temp dir (tmpfs may be
/// mounted noexec), the runtime dir under `test_dir_base`.
fn setup_env() -> PathBuf {
    static ENV: Once = Once::new();
    let name = format!("hushmic-mcb-{}", std::process::id());
    let dir = std::env::temp_dir().join(&name);
    let run = hushmic_shm::test_dir_base().join(&name).join("run");
    ENV.call_once(|| {
        // Earlier runs' directories (this binary cannot clean up after its
        // last test).
        for base in [std::env::temp_dir(), hushmic_shm::test_dir_base()] {
            let Ok(entries) = std::fs::read_dir(&base) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let Some(pid) = name.strip_prefix("hushmic-mcb-") else {
                    continue;
                };
                if !Path::new("/proc").join(pid).exists() {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(run.parent().unwrap());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Set before any thread of this test binary spawns a process.
        std::env::set_var("XDG_RUNTIME_DIR", &run);
        std::env::set_var("PIPEWIRE_REMOTE", dir.join("no-pipewire-here"));
        std::env::set_var("PIPEWIRE_RUNTIME_DIR", &run);
        // pw-cat: --help lists nothing; plays nothing. "exit": fails at
        // once (cannot read a pipe); "hang": never reads, notes its pid in
        // `cats`, and ignores SIGTERM (a stop then takes its full 2 s).
        // pw-play: "plays" while the `play-gate` file is there (the test
        // ends it), and notes each start in `plays`.
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = |name: &str, body: &str| {
            let p = bin.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        script(
            "pw-cat",
            "[ \"$1\" = --help ] && exit 0\n\
             case \"$(cat \"$(dirname \"$0\")/mode\" 2>/dev/null)\" in\n\
             hang) echo $$ >> \"$(dirname \"$0\")/cats\"; trap '' TERM; exec sleep 30 ;;\n\
             *) exit 1 ;;\n\
             esac",
        );
        // pw-record (the keep-alive): fails at once, as without PipeWire;
        // "hold": runs until stopped (no exec: it keeps its name).
        script(
            "pw-record",
            "case \"$(cat \"$(dirname \"$0\")/mode\" 2>/dev/null)\" in\n\
             hold) while :; do sleep 0.1; done ;;\n\
             *) exit 1 ;;\n\
             esac",
        );
        // the playback's latency watch never reaches the box's PipeWire:
        // its tools fail, as without a graph
        script("pw-dump", "exit 1");
        script("pw-top", "exit 1");
        script(
            "pw-play",
            "d=\"$(dirname \"$0\")\"\n\
             echo $$ >> \"$d/plays\"\n\
             while [ -e \"$d/play-gate\" ]; do sleep 0.05; done",
        );
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(&path));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    });
    dir
}

/// The runtime dir (XDG_RUNTIME_DIR) of this run.
fn run_dir() -> PathBuf {
    setup_env();
    hushmic_shm::test_dir_base()
        .join(format!("hushmic-mcb-{}", std::process::id()))
        .join("run")
}

/// One test at a time, and the fake pw-cat's mode for it.
fn serial(mode: &str) -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::write(setup_env().join("bin").join("mode"), mode).unwrap();
    g
}

fn meter_path(tag: &str) -> PathBuf {
    let d = run_dir().join("hushmic");
    std::fs::create_dir_all(&d).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    d.join(format!("meter-{tag}"))
}

/// The plugin's half of the tap: maps requests, writes 10 ms hops in real
/// time while `writing` (catching up after a late wake-up, as the worker
/// does), lets go on close or a stale heartbeat.
struct FakePlugin {
    stop: Arc<AtomicBool>,
    /// Rings mapped so far, and rings released.
    mapped: Arc<AtomicU32>,
    released: Arc<AtomicU32>,
    /// The clean side's gain (f32 bits): what a strength does to it.
    gain: Arc<AtomicU32>,
    thread: Option<std::thread::JoinHandle<()>>,
    _meter: MeterWriter,
}

impl FakePlugin {
    fn start(meter: &Path, writing: bool) -> FakePlugin {
        let w = MeterWriter::create(meter, 1, 5).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mapped = Arc::new(AtomicU32::new(0));
        let released = Arc::new(AtomicU32::new(0));
        let gain = Arc::new(AtomicU32::new(1f32.to_bits()));
        let (s, m, r, g) = (
            Arc::clone(&stop),
            Arc::clone(&mapped),
            Arc::clone(&released),
            Arc::clone(&gain),
        );
        let request = tap::request_path(meter);
        let thread = std::thread::spawn(move || {
            let mut ring: Option<(TapRing, TapCursor)> = None;
            let mut n = 0u64;
            let t0 = Instant::now();
            let mut last_poll = Instant::now() - Duration::from_secs(1);
            while !s.load(Ordering::Relaxed) {
                if last_poll.elapsed() >= Duration::from_millis(tap::POLL_MS) {
                    last_poll = Instant::now();
                    if let Some((t, _)) = &ring {
                        if t.wants_release(hushmic_shm::monotonic_ms()) {
                            t.mark_released();
                            ring = None;
                            r.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    if let Ok(t) = TapRing::attach(&request) {
                        if let Some((old, _)) = ring.take() {
                            old.mark_replaced();
                            r.fetch_add(1, Ordering::Relaxed);
                        }
                        t.pin();
                        t.mark_mapped(9, LAG);
                        t.unlink_if_at(&request);
                        ring = Some((t, TapCursor::default()));
                        m.fetch_add(1, Ordering::Relaxed);
                    }
                }
                let due = t0.elapsed().as_millis() as u64 / 10;
                match ring.as_mut() {
                    Some((t, cur)) if writing => {
                        while n < due {
                            // A tone, both sides; the clean side is what
                            // the engine made LAG frames before (the
                            // reader's shift pairs them again), at the gain.
                            let tone = |i: i64| 0.1 * ((i as f32) * 0.03).sin();
                            let k = f32::from_bits(g.load(Ordering::Relaxed));
                            let at = (n * 480) as i64;
                            let hop: Vec<f32> = (0..480).map(|j| tone(at + j)).collect();
                            let clean: Vec<f32> = (0..480)
                                .map(|j| k * tone(at + j - i64::from(LAG)))
                                .collect();
                            t.write_hop(cur, &hop, &clean);
                            n += 1;
                        }
                    }
                    // No ring: the sound goes by unrecorded.
                    _ => n = due,
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if let Some((t, _)) = ring {
                t.mark_released();
            }
        });
        FakePlugin {
            stop,
            mapped,
            released,
            gain,
            thread: Some(thread),
            _meter: w,
        }
    }
}

impl Drop for FakePlugin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn info(meter: &Path) -> EngineInfo {
    EngineInfo {
        engine: Engine::Filtering,
        meter: Some(meter.to_path_buf()),
        mic_node: None,
        mic_name: "Test mic".into(),
        pipewire: true,
        model: "dpdfnet8_48khz_hr".into(),
        strength_db: 100.0,
    }
}

fn start(meter: &Path) -> MicCheck {
    MicCheck::start(
        Setup {
            info: info(meter),
            columns: true,
            once: false,
        },
        Arc::new(|_| {}),
    )
}

/// Read the view (as a drawing page does) until `f` holds.
fn read_until(check: &MicCheck, what: &str, secs: u64, f: impl Fn(&View) -> bool) -> View {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let v = check.view();
        if f(&v) {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {v:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Read the view every 50 ms for `span`, as a drawing page does. Returns
/// the longest time between two reads (at most: measured from before one
/// read to after the next), counting from `prev`, the previous read.
fn keep_reading(check: &MicCheck, span: Duration, prev: &mut Option<Instant>) -> Duration {
    let t = Instant::now();
    let mut gap = Duration::ZERO;
    while t.elapsed() < span {
        let before = Instant::now();
        check.view();
        if let Some(p) = *prev {
            gap = gap.max(p.elapsed());
        }
        *prev = Some(before);
        std::thread::sleep(Duration::from_millis(50));
    }
    gap
}

/// Wait (without reading the view) until `f` holds.
fn wait_until(what: &str, secs: u64, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// This process's children named pw-record (the keep-alive).
fn recorders() -> Vec<u32> {
    let mut v = Vec::new();
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return v;
    };
    for t in tasks.flatten() {
        let Ok(s) = std::fs::read_to_string(t.path().join("children")) else {
            continue;
        };
        for pid in s.split_whitespace().filter_map(|p| p.parse::<u32>().ok()) {
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            if comm.trim() == "pw-record" {
                v.push(pid);
            }
        }
    }
    v
}

#[test]
fn a_page_that_stops_reading_lets_go_and_comes_back() {
    let _serial = serial("exit");
    let meter = meter_path("look");
    let plugin = FakePlugin::start(&meter, true);
    let check = start(&meter);
    let listening = |v: &View| v.phase == Phase::Listening && !v.timeline.columns.is_empty();
    read_until(&check, "listening on the tap", WAIT, listening);
    assert_eq!(plugin.mapped.load(Ordering::Relaxed), 1);
    // The window stops drawing: no reads while the timeline moves. The
    // backend lets go once a new view stays unread for LOOK_TIMEOUT (and
    // not sooner); the plugin sees it within its poll.
    let last_read = Instant::now();
    check.view();
    wait_until("the tap to be let go", WAIT, || {
        plugin.released.load(Ordering::Relaxed) > 0
    });
    assert!(
        last_read.elapsed() >= LOOK_TIMEOUT,
        "let go {:?} after the last read",
        last_read.elapsed()
    );
    assert_eq!(plugin.released.load(Ordering::Relaxed), 1);
    // The page draws again: one read starts it over. The plugin can let go
    // before the backend's own pause shows in the view (a runner that holds
    // the backend up past the heartbeat timeout, or one that stops it
    // between closing the ring and publishing), so the view may still say
    // Listening from the old ring: wait for the new ring, not the phase.
    check.view();
    read_until(&check, "listening again", WAIT, |v| {
        listening(v) && plugin.mapped.load(Ordering::Relaxed) >= 2
    });
    assert_eq!(plugin.mapped.load(Ordering::Relaxed), 2, "asked again");
    // While the page keeps reading, nothing is let go. A runner that holds
    // this thread up past LOOK_TIMEOUT makes a page that stopped reading,
    // so the check runs over 2.5 s of steady reads after 2 s of them (a
    // let-go from before would land in those).
    let deadline = Instant::now() + Duration::from_secs(3 * WAIT);
    loop {
        let mut prev = None;
        let lead_in = keep_reading(&check, Duration::from_secs(2), &mut prev);
        let before = plugin.released.load(Ordering::Relaxed);
        let gap = keep_reading(&check, Duration::from_millis(2_500), &mut prev).max(lead_in);
        if gap < LOOK_TIMEOUT - Duration::from_millis(500) {
            assert_eq!(
                plugin.released.load(Ordering::Relaxed),
                before,
                "let go while the page read every {gap:?} at most"
            );
            break;
        }
        assert!(Instant::now() < deadline, "never 2.5 s of steady reads");
        // Held up: it may have let go; back to listening before trying again.
        read_until(&check, "listening after a stall", WAIT, listening);
    }
    // Every ring but the live one was let go.
    assert_eq!(
        plugin.released.load(Ordering::Relaxed) + 1,
        plugin.mapped.load(Ordering::Relaxed)
    );
    drop(check);
    drop(plugin);
    let _ = std::fs::remove_file(&meter);
}

#[test]
fn a_tap_that_never_moves_does_not_listen_forever() {
    let _serial = serial("exit");
    let meter = meter_path("stall");
    let plugin = FakePlugin::start(&meter, false);
    let check = start(&meter);
    let mut seen = std::collections::HashSet::new();
    let t = Instant::now();
    // Mapped, but no audio: connecting, then (no microphone known for a
    // fallback) blocked, never "Listening".
    let v = loop {
        let v = check.view();
        seen.extend(recorders());
        assert_ne!(v.phase, Phase::Listening, "listening without audio");
        if v.phase == Phase::Blocked(Blocker::NoMicrophone) {
            break v;
        }
        assert!(t.elapsed() < Duration::from_secs(WAIT), "stuck in {v:?}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(v.phase, Phase::Blocked(Blocker::NoMicrophone));
    assert_eq!(plugin.mapped.load(Ordering::Relaxed), 1);
    assert!(
        t.elapsed() >= Duration::from_secs(3),
        "not before the stall timeout"
    );
    // The keep-alive cannot run here: started again about once a second,
    // not on every 50 ms tick.
    assert!(
        seen.len() as f32 <= t.elapsed().as_secs_f32() + 3.0,
        "{} keep-alive starts in {:?}",
        seen.len(),
        t.elapsed()
    );
    drop(check);
    drop(plugin);
    let _ = std::fs::remove_file(&meter);
}

/// Chain restarts: the plugin lets go of the tap and a new one maps the
/// next request, on the same meter or (a new chain, as the daemon says)
/// on a new one after a moment with none. The keep-alive on hushmic_source (which outlives the
/// chain) stays the same process all along: one stopped and started again
/// as the new chain links up has left a stream stuck on PipeWire 1.0.
#[test]
fn a_chain_restart_keeps_the_keep_alive() {
    let _serial = serial("hold");
    let listening = |v: &View| v.phase == Phase::Listening && !v.timeline.columns.is_empty();
    let listen_again = |check: &MicCheck, plugin: &FakePlugin, first: &[u32]| {
        let t = Instant::now();
        loop {
            let v = check.view();
            assert_eq!(recorders(), first, "the keep-alive changed ({v:?})");
            if plugin.mapped.load(Ordering::Relaxed) >= 1 && listening(&v) {
                break;
            }
            assert!(
                t.elapsed() < Duration::from_secs(WAIT),
                "never listened again: {v:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let meter = meter_path("restart");
    let plugin = FakePlugin::start(&meter, true);
    let check = start(&meter);
    read_until(&check, "listening on the tap", WAIT, listening);
    let first = recorders();
    assert_eq!(first.len(), 1, "one keep-alive: {first:?}");
    // The plugin lets go and its successor comes up on the same meter.
    drop(plugin);
    let plugin = FakePlugin::start(&meter, true);
    listen_again(&check, &plugin, &first);
    // A new chain with its own meter, after a while with none (the
    // daemon says the chain is starting).
    let meter2 = meter_path("restart-2");
    drop(plugin);
    check.send(Command::Engine(EngineInfo {
        meter: None,
        ..info(&meter2)
    }));
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(1) {
        check.view();
        assert_eq!(recorders(), first, "the keep-alive changed while starting");
        std::thread::sleep(Duration::from_millis(10));
    }
    let plugin = FakePlugin::start(&meter2, true);
    check.send(Command::Engine(info(&meter2)));
    listen_again(&check, &plugin, &first);
    // Leaving the page stops it.
    check.send(Command::Visible(false));
    wait_until("the keep-alive to stop", WAIT, || recorders().is_empty());
    drop(check);
    drop(plugin);
    let _ = std::fs::remove_file(&meter);
    let _ = std::fs::remove_file(&meter2);
}

/// A strength change reaches the running chain at once (a live write, no
/// restart): the live picture keeps its columns, and the column the change
/// falls in shows it once complete (at most a column to fill and a 25 ms
/// read later); the next is fully at the new strength.
#[test]
fn a_strength_change_shows_in_the_next_column() {
    let _serial = serial("hold");
    let meter = meter_path("strength");
    let plugin = FakePlugin::start(&meter, true);
    let check = start(&meter);
    read_until(&check, "two seconds of columns", WAIT, |v| {
        v.phase == Phase::Listening && v.timeline.columns.len() >= 40
    });
    let held = check.view().timeline.columns.len();
    // the plugin applies -12 dB, and the daemon says so
    let t = Instant::now();
    plugin.gain.store(0.25f32.to_bits(), Ordering::Relaxed);
    check.send(Command::Engine(EngineInfo {
        strength_db: 12.0,
        ..info(&meter)
    }));
    let mut fewest = usize::MAX;
    let mut first = None;
    let v = loop {
        let v = check.view();
        fewest = fewest.min(v.timeline.columns.len());
        let gap = v.timeline.columns.last().map_or(0.0, |c| c.raw - c.clean);
        // any of it (-1 dB) in the newest column; then all of it (-12 dB)
        if gap > 1.0 / 60.0 && first.is_none() {
            first = Some(t.elapsed());
        }
        if gap > 0.15 {
            break v;
        }
        assert!(
            t.elapsed() < Duration::from_secs(WAIT),
            "never showed: {v:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let (shown, whole) = (first.unwrap(), t.elapsed());
    eprintln!(
        "strength shown after {shown:?}, whole after {whole:?}, fewest columns {fewest} of {held}"
    );
    assert_eq!(v.phase, Phase::Listening);
    assert!(
        fewest >= held,
        "the picture kept its columns: {fewest} of {held}"
    );
    // a column (50 ms) to fill and a read (25 ms), with slack for a busy
    // runner; the next column then whole
    assert!(shown < Duration::from_millis(110), "{shown:?}");
    assert!(whole < Duration::from_millis(160), "{whole:?}");
    drop(check);
    drop(plugin);
    let _ = std::fs::remove_file(&meter);
}

/// A strength change during a replay: it plays on, at the new strength
/// (the take re-rendered: the player gets it where it is), and the
/// replay's picture shows it.
#[test]
fn a_replay_plays_on_at_a_new_strength() {
    let _serial = serial("exit");
    let meter = meter_path("restrength");
    let plugin = FakePlugin::start(&meter, true);
    // what the model leaves at the chain's strength (100 dB): a quarter
    plugin.gain.store(0.25f32.to_bits(), Ordering::Relaxed);
    let check = start(&meter);
    let bin = setup_env().join("bin");
    let (gate, plays) = (bin.join("play-gate"), bin.join("plays"));
    let _ = std::fs::remove_file(&plays);
    let starts = || {
        std::fs::read_to_string(&plays)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    };
    // a buffer recorded at the gain throughout
    read_until(&check, "a full buffer", WAIT, |v| {
        v.phase == Phase::Listening && v.timeline.columns.len() >= 120
    });
    std::fs::write(&gate, "").unwrap();
    check.send(Command::Replay);
    let v = read_until(&check, "the replay", WAIT, |v| {
        v.phase == Phase::Replaying && v.playback == PlaybackKind::Restart
    });
    wait_until("pw-play started", WAIT, || starts() == 1);
    let gap = |v: &View| {
        let c = &v.timeline.columns;
        c.iter().map(|c| c.raw - c.clean).sum::<f32>() / c.len() as f32
    };
    let before = gap(&v);
    // 12 dB leaves 0.25 of the input: 1 - 0.75 * 0.75 of it is heard
    check.send(Command::Engine(EngineInfo {
        strength_db: 12.0,
        ..info(&meter)
    }));
    let v = read_until(&check, "the replay at 12 dB", WAIT, |v| {
        gap(v) < before - 0.05
    });
    assert_eq!(v.phase, Phase::Replaying);
    assert!(!v.timeline.live);
    let want = -20.0 * (1.0f32 - 0.75 * 0.749).log10() / 60.0;
    assert!((gap(&v) - want).abs() < 0.02, "{} for {want}", gap(&v));
    assert!(before > 0.18, "{before}");
    // the player plays the new take from where it was
    wait_until("pw-play started at the new take", WAIT, || starts() == 2);
    check.send(Command::Stop);
    read_until(&check, "the end of the replay", WAIT, |v| {
        v.phase == Phase::Result
    });
    drop(check);
    drop(plugin);
    let _ = std::fs::remove_file(&gate);
    let _ = std::fs::remove_file(&meter);
}

/// Listen until a replay is possible, reading as a drawing page does.
fn ready_to_replay(check: &MicCheck) {
    read_until(check, "a replayable buffer", WAIT, |v| {
        v.phase == Phase::Listening && v.can_replay
    });
}

#[test]
fn a_pw_cat_that_cannot_play_a_pipe_switches_to_the_restart_player() {
    let _serial = serial("exit");
    let meter = meter_path("restart");
    let plugin = FakePlugin::start(&meter, true);
    let check = start(&meter);
    ready_to_replay(&check);
    let bin = setup_env().join("bin");
    let (gate, plays) = (bin.join("play-gate"), bin.join("plays"));
    let _ = std::fs::remove_file(&plays);
    let starts = || {
        std::fs::read_to_string(&plays)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    };
    // the fake pw-play plays until the gate goes
    std::fs::write(&gate, "").unwrap();
    check.send(Command::Replay);
    let play_file = run_dir().join("hushmic/miccheck-play.wav");
    read_until(&check, "the restart player", WAIT, |v| {
        v.phase == Phase::Replaying && v.playback == PlaybackKind::Restart
    });
    assert!(play_file.exists(), "the restart player plays a file");
    wait_until("pw-play started", WAIT, || starts() == 1);
    // pw-play reaches the take's end: the replay loops, a new pw-play
    // from the start, still replaying (held again by the gate).
    std::fs::remove_file(&gate).unwrap();
    wait_until("pw-play started over", WAIT, || starts() >= 2);
    std::fs::write(&gate, "").unwrap();
    // (a pw-play spawned before the gate came back has noted itself by now)
    std::thread::sleep(Duration::from_millis(300));
    let lap = starts();
    let v = check.view();
    assert_eq!(v.phase, Phase::Replaying, "{v:?}");
    assert_eq!(v.playback, PlaybackKind::Restart, "{v:?}");
    let at = v.timeline.position.expect("a playhead");
    assert!(at < 0.5, "the playhead starts over: {at}");
    // only Stop ends it
    check.send(Command::Stop);
    let v = read_until(&check, "the end of the replay", WAIT, |v| {
        v.phase == Phase::Result
    });
    assert_eq!(v.warning, None);
    // Restart mode sticks for this page: the next replay starts there
    // (once the buffer holds enough again).
    read_until(&check, "a replayable buffer again", WAIT, |v| v.can_replay);
    check.send(Command::Replay);
    read_until(&check, "a second replay", WAIT, |v| {
        v.phase == Phase::Replaying && v.playback == PlaybackKind::Restart
    });
    wait_until("pw-play started again", WAIT, || starts() == lap + 1);
    drop(check);
    let _ = std::fs::remove_file(&gate);
    let t = Instant::now();
    while play_file.exists() {
        assert!(
            t.elapsed() < Duration::from_secs(WAIT),
            "the file goes with the page"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(plugin);
    let _ = std::fs::remove_file(&meter);
}

#[test]
fn a_playback_that_never_moves_fails_with_a_warning() {
    let _serial = serial("hang");
    let meter = meter_path("hang");
    let plugin = FakePlugin::start(&meter, true);
    let check = start(&meter);
    let cats_file = setup_env().join("bin").join("cats");
    let _ = std::fs::remove_file(&cats_file);
    let cats = || -> Vec<i32> {
        std::fs::read_to_string(&cats_file)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    };
    ready_to_replay(&check);
    check.send(Command::Replay);
    read_until(&check, "replaying", WAIT, |v| v.phase == Phase::Replaying);
    let t = Instant::now();
    let v = read_until(&check, "the stall", WAIT, |v| v.phase != Phase::Replaying);
    assert!(
        t.elapsed() >= Duration::from_secs(2),
        "not before the stall timeout"
    );
    assert_eq!(v.warning, Some(Warning::PlaybackFailed));
    assert_eq!(
        v.playback,
        PlaybackKind::Seamless,
        "a late failure is no pipe problem"
    );
    // Leaving the page during a replay never holds up the window: the
    // stop fade (here: a pw-cat that never drains it and ignores SIGTERM,
    // so the stop ends only by SIGKILL after its 2 s) runs elsewhere.
    // Told by the pw-cat itself, not a clock: it is still there when drop
    // returns.
    let before = cats().len();
    check.send(Command::Replay);
    read_until(&check, "replaying again", WAIT, |v| {
        v.phase == Phase::Replaying
    });
    wait_until("the replay's pw-cat", WAIT, || cats().len() > before);
    let cat = *cats().last().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    drop(check);
    let stat = std::fs::read_to_string(format!("/proc/{cat}/stat")).unwrap_or_default();
    let state = stat
        .rsplit(')')
        .next()
        .unwrap_or("")
        .trim_start()
        .chars()
        .next();
    assert!(
        state.is_some_and(|c| c != 'Z'),
        "drop waited for the stop: its pw-cat was gone ({stat:?})"
    );
    drop(plugin);
    let _ = std::fs::remove_file(&meter);
}

/// The A/B window's settle rule, kept: a chain that is still coming up
/// (no meter yet) shows Connecting, and only after the settle window is a
/// missing microphone reported.
#[test]
fn a_chain_coming_up_settles_before_it_alarms() {
    let _serial = serial("exit");
    let meter = meter_path("never");
    let check = start(&meter);
    let t = Instant::now();
    let v = loop {
        let v = check.view();
        if v.phase != Phase::Connecting {
            break v;
        }
        assert!(t.elapsed() < Duration::from_secs(WAIT + 10), "{v:?}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(v.phase, Phase::Blocked(Blocker::NoMicrophone));
    assert!(
        t.elapsed() >= Duration::from_secs(7),
        "not before the settle window ({:?})",
        t.elapsed()
    );
}
