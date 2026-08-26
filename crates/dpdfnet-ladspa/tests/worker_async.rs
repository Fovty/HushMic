//! Async-path integration tests (issue #10): the worker + rings + ledger,
//! driven exactly the way the plugin's `run()` drives them, against fake
//! engines — no ONNX Runtime, no models. Alignment assertions are exact
//! and timing-independent; only where a test must observe a stall does it
//! pace in real time, and those assertions stay one-sided so a noisy CI
//! machine cannot flake them.

use dpdfnet_ladspa::{
    Aligner, HopEngine, PopPlan, WorkerHandle, DESIGN_QUANTUM, OUTPUT_LEAD, RING_CAPACITY,
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
            w: WorkerHandle::spawn(engine).expect("spawn"),
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
        let lead = OUTPUT_LEAD + q.saturating_sub(DESIGN_QUANTUM);
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
    // One hop takes 120 ms — four times the 30 ms total cushion. Real
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
    assert!(zeros > 0, "a 120 ms stall must overrun the 30 ms cushion");
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
