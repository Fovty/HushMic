//! Mic check against a running chain: the engine meter flows, the tap
//! delivers raw and clean pairs that line up at lag 0 after the fixed
//! shift, the backend replays and switches sides, and nothing is left in
//! the runtime dir afterwards.
//!
//! Needs a live PipeWire session with HushMic's chain on a virtual
//! microphone that plays a known signal (the CI battery's setup). Gated:
//! set HUSHMIC_E2E_METER to the chain's meter file and HUSHMIC_E2E_MIC to
//! the microphone's node name; HUSHMIC_E2E_PASSTHROUGH=1 when the chain
//! runs with HUSHMIC_DSP_TIER=passthrough (then clean equals raw).

use hushmic::meter::{EngineMeter, MeterState};
use hushmic::miccheck::{Command, Engine, EngineInfo, Listen, MicCheck, Phase, Setup, Source};
use hushmic_shm::tap::{self, TapRing, TapState};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A recorder of the test's own that goes away with it, also when an
/// assertion fails (a leftover would hold the runner's output open).
struct Recorder(std::process::Child);

impl Recorder {
    fn on(node: &str) -> Recorder {
        Recorder(
            std::process::Command::new("pw-record")
                .args([
                    "--target",
                    node,
                    "--rate",
                    "48000",
                    "--channels",
                    "1",
                    "/dev/null",
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("pw-record"),
        )
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn env() -> Option<(PathBuf, String)> {
    let meter = std::env::var_os("HUSHMIC_E2E_METER")?;
    let mic = std::env::var("HUSHMIC_E2E_MIC").unwrap_or_else(|_| "virt_mic".into());
    Some((PathBuf::from(meter), mic))
}

fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Normalised cross-correlation of clean against raw at `lag` samples.
fn ncc(raw: &[f32], clean: &[f32], lag: i64) -> f32 {
    let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
    for n in 600..clean.len() - 600 {
        let r = raw[(n as i64 - lag) as usize] as f64;
        let c = clean[n] as f64;
        ab += r * c;
        aa += r * r;
        bb += c * c;
    }
    (ab / (aa * bb).sqrt().max(1e-12)) as f32
}

#[test]
fn the_tap_lines_up_and_mic_check_replays() {
    let Some((meter, mic)) = env() else {
        eprintln!("skipped: HUSHMIC_E2E_METER not set");
        return;
    };
    let dir = meter.parent().unwrap().to_path_buf();

    // The keep-alive is the window's job; here the test's own request
    // brings one up through MicCheck below, so first check the meter with
    // a direct tap and a recorder of our own.
    let keep = Recorder::on("hushmic_source");
    let mut m = EngineMeter::new();
    m.set_path(Some(&meter));
    let mut cols = Vec::new();
    wait_until("the meter to flow", 10, || {
        m.poll(Instant::now(), &mut cols) == MeterState::Flowing && cols.len() > 10
    });
    assert!(
        cols.iter().any(|c| c.input_peak > 0.01),
        "the mic side moves"
    );
    let passthrough = std::env::var("HUSHMIC_E2E_PASSTHROUGH").as_deref() == Ok("1");
    if passthrough {
        // The raw tier is the input delayed by exactly the shift the meter
        // removes: in and out are the same sound.
        for c in cols.iter().filter(|c| c.complete).skip(2) {
            assert!(
                (c.input_peak - c.output_peak).abs() <= 1e-4 * c.input_peak.max(1e-3),
                "{c:?}"
            );
            // And it draws as unfiltered, as Live shows the step-down.
            assert_eq!(c.mode, hushmic::meter::EngineMode::Bypass, "{c:?}");
        }
    }

    let ring = TapRing::request(&meter).expect("request");
    wait_until("the plugin to map the tap", 3, || {
        ring.heartbeat();
        matches!(ring.state(), TapState::Mapped { .. })
    });
    assert!(!tap::request_path(&meter).exists(), "no name once mapped");
    let (mut raw, mut clean) = (Vec::new(), Vec::new());
    wait_until("two seconds of pairs", 10, || {
        ring.heartbeat();
        raw.clear();
        clean.clear();
        ring.read_pairs(0, 96_000, &mut raw, &mut clean).len >= 96_000
    });
    if passthrough {
        let worst = raw
            .iter()
            .zip(&clean)
            .map(|(r, c)| (r - c).abs())
            .fold(0f32, f32::max);
        assert!(
            worst < 1e-5,
            "passthrough: clean is raw after the shift ({worst})"
        );
    }
    let at0 = ncc(&raw, &clean, 0);
    let best = (-480..=480)
        .map(|l| (l, ncc(&raw, &clean, l)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    assert_eq!(
        best.0, 0,
        "correlation peaks at lag 0 after the shift ({best:?}, at 0: {at0})"
    );
    ring.close();
    wait_until("the plugin to let go", 2, || {
        ring.state() == TapState::Released
    });
    drop(ring);
    drop(keep);

    // The backend: tap source, a replay, the hold, the result.
    let repaints = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let r2 = Arc::clone(&repaints);
    let check = MicCheck::start(
        Setup {
            info: EngineInfo {
                engine: Engine::Filtering,
                meter: Some(meter.clone()),
                mic_node: Some(mic),
                mic_name: "Virtual mic".into(),
                pipewire: true,
                model: "dpdfnet8_48khz_hr".into(),
                strength_db: 100.0,
            },
            columns: true,
            once: false,
        },
        Arc::new(move |_| {
            r2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }),
    );
    wait_until("listening on the tap", 10, || {
        let v = check.view();
        v.source == Source::Tap && v.phase == Phase::Listening && v.can_replay
    });
    let v = check.view();
    assert!(v.timeline.live && v.timeline.columns.len() >= 40);
    check.send(Command::Replay);
    wait_until("the playhead to move", 5, || {
        check.view().timeline.position.is_some_and(|p| p > 0.05)
    });
    check.send(Command::Hold(true));
    wait_until("holding the original", 2, || {
        let v = check.view();
        v.holding && v.listen == Listen::Original && v.phase == Phase::Replaying
    });
    check.send(Command::Hold(false));
    check.send(Command::Stop);
    wait_until("the result", 3, || {
        let v = check.view();
        v.phase == Phase::Result && v.result.is_some() && v.timeline.live
    });
    assert!(repaints.load(std::sync::atomic::Ordering::Relaxed) > 10);
    drop(check);
    wait_until("no tap files left", 3, || {
        std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().contains(".tap"))
    });
}

/// The fallback: a chain whose plugin never maps the tap (here a meter
/// file nobody serves) makes Mic check record the two streams instead and
/// align each take by cross-correlation.
#[test]
fn without_the_tap_the_streams_align_and_replay() {
    let Some((meter, mic)) = env() else {
        eprintln!("skipped: HUSHMIC_E2E_METER not set");
        return;
    };
    let dir = meter.parent().unwrap().to_path_buf();
    let fake = dir.join("meter-999999");
    let writer = hushmic_shm::meter::MeterWriter::create(&fake, 1, 5).unwrap();
    let check = MicCheck::start(
        Setup {
            info: EngineInfo {
                engine: Engine::Filtering,
                meter: Some(fake.clone()),
                mic_node: Some(mic),
                mic_name: "Virtual mic".into(),
                pipewire: true,
                model: "dpdfnet8_48khz_hr".into(),
                strength_db: 100.0,
            },
            columns: true,
            once: false,
        },
        Arc::new(|_| {}),
    );
    wait_until("listening on the streams", 15, || {
        let v = check.view();
        v.source == Source::Streams && v.phase == Phase::Listening && v.can_replay
    });
    assert!(
        !tap::request_path(&fake).exists(),
        "the unanswered request was removed"
    );
    // Let the buffer fill so the alignment has speech to work with,
    // reading the view as a drawing page does (an unread page lets go).
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(6) {
        check.view();
        std::thread::sleep(Duration::from_millis(50));
    }
    check.send(Command::Replay);
    wait_until("the playhead to move", 5, || {
        check.view().timeline.position.is_some_and(|p| p > 0.05)
    });
    let v = check.view();
    assert_eq!(
        v.playback,
        hushmic::miccheck::PlaybackKind::Seamless,
        "the take was aligned"
    );
    check.send(Command::Listen(Listen::Original));
    check.send(Command::Stop);
    wait_until("the result", 3, || check.view().result.is_some());
    drop(check);
    drop(writer);
    let _ = std::fs::remove_file(&fake);
}

/// The pids of this process's children (all threads).
fn children() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
        for t in tasks.flatten() {
            if let Ok(s) = std::fs::read_to_string(t.path().join("children")) {
                v.extend(s.split_whitespace().map(str::to_string));
            }
        }
    }
    v
}

#[test]
fn the_level_probe_hears_the_mic() {
    use hushmic::miccheck::probe::{LevelProbe, ProbeState};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let Ok(mic) = std::env::var("HUSHMIC_E2E_MIC") else {
        eprintln!("skipped: HUSHMIC_E2E_MIC not set");
        return;
    };
    let before = children();
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = Arc::clone(&wakes);
    let probe = LevelProbe::start(
        Some(mic.clone()),
        Arc::new(move |_| {
            w.fetch_add(1, Ordering::Relaxed);
        }),
    );
    assert_eq!(probe.node(), Some(mic.as_str()));
    wait_until("a level from the mic", 5, || {
        probe.level().is_some_and(|l| l > 0.0)
    });
    // The fixture plays continuously: the window is woken with the levels.
    let w0 = wakes.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(500));
    let woken = wakes.load(Ordering::Relaxed) - w0;
    assert!(
        (5..=15).contains(&woken),
        "about 20 wakes a second, got {woken} in 0.5 s"
    );
    // The window stops drawing: no reads while the level moves. The
    // recording ends, and the next read starts it again.
    std::thread::sleep(Duration::from_millis(2_500));
    assert_eq!(children(), before, "not recording while nobody looks");
    wait_until("the level again", 5, || {
        probe.level().is_some_and(|l| l > 0.0)
    });
    let t0 = Instant::now();
    drop(probe);
    assert!(t0.elapsed() < Duration::from_secs(3), "dropping is prompt");
    assert_eq!(children(), before, "the recorder is reaped");

    // A device that is not there.
    let gone = LevelProbe::start(Some("hushmic_test_no_such_mic".into()), Arc::new(|_| {}));
    wait_until("unavailable", 5, || gone.state() == ProbeState::Unavailable);
    drop(gone);

    // "System default" follows the default source (the virtual mic is the
    // only one here).
    let dflt = LevelProbe::start(None, Arc::new(|_| {}));
    wait_until("a level from the default", 8, || {
        dflt.level().is_some_and(|l| l > 0.0)
    });
    drop(dflt);
    assert_eq!(children(), before, "every recorder is reaped");
}

/// A microphone that hears nothing (HUSHMIC_E2E_QUIET_MIC: a source no one
/// plays into): the level stands at 0 and wakes nothing, yet a window that
/// stops drawing still lets go of the microphone.
#[test]
fn the_level_probe_lets_go_in_a_quiet_room_when_nobody_looks() {
    use hushmic::miccheck::probe::{LevelProbe, ProbeState, QUIET_CHECK};
    use hushmic::miccheck::LOOK_TIMEOUT;
    let Ok(mic) = std::env::var("HUSHMIC_E2E_QUIET_MIC") else {
        eprintln!("skipped: HUSHMIC_E2E_QUIET_MIC not set");
        return;
    };
    let before = children();
    let probe = LevelProbe::start(Some(mic), Arc::new(|_| {}));
    wait_until("the quiet level", 5, || probe.level() == Some(0.0));
    // A window that draws keeps it, even with nothing to show.
    let t = Instant::now();
    while t.elapsed() < QUIET_CHECK + LOOK_TIMEOUT + Duration::from_secs(1) {
        assert!(matches!(probe.state(), ProbeState::Live(_)));
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_ne!(children(), before, "still recording");
    // A window that stopped drawing loses it.
    std::thread::sleep(QUIET_CHECK + LOOK_TIMEOUT + Duration::from_secs(1));
    assert_eq!(children(), before, "not recording while nobody looks");
    wait_until("the level again", 5, || probe.level() == Some(0.0));
    drop(probe);
    assert_eq!(children(), before);
}

/// A window that opens mid-call draws the recent past at once: the meter's
/// history holds the call's last seconds, whole and in order.
#[test]
fn a_view_opening_mid_call_gets_the_meters_history() {
    let Some((meter, _)) = env() else {
        eprintln!("skipped: HUSHMIC_E2E_METER not set");
        return;
    };
    let call = Recorder::on("hushmic_source");
    std::thread::sleep(Duration::from_secs(4));
    let mut m = EngineMeter::new();
    m.set_path(Some(&meter));
    let mut cols = Vec::new();
    let state = m.history(Instant::now(), 60, &mut cols);
    drop(call);
    assert_eq!(state, MeterState::Flowing);
    assert_eq!(cols.len(), 60, "3 s of history after 4 s of a call");
    assert!(cols.iter().all(|c| c.complete));
    assert!(cols.windows(2).all(|c| c[1].index == c[0].index + 1));
    assert!(
        cols.iter().any(|c| c.input_level() > 0.0),
        "the fixture's noise is in it"
    );
}
