//! A virtual-time replay of `worker_loop` around the real `AdaptiveEngine`
//! and `Ladder`: hops arrive every 10 ms, the worker runs them back to back
//! at scripted costs, the lag reading, the run guard and the output deadline
//! follow the production rules. Deterministic, so the ladder's behaviour
//! under a held overload with realtime on (the worker runs over budget but
//! is never frozen) can be asserted without real load.

use super::{LagWindow, RunGuard};
use crate::adaptive::AdaptiveEngine;
use crate::ladder::{Ladder, Policy, Tier};
use crate::worker::HopEngine;
use hushmic_denoiser::{Mode, HOP};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HOP_US: u64 = 10_000;
const RATE: u64 = 48_000;
/// rtkit's `RLIMIT_RTTIME`.
const RTTIME: Duration = Duration::from_millis(200);
const QLR: [Tier; 3] = [Tier::Quality, Tier::Light, Tier::Raw];
use crate::ladder::{RAW_RETRY_MAX, TRIAL, TRIAL_COLD, XFADE_PROMOTE};

#[derive(Clone, Copy, Debug)]
pub(super) struct Scenario {
    /// Warm cost of each model while the load is on, as a fraction of a hop.
    pub quality_load: f32,
    pub light_load: f32,
    pub quality_idle: f32,
    pub light_idle: f32,
    /// A model that did not run on the previous hop runs its next
    /// `cold_hops` hops at `cold` times its warm cost.
    pub cold: f32,
    pub cold_hops: u32,
    /// The load is on from this hop to `load_until`.
    pub load_from: u64,
    pub load_until: u64,
    /// Bursts inside that window: on for `.0` hops, off for `.1`.
    pub bursts: Option<(u64, u64)>,
    /// The worker has realtime: the run guard is armed.
    pub realtime: bool,
    /// Uniform multiplicative cost noise (0.1 = ±10 %), seeded.
    pub jitter: f32,
    pub seed: u64,
    /// The worker is kept off the CPU for these hops (start, length).
    pub freeze: Option<(u64, u64)>,
    /// The host's cycle in samples: a cycle pushes this many samples at
    /// once, so hops arrive in bursts and the lag window smooths over
    /// `ceil(quantum / HOP)` readings.
    pub quantum: u64,
    /// No consumer for these hops (start, length): no audio flows, and
    /// the host resets the plugin as the stream pauses (filter-chain's
    /// PAUSED handler deactivates and activates every instance).
    pub idle: Option<(u64, u64)>,
    pub hops: u64,
}

impl Scenario {
    /// A held overload with realtime on: quality runs over budget from
    /// hop 300 for ten seconds, light fits.
    pub(super) fn held(quality_load: f32) -> Scenario {
        Scenario {
            quality_load,
            light_load: quality_load * 0.3,
            quality_idle: 0.5,
            light_idle: 0.15,
            cold: 2.0,
            cold_hops: 2,
            load_from: 300,
            load_until: 1300,
            bursts: None,
            realtime: true,
            jitter: 0.0,
            seed: 1,
            freeze: None,
            quantum: HOP as u64,
            idle: None,
            hops: 1300,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Heard {
    Quality,
    Light,
    Raw,
    Ducked,
    Fade,
    /// The hop missed its deadline: the aligner substituted zeros.
    Zeros,
    /// No consumer: the chain did not run.
    Idle,
    /// Exact silence on time: the delay lines after a reset.
    Silent,
}

pub(super) struct Replay {
    pub heard: Vec<Heard>,
    /// Hop, contract line.
    pub log: Vec<(u64, String)>,
    /// The lag reading on the first hop of every raw phase: the backlog
    /// an emergency's rejoin starts against.
    pub raw_lag: Vec<u32>,
}

impl Replay {
    fn count(&self, from: u64, what: Heard) -> usize {
        self.heard[from as usize..]
            .iter()
            .filter(|&&h| h == what)
            .count()
    }

    pub(super) fn zeros(&self, from: u64) -> usize {
        self.count(from, Heard::Zeros)
    }

    pub(super) fn raw(&self, from: u64) -> usize {
        self.count(from, Heard::Raw)
    }

    pub(super) fn ducked(&self, from: u64) -> usize {
        self.count(from, Heard::Ducked)
    }

    /// Hops after `from` until a model tier is heard on its own.
    pub(super) fn model_after(&self, from: u64) -> Option<u64> {
        self.heard[from as usize..]
            .iter()
            .position(|&h| h == Heard::Light || h == Heard::Quality)
            .map(|p| p as u64)
    }

    /// Hops after `from` until the light tier is heard on its own.
    pub(super) fn light_after(&self, from: u64) -> Option<u64> {
        self.heard[from as usize..]
            .iter()
            .position(|&h| h == Heard::Light)
            .map(|p| p as u64)
    }

    pub(super) fn lines(&self) -> String {
        self.log
            .iter()
            .map(|(h, l)| format!("{h}: {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

struct Clock {
    now_us: AtomicU64,
    hop: AtomicU64,
}

struct Model {
    clock: Arc<Clock>,
    gain: f32,
    idle: f32,
    load: f32,
    sc: Scenario,
    last: Option<u64>,
    cold_left: u32,
    rng: u64,
}

impl Model {
    fn new(clock: &Arc<Clock>, gain: f32, idle: f32, load: f32, sc: Scenario) -> Model {
        Model {
            clock: Arc::clone(clock),
            gain,
            idle,
            load,
            sc,
            last: None,
            cold_left: 0,
            rng: sc.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ gain.to_bits() as u64 | 1,
        }
    }

    fn noise(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }
}

impl HopEngine for Model {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        let hop = self.clock.hop.load(Ordering::Relaxed);
        if self.last.is_none_or(|l| l + 1 != hop) {
            self.cold_left = self.sc.cold_hops;
        }
        self.last = Some(hop);
        let loaded = (self.sc.load_from..self.sc.load_until).contains(&hop)
            && self
                .sc
                .bursts
                .is_none_or(|(on, off)| (hop - self.sc.load_from) % (on + off) < on);
        let mut cost = if loaded { self.load } else { self.idle };
        if self.cold_left > 0 {
            self.cold_left -= 1;
            cost *= self.sc.cold;
        }
        cost *= 1.0 + self.sc.jitter * self.noise();
        let start = self.clock.now_us.load(Ordering::Relaxed);
        let mut end = start + (cost.max(0.0) * HOP_US as f32) as u64;
        if let Some((from, len)) = self.sc.freeze {
            let (fs, fe) = (from * HOP_US, (from + len) * HOP_US);
            if start < fe && end > fs {
                end += fe - fs.max(start);
            }
        }
        self.clock.now_us.store(end, Ordering::Relaxed);
        for (o, i) in output.iter_mut().zip(input) {
            *o = i * self.gain;
        }
        Ok(())
    }
    fn reset(&mut self) {}
    fn set_mode(&mut self, _: Mode) {}
    fn set_attenuation_limit_db(&mut self, _: f32) {}
}

/// Run `sc` against `policy` and record what the RT side would emit.
pub(super) fn replay_with<P: Policy>(sc: Scenario, policy: P) -> Replay {
    let clock = Arc::new(Clock {
        now_us: AtomicU64::new(0),
        hop: AtomicU64::new(0),
    });
    let base = Instant::now();
    let c = Arc::clone(&clock);
    let log = Arc::new(Mutex::new(Vec::new()));
    let (sink_log, sink_clock) = (Arc::clone(&log), Arc::clone(&clock));
    let mut engine = AdaptiveEngine::with_clock_and_sink(
        Some(Model::new(
            &clock,
            2.0,
            sc.quality_idle,
            sc.quality_load,
            sc,
        )),
        Some(Model::new(&clock, 3.0, sc.light_idle, sc.light_load, sc)),
        policy,
        Box::new(move || base + Duration::from_micros(c.now_us.load(Ordering::Relaxed))),
        Box::new(move |l: &str| {
            let hop = sink_clock.hop.load(Ordering::Relaxed);
            sink_log.lock().unwrap().push((hop, l.to_string()));
        }),
    );
    let mut guard = RunGuard::default();
    guard.prepare(sc.realtime, false, sc.realtime);
    let mut window = LagWindow::default();
    let input = [1.0f32; HOP];
    let mut out = [0.0f32; HOP];
    let mut t = 0u64;
    let mut heard = Vec::with_capacity(sc.hops as usize);
    let mut raw_lag = Vec::new();
    let mut was_raw = false;
    let q = sc.quantum;
    let hop = HOP as u64;
    let per_cycle = q.div_ceil(hop) as usize;
    let lead = crate::align::required_lead(q as usize) as u64;
    // Cycle `c` runs at `c * q` samples and pushes samples up to `(c + 1) * q`.
    let cycle_us = |c: u64| c * q * 1_000_000 / RATE;
    for k in 0..sc.hops {
        // The cycle that completes hop `k`, and the one that pops the
        // first sample of its output, `lead` samples later (a hop whose
        // output spans two cycles is due at the first). At a 480 quantum:
        // `k` and `k + 3`.
        let arrival = cycle_us(((k + 1) * hop).div_ceil(q) - 1);
        let deadline = cycle_us((k * hop + lead) / q);
        if let Some((from, len)) = sc.idle {
            if (from..from + len).contains(&k) {
                if k == from {
                    engine.reset();
                    window = LagWindow::default();
                }
                guard.parked(Duration::from_micros(HOP_US), true);
                t = t.max(arrival + HOP_US);
                heard.push(Heard::Idle);
                continue;
            }
        }
        if t < arrival {
            guard.parked(Duration::from_micros(arrival - t), true);
            t = arrival;
        }
        if let Some((from, len)) = sc.freeze {
            if (from * HOP_US..(from + len) * HOP_US).contains(&t) {
                t = (from + len) * HOP_US;
            }
        }
        if let Some(pause) = guard.pause(Some(RTTIME)) {
            t += pause.as_micros() as u64;
            engine.note_pause(pause);
            guard.run = Duration::ZERO;
        }
        let pushed = (t * RATE / (q * 1_000_000) + 1) * q;
        let queued = (pushed / hop).saturating_sub(k) as usize;
        let lag = window.observe(queued, per_cycle);
        engine.set_lag_hops(lag);
        let raw = engine.live() == Tier::Raw;
        if raw && !was_raw {
            raw_lag.push(lag);
        }
        was_raw = raw;
        clock.hop.store(k, Ordering::Relaxed);
        clock.now_us.store(t, Ordering::Relaxed);
        engine.process_hop(&input, &mut out).unwrap();
        let end = clock.now_us.load(Ordering::Relaxed);
        guard.account(Duration::from_micros(end - t));
        t = end;
        let mean = out.iter().sum::<f32>() / HOP as f32;
        heard.push(if t > deadline {
            Heard::Zeros
        } else if (mean - 2.0).abs() < 1e-3 {
            Heard::Quality
        } else if (mean - 3.0).abs() < 1e-3 {
            Heard::Light
        } else if (mean - 1.0).abs() < 1e-3 {
            Heard::Raw
        } else if mean == 0.0 {
            Heard::Silent
        } else if mean < 0.02 {
            Heard::Ducked
        } else {
            Heard::Fade
        });
    }
    let log = std::mem::take(&mut *log.lock().unwrap());
    Replay {
        heard,
        log,
        raw_lag,
    }
}

pub(super) fn replay(sc: Scenario) -> Replay {
    replay_with(sc, Ladder::new(&QLR))
}

#[test]
fn a_calm_session_stays_on_quality() {
    let mut sc = Scenario::held(1.2);
    sc.load_from = sc.hops;
    let r = replay(sc);
    assert_eq!(r.zeros(200), 0);
    assert!(r.heard[200..].iter().all(|&h| h == Heard::Quality));
    assert!(
        r.log.iter().all(|(_, l)| !l.contains("cpu")),
        "{}",
        r.lines()
    );
}

/// Emergencies whose raw phase starts 1 to 5 hops behind: held overloads
/// at quality 1.1 and 1.2 (realtime off), quality 1.2 with realtime on (the
/// run guard's sleep adds the third hop), a starved one at 2.5 (no fade,
/// `STARVED_COST`, 4) and a freeze that takes the panic path (5):
/// light must be back within a tenth of a second of the load, with raw
/// ducked all the way and no zero once light is live. Before the drain
/// phase every case but the first played about five seconds of unducked
/// raw: the rejoin's first hop read a lag of `LAG_ABORT` and ended it.
#[test]
fn an_emergency_returns_to_light_at_any_backlog() {
    let mut freeze = Scenario::held(0.5);
    freeze.realtime = false;
    freeze.freeze = Some((400, 6));
    freeze.load_from = 400;
    let mut cases = Vec::new();
    for (q, realtime, backlog) in [(1.1, false, 1), (1.2, false, 2), (2.5, false, 4)] {
        let mut sc = Scenario::held(q);
        sc.realtime = realtime;
        cases.push((sc, backlog));
    }
    cases.push((Scenario::held(1.2), 3));
    cases.push((freeze, 5));
    for (sc, backlog) in cases {
        let r = replay(sc);
        let from = sc.load_from;
        let what = format!("{sc:?}\n{}", r.lines());
        assert_eq!(r.raw_lag, [backlog], "{what}");
        let light = r.light_after(from).expect(&what);
        assert!(light <= 30, "light after {light} hops: {what}");
        assert_eq!(r.raw(from), 0, "unducked raw: {what}");
        assert!(r.ducked(from) <= 10, "{what}");
        assert_eq!(r.zeros(from + light), 0, "{what}");
    }
}

/// A light tier that does not fit (twice its cost cold, and over budget
/// warm) cannot rejoin: raw returns to full gain at once, and waits out
/// the retry dwell audibly instead of ducked.
#[test]
fn a_light_tier_that_does_not_fit_leaves_audible_raw() {
    for light in [0.9, 1.1] {
        let mut sc = Scenario::held(1.5);
        sc.light_load = light;
        let r = replay(sc);
        let what = r.lines();
        assert!(r.light_after(sc.load_from).is_none(), "{what}");
        assert!(r.ducked(sc.load_from) <= 10, "{what}");
        assert!(r.zeros(sc.load_from) <= 10, "{what}");
        assert!(
            r.log
                .iter()
                .any(|(_, l)| l.contains("trial of light failed")),
            "{what}"
        );
    }
}

/// Prints the outcome of held overloads (realtime on and off), freezes
/// (the panic path) and a light tier that does not fit, averaged over
/// seeded cost noise: the numbers in
/// docs/superpowers/ladder-rejoin-evaluation.md.
/// `cargo test -p dpdfnet-ladspa --lib rejoin_sweep -- --ignored --nocapture`
#[test]
#[ignore]
fn rejoin_sweep() {
    let mut rows: Vec<(String, Scenario)> = Vec::new();
    for realtime in [true, false] {
        for q in [1.05, 1.1, 1.2, 1.3, 1.5, 2.0] {
            let mut sc = Scenario::held(q);
            sc.realtime = realtime;
            rows.push((format!("held q {q:.2} rt {}", realtime as u8), sc));
        }
    }
    for len in [4, 6, 10, 20] {
        let mut sc = Scenario::held(0.5);
        sc.realtime = false;
        sc.freeze = Some((400, len));
        sc.load_from = 400;
        rows.push((format!("freeze {len} hops"), sc));
    }
    for light in [0.9, 1.1] {
        let mut sc = Scenario::held(1.5);
        sc.light_load = light;
        rows.push((format!("q 1.50 light {light:.2} rt 1"), sc));
    }
    println!("scenario                  raw-lag  fail%  zeros  raw    ducked  light-after");
    for (name, sc) in rows {
        let seeds = 100;
        let (mut fails, mut zeros, mut raw, mut ducked, mut light) = (0, 0, 0, 0, 0);
        let mut lag0 = Vec::new();
        for seed in 0..seeds {
            let mut sc = sc;
            sc.jitter = 0.1;
            sc.seed = seed + 1;
            let r = replay(sc);
            let from = sc.load_from;
            if r.raw(from) > 10 {
                fails += 1;
            }
            zeros += r.zeros(from);
            raw += r.raw(from);
            ducked += r.ducked(from);
            light += r.light_after(from).unwrap_or(1000);
            if let Some(&l) = r.raw_lag.first() {
                lag0.push(l);
            }
        }
        lag0.sort();
        let n = seeds as f32;
        println!(
            "{name:22} {:>10}  {:5.1}  {:5.1}  {:5.1}  {:6.1}  {:11.1}",
            match (lag0.first(), lag0.last()) {
                (Some(a), Some(b)) => format!("{a}..{b}"),
                _ => "-".to_string(),
            },
            100.0 * fails as f32 / n,
            zeros as f32 / n,
            raw as f32 / n,
            ducked as f32 / n,
            light as f32 / n
        );
    }
}

/// The CI flap step's schedule (three 4 s bursts, 4 s apart, the worker
/// starved so far in each that neither model fits), audio flowing throughout: a
/// model tier must be live again well inside 30 s of the last burst. The
/// trial gate (load under 0.70, lag zero) opens on the first raw hop:
/// raw's load is seeded at zero and costs nothing.
#[test]
fn a_flapping_load_returns_to_a_model_tier_while_audio_flows() {
    for realtime in [false, true] {
        for seed in 1..=20 {
            let mut sc = Scenario::held(1.5);
            sc.light_load = 1.2;
            sc.load_from = 300;
            sc.bursts = Some((400, 400));
            sc.load_until = 300 + 400 * 5;
            sc.hops = sc.load_until + 3000;
            sc.realtime = realtime;
            sc.jitter = 0.1;
            sc.seed = seed;
            let r = replay(sc);
            let back = r.model_after(sc.load_until);
            assert!(
                back.is_some_and(|h| h <= RAW_RETRY_MAX as u64 + 100),
                "back after {back:?} hops: {sc:?}\n{}",
                r.lines()
            );
        }
    }
}

/// A call ends in passthrough shortly after an overload, and the next one
/// starts three seconds later, inside WirePlumber's suspend timeout (so
/// the instance and its ladder survive; the host only reset it as the
/// stream paused). The load is gone: a model tier must be back within a
/// trial and its fade, not after the rest of the raw dwell.
#[test]
fn the_next_call_does_not_inherit_the_raw_dwell() {
    let mut sc = Scenario::held(1.5);
    sc.light_load = 1.2;
    sc.load_until = 600;
    sc.idle = Some((650, 300));
    sc.hops = 2000;
    let r = replay(sc);
    let what = r.lines();
    assert_eq!(r.heard[649], Heard::Raw, "{what}");
    let resume = 950;
    let back = r.model_after(resume).expect(&what);
    assert!(
        back <= (TRIAL + TRIAL_COLD + XFADE_PROMOTE) as u64 + 2,
        "back after {back} hops: {what}"
    );
    assert_eq!(r.zeros(resume), 0, "{what}");
}

/// The same, but the overload is still there when the next call starts:
/// the retry is a silent probe, cut short, the stream has no zero and raw
/// stays audible.
#[test]
fn a_retry_into_a_lasting_overload_stays_audible() {
    // Light fits warm (0.9) but not through its cold hops (1.8): every
    // probe is dropped by the backlog model before a zero.
    let mut sc = Scenario::held(1.5);
    sc.light_load = 0.9;
    sc.load_until = 2000;
    sc.idle = Some((650, 300));
    sc.hops = 2000;
    let r = replay(sc);
    let what = r.lines();
    let resume = 950;
    assert!(r.model_after(resume).is_none(), "{what}");
    assert_eq!(r.zeros(resume), 0, "{what}");
    assert_eq!(r.ducked(resume), 0, "{what}");
    assert!(r.raw(resume) >= 1000, "{what}");
}

/// A two-hop contention blip (quality at 1.6 to 2.2 budgets for two hops,
/// every six seconds) leaves a backlog that drains on its own within the
/// cushion: no emergency, no ducked voice, quality stays live. (Higher
/// blips can hold the lag for three hops at a high cost, and the held rule
/// takes them as it always did; whether it does depends on how the
/// fractional backlog falls on the readings, not monotonically on the
/// height: it fires at 2.3, 2.4 and 3.0 but not at 2.6.)
#[test]
fn a_two_hop_burst_then_recovery_stays_on_quality() {
    for height in [1.6, 1.8, 2.0, 2.2] {
        for realtime in [false, true] {
            let mut sc = Scenario::held(height);
            sc.load_from = 300;
            sc.bursts = Some((2, 598));
            sc.load_until = 300 + 600 * 5;
            sc.hops = sc.load_until;
            sc.realtime = realtime;
            let r = replay(sc);
            let what = format!("height {height} realtime {realtime}\n{}", r.lines());
            assert!(
                r.log.iter().all(|(_, l)| !l.contains("passthrough")),
                "{what}"
            );
            assert_eq!(r.ducked(300), 0, "{what}");
            assert!(
                r.heard[300..]
                    .iter()
                    .all(|&h| h == Heard::Quality || h == Heard::Zeros),
                "{what}"
            );
            assert!(r.zeros(300) <= 5, "{what}");
        }
    }
}

/// At a 1024-sample quantum hops arrive in bursts of two or three and the
/// worker smooths the lag over a cycle, so the drain can read lag 0 a cycle
/// before the worker parks: with realtime on, the run guard's sleep then
/// lands in a rejoin hop (it must not be projected onto the next one). Held
/// overloads and a 100 ms freeze still return to light, raw stays ducked,
/// and there is no zero once light is live.
#[test]
fn a_large_quantum_drains_before_the_rejoin() {
    let mut freeze = Scenario::held(0.5);
    freeze.realtime = false;
    freeze.freeze = Some((400, 10));
    freeze.load_from = 400;
    for mut sc in [Scenario::held(1.2), Scenario::held(1.5), freeze] {
        sc.quantum = 1024;
        let r = replay(sc);
        let from = sc.load_from;
        let what = format!("{sc:?}\n{}", r.lines());
        assert!(
            r.log.iter().any(|(_, l)| l.contains("cpu overloaded")),
            "{what}"
        );
        let light = r.light_after(from).expect(&what);
        assert!(light <= 40, "light after {light} hops: {what}");
        assert_eq!(r.raw(from), 0, "unducked raw: {what}");
        assert_eq!(r.zeros(from + light), 0, "{what}");
    }
}
