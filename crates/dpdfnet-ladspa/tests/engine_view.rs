//! The engine view (meter + Mic check tap) through the real worker: the
//! worker feeds both once per hop with the engine call's own input and
//! output, so the tap's pairs are exact after the fixed latency shift and
//! the meter's in and out describe the same sound. Fake engines, no ONNX
//! Runtime, except the asset-gated real-model case at the end.

mod common;

use dpdfnet_ladspa::{Aligner, EngineView, HopEngine, WorkerHandle};
use hushmic_denoiser::{Mode, HOP, LATENCY_SAMPLES};
use hushmic_shm::meter::{
    Hop, MeterReader, MODE_BYPASS, MODE_MUTE, MODE_PASSTHROUGH, MODE_PROCESS,
};
use hushmic_shm::tap::{self, TapRing, TapState};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const LAG: u64 = LATENCY_SAMPLES as u64;

/// How long anything that should happen gets: only a hang waits that
/// long (a loaded runner is slow, never this slow).
const HANG: Duration = Duration::from_secs(60);

/// Output = input delayed by exactly LATENCY_SAMPLES, times a gain per
/// mode (process 0.5, bypass 1, mute 0); reset clears the delay line.
struct DelayEngine {
    line: Vec<f32>,
    pos: usize,
    mode: Mode,
    process_gain: f32,
    /// Reports its hops as the passthrough tier's.
    raw: bool,
}

impl DelayEngine {
    fn new(process_gain: f32) -> DelayEngine {
        DelayEngine {
            line: vec![0.0; LATENCY_SAMPLES],
            pos: 0,
            mode: Mode::Process,
            process_gain,
            raw: false,
        }
    }
}

impl HopEngine for DelayEngine {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        let gain = match self.mode {
            Mode::Mute => 0.0,
            Mode::Bypass => 1.0,
            _ => self.process_gain,
        };
        for (o, &x) in output.iter_mut().zip(input) {
            *o = self.line[self.pos] * gain;
            self.line[self.pos] = x;
            self.pos = (self.pos + 1) % self.line.len();
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.line.iter_mut().for_each(|s| *s = 0.0);
        self.pos = 0;
    }
    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }
    fn set_attenuation_limit_db(&mut self, _: f32) {}
    fn unfiltered(&self) -> bool {
        self.raw
    }
}

fn scratch(tag: &str) -> PathBuf {
    let d = hushmic_shm::test_dir_base().join(format!("hushmic-view-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    d
}

/// A unique, exactly representable, non-zero value per input sample.
fn v(i: u64) -> f32 {
    ((i % 999_983) + 1) as f32 / 1_000_000.0
}

/// Drives the worker like the plugin's run() (push, wake, pop through the
/// aligner) and keeps everything it emitted.
struct Driver {
    w: WorkerHandle,
    a: Aligner,
    accepted: usize,
    popped: usize,
    emitted: Vec<f32>,
    next_sample: u64,
}

impl Driver {
    fn new(view: EngineView, gain: f32) -> Driver {
        Driver::with_engine(view, DelayEngine::new(gain))
    }

    fn with_engine(view: EngineView, engine: DelayEngine) -> Driver {
        let mut d = Driver {
            w: WorkerHandle::spawn_timeshare_with_view(engine, view).expect("spawn"),
            a: Aligner::new(),
            accepted: 0,
            popped: 0,
            emitted: Vec::new(),
            next_sample: 0,
        };
        d.activate();
        d
    }

    fn activate(&mut self) {
        assert!(self.w.request_reset_and_wait(HANG));
        self.w.drain_output();
        self.a.reset();
        self.accepted = 0;
        self.popped = 0;
    }

    /// Push `n` samples of the `v()` sequence in host cycles of `quantum`,
    /// waiting for the worker after each cycle (never overflowing).
    fn feed(&mut self, n: usize, quantum: usize) {
        let mut left = n;
        while left > 0 {
            let q = quantum.min(left);
            let input: Vec<f32> = (0..q as u64).map(|k| v(self.next_sample + k)).collect();
            self.next_sample += q as u64;
            self.w.note_quantum(q);
            assert_eq!(self.w.push_input(&input), q, "no overflow");
            self.accepted += q;
            self.w.wake();
            self.wait_caught_up();
            let plan = self.a.plan(q, self.w.output_available());
            for _ in 0..plan.discard {
                self.w.pop_output();
            }
            self.emitted
                .extend(std::iter::repeat_n(0.0, plan.lead_zeros));
            for _ in 0..plan.real {
                self.emitted.push(self.w.pop_output().unwrap_or(0.0));
            }
            self.popped += plan.discard + plan.real;
            self.emitted
                .extend(std::iter::repeat_n(0.0, plan.tail_zeros));
            left -= q;
        }
    }

    fn wait_caught_up(&self) {
        let expect = (self.accepted / HOP) * HOP - self.popped;
        let deadline = Instant::now() + HANG;
        while self.w.output_available() < expect {
            assert!(Instant::now() < deadline, "worker never caught up");
            self.w.wake();
            std::thread::sleep(Duration::from_micros(200));
        }
    }

    fn set_mode(&mut self, mode: f32) {
        self.w.set_mode_control(mode);
    }
}

/// Request the tap and wait until the plugin's helper has mapped it.
fn tap_on(meter: &Path) -> Arc<TapRing> {
    let ring = Arc::new(TapRing::request(meter).expect("request"));
    let deadline = Instant::now() + HANG;
    loop {
        if let TapState::Mapped { clean_lag, .. } = ring.state() {
            assert_eq!(clean_lag as u64, LAG);
            break;
        }
        assert!(Instant::now() < deadline, "the helper never mapped the tap");
        ring.heartbeat();
        std::thread::sleep(Duration::from_millis(20));
    }
    // The helper marks it mapped, then takes its name.
    while tap::request_path(meter).exists() {
        assert!(Instant::now() < deadline, "a mapped ring keeps its name");
        std::thread::sleep(Duration::from_millis(20));
    }
    ring
}

/// The window's heartbeat, on its own thread as in the window (every
/// [`tap::HEARTBEAT_EVERY_MS`] or sooner), whatever the test thread is
/// doing; it stops when dropped (the window hidden or crashed).
struct Beat {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn beat(ring: &Arc<TapRing>) -> Beat {
    let (ring, stop) = (Arc::clone(ring), Arc::new(AtomicBool::new(false)));
    let st = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        while !st.load(Ordering::Relaxed) {
            ring.heartbeat();
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    Beat {
        stop,
        thread: Some(thread),
    }
}

impl Drop for Beat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn mapped(ring: &TapRing) -> bool {
    matches!(ring.state(), TapState::Mapped { .. })
}

#[test]
fn tap_pairs_are_exact_across_wraps_a_reset_and_mode_switches() {
    let dir = scratch("exact");
    let meter = dir.join("meter-1");
    let mut d = Driver::new(EngineView::create(&meter).unwrap(), 1.0);
    let ring = tap_on(&meter);
    let _beat = beat(&ring);
    let (mut raw, mut clean) = (Vec::new(), Vec::new());
    let mut next = 0u64;
    fn drain(
        ring: &TapRing,
        next: &mut u64,
        raw: &mut Vec<f32>,
        clean: &mut Vec<f32>,
    ) -> tap::Pairs {
        let p = ring.read_pairs(*next, usize::MAX, raw, clean);
        *next = p.start + p.len as u64;
        p
    }
    // Two and a half ring lengths in odd host cycles, read every ~50 ms of
    // audio like the window does.
    let total = (tap::CAPACITY * 5 / 2) as usize;
    let mut fed = 0;
    while fed < total {
        d.feed(2400, 1024);
        fed += 2400;
        ring.heartbeat();
        drain(&ring, &mut next, &mut raw, &mut clean);
    }
    assert!(next > tap::CAPACITY * 2, "read across wraps ({next})");
    assert_eq!(raw.len(), clean.len());
    for (k, (r, c)) in raw.iter().zip(&clean).enumerate() {
        assert_eq!(r, c, "pair {k} after the shift");
    }
    assert!(raw.iter().all(|&x| x != 0.0), "every pair carried audio");

    // An engine reset (re-activation): pairs straddling it are never
    // returned, pairs after it are exact again.
    let before_reset = raw.len();
    d.activate();
    d.feed(48_000, 512);
    assert!(mapped(&ring), "the beating window keeps its tap");
    let epoch_before = ring.head().epoch;
    let p = drain(&ring, &mut next, &mut raw, &mut clean);
    assert_eq!(p.epoch, epoch_before);
    assert!(p.epoch >= 1);
    for (r, c) in raw[before_reset..].iter().zip(&clean[before_reset..]) {
        assert_eq!(r, c, "exact after the reset");
    }

    // Bypass keeps the pairs equal, mute silences the clean side only.
    let before_modes = raw.len();
    d.set_mode(1.0);
    d.feed(24_000, 480);
    d.set_mode(2.0);
    d.feed(24_000, 480);
    d.set_mode(0.0);
    d.feed(24_000, 480);
    ring.heartbeat();
    drain(&ring, &mut next, &mut raw, &mut clean);
    let (mut muted, mut equal) = (0, 0);
    for (r, c) in raw[before_modes..].iter().zip(&clean[before_modes..]) {
        if *c == 0.0 {
            muted += 1;
        } else {
            assert_eq!(r, c, "a pair is either the same sample or muted");
            equal += 1;
        }
    }
    assert!(
        muted >= 20_000 && equal >= 40_000,
        "{muted} muted, {equal} equal"
    );

    // The RT side never noticed: up to the reset every emitted sample sits
    // at one constant latency after its input (the aligner's lead for this
    // host cycle plus the engine's delay).
    let emitted = &d.emitted;
    let latency = emitted.iter().position(|&x| x != 0.0).unwrap();
    assert_eq!(emitted[latency], v(0));
    assert!(latency >= LATENCY_SAMPLES);
    let mut checked = 0;
    for (p, &x) in emitted.iter().enumerate() {
        if x != 0.0 && p < fed {
            assert_eq!(x, v((p - latency) as u64), "RT output at {p}");
            checked += 1;
        }
    }
    assert!(checked > total / 2);
    ring.close();
    drop(d);
    let _ = std::fs::remove_dir_all(&dir);
}

fn read_meter(r: &mut MeterReader) -> Vec<Hop> {
    let mut out = Vec::new();
    let rep = r.read(&mut out);
    assert_eq!(rep.lost, 0);
    out
}

#[test]
fn meter_pairs_in_and_out_of_the_same_sound_in_every_mode() {
    let dir = scratch("meter");
    let meter = dir.join("meter-2");
    let view = EngineView::create(&meter).unwrap();
    let instance = view.instance();
    let mut d = Driver::new(view, 0.5);
    let mut r = MeterReader::open(&meter).unwrap();
    assert_eq!(r.instance(), instance);
    assert_eq!(r.lag_hops() as usize, LATENCY_SAMPLES / HOP);
    let mut hops: Vec<Hop> = Vec::new();
    for mode in [0.0, 1.0, 2.0, 0.0] {
        d.set_mode(mode);
        d.feed(30 * HOP, 1024);
        hops.extend(read_meter(&mut r));
    }
    let lag = LATENCY_SAMPLES / HOP;
    assert!(hops.len() >= 110);
    let seen: Vec<u32> = hops.iter().map(|h| h.mode).collect();
    for m in [MODE_PROCESS, MODE_BYPASS, MODE_MUTE] {
        assert!(seen.contains(&m), "mode {m} published");
    }
    for h in hops.iter().filter(|h| h.index as usize >= lag) {
        let want = match h.mode {
            MODE_PROCESS => h.in_peak * 0.5,
            MODE_BYPASS => h.in_peak,
            _ => 0.0,
        };
        assert_eq!(h.out_peak, want, "hop {} in mode {}", h.index, h.mode);
        assert!(h.in_peak > 0.0, "the input side moves");
    }
    // Activation is an engine reset: a new epoch, the input side restarts.
    // (A runner stalled past the idle gap may start a run of its own in
    // between, so epochs only ever grow.)
    assert!(hops.windows(2).all(|w| w[1].epoch >= w[0].epoch));
    let epoch = hops.last().unwrap().epoch;
    d.activate();
    d.feed(10 * HOP, 480);
    let after = read_meter(&mut r);
    assert!(after[0].epoch > epoch, "a new run");
    assert!(after.windows(2).all(|w| w[1].epoch >= w[0].epoch));
    assert_eq!(
        after[0].in_peak, 0.0,
        "the delayed input starts from silence"
    );
    drop(d);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_tap_goes_away_on_close_and_on_a_stale_heartbeat() {
    let dir = scratch("life");
    let meter = dir.join("meter-3");
    let view = EngineView::create(&meter).unwrap();
    let slot = std::sync::Arc::clone(view.tap_slot());
    let mut d = Driver::new(view, 1.0);
    // Closed: released within the helper's poll.
    let ring = tap_on(&meter);
    let ring_beat = beat(&ring);
    assert!(slot.attached());
    d.feed(4800, 480);
    assert!(ring.head().write_frames >= 4800);
    let t = Instant::now();
    ring.close();
    while ring.state() != TapState::Released {
        assert!(t.elapsed() < HANG, "close ignored");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(ring_beat);
    assert!(!slot.attached());
    let frozen = ring.head().write_frames;
    d.feed(4800, 480);
    assert_eq!(ring.head().write_frames, frozen, "no writes after release");

    // A window that stops beating (hidden, crashed) loses it, not before
    // the heartbeat timeout (counted from before its last beat).
    let ring = tap_on(&meter);
    let t = Instant::now();
    ring.heartbeat();
    while ring.state() != TapState::Released {
        assert!(t.elapsed() < HANG, "stale heartbeat ignored");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        t.elapsed() >= Duration::from_millis(tap::HEARTBEAT_TIMEOUT_MS - 2),
        "not before the timeout ({:?})",
        t.elapsed()
    );
    assert!(!slot.attached());

    // A malformed request never reaches the worker and is left alone.
    let bad = tap::request_path(&meter);
    std::fs::write(&bad, b"not a ring").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::thread::sleep(Duration::from_millis(700));
    assert!(!slot.attached());
    assert!(bad.exists());
    std::fs::remove_file(&bad).unwrap();

    // The worker keeps streaming throughout; dropping the plugin with a
    // tap attached releases it too.
    let ring = tap_on(&meter);
    d.feed(4800, 480);
    drop(d);
    let t = Instant::now();
    while ring.state() != TapState::Released {
        assert!(t.elapsed() < HANG, "drop kept the tap");
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The passthrough tier passes the raw microphone while processing is on:
/// the meter says so (unfiltered), and bypass and mute stay what they are.
#[test]
fn the_passthrough_tier_shows_as_unfiltered_on_the_meter() {
    let dir = scratch("raw");
    let meter = dir.join("meter-5");
    let view = EngineView::create(&meter).unwrap();
    let engine = DelayEngine {
        raw: true,
        ..DelayEngine::new(1.0)
    };
    let mut d = Driver::with_engine(view, engine);
    let mut r = MeterReader::open(&meter).unwrap();
    let mut modes = Vec::new();
    for mode in [0.0, 1.0, 2.0] {
        d.set_mode(mode);
        d.feed(10 * HOP, 480);
        modes.push(read_meter(&mut r).last().unwrap().mode);
    }
    assert_eq!(modes, [MODE_PASSTHROUGH, MODE_BYPASS, MODE_MUTE]);
    drop(d);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A window re-created after a crash asks again while the plugin still
/// holds the dead window's ring: the new request is served at once, not
/// after the old heartbeat goes stale, and the worker writes into it from
/// frame 0. The old ring keeps beating until the new one is mapped (the
/// plugin cannot tell a dead window from a live one before the timeout),
/// so only the replacement can let it go.
#[test]
fn a_new_request_replaces_a_crashed_windows_ring() {
    let dir = scratch("crash");
    let meter = dir.join("meter-4");
    let view = EngineView::create(&meter).unwrap();
    let mut d = Driver::new(view, 1.0);
    for delay in [Duration::from_millis(0), Duration::from_millis(300)] {
        let old = tap_on(&meter);
        let old_beat = beat(&old);
        d.feed(4800, 480);
        // The window crashes (no close) and is back before its heartbeat
        // goes stale.
        std::thread::sleep(delay);
        let new = tap_on(&meter);
        let _new_beat = beat(&new);
        drop(old_beat);
        // a stale heartbeat would have Released it
        assert_eq!(old.state(), TapState::Replaced, "the dead ring is let go");
        let before = old.head().write_frames;
        d.feed(9600, 480);
        assert_eq!(old.head().write_frames, before);
        let head = new.head();
        assert_eq!(head.write_frames, 9600, "a fresh position in the new ring");
        let (mut raw, mut clean) = (Vec::new(), Vec::new());
        let p = new.read_pairs(0, usize::MAX, &mut raw, &mut clean);
        assert_eq!(p.len as u64, 9600 - LAG);
        assert_eq!(raw, clean, "gain 1: clean[n] == raw[n - lag]");
        new.close();
        let t = Instant::now();
        while new.state() != TapState::Released {
            assert!(t.elapsed() < HANG);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    drop(d);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The real models: the tap's clean side in bypass is the raw side after
/// the fixed shift (the engine's own bypass is a spectral pass-through,
/// so equal to float precision), and mute is silence.
#[test]
fn real_engine_bypass_lines_up_at_the_fixed_shift() {
    let (Some(model), Some(rt)) = (
        common::model_path("dpdfnet2_48khz_hr.onnx"),
        common::runtime_path(),
    ) else {
        eprintln!("skipped: assets not provisioned");
        return;
    };
    hushmic_denoiser::init_runtime(rt).expect("runtime");
    let den = hushmic_denoiser::Denoiser::from_file(&model).expect("model");
    let dir = scratch("real");
    let meter = dir.join("meter-4");
    let view = EngineView::create(&meter).unwrap();
    let mut w = WorkerHandle::spawn_timeshare_with_view(den, view).expect("spawn");
    assert!(w.request_reset_and_wait(HANG));
    w.drain_output();
    let ring = tap_on(&meter);
    let _beat = beat(&ring);
    w.set_mode_control(1.0);
    // A 440 Hz tone with a click every 100 ms.
    let signal: Vec<f32> = (0..96_000u64)
        .map(|i| {
            let t = i as f32 / 48_000.0;
            0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
                + if i % 4800 == 0 { 0.5 } else { 0.0 }
        })
        .collect();
    for chunk in signal.chunks(960) {
        assert_eq!(w.push_input(chunk), chunk.len());
        w.wake();
        let deadline = Instant::now() + HANG;
        while w.output_available() < chunk.len() - chunk.len() % HOP {
            assert!(Instant::now() < deadline);
            w.wake();
            std::thread::sleep(Duration::from_micros(500));
        }
        while w.pop_output().is_some() {}
        ring.heartbeat();
    }
    let (mut raw, mut clean) = (Vec::new(), Vec::new());
    let p = ring.read_pairs(0, usize::MAX, &mut raw, &mut clean);
    assert!(p.len > 80_000);
    // Skip the bypass ramp and the analysis warm-up.
    let worst = raw[9600..]
        .iter()
        .zip(&clean[9600..])
        .map(|(r, c)| (r - c).abs())
        .fold(0f32, f32::max);
    assert!(
        worst < 1e-3,
        "bypass is the raw side at the fixed shift ({worst})"
    );
    // One sample off would be far worse: the tone moves ~0.017 per sample.
    let off_by_one = raw[9600..raw.len() - 1]
        .iter()
        .zip(&clean[9601..])
        .map(|(r, c)| (r - c).abs())
        .fold(0f32, f32::max);
    assert!(
        off_by_one > 10.0 * worst.max(1e-5),
        "{off_by_one} vs {worst}"
    );
    ring.close();
    drop(w);
    let _ = std::fs::remove_dir_all(&dir);
}
