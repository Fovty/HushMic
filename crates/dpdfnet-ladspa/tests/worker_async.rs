//! Async-path integration tests (issue #10): the worker + rings + ledger,
//! driven exactly the way the plugin's `run()` drives them, against fake
//! engines — no ONNX Runtime, no models. Alignment assertions are exact
//! and timing-independent; only where a test must observe a stall does it
//! pace in real time, and those assertions stay one-sided so a noisy CI
//! machine cannot flake them.

use dpdfnet_ladspa::{
    required_lead, AdaptiveEngine, Aligner, HopEngine, Ladder, PopPlan, Tier, WorkerHandle,
    OUTPUT_LEAD, RING_CAPACITY, RUN_GUARD_MIN_PARK,
};
use hushmic_denoiser::{Mode, HOP};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Identity DSP: output = input, never fails.
struct IdentityEngine;
impl HopEngine for IdentityEngine {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        output.copy_from_slice(input);
        Ok(())
    }
    fn reset(&mut self) {}
    fn set_mode(&mut self, _: Mode) {}
    fn set_attenuation_limit_db(&mut self, _: f32) {}
}

/// Identity, but hop `stall_at` takes `stall` of wall time.
struct StallingEngine {
    hop: usize,
    stall_at: usize,
    stall: Duration,
}
impl HopEngine for StallingEngine {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        if self.hop == self.stall_at {
            std::thread::sleep(self.stall);
        }
        self.hop += 1;
        output.copy_from_slice(input);
        Ok(())
    }
    fn reset(&mut self) {
        self.hop = 0;
    }
    fn set_mode(&mut self, _: Mode) {}
    fn set_attenuation_limit_db(&mut self, _: f32) {}
}

/// Panics on hop `at`.
struct PanickingEngine {
    hop: usize,
    at: usize,
}
impl HopEngine for PanickingEngine {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        assert!(self.hop != self.at, "injected engine panic");
        self.hop += 1;
        output.copy_from_slice(input);
        Ok(())
    }
    fn reset(&mut self) {}
    fn set_mode(&mut self, _: Mode) {}
    fn set_attenuation_limit_db(&mut self, _: f32) {}
}

/// Every hop burns `busy` of wall time and records when it started.
struct BusyEngine {
    busy: Duration,
    starts: Arc<Mutex<Vec<Instant>>>,
    lags: Arc<Mutex<Vec<u32>>>,
}
impl HopEngine for BusyEngine {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        self.starts.lock().unwrap().push(Instant::now());
        let t = Instant::now();
        while t.elapsed() < self.busy {
            std::hint::spin_loop();
        }
        output.copy_from_slice(input);
        Ok(())
    }
    fn reset(&mut self) {}
    fn set_mode(&mut self, _: Mode) {}
    fn set_attenuation_limit_db(&mut self, _: f32) {}
    fn set_lag_hops(&mut self, hops: u32) {
        self.lags.lock().unwrap().push(hops);
    }
}

/// Always errs, filling output with a marker (the engine contract:
/// output is valid even on Err).
struct FailingEngine {
    hops: Arc<AtomicUsize>,
}
impl HopEngine for FailingEngine {
    fn process_hop(&mut self, _: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        output.fill(0.5);
        self.hops.fetch_add(1, Ordering::SeqCst);
        Err("synthetic inference failure".into())
    }
    fn reset(&mut self) {}
    fn set_mode(&mut self, _: Mode) {}
    fn set_attenuation_limit_db(&mut self, _: f32) {}
}

/// Records control calls.
#[derive(Default)]
struct Recording {
    modes: Vec<Mode>,
    attns: Vec<f32>,
    resets: usize,
}
struct RecordingEngine {
    log: Arc<Mutex<Recording>>,
}
impl HopEngine for RecordingEngine {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        output.copy_from_slice(input);
        Ok(())
    }
    fn reset(&mut self) {
        self.log.lock().unwrap().resets += 1;
    }
    fn set_mode(&mut self, m: Mode) {
        self.log.lock().unwrap().modes.push(m);
    }
    fn set_attenuation_limit_db(&mut self, db: f32) {
        self.log.lock().unwrap().attns.push(db);
    }
}

/// Drives the worker exactly like the plugin's `run()` and keeps the
/// bookkeeping the tests need (accepted pushes, ring pops incl. discards).
struct Driver {
    w: WorkerHandle,
    a: Aligner,
    out: Vec<f32>,
    accepted: usize,
    ring_popped: usize,
    restarting: bool,
    restarts: usize,
}

impl Driver {
    fn new(engine: impl HopEngine) -> Driver {
        Driver {
            // Timeshare like the driving thread; see spawn_timeshare.
            w: WorkerHandle::spawn_timeshare(engine).expect("spawn"),
            a: Aligner::new(),
            out: Vec::new(),
            accepted: 0,
            ring_popped: 0,
            restarting: false,
            restarts: 0,
        }
    }

    /// One callback, exactly the plugin's `run()`: push, wake, plan,
    /// discard, emit — including the overflow -> stream-restart path.
    fn drive(&mut self, input: &[f32]) -> PopPlan {
        let silence = PopPlan {
            discard: 0,
            lead_zeros: input.len(),
            real: 0,
            tail_zeros: 0,
        };
        if self.restarting {
            // Ack BEFORE the final drain, mirroring DpdfnetPlugin::run —
            // a drain-first order can leak a stale partial hop pushed in
            // the drain->ack window into the fresh stream.
            if self.w.reset_acked() {
                self.w.drain_output();
                self.restarting = false;
                // fall through: this callback streams fresh audio
            } else {
                self.w.drain_output();
                self.out.extend(std::iter::repeat_n(0.0, input.len()));
                return silence;
            }
        }
        self.w.note_quantum(input.len());
        let accepted = self.w.push_input(input);
        if accepted < input.len() {
            // Ring overflow: the worker is >680 ms behind. Restart the
            // stream — resync to now instead of replaying stale audio.
            self.w.request_reset();
            self.restarting = true;
            self.restarts += 1;
            self.a.reset();
            self.w.drain_output();
            self.accepted = 0;
            self.ring_popped = 0;
            self.out.extend(std::iter::repeat_n(0.0, input.len()));
            return silence;
        }
        self.accepted += accepted;
        self.w.wake();
        let plan = self.a.plan(input.len(), self.w.output_available());
        for _ in 0..plan.discard {
            self.w.pop_output();
        }
        self.out.extend(std::iter::repeat_n(0.0, plan.lead_zeros));
        for _ in 0..plan.real {
            self.out.push(self.w.pop_output().unwrap_or(0.0));
        }
        self.ring_popped += plan.discard + plan.real;
        self.out.extend(std::iter::repeat_n(0.0, plan.tail_zeros));
        plan
    }

    /// Block until every accepted whole hop has been produced and sits in
    /// the output ring (minus what was already popped).
    fn wait_caught_up(&self) {
        // Cap at what the output ring can physically hold: with a larger
        // backlog the worker parks on a full ring until pops make room.
        let expect = ((self.accepted / HOP) * HOP - self.ring_popped).min(RING_CAPACITY - HOP);
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.w.output_available() < expect {
            assert!(Instant::now() < deadline, "worker never caught up");
            self.w.wake();
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The plugin's activation sequence.
    fn activate(&mut self) {
        assert!(self.w.request_reset_and_wait(Duration::from_secs(2)));
        self.w.drain_output();
        self.a.reset();
        self.accepted = 0;
        self.ring_popped = 0;
    }
}

/// Every non-silence sample must be exactly `latency` after its input
/// (ramp values are 1-based input indices).
fn assert_alignment(emitted: &[f32], latency: usize, from: usize) {
    for (p, &v) in emitted.iter().enumerate().skip(from) {
        if v != 0.0 {
            assert_eq!(
                p,
                (v as usize - 1) + latency,
                "value {v} at position {p}, want latency {latency}"
            );
        }
    }
}

fn ramp(from: usize, n: usize) -> Vec<f32> {
    (from..from + n).map(|i| i as f32).collect()
}

#[test]
fn ramp_alignment_is_exact_at_every_quantum() {
    // 512 covers the non-hop-multiple residue regime (cumulative pushes
    // hit hop boundaries only every 15 cycles) deterministically.
    for q in [64usize, 480, 512, 1024, 8192] {
        let lead = required_lead(q);
        let cycles = lead / q + 8; // enough to get well past the prefill
        let mut d = Driver::new(IdentityEngine);
        for cycle in 0..cycles {
            d.drive(&ramp(1 + cycle * q, q));
            // Deterministic pacing: the worker finishes before the next
            // pop — the cushion, not compute speed, decides availability.
            d.wait_caught_up();
        }
        assert_alignment(&d.out, lead, 0);
        let zeros = d.out.iter().filter(|&&v| v == 0.0).count();
        assert_eq!(zeros, lead, "q={q}: only the lead is silence");
        assert_eq!(d.out.len(), cycles * q);
    }
}

#[test]
fn a_long_stall_substitutes_silence_then_realigns_exactly() {
    // One hop takes 120 ms — more than twice the 50 ms total cushion. Real
    // pacing: a 480-sample callback every 10 ms.
    let mut d = Driver::new(StallingEngine {
        hop: 0,
        stall_at: 20,
        stall: Duration::from_millis(120),
    });
    for cycle in 0..60 {
        d.drive(&ramp(1 + cycle * 480, 480));
        std::thread::sleep(Duration::from_millis(10));
    }
    // Deterministic tail: let the worker catch up fully, then run final
    // cycles — they must be pure, exactly-aligned audio again.
    d.wait_caught_up();
    for cycle in 60..64 {
        d.drive(&ramp(1 + cycle * 480, 480));
        d.wait_caught_up();
    }
    assert_alignment(&d.out, OUTPUT_LEAD, 0);
    let zeros = d.out[OUTPUT_LEAD..].iter().filter(|&&v| v == 0.0).count();
    assert!(zeros > 0, "a 120 ms stall must overrun the 50 ms cushion");
    assert!(
        d.out[d.out.len() - 4 * 480..].iter().all(|&v| v != 0.0),
        "after recovery the stream is pure audio again"
    );
}

#[test]
fn reset_drops_stale_audio_and_realigns_fresh() {
    let mut d = Driver::new(IdentityEngine);
    for cycle in 0..8 {
        d.drive(&ramp(1 + cycle * 480, 480));
        d.wait_caught_up();
    }
    d.activate();
    d.out.clear();
    for cycle in 0..8 {
        d.drive(&ramp(1_000_001 + cycle * 480, 480));
        d.wait_caught_up();
    }
    assert!(
        d.out.iter().all(|&v| v == 0.0 || v >= 1_000_001.0),
        "no pre-reset sample may survive the handshake"
    );
    // Fresh session, fresh prefill, exact alignment for the new ramp.
    for (p, &v) in d.out.iter().enumerate() {
        if v != 0.0 {
            assert_eq!(p, (v as usize - 1_000_001) + OUTPUT_LEAD);
        }
    }
}

#[test]
fn an_engine_panic_goes_silent_not_undefined() {
    let mut d = Driver::new(PanickingEngine { hop: 0, at: 2 });
    for cycle in 0..4 {
        d.drive(&ramp(1 + cycle * 480, 480));
        std::thread::sleep(Duration::from_millis(5));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !d.w.engine_dead() {
        assert!(Instant::now() < deadline, "panic must mark the engine dead");
        std::thread::sleep(Duration::from_millis(1));
    }
    // The handshake reports the death instead of hanging.
    assert!(!d.w.request_reset_and_wait(Duration::from_secs(2)));
}

#[test]
fn inference_errors_keep_the_stream_flowing() {
    let hops = Arc::new(AtomicUsize::new(0));
    let mut d = Driver::new(FailingEngine {
        hops: Arc::clone(&hops),
    });
    for cycle in 0..8 {
        d.drive(&ramp(1 + cycle * 480, 480));
        d.wait_caught_up();
    }
    assert!(!d.w.engine_dead(), "per-hop Err is transient, not death");
    assert!(
        hops.load(Ordering::SeqCst) >= 7,
        "the engine keeps being fed"
    );
    // The Err-path output (0.5 markers) flows through like real audio.
    assert!(d.out[OUTPUT_LEAD..].iter().all(|&v| v == 0.5));
}

#[test]
fn input_overflow_restarts_the_stream_fresh_at_nominal_latency() {
    // Stall the worker far beyond the input ring (~680 ms of audio) while
    // shoving samples in with no pacing: the ring must overflow, which
    // triggers the stream restart — silence until the worker acks, then
    // fresh audio at exactly nominal latency, never seconds-stale replay.
    let mut d = Driver::new(StallingEngine {
        hop: 0,
        stall_at: 4,
        stall: Duration::from_millis(400),
    });
    for cycle in 0..100 {
        d.drive(&ramp(1 + cycle * 480, 480));
    }
    assert!(d.restarts >= 1, "the test must actually overflow");
    let stale_boundary = 100 * 480;
    // Recovery: paced cycles until the ack lands and streaming resumes.
    for cycle in 100..160 {
        d.drive(&ramp(1 + stale_boundary + (cycle - 100) * 480, 480));
        std::thread::sleep(Duration::from_millis(10));
    }
    d.wait_caught_up();
    let resume = d.out.len();
    for cycle in 0..4 {
        d.drive(&ramp(1 + stale_boundary + (60 + cycle) * 480, 480));
        d.wait_caught_up();
    }
    // The deterministic tail is pure fresh audio...
    assert!(
        d.out[resume..].iter().all(|&v| v != 0.0),
        "recovered stream is pure audio"
    );
    // ...none of the pre-restart audio ever surfaces after the restart
    // point (freshness: stale audio is dropped, not replayed)...
    let restart_pos = d
        .out
        .iter()
        .position(|&v| v as usize > stale_boundary)
        .expect("fresh audio must appear");
    assert!(
        d.out[restart_pos..]
            .iter()
            .all(|&v| v == 0.0 || v as usize > stale_boundary),
        "stale pre-restart audio must not replay"
    );
    // ...and the fresh stream sits at EXACTLY nominal latency relative
    // to its own start: reconstruct from the restart cycle boundary.
    let first_fresh = d.out[restart_pos] as usize;
    // first_fresh was pushed at a cycle boundary; every fresh value v
    // must appear exactly (v - first_fresh) after the first one.
    for (off, &v) in d.out[restart_pos..].iter().enumerate() {
        if v != 0.0 {
            assert_eq!(
                off,
                (v as usize) - first_fresh,
                "fresh stream must be gapless and ordered"
            );
        }
    }
}

#[test]
fn controls_reach_the_engine_before_the_next_hop() {
    let log = Arc::new(Mutex::new(Recording::default()));
    let mut d = Driver::new(RecordingEngine {
        log: Arc::clone(&log),
    });
    d.w.set_attn_db(42.0);
    d.w.set_mode_control(1.0); // Bypass
    d.drive(&ramp(1, 480));
    d.wait_caught_up();
    {
        let l = log.lock().unwrap();
        assert_eq!(l.attns, vec![42.0]);
        assert_eq!(l.modes, vec![Mode::Bypass]);
    }
    // A change applies from the next hop.
    d.w.set_mode_control(2.0); // Mute
    d.drive(&ramp(481, 480));
    d.wait_caught_up();
    let l = log.lock().unwrap();
    assert_eq!(l.modes, vec![Mode::Bypass, Mode::Mute]);
    assert_eq!(l.resets, 0, "no reset was requested");
}

#[test]
fn dropping_the_handle_joins_promptly() {
    let w = WorkerHandle::spawn(IdentityEngine).expect("spawn");
    let t0 = Instant::now();
    drop(w);
    assert!(t0.elapsed() < Duration::from_secs(2), "join must not hang");
}

#[test]
fn run_guard_parks_during_a_catch_up_burst_and_reports_lag() {
    // 12 hops queued at once, each burning 30 ms: without the guard the
    // worker would run 360 ms without blocking. With it (realtime forced
    // on; the test binary gets real realtime only as root), every 120 ms of run time is
    // followed by a sleep of one eighth of that run. One-sided: a busy
    // machine only makes the pauses longer.
    let starts = Arc::new(Mutex::new(Vec::new()));
    let lags = Arc::new(Mutex::new(Vec::new()));
    let mut d = Driver::new(BusyEngine {
        busy: Duration::from_millis(30),
        starts: Arc::clone(&starts),
        lags: Arc::clone(&lags),
    });
    d.activate();
    // The realtime handshake wakes the worker; let it finish before the
    // burst is queued, or the first hop runs while hops are still pushed.
    let settle = Instant::now() + Duration::from_secs(6);
    while !d.w.rt_settled() && Instant::now() < settle {
        std::thread::sleep(Duration::from_millis(5));
    }
    d.w.force_run_guard();
    for cycle in 0..12 {
        assert_eq!(d.w.push_input(&ramp(1 + cycle * 480, 480)), 480);
        d.accepted += 480;
    }
    d.w.wake();
    d.wait_caught_up();
    let starts = starts.lock().unwrap();
    assert_eq!(starts.len(), 12);
    let gaps: Vec<Duration> = starts.windows(2).map(|w| w[1] - w[0]).collect();
    // The first hop's lag must be 11 (twelve hops queued, one being run).
    assert_eq!(lags.lock().unwrap()[0], 11);
    // Four hops reach 120 ms of run: a sleep of about 15 ms precedes hops
    // 5 and 9 at least, and no run of more than four hops is bare.
    let guarded = |g: &Duration| *g >= Duration::from_millis(30) + RUN_GUARD_MIN_PARK;
    assert!(
        gaps.iter().filter(|g| guarded(g)).count() >= 2,
        "expected guard sleeps in a 360 ms burst; gaps: {gaps:?}"
    );
    let mut run = 0;
    for g in &gaps {
        if guarded(g) {
            run = 0;
        } else {
            run += 1;
            assert!(
                run <= 4,
                "worker ran too long without a sleep; gaps: {gaps:?}"
            );
        }
    }
    // And the guard costs about one eighth, not more than a quarter.
    let total: Duration = gaps.iter().sum();
    assert!(
        total < Duration::from_millis(11 * 30 * 5 / 4),
        "guard sleeps too long; gaps: {gaps:?}"
    );
}

/// Output = input * gain after burning `busy` of wall time per hop.
/// `overrun` records the worst wall time beyond `busy` a hop took: on a
/// contended box the worker is descheduled mid-spin, and the paced tests
/// below then skip their verdicts (the ladder's decisions are right for
/// the costs it saw, but the costs are not the ones the test set up).
struct CostlyGain {
    gain: f32,
    busy: Duration,
    overrun: Arc<Mutex<Duration>>,
}
impl CostlyGain {
    fn new(gain: f32, busy_ms: u64, overrun: &Arc<Mutex<Duration>>) -> CostlyGain {
        CostlyGain {
            gain,
            busy: Duration::from_millis(busy_ms),
            overrun: Arc::clone(overrun),
        }
    }
}
impl HopEngine for CostlyGain {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        let t = Instant::now();
        while t.elapsed() < self.busy {
            std::hint::spin_loop();
        }
        let over = t.elapsed() - self.busy;
        let mut worst = self.overrun.lock().unwrap();
        if over > *worst {
            *worst = over;
        }
        for (o, i) in output.iter_mut().zip(input) {
            *o = i * self.gain;
        }
        Ok(())
    }
    fn reset(&mut self) {}
    fn set_mode(&mut self, _: Mode) {}
    fn set_attenuation_limit_db(&mut self, _: f32) {}
}

#[test]
fn ladder_switches_to_the_cheap_tier_without_a_hole_in_the_stream() {
    // Quality costs 1.2 hops of wall time per hop, Light 0.3. Driven at
    // real pacing the ladder must leave Quality (emergency to raw, then
    // the immediate Light trial) and the stream must stay continuous:
    // one-sided bounds, CI-noise tolerant.
    let overrun = Arc::new(Mutex::new(Duration::ZERO));
    let quality = CostlyGain::new(2.0, 12, &overrun);
    let light = CostlyGain::new(3.0, 3, &overrun);
    let engine = AdaptiveEngine::new(
        Some(quality),
        Some(light),
        Ladder::new(&[Tier::Quality, Tier::Light, Tier::Raw]).with_dwell_base(100),
    );
    let probe = StallProbe::start();
    let mut d = Driver::new(engine);
    d.activate();
    // The ladder's start-up grace (1 s) deliberately tolerates the cold
    // overload; the emergency and the climb to Light follow within ~0.5 s.
    // After that window the stream must have no hole at all, including
    // across the quality trial the 100-hop dwell triggers and its abort.
    const SETTLED: usize = 150;
    let mut late_zeros = 0usize;
    let started = Instant::now();
    let mut late = Duration::ZERO;
    for cycle in 0..300 {
        let due = Duration::from_millis(cycle as u64 * 10);
        if let Some(wait) = due.checked_sub(started.elapsed()) {
            std::thread::sleep(wait);
        }
        if cycle >= SETTLED {
            late = late.max(started.elapsed().saturating_sub(due));
        }
        let plan = d.drive(&ramp(1 + cycle * 480, 480));
        if cycle >= SETTLED {
            late_zeros += plan.tail_zeros;
        }
    }
    d.wait_caught_up();
    if contended(&overrun, &probe, late) {
        return;
    }
    assert_eq!(
        d.restarts, 0,
        "the ladder must act long before the ring overflows"
    );
    assert_eq!(late_zeros, 0, "zeros after the switch: {late_zeros}");
    // The last second of output carries the light tier's gain (3x): the
    // ladder left Quality (2x) and did not stay on raw (1x) either.
    assert_eq!(gain_histogram(&d.out, 48_000, OUTPUT_LEAD), (0, 48_000, 0));
}

/// Measures how late the scheduler wakes a thread: sleeps 1 ms at a time
/// and keeps the worst oversleep. A CPU quota (a throttled CI pod) freezes
/// every thread for tens of milliseconds; a freeze that lands while the
/// driver sleeps between callbacks shows up here and nowhere else.
struct StallProbe {
    stop: Arc<std::sync::atomic::AtomicBool>,
    worst: Arc<Mutex<Duration>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StallProbe {
    fn start() -> StallProbe {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worst = Arc::new(Mutex::new(Duration::ZERO));
        let (s, w) = (Arc::clone(&stop), Arc::clone(&worst));
        let thread = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                let t = Instant::now();
                std::thread::sleep(Duration::from_millis(1));
                let over = t.elapsed().saturating_sub(Duration::from_millis(1));
                let mut w = w.lock().unwrap();
                *w = (*w).max(over);
            }
        });
        StallProbe {
            stop,
            worst,
            thread: Some(thread),
        }
    }

    fn worst(&self) -> Duration {
        *self.worst.lock().unwrap()
    }
}

impl Drop for StallProbe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A paced test's verdict only holds when the engines' costs and the
/// pacing were the ones it set up: a hop descheduled for more than a few
/// milliseconds, a callback pushed late, or a thread woken late means
/// another process (or a CPU quota) had the CPU, and the test says so and
/// stops.
fn contended(overrun: &Arc<Mutex<Duration>>, probe: &StallProbe, late: Duration) -> bool {
    let worst = *overrun.lock().unwrap();
    let stall = probe.worst();
    if worst > Duration::from_millis(3)
        || stall > Duration::from_millis(5)
        || late > Duration::from_millis(3)
    {
        eprintln!(
            "skipping the verdict (contended box): hop overrun {worst:?}, wake stall {stall:?}, callback lateness {late:?}"
        );
        return true;
    }
    false
}

/// Count samples of the last `n` output samples that match the quality
/// (2x), light (3x) and raw (1x) gains against the ramp input, `lead`
/// samples behind (the aligner's lead grows with a larger quantum).
fn gain_histogram(out: &[f32], n: usize, lead: usize) -> (usize, usize, usize) {
    let start = out.len() - n;
    let (mut q, mut l, mut r) = (0, 0, 0);
    for (i, &v) in out[start..].iter().enumerate() {
        let input = (start + i) as f32 - lead as f32 + 1.0;
        let ratio = v / input;
        if (ratio - 2.0).abs() < 1e-4 {
            q += 1;
        } else if (ratio - 3.0).abs() < 1e-4 {
            l += 1;
        } else if (ratio - 1.0).abs() < 1e-4 {
            r += 1;
        }
    }
    (q, l, r)
}

#[test]
fn ladder_still_acts_when_the_host_cycle_spans_several_hops() {
    // A forced 1024-sample quantum pushes 2.13 hops per callback, so the
    // raw lag reading alternates every hop; the worker smooths it over one
    // cycle. Quality at 1.1 must still be left within a couple of seconds
    // and the tail must be light (3x) with no zeros in its last half.
    let overrun = Arc::new(Mutex::new(Duration::ZERO));
    let quality = CostlyGain::new(2.0, 11, &overrun);
    let light = CostlyGain::new(3.0, 3, &overrun);
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink_log = log.clone();
    let t0 = Instant::now();
    // The sink sees every contract line with its time since construction.
    let engine = AdaptiveEngine::with_clock_and_sink(
        Some(quality),
        Some(light),
        Ladder::new(&[Tier::Quality, Tier::Light, Tier::Raw]).with_dwell_base(100),
        Box::new(Instant::now),
        Box::new(move |l: &str| {
            sink_log
                .lock()
                .unwrap()
                .push(format!("{:?} {l}", t0.elapsed()))
        }),
    );
    let probe = StallProbe::start();
    let mut d = Driver::new(engine);
    d.activate();
    let q = 1024usize;
    let cycles = 48_000 * 4 / q; // 4 s
    let mut late_zeros = 0usize;
    let mut pushed = 0usize;
    let started = Instant::now();
    let mut late = Duration::ZERO;
    for c in 0..cycles {
        // Real pacing: one callback per 1024 samples.
        let due = Duration::from_micros((pushed as u64 * 1_000_000) / 48_000);
        if let Some(wait) = due.checked_sub(started.elapsed()) {
            std::thread::sleep(wait);
        }
        if c >= cycles / 2 {
            late = late.max(started.elapsed().saturating_sub(due));
        }
        let plan = d.drive(&ramp(1 + pushed, q));
        pushed += q;
        if c >= cycles / 2 {
            late_zeros += plan.tail_zeros;
        }
    }
    d.wait_caught_up();
    let log = log.lock().unwrap().join("\n");
    eprintln!("{log}");
    if contended(&overrun, &probe, late) {
        return;
    }
    assert_eq!(d.restarts, 0, "the ring must never overflow");
    assert_eq!(late_zeros, 0, "zeros in the second half: {late_zeros}");
    // The aligner grew its lead once for the larger quantum (align.rs).
    let lead = required_lead(q);
    assert!(
        log.contains("engine: light (recovered)"),
        "light never took over:\n{log}"
    );
    let (qn, ln, rn) = gain_histogram(&d.out, 48_000, lead);
    assert_eq!(
        qn, 0,
        "quality still live at the end (q={qn} l={ln} r={rn})"
    );
    assert!(
        ln > 47_000,
        "light not live at the end (q={qn} l={ln} r={rn})"
    );
}

#[test]
fn caught_up_large_quantum_never_reports_panic_lag() {
    struct LagPolicy {
        ladder: Ladder,
        lags: Arc<Mutex<Vec<u32>>>,
    }
    impl dpdfnet_ladspa::Policy for LagPolicy {
        fn step(&self) -> dpdfnet_ladspa::Step {
            self.ladder.step()
        }
        fn observe(
            &mut self,
            live: f32,
            shadow: Option<f32>,
            lag: u32,
        ) -> Option<dpdfnet_ladspa::Event> {
            self.lags.lock().unwrap().push(lag);
            self.ladder.observe(live, shadow, lag)
        }
        fn reset(&mut self) {
            self.ladder.reset();
        }
        fn live(&self) -> Tier {
            self.ladder.live()
        }
    }
    let lags = Arc::new(Mutex::new(Vec::new()));
    let policy = LagPolicy {
        ladder: Ladder::new(&[Tier::Quality, Tier::Raw]),
        lags: Arc::clone(&lags),
    };
    let mut d = Driver::new(AdaptiveEngine::new(Some(IdentityEngine), None, policy));
    for cycle in 0..40 {
        d.drive(&ramp(1 + cycle * 8192, 8192));
        d.wait_caught_up();
    }
    let lags = lags.lock().unwrap();
    assert!(lags.len() > 600);
    assert!(
        lags.iter().all(|&lag| lag < 4),
        "caught up burst lags: {lags:?}"
    );
}

fn marginal_cost_with_guard(busy_us: u64, with_light: bool) {
    let overrun = Arc::new(Mutex::new(Duration::ZERO));
    let quality = CostlyGain {
        gain: 2.0,
        busy: Duration::from_micros(busy_us),
        overrun: Arc::clone(&overrun),
    };
    let light = with_light.then(|| CostlyGain::new(3.0, 1, &overrun));
    let tiers = if with_light {
        vec![Tier::Quality, Tier::Light, Tier::Raw]
    } else {
        vec![Tier::Quality, Tier::Raw]
    };
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&lines);
    let engine = AdaptiveEngine::with_clock_and_sink(
        Some(quality),
        light,
        Ladder::new(&tiers),
        Box::new(Instant::now),
        Box::new(move |line| sink.lock().unwrap().push(line.to_owned())),
    );
    let probe = StallProbe::start();
    let mut d = Driver::new(engine);
    d.activate();
    d.w.force_run_guard();
    let start = Instant::now();
    let mut late = Duration::ZERO;
    let mut zeros = 0;
    for cycle in 0..350 {
        let due = Duration::from_millis(cycle as u64 * 10);
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
        late = late.max(start.elapsed().saturating_sub(due));
        let plan = d.drive(&ramp(1 + cycle * HOP, HOP));
        if cycle > 200 {
            zeros += plan.tail_zeros;
        }
    }
    d.wait_caught_up();
    if contended(&overrun, &probe, late) {
        return;
    }
    assert_eq!(d.restarts, 0);
    let lines = lines.lock().unwrap().join("\n");
    assert!(
        !lines.contains("passthrough"),
        "guard caused raw fallback: {lines}"
    );
    if with_light {
        // Cost 0.95 already warrants the policy's preventive model change.
        assert!(lines.contains("engine: light (cpu tight"), "{lines}");
    } else {
        assert!(!lines.contains("engine: light"), "{lines}");
        assert_eq!(gain_histogram(&d.out, 48000, OUTPUT_LEAD), (48000, 0, 0));
    }
    assert_eq!(zeros, 0, "guard consumed the output cushion");
}

#[test]
fn force_guard_at_cost_085_keeps_quality_on_the_two_tier_ladder() {
    marginal_cost_with_guard(8500, false);
}

#[test]
fn force_guard_at_cost_095_only_causes_the_expected_preventive_change() {
    marginal_cost_with_guard(9500, true);
}

#[test]
fn drop_is_bounded_while_the_worker_is_inside_a_call() {
    struct Blocked {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    }
    impl HopEngine for Blocked {
        fn process_hop(
            &mut self,
            input: &[f32; HOP],
            output: &mut [f32; HOP],
        ) -> Result<(), String> {
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
            output.copy_from_slice(input);
            Ok(())
        }
        fn reset(&mut self) {}
        fn set_mode(&mut self, _: Mode) {}
        fn set_attenuation_limit_db(&mut self, _: f32) {}
    }
    let (entered, entry) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let mut w = WorkerHandle::spawn(Blocked {
        entered,
        release: released,
    })
    .unwrap();
    w.push_input(&[1.0; HOP]);
    w.wake();
    entry.recv_timeout(Duration::from_secs(1)).unwrap();
    let start = Instant::now();
    drop(w);
    let elapsed = start.elapsed();
    release.send(()).unwrap();
    assert!(
        elapsed < Duration::from_millis(500),
        "drop took {elapsed:?}"
    );
}
