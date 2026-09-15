//! The adaptive engine (issue #14): owns up to two model engines and a raw
//! delay line, runs whichever tier the [`Policy`] says is live, warms or
//! trials a second one in the shadow slot, and crossfades between them.
//!
//! Runs on the worker thread only. Every tier is sample-aligned with the
//! others: the models share their algorithmic latency, and the raw path
//! delays the input by exactly that (`hushmic_denoiser::LATENCY_SAMPLES`),
//! so the RT-side aligner sees one continuous stream whatever is live.

use crate::ladder::{Event, Policy, Step, Tier};
use crate::worker::HopEngine;
use hushmic_denoiser::{GainRamp, Mode, HOP, LATENCY_SAMPLES};
use std::time::{Duration, Instant};

/// Raw attenuation in dB during an emergency return to a model.
pub const RAW_DUCK_DB: f32 = -40.0;

/// One hop of the budget.
const HOP_BUDGET: Duration = Duration::from_millis(10);
/// Hops of the initial tier before the baseline cost line is logged.
const BASELINE_AFTER_HOPS: u64 = 500;
/// Live costs kept for the baseline median.
const BASELINE_WINDOW: usize = 100;

/// The raw path: input delayed by the engines' algorithmic latency.
pub struct RawDelay {
    buf: Vec<f32>,
    pos: usize,
}

impl Default for RawDelay {
    fn default() -> Self {
        Self::new()
    }
}

impl RawDelay {
    pub fn new() -> RawDelay {
        RawDelay {
            buf: vec![0.0; LATENCY_SAMPLES],
            pos: 0,
        }
    }

    pub fn reset(&mut self) {
        self.buf.iter_mut().for_each(|s| *s = 0.0);
        self.pos = 0;
    }

    /// `out[i]` is the input from `LATENCY_SAMPLES` samples ago.
    pub fn process(&mut self, input: &[f32; HOP], out: &mut [f32; HOP]) {
        for (i, &x) in input.iter().enumerate() {
            out[i] = self.buf[self.pos];
            self.buf[self.pos] = x;
            self.pos += 1;
            if self.pos == self.buf.len() {
                self.pos = 0;
            }
        }
    }
}

type Clock = Box<dyn FnMut() -> Instant + Send>;
type Sink = Box<dyn FnMut(&str) + Send>;

/// See the module docs. Generic over the engine (`Denoiser` in production,
/// fakes in tests) and the policy (`Ladder` or `Pinned`).
pub struct AdaptiveEngine<E: HopEngine, P: Policy> {
    quality: Option<E>,
    light: Option<E>,
    raw: RawDelay,
    raw_out: [f32; HOP],
    raw_ramp: GainRamp,
    raw_duck_gain: f32,
    muted: bool,
    policy: P,
    clock: Clock,
    sink: Sink,
    lag: u32,
    /// Guard pauses since the last hop, in hops (`note_pause`).
    pause_hops: f32,
    /// `HUSHMIC_DSP_DEBUG` is set: log the policy's state after every
    /// event and every two seconds.
    debug: bool,
    hops: u64,
    baseline: Vec<f32>,
    baseline_done: bool,
    announced_tier: Option<Tier>,
}

impl<E: HopEngine, P: Policy> AdaptiveEngine<E, P> {
    /// At least one engine must be present for a model tier the policy can
    /// name; a tier without an engine plays the raw path.
    pub fn new(quality: Option<E>, light: Option<E>, policy: P) -> Self {
        Self::with_clock_and_sink(
            quality,
            light,
            policy,
            Box::new(Instant::now),
            Box::new(crate::log::contract_line),
        )
    }

    /// Test seam: an injected clock (for costs) and log sink.
    pub fn with_clock_and_sink(
        quality: Option<E>,
        light: Option<E>,
        policy: P,
        clock: Clock,
        sink: Sink,
    ) -> Self {
        AdaptiveEngine {
            quality,
            light,
            raw: RawDelay::new(),
            raw_out: [0.0; HOP],
            // Unprimed, like the model tiers' ramps: the first `set_mode`
            // snaps instead of fading (matches `Denoiser`).
            raw_ramp: GainRamp::new(),
            raw_duck_gain: 1.0,
            muted: false,
            policy,
            clock,
            sink,
            lag: 0,
            pause_hops: 0.0,
            debug: std::env::var_os("HUSHMIC_DSP_DEBUG").is_some(),
            hops: 0,
            baseline: Vec::with_capacity(BASELINE_WINDOW),
            baseline_done: false,
            announced_tier: None,
        }
    }

    /// The tier whose output is live (during a crossfade: the one being
    /// left).
    pub fn live(&self) -> Tier {
        self.policy.live()
    }

    pub fn policy(&self) -> &P {
        &self.policy
    }

    /// Run `tier` into `out`; returns the engine result and the cost as a
    /// fraction of the hop budget. Raw costs nothing and never fails.
    fn run_into(
        &mut self,
        tier: Tier,
        input: &[f32; HOP],
        out: &mut [f32; HOP],
    ) -> (Result<(), String>, f32) {
        let engine = match tier {
            Tier::Quality => self.quality.as_mut(),
            Tier::Light => self.light.as_mut(),
            Tier::Raw => None,
        };
        let Some(engine) = engine else {
            out.copy_from_slice(&self.raw_out);
            return (Ok(()), 0.0);
        };
        let t0 = (self.clock)();
        let r = engine.process_hop(input, out);
        let cost = (self.clock)().duration_since(t0).as_secs_f32() / HOP_BUDGET.as_secs_f32();
        (r, cost)
    }

    fn log_event(&mut self, event: Event) {
        let live = self.policy.live();
        let line = match event {
            Event::Demoted { to, lag, .. } if lag > 0 => {
                format!("engine: {} (cpu overloaded, lag {lag} hops)", to.word())
            }
            Event::Demoted { to, cost, .. } => format!(
                "engine: {} (cpu tight, {:.1} ms per 10 ms hop)",
                to.word(),
                cost * 10.0
            ),
            Event::Promoted { to } => format!("engine: {} (recovered)", to.word()),
            Event::TrialFailed { tier, cost } => format!(
                "engine: {} (trial of {} failed, {:.1} ms per 10 ms hop)",
                live.word(),
                tier.word(),
                cost * 10.0
            ),
        };
        (self.sink)(&line);
        self.announced_tier = Some(match event {
            Event::Demoted { to, .. } | Event::Promoted { to } => to,
            Event::TrialFailed { .. } => live,
        });
    }

    fn note_baseline(&mut self, cost: f32, steady: bool) {
        if self.baseline_done {
            return;
        }
        if !steady {
            // A transition before the baseline: the number would describe
            // a mix of tiers; the event line already carries a cost.
            self.baseline_done = true;
            return;
        }
        if self.baseline.len() == BASELINE_WINDOW {
            self.baseline.remove(0);
        }
        self.baseline.push(cost);
        if self.hops >= BASELINE_AFTER_HOPS && self.baseline.len() == BASELINE_WINDOW {
            let mut v = self.baseline.clone();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = v[v.len() / 2];
            let line = format!("engine: {} (cost {:.2})", self.policy.live().word(), median);
            (self.sink)(&line);
            self.announced_tier = Some(self.policy.live());
            self.baseline_done = true;
        }
    }
}

impl<E: HopEngine, P: Policy> HopEngine for AdaptiveEngine<E, P> {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        // The raw path runs every hop so its delay line is always warm and
        // its mute ramp has always reached its target before raw goes live.
        let mut raw = [0.0f32; HOP];
        self.raw.process(input, &mut raw);
        self.raw_ramp.process(&mut raw);
        // A separate gain preserves mute and reaches either target in one hop.
        // The policy holds the duck through the entire fade back to a model.
        let target = if self.policy.duck_raw() {
            10.0f32.powf(RAW_DUCK_DB / 20.0)
        } else {
            1.0
        };
        let start = self.raw_duck_gain;
        for (i, sample) in raw.iter_mut().enumerate() {
            let a = (i + 1) as f32 / HOP as f32;
            *sample *= start * (1.0 - a) + target * a;
        }
        self.raw_duck_gain = target;
        self.raw_out = raw;
        self.hops += 1;

        let step = self.policy.step();
        let (result, live_cost, shadow_cost) = match step {
            Step::Steady { live } => {
                let (r, c) = self.run_into(live, input, output);
                (r, c, None)
            }
            Step::Shadow {
                live,
                shadow,
                kind: _,
            } => {
                let (r, c) = self.run_into(live, input, output);
                let mut scratch = [0.0f32; HOP];
                let (_, sc) = self.run_into(shadow, input, &mut scratch);
                (r, c, Some(sc))
            }
            Step::Crossfade { from, to, hop, of } => {
                let (r, c) = self.run_into(from, input, output);
                let mut scratch = [0.0f32; HOP];
                let (_, sc) = self.run_into(to, input, &mut scratch);
                let of = of.max(1) as f32;
                let start = hop as f32 / of;
                let step_per_sample = 1.0 / (of * HOP as f32);
                for (i, o) in output.iter_mut().enumerate() {
                    let a = (start + step_per_sample * (i as f32 + 1.0)).min(1.0);
                    *o = *o * (1.0 - a) + scratch[i] * a;
                }
                (r, c, Some(sc))
            }
        };
        let steady = matches!(step, Step::Steady { .. });
        // A guard pause since the last hop is time the live path did not
        // produce: it counts as live cost, once.
        let live_cost = live_cost + std::mem::take(&mut self.pause_hops);
        let event = self.policy.observe(live_cost, shadow_cost, self.lag);
        if let Some(e) = event {
            self.log_event(e);
            self.baseline_done = true;
        }
        self.note_baseline(live_cost, steady);
        if self.debug && (event.is_some() || self.hops.is_multiple_of(200)) {
            let line = format!(
                "debug: hop {} cost {:.2} lag {} {}",
                self.hops,
                live_cost,
                self.lag,
                self.policy.describe()
            );
            (self.sink)(&line);
        }
        result
    }

    fn reset(&mut self) {
        if let Some(e) = self.quality.as_mut() {
            e.reset();
        }
        if let Some(e) = self.light.as_mut() {
            e.reset();
        }
        self.raw.reset();
        self.raw_out = [0.0; HOP];
        self.raw_ramp.reset_to(self.muted);
        self.raw_duck_gain = 1.0;
        let before = self.policy.live();
        self.policy.reset();
        let after = self.policy.live();
        if after != self.announced_tier.unwrap_or(before) {
            // A fade announces its destination before it lands. Reset can
            // return to its source, so correct the last announcement.
            let line = format!("engine: {} (stream restart)", after.word());
            (self.sink)(&line);
            self.announced_tier = Some(after);
        }
    }

    fn set_mode(&mut self, mode: Mode) {
        if let Some(e) = self.quality.as_mut() {
            e.set_mode(mode);
        }
        if let Some(e) = self.light.as_mut() {
            e.set_mode(mode);
        }
        self.muted = mode == Mode::Mute;
        self.raw_ramp.set_muted(self.muted);
    }

    fn set_attenuation_limit_db(&mut self, db: f32) {
        if let Some(e) = self.quality.as_mut() {
            e.set_attenuation_limit_db(db);
        }
        if let Some(e) = self.light.as_mut() {
            e.set_attenuation_limit_db(db);
        }
    }

    fn set_lag_hops(&mut self, hops: u32) {
        self.lag = hops;
    }

    fn note_pause(&mut self, pause: Duration) {
        self.pause_hops += pause.as_secs_f32() * 100.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ladder::{Pinned, ShadowKind};
    use std::sync::{Arc, Mutex};

    /// Output = input * gain; records controls; advances a shared fake
    /// clock by `cost` hops per call.
    struct Gain {
        gain: f32,
        cost_hops: f32,
        clock: Arc<Mutex<Duration>>,
        modes: Arc<Mutex<Vec<Mode>>>,
        attns: Arc<Mutex<Vec<f32>>>,
        resets: Arc<Mutex<u32>>,
    }
    impl HopEngine for Gain {
        fn process_hop(
            &mut self,
            input: &[f32; HOP],
            output: &mut [f32; HOP],
        ) -> Result<(), String> {
            for (o, i) in output.iter_mut().zip(input) {
                *o = i * self.gain;
            }
            *self.clock.lock().unwrap() += HOP_BUDGET.mul_f32(self.cost_hops);
            Ok(())
        }
        fn reset(&mut self) {
            *self.resets.lock().unwrap() += 1;
        }
        fn set_mode(&mut self, mode: Mode) {
            self.modes.lock().unwrap().push(mode);
        }
        fn set_attenuation_limit_db(&mut self, db: f32) {
            self.attns.lock().unwrap().push(db);
        }
    }

    struct Fakes {
        clock: Arc<Mutex<Duration>>,
        modes: Arc<Mutex<Vec<Mode>>>,
        attns: Arc<Mutex<Vec<f32>>>,
        resets: Arc<Mutex<u32>>,
        lines: Arc<Mutex<Vec<String>>>,
    }

    impl Fakes {
        fn new() -> Fakes {
            Fakes {
                clock: Arc::new(Mutex::new(Duration::ZERO)),
                modes: Arc::new(Mutex::new(Vec::new())),
                attns: Arc::new(Mutex::new(Vec::new())),
                resets: Arc::new(Mutex::new(0)),
                lines: Arc::new(Mutex::new(Vec::new())),
            }
        }
        fn gain(&self, gain: f32, cost_hops: f32) -> Gain {
            Gain {
                gain,
                cost_hops,
                clock: Arc::clone(&self.clock),
                modes: Arc::clone(&self.modes),
                attns: Arc::clone(&self.attns),
                resets: Arc::clone(&self.resets),
            }
        }
        fn engine<P: Policy>(
            &self,
            q: Option<Gain>,
            l: Option<Gain>,
            p: P,
        ) -> AdaptiveEngine<Gain, P> {
            let clock = Arc::clone(&self.clock);
            let base = Instant::now();
            let lines = Arc::clone(&self.lines);
            AdaptiveEngine::with_clock_and_sink(
                q,
                l,
                p,
                Box::new(move || base + *clock.lock().unwrap()),
                Box::new(move |s| lines.lock().unwrap().push(s.to_string())),
            )
        }
    }

    type Seen = Arc<Mutex<Vec<(f32, Option<f32>, u32)>>>;

    /// A policy that replays a scripted step sequence and records observes.
    struct Scripted {
        steps: Vec<Step>,
        i: usize,
        seen: Seen,
    }
    impl Policy for Scripted {
        fn step(&self) -> Step {
            self.steps[self.i.min(self.steps.len() - 1)]
        }
        fn observe(&mut self, c: f32, s: Option<f32>, lag: u32) -> Option<Event> {
            self.seen.lock().unwrap().push((c, s, lag));
            self.i += 1;
            None
        }
        fn reset(&mut self) {}
        fn live(&self) -> Tier {
            match self.step() {
                Step::Steady { live } | Step::Shadow { live, .. } => live,
                Step::Crossfade { from, .. } => from,
            }
        }
    }

    fn dc(v: f32) -> [f32; HOP] {
        [v; HOP]
    }

    fn emergency_engine(f: &Fakes) -> AdaptiveEngine<Gain, crate::ladder::Ladder> {
        let mut e = f.engine(
            Some(f.gain(1.0, 0.5)),
            Some(f.gain(0.25, 0.2)),
            crate::ladder::Ladder::new(&[Tier::Quality, Tier::Light, Tier::Raw]),
        );
        let mut out = dc(0.0);
        for _ in 0..crate::ladder::START_GRACE {
            e.process_hop(&dc(1.0), &mut out).unwrap();
        }
        e.set_lag_hops(crate::ladder::LAG_PANIC);
        e.process_hop(&dc(1.0), &mut out).unwrap();
        e.set_lag_hops(0);
        assert!(e.policy().duck_raw());
        e
    }

    fn continuous(previous: f32, samples: &[f32], bound: f32) {
        let mut prev = previous;
        for &sample in samples {
            assert!((sample - prev).abs() <= bound, "{prev} to {sample}");
            prev = sample;
        }
    }

    #[test]
    fn emergency_raw_ducks_holds_and_fades_into_model_without_a_hole() {
        let f = Fakes::new();
        let mut e = emergency_engine(&f);
        let duck = 10.0f32.powf(RAW_DUCK_DB / 20.0);
        let mut out = dc(0.0);
        e.process_hop(&dc(1.0), &mut out).unwrap();
        continuous(1.0, &out, 2.0 / HOP as f32);
        assert!(out.windows(2).all(|w| w[1] <= w[0]));
        assert_eq!(out[HOP - 1], duck);
        let mut previous_raw = 1.0;
        continuous(previous_raw, &e.raw_out, 1.0 / HOP as f32);
        assert!(e.raw_out.windows(2).all(|w| w[1] <= w[0]));
        previous_raw = e.raw_out[HOP - 1];
        for _ in 0..crate::ladder::WARMUP {
            e.process_hop(&dc(1.0), &mut out).unwrap();
            assert!(out.iter().all(|&v| (v - duck).abs() < 1e-7 && v > 0.0));
            continuous(previous_raw, &e.raw_out, 1e-7);
            previous_raw = e.raw_out[HOP - 1];
        }
        assert!(e.policy().duck_raw());
        e.process_hop(&dc(1.0), &mut out).unwrap();
        continuous(duck, &out, 1.0 / HOP as f32);
        for (i, &sample) in out.iter().enumerate() {
            let a = (i + 1) as f32 / HOP as f32;
            assert!((sample - (duck * (1.0 - a) + 0.25 * a)).abs() < 1e-7);
            assert!(sample > 0.0);
        }
        assert!(out.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(out[HOP - 1], 0.25);
        assert!(!e.policy().duck_raw());
        e.process_hop(&dc(1.0), &mut out).unwrap();
        assert_eq!(out, dc(0.25));
    }

    #[test]
    fn failed_rejoin_restores_raw_with_a_continuous_ramp_and_preserves_mute() {
        for muted in [false, true] {
            for failure_cost in [0.2, 3.0] {
                let f = Fakes::new();
                let mut e = emergency_engine(&f);
                let mut out = dc(0.0);
                if muted {
                    e.set_mode(Mode::Mute);
                }
                e.process_hop(&dc(1.0), &mut out).unwrap();
                e.light.as_mut().unwrap().cost_hops = failure_cost;
                if failure_cost < 1.0 {
                    e.set_lag_hops(crate::ladder::LAG_ABORT);
                }
                e.process_hop(&dc(1.0), &mut out).unwrap();
                assert!(out.iter().all(|&v| if muted {
                    v == 0.0
                } else {
                    v > 0.0 && v < 0.011
                }));
                assert_eq!(e.policy().step(), Step::Steady { live: Tier::Raw });
                assert!(!e.policy().duck_raw());
                let previous = out[HOP - 1];
                e.set_lag_hops(0);
                e.process_hop(&dc(1.0), &mut out).unwrap();
                continuous(previous, &out, 1.0 / HOP as f32);
                assert!(out.windows(2).all(|w| w[1] >= w[0]));
                assert_eq!(out[HOP - 1], if muted { 0.0 } else { 1.0 });
                e.process_hop(&dc(1.0), &mut out).unwrap();
                assert_eq!(out, dc(if muted { 0.0 } else { 1.0 }));
            }
        }
    }

    #[test]
    fn mute_remains_exact_zero_through_the_emergency_and_successful_rejoin() {
        let f = Fakes::new();
        let mut e = emergency_engine(&f);
        e.set_mode(Mode::Mute);
        // The fake models stand in for already muted model outputs.
        e.quality.as_mut().unwrap().gain = 0.0;
        e.light.as_mut().unwrap().gain = 0.0;
        let mut out = dc(1.0);
        for _ in
            0..crate::ladder::XFADE_PANIC + crate::ladder::WARMUP + crate::ladder::XFADE_REJOIN + 1
        {
            e.process_hop(&dc(1.0), &mut out).unwrap();
            assert_eq!(out, dc(0.0));
        }
        assert_eq!(e.live(), Tier::Light);
    }

    #[test]
    fn preventive_raw_stays_at_full_gain_with_the_real_ladder() {
        let f = Fakes::new();
        let mut e = f.engine(
            Some(f.gain(1.0, 0.94)),
            None,
            crate::ladder::Ladder::new(&[Tier::Quality, Tier::Raw]),
        );
        let mut out = dc(0.0);
        for _ in 0..400 {
            assert!(!e.policy().duck_raw());
            e.process_hop(&dc(1.0), &mut out).unwrap();
            assert!(out.iter().all(|&v| (v - 1.0).abs() < 1e-7));
        }
        assert_eq!(e.policy().step(), Step::Steady { live: Tier::Raw });
    }

    #[test]
    fn a_guard_pause_counts_as_live_cost_once() {
        let f = Fakes::new();
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let p = Scripted {
            steps: vec![Step::Steady {
                live: Tier::Quality,
            }],
            i: 0,
            seen: Arc::clone(&seen),
        };
        let mut e = f.engine(Some(f.gain(1.0, 0.5)), None, p);
        let mut out = [0.0; HOP];
        e.note_pause(Duration::from_millis(10));
        e.process_hop(&dc(1.0), &mut out).unwrap();
        e.process_hop(&dc(1.0), &mut out).unwrap();
        let seen = seen.lock().unwrap();
        assert!((seen[0].0 - 1.5).abs() < 1e-3, "{:?}", seen[0]);
        assert!((seen[1].0 - 0.5).abs() < 1e-3, "{:?}", seen[1]);
    }

    #[test]
    fn raw_is_input_delayed_by_engine_latency() {
        let f = Fakes::new();
        let mut e = f.engine(Some(f.gain(2.0, 0.1)), None, Pinned(Tier::Raw));
        let mut out = [0.0; HOP];
        let mut input = [0.0; HOP];
        input[7] = 1.0;
        let mut stream = Vec::new();
        e.process_hop(&input, &mut out).unwrap();
        stream.extend_from_slice(&out);
        for _ in 0..6 {
            e.process_hop(&dc(0.0), &mut out).unwrap();
            stream.extend_from_slice(&out);
        }
        let hits: Vec<usize> = stream
            .iter()
            .enumerate()
            .filter(|(_, &v)| v != 0.0)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(hits, vec![7 + LATENCY_SAMPLES]);
    }

    #[test]
    fn live_gain_follows_policy_and_shadow_never_leaks() {
        let f = Fakes::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let steps = vec![
            Step::Steady {
                live: Tier::Quality,
            },
            Step::Shadow {
                live: Tier::Quality,
                shadow: Tier::Light,
                kind: ShadowKind::Demote,
            },
            Step::Crossfade {
                from: Tier::Quality,
                to: Tier::Light,
                hop: 0,
                of: 2,
            },
            Step::Crossfade {
                from: Tier::Quality,
                to: Tier::Light,
                hop: 1,
                of: 2,
            },
            Step::Steady { live: Tier::Light },
        ];
        let p = Scripted {
            steps,
            i: 0,
            seen: Arc::clone(&seen),
        };
        let mut e = f.engine(Some(f.gain(1.0, 0.5)), Some(f.gain(3.0, 0.2)), p);
        let mut out = [0.0; HOP];
        e.set_lag_hops(1);
        e.process_hop(&dc(1.0), &mut out).unwrap();
        assert!(out.iter().all(|&v| v == 1.0));
        e.process_hop(&dc(1.0), &mut out).unwrap();
        assert!(out.iter().all(|&v| v == 1.0), "shadow output leaked");
        e.process_hop(&dc(1.0), &mut out).unwrap();
        // First crossfade hop: from 1.0 towards 2.0 (halfway at the end).
        assert!(out[0] > 1.0 && out[0] < 1.01, "{}", out[0]);
        assert!((out[HOP - 1] - 2.0).abs() < 0.01, "{}", out[HOP - 1]);
        assert!(out.windows(2).all(|w| w[1] >= w[0]), "monotone");
        e.process_hop(&dc(1.0), &mut out).unwrap();
        assert!((out[HOP - 1] - 3.0).abs() < 1e-5, "{}", out[HOP - 1]);
        e.process_hop(&dc(1.0), &mut out).unwrap();
        assert!(out.iter().all(|&v| (v - 3.0).abs() < 1e-6));
        // Costs: live 0.5 in the first hop, shadow 0.2 from the second, lag 1.
        let s = seen.lock().unwrap();
        assert!((s[0].0 - 0.5).abs() < 1e-3 && s[0].1.is_none() && s[0].2 == 1);
        assert!((s[1].0 - 0.5).abs() < 1e-3 && (s[1].1.unwrap() - 0.2).abs() < 1e-3);
        assert!((s[4].0 - 0.2).abs() < 1e-3 && s[4].1.is_none());
    }

    #[test]
    fn crossfade_of_equal_signals_is_flat() {
        let f = Fakes::new();
        let steps = vec![
            Step::Crossfade {
                from: Tier::Quality,
                to: Tier::Light,
                hop: 0,
                of: 3,
            },
            Step::Crossfade {
                from: Tier::Quality,
                to: Tier::Light,
                hop: 1,
                of: 3,
            },
            Step::Crossfade {
                from: Tier::Quality,
                to: Tier::Light,
                hop: 2,
                of: 3,
            },
        ];
        let p = Scripted {
            steps,
            i: 0,
            seen: Arc::new(Mutex::new(Vec::new())),
        };
        let mut e = f.engine(Some(f.gain(1.0, 0.1)), Some(f.gain(1.0, 0.1)), p);
        let mut out = [0.0; HOP];
        for _ in 0..3 {
            e.process_hop(&dc(0.5), &mut out).unwrap();
            assert!(out.iter().all(|&v| (v - 0.5).abs() < 1e-6));
        }
    }

    #[test]
    fn one_hop_raw_rejoin_has_complementary_gains_and_no_hole() {
        for gain in [1.0, 0.25] {
            let f = Fakes::new();
            let warm_hops = LATENCY_SAMPLES.div_ceil(HOP) + 1;
            let mut steps = vec![Step::Steady { live: Tier::Raw }; warm_hops];
            steps.push(Step::Crossfade {
                from: Tier::Raw,
                to: Tier::Light,
                hop: 0,
                of: 1,
            });
            steps.push(Step::Steady { live: Tier::Light });
            let p = Scripted {
                steps,
                i: 0,
                seen: Arc::new(Mutex::new(Vec::new())),
            };
            let mut e = f.engine(None, Some(f.gain(gain, 0.1)), p);
            let mut out = [0.0; HOP];
            for _ in 0..warm_hops {
                e.process_hop(&dc(1.0), &mut out).unwrap();
            }
            assert_eq!(out, dc(1.0));
            e.process_hop(&dc(1.0), &mut out).unwrap();
            for (i, &sample) in out.iter().enumerate() {
                let a = (i + 1) as f32 / HOP as f32;
                assert!((sample - (1.0 - a + gain * a)).abs() < 1e-6);
                assert!(sample >= gain);
            }
            assert!(out
                .windows(2)
                .all(|w| (w[1] - w[0]).abs() <= 1.0 / HOP as f32));
            assert_eq!(out[HOP - 1], gain);
            e.process_hop(&dc(1.0), &mut out).unwrap();
            assert_eq!(out, dc(gain));
        }
    }

    #[test]
    fn raw_while_muted_is_exact_zero_from_first_sample() {
        let f = Fakes::new();
        let warm_hops = LATENCY_SAMPLES.div_ceil(HOP) + 3;
        let mut steps = vec![
            Step::Steady {
                live: Tier::Quality
            };
            warm_hops
        ];
        steps.push(Step::Steady { live: Tier::Raw });
        let p = Scripted {
            steps,
            i: 0,
            seen: Arc::new(Mutex::new(Vec::new())),
        };
        let mut e = f.engine(Some(f.gain(1.0, 0.1)), None, p);
        let mut out = [0.0; HOP];
        // Fill the delay with audible input before starting the mute ramp.
        e.set_mode(Mode::Process);
        for _ in 0..warm_hops - 3 {
            e.process_hop(&dc(1.0), &mut out).unwrap();
        }
        assert!(e.raw.buf.iter().all(|&v| v == 1.0));
        e.set_mode(Mode::Mute);
        for _ in 0..3 {
            e.process_hop(&dc(1.0), &mut out).unwrap();
        }
        assert_eq!(e.policy().step(), Step::Steady { live: Tier::Raw });
        e.process_hop(&dc(1.0), &mut out).unwrap();
        assert!(
            out.iter().all(|&v| v == 0.0),
            "raw tier leaked audio while muted"
        );
        assert_eq!(
            f.modes.lock().unwrap().len(),
            2,
            "engine saw every mode change"
        );
    }

    #[test]
    fn controls_reach_both_engines() {
        let f = Fakes::new();
        let mut e = f.engine(
            Some(f.gain(1.0, 0.1)),
            Some(f.gain(1.0, 0.1)),
            Pinned(Tier::Quality),
        );
        e.set_attenuation_limit_db(12.0);
        e.set_mode(Mode::Bypass);
        assert_eq!(*f.attns.lock().unwrap(), vec![12.0, 12.0]);
        assert_eq!(*f.modes.lock().unwrap(), vec![Mode::Bypass, Mode::Bypass]);
    }

    #[test]
    fn reset_clears_delay_and_resets_engines() {
        let f = Fakes::new();
        let mut e = f.engine(
            Some(f.gain(1.0, 0.1)),
            Some(f.gain(1.0, 0.1)),
            Pinned(Tier::Raw),
        );
        let mut out = [0.0; HOP];
        for _ in 0..6 {
            e.process_hop(&dc(1.0), &mut out).unwrap();
        }
        assert!(out.iter().all(|&v| v == 1.0));
        e.reset();
        e.process_hop(&dc(0.0), &mut out).unwrap();
        assert!(out.iter().all(|&v| v == 0.0), "delay line kept old audio");
        assert_eq!(*f.resets.lock().unwrap(), 2);
        assert_eq!(e.live(), Tier::Raw);
    }

    #[test]
    fn baseline_line_after_500_steady_hops() {
        let f = Fakes::new();
        let mut e = f.engine(Some(f.gain(1.0, 0.42)), None, Pinned(Tier::Quality));
        let mut out = [0.0; HOP];
        for _ in 0..499 {
            e.process_hop(&dc(0.1), &mut out).unwrap();
        }
        assert!(f.lines.lock().unwrap().is_empty());
        e.process_hop(&dc(0.1), &mut out).unwrap();
        e.process_hop(&dc(0.1), &mut out).unwrap();
        let lines = f.lines.lock().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "engine: quality (cost 0.42)");
    }

    /// A policy whose reset moves the live tier (a phase was in flight).
    struct ResetMoves {
        live: Tier,
    }
    impl Policy for ResetMoves {
        fn step(&self) -> Step {
            Step::Steady { live: self.live }
        }
        fn observe(&mut self, _: f32, _: Option<f32>, _: u32) -> Option<Event> {
            None
        }
        fn reset(&mut self) {
            self.live = Tier::Light;
        }
        fn live(&self) -> Tier {
            self.live
        }
    }

    #[test]
    fn reset_that_moves_the_tier_is_logged() {
        let f = Fakes::new();
        let lines = Arc::clone(&f.lines);
        let mut e = f.engine(
            Some(f.gain(1.0, 0.1)),
            Some(f.gain(1.0, 0.1)),
            ResetMoves {
                live: Tier::Quality,
            },
        );
        e.reset();
        assert_eq!(e.live(), Tier::Light);
        assert_eq!(
            lines.lock().unwrap()[..],
            ["engine: light (stream restart)"]
        );
        // A reset that keeps the tier says nothing.
        e.reset();
        assert_eq!(lines.lock().unwrap().len(), 1);
    }

    #[test]
    fn reset_during_real_promotion_corrects_the_announced_tier() {
        let f = Fakes::new();
        let mut e = f.engine(
            Some(f.gain(1.0, 0.5)),
            Some(f.gain(1.0, 0.15)),
            crate::ladder::Ladder::new(&[Tier::Quality, Tier::Light, Tier::Raw])
                .with_dwell_base(200),
        );
        let mut out = [0.0; HOP];
        for _ in 0..crate::ladder::START_GRACE {
            e.process_hop(&dc(1.0), &mut out).unwrap();
        }
        e.set_lag_hops(crate::ladder::LAG_PANIC);
        e.process_hop(&dc(1.0), &mut out).unwrap();
        e.set_lag_hops(0);
        for _ in 0..500 {
            if matches!(
                e.policy().step(),
                Step::Crossfade {
                    from: Tier::Light,
                    to: Tier::Quality,
                    ..
                }
            ) {
                assert_eq!(
                    f.lines.lock().unwrap().last().unwrap(),
                    "engine: quality (recovered)"
                );
                e.reset();
                assert_eq!(e.live(), Tier::Light);
                assert_eq!(
                    f.lines.lock().unwrap().last().unwrap(),
                    "engine: light (stream restart)"
                );
                let count = f.lines.lock().unwrap().len();
                e.reset();
                assert_eq!(f.lines.lock().unwrap().len(), count);
                return;
            }
            e.process_hop(&dc(1.0), &mut out).unwrap();
        }
        panic!("no promotion to quality");
    }

    #[test]
    fn event_lines_name_the_live_tier() {
        let f = Fakes::new();
        let lines = Arc::clone(&f.lines);
        let mut e = f.engine(
            Some(f.gain(1.0, 0.1)),
            Some(f.gain(1.0, 0.1)),
            Pinned(Tier::Light),
        );
        e.log_event(Event::Demoted {
            to: Tier::Raw,
            cost: 0.9,
            lag: 3,
        });
        e.log_event(Event::Demoted {
            to: Tier::Light,
            cost: 0.92,
            lag: 0,
        });
        e.log_event(Event::Promoted { to: Tier::Quality });
        e.log_event(Event::TrialFailed {
            tier: Tier::Quality,
            cost: 0.81,
        });
        let l = lines.lock().unwrap();
        assert_eq!(l[0], "engine: passthrough (cpu overloaded, lag 3 hops)");
        assert_eq!(l[1], "engine: light (cpu tight, 9.2 ms per 10 ms hop)");
        assert_eq!(l[2], "engine: quality (recovered)");
        assert_eq!(
            l[3],
            "engine: light (trial of quality failed, 8.1 ms per 10 ms hop)"
        );
    }
}
