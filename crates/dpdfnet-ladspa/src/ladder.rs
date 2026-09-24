//! Tier policy for the adaptive engine (issue #14): decides, from the
//! live engine's cost and the input lag, when to fall back from the quality
//! model to the light model or to raw audio, and when to climb back.
//!
//! Pure arithmetic over hop counts: no I/O, no clocks, no allocation after
//! construction. Time is counted in hops (10 ms). Costs are fractions of the
//! 10 ms budget. The design and every constant below are explained in
//! `docs/superpowers/specs/2026-09-14-adaptive-engine-design.md`.

/// EWMA window of the live cost, in hops.
pub const LOAD_WINDOW: f32 = 50.0;
/// Lag (hops queued beyond the one being processed) that arms an
/// emergency. The RT side's real cushion is the aligner's stall headroom,
/// two hops (20 ms), and the worker's reading is coarse (it samples the
/// queue at hop start), so zeros begin around a reading of 2: the rules
/// react at 1.
pub const LAG_WARN: u32 = 1;
/// Consecutive hops at `lag >= LAG_WARN` without shrinking that trigger an
/// emergency (a transient stall drains and never gets there).
pub const LAG_HOLD: u32 = 3;
/// Lag that aborts any shadow phase at once: the pair does not fit.
pub const LAG_ABORT: u32 = 2;
/// Lag that is an emergency at once, without the hold and the cost
/// condition: a worker that was kept off the CPU for 40 ms is already
/// substituting zeros, and at any model cost the backlog drains too slowly
/// for the hold to matter. Raw drains it in one hop.
pub const LAG_PANIC: u32 = 4;
/// A trial also aborts after this many consecutive hops at `lag >= LAG_WARN`
/// (a trial is optional, zeros are not).
pub const TRIAL_LAG_HOLD: u32 = 3;
/// A trial or rejoin aborts after this many judged hops whose candidate cost
/// alone reaches one budget: hopeless, no need to wait for the lag.
pub const TRIAL_OVER_HOLD: u32 = 2;
/// The first shadow hops of a trial run with cold caches (an idle model's
/// weights come back from DRAM at about twice the cost); they are neither
/// judged nor counted.
pub const TRIAL_COLD: u32 = 2;
/// The backlog, in hops, a shadow pair may build before it is abandoned:
/// the running sum of `live + shadow - 1` over the pair's hops, floored
/// at zero, plus one more hop like the last (the decision is taken after a
/// hop has run, so the next one must still fit). The output cushion is two
/// hops (`STALL_HEADROOM`) and the run guard adds up to one eighth on top
/// of the modelled costs, so the pair stops before a zero is audible —
/// whether it is a hopeless candidate's cold first hop or a candidate
/// that merely does not fit next to the live tier. A promotable candidate
/// with a cold first hop at twice its cost (0.6 -> 1.2, next to a live
/// 0.3) projects to 1.0 and survives. The lag reading is the backstop for
/// what this model misses (scheduling delays).
pub const SHADOW_EXCESS: f32 = 1.2;
/// Hops after construction or a reset during which the ladder observes but
/// never demotes: the first second of a chain is noisy on every machine
/// (runtime warm-up, the host's extra plugin instantiations, links
/// settling), a cold-start emergency costs a raw phase plus trials, and
/// zeros at chain start are inaudible.
pub const START_GRACE: u32 = 100;
/// Grace ends early for a lag this large: a tier that is hopeless from the
/// first hop would otherwise stutter for the whole grace period. Inside the
/// grace the emergency still needs the hold and the cost condition (a
/// single first-inference stall drains and stays inaudible); `LAG_PANIC`
/// applies only once the grace is over.
pub const GRACE_LAG: u32 = 5;
/// An emergency also needs the recent live cost near or above the budget:
/// a lag that drains slowly after a one-off stall at cost 0.8 is not an
/// overload, and the preventive rule handles that band.
pub const EMERGENCY_COST: f32 = 0.95;
/// Sustained load that triggers a preventive demotion to a model tier.
pub const PREVENT_LOAD_MODEL: f32 = 0.80;
/// Sustained load that triggers a preventive demotion to raw.
pub const PREVENT_LOAD_RAW: f32 = 0.90;
/// Consecutive hops above the preventive threshold.
pub const PREVENT_CONFIRM: u32 = 50;
/// Shadow hops before a preventive demotion (one hop of STFT framing plus
/// the models' four-hop group delay is the structural minimum).
pub const WARMUP: u32 = 5;
/// Judged shadow hops before a promotion, after `TRIAL_COLD` cold hops.
pub const TRIAL: u32 = 30;
/// A trial median below this promotes; the live load must also be below it
/// for a trial to start.
pub const PROMOTE_LOAD: f32 = 0.70;
/// Crossfade hops for a preventive demotion.
pub const XFADE_DEMOTE: u32 = 3;
/// Crossfade hops into raw under emergency.
pub const XFADE_EMERGENCY: u32 = 2;
/// Crossfade hops into raw after a panic: the output is already zeros
/// behind a lag of `LAG_PANIC`, and every crossfade hop still runs the
/// failed model at its inflated cost, so one hop (a ramp over 480
/// samples) is all the smoothing that is worth its 10 ms.
pub const XFADE_PANIC: u32 = 1;
/// Crossfade hops for a promotion.
pub const XFADE_PROMOTE: u32 = 5;
/// Rejoin fade after the model has filled its delay line. A linear ramp
/// over one hop keeps complementary gains and reaches the model at its end.
pub const XFADE_REJOIN: u32 = 1;
/// Hops in a tier before the first trial (20 s).
pub const DWELL_BASE: u32 = 2000;
/// The dwell base of the tier just above Raw (5 s): raw is the worst state
/// (nothing is filtered) and its rejoin costs the light model's shadow only,
/// so it is retried sooner; sustained overload still backs off through the
/// oscillation guard's doubling.
pub const RAW_RETRY_BASE: u32 = 500;
/// Cap for that dwell (20 s): a load that comes and goes knocks the light
/// tier out again and again, and every knock-out shortly after a rejoin
/// doubles its dwell; raw filters nothing, so the doubling stops here and
/// the worst case after the load is gone is 20 s of passthrough. The model
/// tiers above keep the full cap.
pub const RAW_RETRY_MAX: u32 = 2000;
/// Dwell cap (5 min).
pub const DWELL_MAX: u32 = 30_000;
/// A demotion this soon after a promotion doubles the dwell (60 s).
pub const RECENT_PROMOTION: u32 = 6000;
/// Holding the top tier this long resets the dwell to its base (5 min).
pub const HOLD_RESET: u32 = 30_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// `dpdfnet8_48khz_hr`.
    Quality,
    /// `dpdfnet2_48khz_hr`.
    Light,
    /// The input, delayed by the engines' algorithmic latency.
    Raw,
}

impl Tier {
    /// The word used in the log contract lines and by the app.
    pub fn word(self) -> &'static str {
        match self {
            Tier::Quality => "quality",
            Tier::Light => "light",
            Tier::Raw => "passthrough",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShadowKind {
    /// Warm-up before a preventive demotion; the shadow output is discarded.
    Demote,
    /// A promotion candidate being measured; the shadow output is discarded.
    Trial,
    /// A tier expected to fit rejoins after an emergency: it warms up
    /// without a median verdict, but the budget guards still apply. This
    /// is the light tier after quality failed, or a tier a cheap panic
    /// took down. Raw exposure is `WARMUP` hops plus the rejoin fade.
    Rejoin,
}

/// What the engine should run this hop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Steady {
        live: Tier,
    },
    Shadow {
        live: Tier,
        shadow: Tier,
        kind: ShadowKind,
    },
    /// Run both, mix: the gain of `to` ramps from `hop/of` to `(hop+1)/of`
    /// across the hop.
    Crossfade {
        from: Tier,
        to: Tier,
        hop: u32,
        of: u32,
    },
}

/// A decision worth a log line. Fires when a crossfade starts.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Event {
    /// `lag > 0` marks the emergency path.
    Demoted {
        to: Tier,
        cost: f32,
        lag: u32,
    },
    Promoted {
        to: Tier,
    },
    TrialFailed {
        tier: Tier,
        cost: f32,
    },
}

pub trait Policy: Send + 'static {
    fn step(&self) -> Step;
    /// Raw is briefly audible on an emergency return to a model.
    fn duck_raw(&self) -> bool {
        false
    }
    /// Costs are fractions of the 10 ms budget; `lag_hops` is the number of
    /// hops queued beyond the one just processed.
    fn observe(&mut self, live_cost: f32, shadow_cost: Option<f32>, lag_hops: u32)
        -> Option<Event>;
    /// The DSP state was reset: drop any phase in flight, the lower tier of
    /// the pair becomes live. Backoff survives; a changed tier starts its age anew.
    fn reset(&mut self);
    fn live(&self) -> Tier;
    /// One line of internal state for `HUSHMIC_DSP_DEBUG` logging.
    fn describe(&self) -> String {
        String::new()
    }
}

impl<P: Policy + ?Sized> Policy for Box<P> {
    fn duck_raw(&self) -> bool {
        (**self).duck_raw()
    }
    fn step(&self) -> Step {
        (**self).step()
    }
    fn observe(
        &mut self,
        live_cost: f32,
        shadow_cost: Option<f32>,
        lag_hops: u32,
    ) -> Option<Event> {
        (**self).observe(live_cost, shadow_cost, lag_hops)
    }
    fn reset(&mut self) {
        (**self).reset()
    }
    fn live(&self) -> Tier {
        (**self).live()
    }
    fn describe(&self) -> String {
        (**self).describe()
    }
}

/// A policy that never moves: `HUSHMIC_DSP_TIER` and the integration tests.
pub struct Pinned(pub Tier);

impl Policy for Pinned {
    fn step(&self) -> Step {
        Step::Steady { live: self.0 }
    }
    fn observe(&mut self, _: f32, _: Option<f32>, _: u32) -> Option<Event> {
        None
    }
    fn reset(&mut self) {}
    fn live(&self) -> Tier {
        self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Change {
    Prevent,
    /// The index of the tier that failed.
    Emergency(usize),
    Promote,
    Rejoin,
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Steady,
    Shadow {
        target: usize,
        kind: ShadowKind,
        hop: u32,
        costs: [f32; TRIAL as usize],
        n: usize,
    },
    Crossfade {
        from: usize,
        to: usize,
        hop: u32,
        of: u32,
        seed: f32,
        change: Change,
    },
}

/// The ladder. Tiers are indexed top (0) to bottom, cheapest last.
pub struct Ladder {
    tiers: [Tier; 3],
    n: usize,
    live: usize,
    phase: Phase,
    load: f32,
    confirm: u32,
    prev_lag: u32,
    lag_hold: u32,
    /// Trial backoff per target tier (index into `tiers`): failed trials
    /// of one tier must not delay the retrial of another. A machine stuck
    /// in the light tier fails its quality trials until that dwell is five
    /// minutes; a starvation spike that knocks it to raw still gets its
    /// light back after the base dwell.
    dwell: [u32; 3],
    /// The bases the dwells return to (`HOLD_RESET`): `DWELL_BASE`, except
    /// `RAW_RETRY_BASE` for the tier just above Raw.
    base: [u32; 3],
    /// Hops since the live tier was entered.
    since_change: u32,
    /// Hops since the tier was entered or the last trial ended.
    since_trial: u32,
    next_trial_at: u32,
    entered_by_promotion: bool,
    /// Consecutive judged trial or rejoin hops at or above budget.
    trial_over: u32,
    /// Modelled backlog the current shadow pair has built (see
    /// `SHADOW_EXCESS`).
    excess: f32,
    /// Recent pair costs above budget, used to reserve the promotion fade.
    pair_over: [f32; LAG_HOLD as usize],
    /// The last `LAG_HOLD` live costs, for the emergency rule.
    recent: [f32; LAG_HOLD as usize],
    /// Remaining start-up grace hops.
    grace: u32,
    /// The emergency in flight was a panic at a low recent cost: the tier
    /// itself was fine, something else held the CPU.
    cheap_panic: bool,
}

impl Ladder {
    /// `tiers` top to bottom; one to three entries, optionally ending in Raw.
    pub fn new(tiers: &[Tier]) -> Ladder {
        assert!(
            (1..=3).contains(&tiers.len()),
            "ladder must contain one to three tiers"
        );
        let mut t = [Tier::Raw; 3];
        t[..tiers.len()].copy_from_slice(tiers);
        let mut base = [DWELL_BASE; 3];
        if tiers.len() > 1 && tiers.last() == Some(&Tier::Raw) {
            base[tiers.len() - 2] = RAW_RETRY_BASE;
        }
        Ladder {
            tiers: t,
            n: tiers.len(),
            live: 0,
            phase: Phase::Steady,
            load: 0.0,
            confirm: 0,
            prev_lag: 0,
            lag_hold: 0,
            dwell: base,
            base,
            since_change: 0,
            since_trial: 0,
            next_trial_at: base[0],
            entered_by_promotion: false,
            trial_over: 0,
            excess: 0.0,
            pair_over: [0.0; LAG_HOLD as usize],
            recent: [0.0; LAG_HOLD as usize],
            grace: START_GRACE,
            cheap_panic: false,
        }
    }

    /// Replace every dwell base (the `HUSHMIC_DSP_DWELL_HOPS` override).
    pub fn with_dwell_base(mut self, hops: u32) -> Ladder {
        let hops = hops.max(1);
        self.base = [hops; 3];
        self.dwell = [hops; 3];
        self.next_trial_at = hops;
        self
    }

    /// The tiers in this ladder, top to bottom.
    pub fn tiers(&self) -> &[Tier] {
        &self.tiers[..self.n]
    }

    #[cfg(test)]
    fn load(&self) -> f32 {
        self.load
    }

    #[cfg(test)]
    fn next_trial_at(&self) -> u32 {
        self.next_trial_at
    }

    fn bottom(&self) -> usize {
        self.n - 1
    }

    fn double_dwell(&mut self, tier: usize) {
        let cap = if self.tiers[self.bottom()] == Tier::Raw && tier + 2 == self.n {
            RAW_RETRY_MAX.max(self.base[tier])
        } else {
            DWELL_MAX
        };
        self.dwell[tier] = (self.dwell[tier].saturating_mul(2)).min(cap);
    }

    /// `verdict`: the trial ran its full length and the candidate was judged
    /// too expensive (the machine is marginal for it), which doubles the
    /// backoff. A trial cut short because the CPU was plainly busy at that
    /// moment (the backlog model, the lag, a candidate over a whole budget)
    /// was a silent probe that cost nothing audible: the same dwell again,
    /// so a load that comes and goes does not stack minutes of passthrough.
    /// Audible flapping (a tier rejoining and failing within
    /// `RECENT_PROMOTION`) backs off through the oscillation guard.
    fn trial_failed(&mut self, target: usize, cost: f32, verdict: bool) -> Option<Event> {
        if verdict {
            self.double_dwell(target);
        }
        self.next_trial_at = self.dwell[target];
        self.since_trial = 0;
        self.trial_over = 0;
        // The lag the pair built up drains now; the emergency rule starts
        // counting afresh instead of firing on that tail.
        self.lag_hold = 0;
        self.phase = Phase::Steady;
        Some(Event::TrialFailed {
            tier: self.tiers[target],
            cost,
        })
    }

    fn start_emergency(&mut self, lag: u32, cheap: bool) -> Option<Event> {
        if self.tiers[self.bottom()] != Tier::Raw {
            let cost = self.load;
            // Stop the overloaded model immediately. A cold model may need
            // to refill its delay line, but unfiltered audio is never exposed.
            self.finish_crossfade(self.bottom(), 0.0, Change::Prevent);
            return Some(Event::Demoted {
                to: self.tiers[self.live],
                cost,
                lag,
            });
        }
        let failed = self.live;
        self.cheap_panic = cheap;
        self.phase = Phase::Crossfade {
            from: self.live,
            to: self.bottom(),
            hop: 0,
            of: if lag >= LAG_PANIC {
                XFADE_PANIC
            } else {
                XFADE_EMERGENCY
            },
            seed: 0.0,
            change: Change::Emergency(failed),
        };
        Some(Event::Demoted {
            to: Tier::Raw,
            cost: self.load,
            lag,
        })
    }

    // Use the age at fade completion so the intent and landing agree even
    // when the promotion guard expires during the fade.
    fn emergency_rejoins(&self, failed: usize, to: usize, remaining: u32) -> bool {
        let recently_promoted = self.entered_by_promotion
            && self.since_change.saturating_add(remaining) < RECENT_PROMOTION;
        to > 0
            && self.tiers[to - 1] != Tier::Raw
            && (to - 1 != failed || (self.cheap_panic && !recently_promoted))
    }

    fn finish_crossfade(&mut self, to: usize, seed: f32, change: Change) {
        let rejoin = matches!(change, Change::Emergency(failed)
            if self.emergency_rejoins(failed, to, 0));
        let promotion = matches!(change, Change::Promote | Change::Rejoin);
        let recently_promoted = self.entered_by_promotion && self.since_change < RECENT_PROMOTION;
        if !promotion && recently_promoted {
            // The tier being left could not hold: its next trial waits longer.
            self.double_dwell(self.live);
        }
        self.live = to;
        self.load = seed;
        self.confirm = 0;
        self.lag_hold = 0;
        self.since_change = 0;
        self.since_trial = 0;
        self.entered_by_promotion = promotion;
        self.next_trial_at = match change {
            Change::Emergency(_) if rejoin => 0,
            // Otherwise the next trial targets the tier above, at that
            // tier's own backoff (no trial from the top tier).
            _ if to > 0 => self.dwell[to - 1],
            _ => u32::MAX,
        };
        if self.next_trial_at == 0 {
            self.start_shadow(to - 1, ShadowKind::Rejoin);
        } else {
            self.phase = Phase::Steady;
        }
    }

    fn start_shadow(&mut self, target: usize, kind: ShadowKind) {
        self.trial_over = 0;
        self.excess = 0.0;
        self.pair_over.fill(0.0);
        self.phase = Phase::Shadow {
            target,
            kind,
            hop: 0,
            costs: [0.0; TRIAL as usize],
            n: 0,
        };
    }

    fn median(costs: &[f32]) -> f32 {
        if costs.is_empty() {
            return 0.0;
        }
        let mut v: [f32; TRIAL as usize] = [0.0; TRIAL as usize];
        let n = costs.len().min(v.len());
        v[..n].copy_from_slice(&costs[..n]);
        let s = &mut v[..n];
        s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        s[n / 2]
    }
}

impl Policy for Ladder {
    fn duck_raw(&self) -> bool {
        match self.phase {
            Phase::Crossfade {
                to,
                hop,
                of,
                change: Change::Emergency(failed),
                ..
            } => self.emergency_rejoins(failed, to, of - hop),
            Phase::Shadow {
                kind: ShadowKind::Rejoin,
                ..
            }
            | Phase::Crossfade {
                change: Change::Rejoin,
                ..
            } => true,
            _ => false,
        }
    }

    fn step(&self) -> Step {
        match self.phase {
            Phase::Steady => Step::Steady {
                live: self.tiers[self.live],
            },
            Phase::Shadow { target, kind, .. } => Step::Shadow {
                live: self.tiers[self.live],
                shadow: self.tiers[target],
                kind,
            },
            Phase::Crossfade {
                from, to, hop, of, ..
            } => Step::Crossfade {
                from: self.tiers[from],
                to: self.tiers[to],
                hop,
                of,
            },
        }
    }

    fn observe(&mut self, live_cost: f32, shadow_cost: Option<f32>, lag: u32) -> Option<Event> {
        self.since_change = self.since_change.saturating_add(1);
        self.since_trial = self.since_trial.saturating_add(1);
        // Holding the top tier for five minutes forgives every backoff; a
        // tier that holds for RECENT_PROMOTION hops after being promoted
        // has proved itself and earns its own base dwell back (the guard
        // doubled it on every quick failure; a session of load spikes had
        // driven the quality dwell to the five-minute cap otherwise).
        if self.live == 0 && self.since_change >= HOLD_RESET {
            self.dwell = self.base;
        }
        if self.entered_by_promotion && self.since_change == RECENT_PROMOTION {
            self.dwell[self.live] = self.base[self.live];
        }
        let c = if live_cost.is_finite() {
            live_cost.clamp(0.0, 2.0)
        } else {
            2.0
        };
        self.load += (c - self.load) / LOAD_WINDOW;
        self.recent.rotate_left(1);
        self.recent[LAG_HOLD as usize - 1] = c;
        let recent_mean = self.recent.iter().sum::<f32>() / LAG_HOLD as f32;
        if lag >= LAG_WARN && lag >= self.prev_lag {
            self.lag_hold = self.lag_hold.saturating_add(1);
        } else {
            self.lag_hold = 0;
        }
        self.prev_lag = lag;
        let at_bottom = self.live == self.bottom();
        let in_grace = self.grace > 0 && lag < GRACE_LAG;
        // Panic starts on the hop after grace expires. A held costly lag
        // can still end grace early through GRACE_LAG.
        let panic = self.grace == 0 && lag >= LAG_PANIC;
        self.grace = self.grace.saturating_sub(1);
        let held = self.lag_hold >= LAG_HOLD && recent_mean >= EMERGENCY_COST;
        let emergency = !at_bottom && !in_grace && (held || panic);
        let cheap = panic && !held && recent_mean < PROMOTE_LOAD;

        match self.phase {
            Phase::Crossfade {
                to,
                hop,
                of,
                seed,
                change,
                ..
            } => {
                // Crossfades always complete: they are short, and aborting
                // one would need a second fade from the middle.
                if hop + 1 >= of {
                    self.finish_crossfade(to, seed, change);
                } else if let Phase::Crossfade { hop, .. } = &mut self.phase {
                    *hop += 1;
                }
                None
            }
            Phase::Shadow {
                target,
                kind,
                hop,
                mut costs,
                mut n,
            } => {
                let judged = kind == ShadowKind::Demote || hop >= TRIAL_COLD;
                let mut last = 0.0;
                let mut projected = self.excess;
                if let Some(s) = shadow_cost {
                    let s = if s.is_finite() { s.max(0.0) } else { 2.0 };
                    last = s;
                    let over = c + s - 1.0;
                    self.pair_over.rotate_left(1);
                    self.pair_over[LAG_HOLD as usize - 1] = over;
                    self.excess = (self.excess + over).max(0.0);
                    // The next hop is projected like this one, except after
                    // a cold hop, which the next warm one costs about half.
                    let next = if judged { over } else { c + s / 2.0 - 1.0 };
                    projected = self.excess + next.max(0.0);
                    if judged {
                        if n < costs.len() {
                            costs[n] = s;
                            n += 1;
                        }
                        if kind != ShadowKind::Demote {
                            if s >= 1.0 {
                                self.trial_over += 1;
                            } else {
                                self.trial_over = 0;
                            }
                        }
                    }
                }
                let abort = emergency
                    || lag >= LAG_ABORT
                    || projected >= SHADOW_EXCESS
                    || (kind == ShadowKind::Trial && self.lag_hold >= TRIAL_LAG_HOLD)
                    || (kind != ShadowKind::Demote && self.trial_over >= TRIAL_OVER_HOLD);
                if abort {
                    // The pair is too expensive: a trial fails, a demotion
                    // warm-up escalates. Either way raw is next when the lag
                    // is already critical.
                    let ev = match kind {
                        // Judged on the warm hops; a trial that never got
                        // that far reports the cold hop that ended it.
                        ShadowKind::Trial | ShadowKind::Rejoin if n == 0 => {
                            self.trial_failed(target, last, false)
                        }
                        ShadowKind::Trial | ShadowKind::Rejoin => {
                            self.trial_failed(target, Self::median(&costs[..n]), false)
                        }
                        ShadowKind::Demote => {
                            self.phase = Phase::Steady;
                            None
                        }
                    };
                    if kind == ShadowKind::Demote || emergency {
                        return self.start_emergency(lag, cheap);
                    }
                    return ev;
                }
                let hop = hop + 1;
                let median = Self::median(&costs[..n]);
                match kind {
                    ShadowKind::Demote if hop >= WARMUP => {
                        self.phase = Phase::Crossfade {
                            from: self.live,
                            to: target,
                            hop: 0,
                            of: XFADE_DEMOTE,
                            seed: median,
                            change: Change::Prevent,
                        };
                        Some(Event::Demoted {
                            to: self.tiers[target],
                            cost: self.load,
                            lag: 0,
                        })
                    }
                    ShadowKind::Rejoin if hop >= WARMUP => {
                        self.phase = Phase::Crossfade {
                            from: self.live,
                            to: target,
                            hop: 0,
                            of: XFADE_REJOIN,
                            seed: median,
                            change: Change::Rejoin,
                        };
                        Some(Event::Promoted {
                            to: self.tiers[target],
                        })
                    }
                    ShadowKind::Trial if hop >= TRIAL + TRIAL_COLD => {
                        if median < PROMOTE_LOAD {
                            // Reserve every pair hop of the uninterruptible
                            // fade at the highest recent pair cost. Keep the
                            // same safety margin as the shadow backlog guard.
                            let over = self.pair_over.iter().copied().fold(0.0, f32::max);
                            if self.excess + over * XFADE_PROMOTE as f32 >= SHADOW_EXCESS {
                                return self.trial_failed(target, median, false);
                            }
                            self.phase = Phase::Crossfade {
                                from: self.live,
                                to: target,
                                hop: 0,
                                of: XFADE_PROMOTE,
                                seed: median,
                                change: Change::Promote,
                            };
                            Some(Event::Promoted {
                                to: self.tiers[target],
                            })
                        } else {
                            self.trial_failed(target, median, true)
                        }
                    }
                    _ => {
                        self.phase = Phase::Shadow {
                            target,
                            kind,
                            hop,
                            costs,
                            n,
                        };
                        None
                    }
                }
            }
            Phase::Steady => {
                if emergency {
                    return self.start_emergency(lag, cheap);
                }
                if !at_bottom {
                    let next = self.live + 1;
                    let threshold = if self.tiers[next] == Tier::Raw {
                        PREVENT_LOAD_RAW
                    } else {
                        PREVENT_LOAD_MODEL
                    };
                    if self.load > threshold && !in_grace {
                        self.confirm += 1;
                    } else {
                        self.confirm = 0;
                    }
                    if self.confirm >= PREVENT_CONFIRM {
                        self.confirm = 0;
                        if self.tiers[next] == Tier::Raw {
                            self.phase = Phase::Crossfade {
                                from: self.live,
                                to: next,
                                hop: 0,
                                of: XFADE_DEMOTE,
                                seed: 0.0,
                                change: Change::Prevent,
                            };
                            return Some(Event::Demoted {
                                to: Tier::Raw,
                                cost: self.load,
                                lag: 0,
                            });
                        }
                        self.start_shadow(next, ShadowKind::Demote);
                        return None;
                    }
                }
                if self.live > 0
                    && self.since_trial >= self.next_trial_at
                    && self.load < PROMOTE_LOAD
                    && lag == 0
                {
                    self.start_shadow(self.live - 1, ShadowKind::Trial);
                }
                None
            }
        }
    }

    fn describe(&self) -> String {
        let phase = match self.phase {
            Phase::Steady => "steady".to_string(),
            Phase::Shadow { kind, hop, .. } => format!("shadow {kind:?} hop {hop}"),
            Phase::Crossfade { hop, of, .. } => format!("crossfade {hop}/{of}"),
        };
        format!(
            "live {} {phase} load {:.2} lag_hold {} since_trial {} next_trial_at {} dwell {:?} grace {} excess {:.2}",
            self.tiers[self.live].word(),
            self.load,
            self.lag_hold,
            self.since_trial,
            self.next_trial_at,
            &self.dwell[..self.n],
            self.grace,
            self.excess
        )
    }

    fn reset(&mut self) {
        let before = self.live;
        match self.phase {
            Phase::Shadow { target, .. } => self.live = self.live.max(target),
            Phase::Crossfade { from, to, .. } => self.live = from.max(to),
            Phase::Steady => {}
        }
        if self.live != before && self.live > 0 {
            // Landed lower: the tier above gets its usual trial timer (the
            // one carried over belonged to the tier just left; at the top
            // it was "never").
            self.next_trial_at = self.dwell[self.live - 1];
            self.since_trial = 0;
            self.since_change = 0;
            self.entered_by_promotion = false;
        }
        self.phase = Phase::Steady;
        self.prev_lag = 0;
        self.lag_hold = 0;
        self.grace = START_GRACE;
    }

    fn live(&self) -> Tier {
        self.tiers[self.live]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QLR: [Tier; 3] = [Tier::Quality, Tier::Light, Tier::Raw];

    #[test]
    fn duck_intent_covers_emergency_rejoin_and_its_fade() {
        for (lag, hops) in [(LAG_PANIC, XFADE_PANIC), (LAG_WARN, XFADE_EMERGENCY)] {
            let mut l = warm(&QLR);
            assert!(!l.duck_raw());
            l.start_emergency(lag, false);
            for _ in 0..hops {
                assert!(l.duck_raw());
                drive(&mut l, 1, 1.2, 0.0, 0);
            }
            for _ in 0..WARMUP {
                assert!(matches!(
                    l.step(),
                    Step::Shadow {
                        kind: ShadowKind::Rejoin,
                        ..
                    }
                ));
                assert!(l.duck_raw());
                drive(&mut l, 1, 0.0, 0.2, 0);
            }
            assert!(matches!(
                l.phase,
                Phase::Crossfade {
                    change: Change::Rejoin,
                    ..
                }
            ));
            assert!(l.duck_raw());
            drive(&mut l, XFADE_REJOIN, 0.0, 0.2, 0);
            assert!(steady(&l, Tier::Light));
            assert!(!l.duck_raw());
            assert!(l.entered_by_promotion);
        }
    }

    #[test]
    fn duck_intent_excludes_sustained_raw_and_normal_trials() {
        assert!(!Pinned(Tier::Raw).duck_raw());
        let mut l = Ladder::new(&QLR);
        l.live = l.bottom();
        assert!(!l.duck_raw());
        l.start_shadow(1, ShadowKind::Trial);
        assert!(!l.duck_raw());
        drive(&mut l, TRIAL_HOPS, 0.0, 0.2, 0);
        assert!(matches!(
            l.step(),
            Step::Crossfade {
                from: Tier::Raw,
                ..
            }
        ));
        assert!(!l.duck_raw());

        let mut l = warm(&[Tier::Light, Tier::Raw]);
        for _ in 0..400 {
            assert!(!l.duck_raw());
            drive(&mut l, 1, 0.94, 0.0, 0);
        }
        assert!(steady(&l, Tier::Raw));
        assert!(!l.duck_raw());
        // Even a malformed ladder with no model cannot request a duck.
        let mut l = warm(&[Tier::Raw, Tier::Raw]);
        l.start_emergency(LAG_PANIC, true);
        assert!(!l.duck_raw());
    }

    #[test]
    fn duck_intent_handles_rejected_rejoin_dwell_and_reset() {
        for tiers in [
            &[Tier::Quality, Tier::Raw][..],
            &[Tier::Light, Tier::Raw][..],
        ] {
            for cheap in [false, true] {
                for promoted in [false, true] {
                    let mut l = warm(tiers);
                    l.entered_by_promotion = promoted;
                    l.start_emergency(LAG_PANIC, cheap);
                    let immediate = cheap && !promoted;
                    assert_eq!(l.duck_raw(), immediate);
                    drive(&mut l, XFADE_PANIC, 0.5, 0.0, 0);
                    assert_eq!(l.duck_raw(), immediate);
                    if immediate {
                        drive(&mut l, 1, 0.0, 0.2, LAG_ABORT);
                    }
                    assert!(steady(&l, Tier::Raw));
                    for _ in 0..10 {
                        assert!(!l.duck_raw());
                        drive(&mut l, 1, 0.0, 0.0, 0);
                    }
                }
            }
        }
        let mut l = warm(&QLR);
        l.start_emergency(LAG_PANIC, false);
        assert!(l.duck_raw());
        l.reset();
        assert!(steady(&l, Tier::Raw));
        assert!(!l.duck_raw());
    }

    #[test]
    fn boxed_policy_forwards_duck_and_promotion_age_boundary_agrees() {
        for age in [RECENT_PROMOTION - 2, RECENT_PROMOTION - 1] {
            let mut l = warm(&[Tier::Light, Tier::Raw]);
            l.entered_by_promotion = true;
            l.since_change = age;
            l.start_emergency(LAG_PANIC, true);
            let expected = age + XFADE_PANIC >= RECENT_PROMOTION;
            let mut p: Box<dyn Policy> = Box::new(l);
            assert_eq!(p.duck_raw(), expected);
            p.observe(0.2, Some(0.0), 0);
            assert_eq!(p.duck_raw(), expected);
        }
    }

    /// A ladder past its start-up grace (the rules under test apply).
    fn warm(tiers: &[Tier]) -> Ladder {
        let mut l = Ladder::new(tiers);
        drive(&mut l, START_GRACE, 0.3, 0.0, 0);
        l
    }

    /// Hops from "raw is live" to "light is live" after an emergency: the
    /// rejoin warm-up plus its crossfade.
    const REJOIN_HOPS: u32 = WARMUP + XFADE_REJOIN;
    const TRIAL_HOPS: u32 = TRIAL + TRIAL_COLD;

    /// Drive `hops` hops at a constant live cost and lag; the shadow cost is
    /// `shadow` whenever the step asks for a shadow. Returns the events in
    /// order with the hop index they fired on.
    fn drive(l: &mut Ladder, hops: u32, cost: f32, shadow: f32, lag: u32) -> Vec<(u32, Event)> {
        let mut events = Vec::new();
        for h in 0..hops {
            let sc = match l.step() {
                Step::Steady { .. } => None,
                _ => Some(shadow),
            };
            if let Some(e) = l.observe(cost, sc, lag) {
                events.push((h, e));
            }
        }
        events
    }

    fn steady(l: &Ladder, t: Tier) -> bool {
        l.step() == Step::Steady { live: t }
    }

    /// Hold a lag of one for LAG_HOLD hops at a hopeless cost: the ladder
    /// must start the emergency crossfade on the last one.
    fn overload(l: &mut Ladder, cost: f32) {
        for i in 0..LAG_HOLD {
            let ev = l.observe(cost, None, 1);
            if i + 1 < LAG_HOLD {
                assert!(ev.is_none(), "emergency too early: {ev:?}");
            } else {
                assert!(
                    matches!(
                        ev,
                        Some(Event::Demoted {
                            to: Tier::Raw,
                            lag: 1,
                            ..
                        })
                    ),
                    "{ev:?}"
                );
            }
        }
        // The crossfade into raw completes regardless of lag.
        drive(l, XFADE_EMERGENCY, cost, 0.0, 1);
        assert_eq!(l.live(), Tier::Raw);
    }

    fn shadow_at(live: usize, kind: ShadowKind) -> Ladder {
        let mut l = warm(&QLR);
        l.live = live;
        l.phase = Phase::Shadow {
            target: live - 1,
            kind,
            hop: 0,
            costs: [0.0; TRIAL as usize],
            n: 0,
        };
        l
    }

    #[test]
    fn panic_raw_phase_is_seven_hops() {
        let mut l = warm(&QLR);
        l.observe(0.5, None, LAG_PANIC);
        drive(&mut l, XFADE_PANIC, 0.5, 0.0, 0);
        assert!(matches!(
            l.step(),
            Step::Shadow {
                kind: ShadowKind::Rejoin,
                ..
            }
        ));
        drive(&mut l, WARMUP, 0.0, 0.3, 0);
        assert!(matches!(
            l.step(),
            Step::Crossfade {
                from: Tier::Raw,
                to: Tier::Light,
                of: 1,
                ..
            }
        ));
        drive(&mut l, 1, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Light));
        assert_eq!(XFADE_PANIC + WARMUP + 1, 7);
    }

    #[test]
    fn promotion_reserves_the_whole_fade() {
        let mut l = shadow_at(1, ShadowKind::Trial);
        assert!(drive(&mut l, TRIAL_HOPS - 3, 0.3, 0.5, 0).is_empty());
        let ev = drive(&mut l, 3, 0.3, 0.99, 0);
        assert!(
            matches!(ev.as_slice(), [(_, Event::TrialFailed { .. })]),
            "{ev:?}"
        );
        assert!(steady(&l, Tier::Light));
        assert!((l.excess - 0.87).abs() < 1e-5);
        assert_eq!(l.dwell[0], DWELL_BASE);
        assert_eq!(l.next_trial_at(), DWELL_BASE);
    }

    #[test]
    fn promotion_reservation_keeps_recent_pair_peaks() {
        let mut l = shadow_at(1, ShadowKind::Trial);
        drive(&mut l, TRIAL_HOPS - 3, 0.3, 0.5, 0);
        assert!(l.observe(0.3, Some(0.99), 0).is_none());
        assert!(l.observe(0.3, Some(0.99), 0).is_none());
        assert!(matches!(
            l.observe(0.3, Some(0.5), 0),
            Some(Event::TrialFailed { .. })
        ));
        assert_eq!(l.next_trial_at(), DWELL_BASE);
    }

    #[test]
    fn promotion_with_room_for_the_fade_still_succeeds() {
        let mut l = shadow_at(1, ShadowKind::Trial);
        drive(&mut l, TRIAL_HOPS - 3, 0.3, 0.5, 0);
        let ev = drive(&mut l, 3, 0.3, 0.8, 0);
        assert!(matches!(
            ev.as_slice(),
            [(_, Event::Promoted { to: Tier::Quality })]
        ));
        drive(&mut l, XFADE_PROMOTE, 0.3, 0.8, 0);
        assert!(steady(&l, Tier::Quality));
    }

    #[test]
    fn reset_on_same_tier_keeps_its_promotion_age() {
        let mut l = warm(&QLR);
        l.entered_by_promotion = true;
        l.since_change = 123;
        l.reset();
        assert!(l.entered_by_promotion);
        assert_eq!(l.since_change, 123);
    }

    #[test]
    fn rejoin_rejects_two_judged_over_budget_hops() {
        let mut l = shadow_at(2, ShadowKind::Rejoin);
        assert!(drive(&mut l, TRIAL_COLD, 0.0, 1.1, 0).is_empty());
        assert!(l.observe(0.0, Some(1.1), 0).is_none());
        assert!(matches!(
            l.observe(0.0, Some(1.1), 0),
            Some(Event::TrialFailed {
                tier: Tier::Light,
                ..
            })
        ));
        assert!(steady(&l, Tier::Raw));
        assert_eq!(l.dwell[1], RAW_RETRY_BASE);
    }

    #[test]
    fn reset_lower_tier_does_not_inherit_promotion_age() {
        let mut l = warm(&QLR);
        l.entered_by_promotion = true;
        l.since_change = RECENT_PROMOTION - START_GRACE;
        l.dwell[1] = 2 * RAW_RETRY_BASE;
        l.phase = Phase::Shadow {
            target: 1,
            kind: ShadowKind::Demote,
            hop: 1,
            costs: [0.0; TRIAL as usize],
            n: 0,
        };
        l.reset();
        drive(&mut l, START_GRACE, 0.3, 0.0, 0);
        assert_eq!(l.dwell[1], 2 * RAW_RETRY_BASE);
        assert_eq!(l.since_change, START_GRACE);
        assert!(!l.entered_by_promotion);
    }

    #[test]
    fn trial_has_thirty_judged_hops_after_cold_hops() {
        let mut l = shadow_at(1, ShadowKind::Trial);
        assert!(drive(&mut l, TRIAL_COLD, 0.3, 1.2, 0).is_empty());
        assert!(drive(&mut l, TRIAL - 1, 0.3, 0.5, 0).is_empty());
        assert_eq!(
            l.observe(0.3, Some(0.5), 0),
            Some(Event::Promoted { to: Tier::Quality })
        );
    }

    #[test]
    fn lag_hold_saturates() {
        let mut l = warm(&QLR);
        l.prev_lag = LAG_WARN;
        l.lag_hold = u32::MAX;
        assert!(l.observe(0.3, None, LAG_WARN).is_none());
        assert_eq!(l.lag_hold, u32::MAX);
    }

    #[test]
    fn panic_waits_until_after_the_last_grace_hop() {
        for lag in [LAG_PANIC, GRACE_LAG] {
            let mut l = Ladder::new(&QLR);
            drive(&mut l, START_GRACE - 1, 0.3, 0.0, 0);
            assert!(l.observe(0.3, None, lag).is_none(), "lag {lag}");
            assert!(matches!(
                l.observe(0.3, None, lag),
                Some(Event::Demoted { to: Tier::Raw, .. })
            ));
        }
    }

    #[test]
    fn start_up_grace_never_demotes() {
        let mut l = Ladder::new(&QLR);
        // A hopeless first second: cost 2.0, lag held. Nothing happens.
        assert!(drive(&mut l, START_GRACE, 2.0, 0.0, 3).is_empty());
        assert!(steady(&l, Tier::Quality));
        // Right after the grace the same input is an emergency.
        let ev = drive(&mut l, LAG_HOLD, 2.0, 0.0, 3);
        assert!(
            matches!(ev[..], [(_, Event::Demoted { to: Tier::Raw, .. })]),
            "{ev:?}"
        );
        // A reset re-arms the grace.
        l.reset();
        assert!(drive(&mut l, START_GRACE, 2.0, 0.0, 3).is_empty());
    }

    #[test]
    fn grace_ends_early_for_a_large_held_lag() {
        let mut l = Ladder::new(&QLR);
        // Hopeless from the first hop: the lag climbs through the grace
        // and the emergency fires as soon as it is held at GRACE_LAG (the
        // hold and the cost condition still apply inside the grace; the
        // panic rule does not).
        assert!(drive(&mut l, 10, 2.0, 0.0, GRACE_LAG - 1).is_empty());
        let ev = drive(&mut l, LAG_HOLD, 2.0, 0.0, GRACE_LAG);
        assert!(
            matches!(
                ev[..],
                [
                    (_, Event::Demoted { to: Tier::Raw, .. }),
                    (
                        _,
                        Event::TrialFailed {
                            tier: Tier::Light,
                            ..
                        }
                    )
                ]
            ),
            "{ev:?}"
        );
        // A single first-inference stall at a normal cost drains: no event.
        let mut l = Ladder::new(&QLR);
        for lag in [6, 5, 4, 3, 2, 1, 0] {
            assert!(l.observe(0.5, None, lag).is_none());
        }
        assert!(steady(&l, Tier::Quality));
    }

    #[test]
    fn a_cheap_panic_retries_the_tier_at_once() {
        // The CPU was taken away from a tier that was fine (cost 0.5):
        // raw drains the backlog, and the tier is tried again right away
        // instead of after the dwell.
        let mut l = warm(&[Tier::Quality, Tier::Raw]);
        drive(&mut l, 100, 0.5, 0.0, 0);
        let ev = l.observe(0.5, None, LAG_PANIC);
        assert!(
            matches!(ev, Some(Event::Demoted { to: Tier::Raw, .. })),
            "{ev:?}"
        );
        drive(&mut l, XFADE_PANIC, 0.5, 0.0, 0);
        assert_eq!(l.next_trial_at(), 0);
        assert!(matches!(
            l.step(),
            Step::Shadow {
                shadow: Tier::Quality,
                kind: ShadowKind::Rejoin,
                ..
            }
        ));
        // Rejoined, then hit again within RECENT_PROMOTION: the
        // oscillation guard's dwell applies this time.
        drive(&mut l, REJOIN_HOPS, 0.0, 0.5, 0);
        assert!(steady(&l, Tier::Quality));
        drive(&mut l, 100, 0.5, 0.0, 0);
        l.observe(0.5, None, LAG_PANIC);
        drive(&mut l, XFADE_PANIC, 0.5, 0.0, 0);
        assert_eq!(l.next_trial_at(), 2 * RAW_RETRY_BASE);
        // An overload emergency (held, cost 1.2) is never cheap.
        let mut l = warm(&[Tier::Quality, Tier::Raw]);
        overload(&mut l, 1.2);
        drive(&mut l, XFADE_EMERGENCY, 1.2, 0.0, 0);
        assert_eq!(l.next_trial_at(), RAW_RETRY_BASE);
    }

    #[test]
    fn a_reset_that_lands_lower_arms_the_trial_timer() {
        // In quality by promotion (trial timer "never"); a warm-up towards
        // light is in flight when the stream restarts: the ladder lands on
        // light and must try quality again after light's usual dwell.
        let mut l = warm(&QLR).with_dwell_base(50);
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Light));
        drive(&mut l, 50 + TRIAL_HOPS + XFADE_PROMOTE + 1, 0.3, 0.5, 0);
        assert!(steady(&l, Tier::Quality));
        assert_eq!(l.next_trial_at(), u32::MAX);
        let mut hop = 0;
        while !matches!(l.step(), Step::Shadow { .. }) {
            hop += 1;
            l.observe(0.85, None, 0);
            assert!(hop < 300);
        }
        l.reset();
        assert!(steady(&l, Tier::Light));
        assert_eq!(l.next_trial_at(), 50);
        drive(&mut l, 51, 0.3, 0.0, 0);
        assert!(matches!(
            l.step(),
            Step::Shadow {
                shadow: Tier::Quality,
                kind: ShadowKind::Trial,
                ..
            }
        ));
    }

    #[test]
    fn a_promotable_candidate_survives_two_cold_hops() {
        // Candidate 0.6 next to a live 0.3, both cold hops at twice its
        // cost: backlog 0.5 then 1.0, projected with a warm next hop, so
        // the trial goes on and the warm hops promote it.
        let mut l = warm(&QLR).with_dwell_base(50);
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        drive(&mut l, 50, 0.3, 0.3, 0);
        assert!(matches!(
            l.step(),
            Step::Shadow {
                kind: ShadowKind::Trial,
                ..
            }
        ));
        assert!(l.observe(0.3, Some(1.2), 0).is_none());
        assert!(l.observe(0.3, Some(1.2), 0).is_none());
        let ev = drive(&mut l, TRIAL, 0.3, 0.6, 0);
        assert!(
            matches!(ev[..], [(_, Event::Promoted { to: Tier::Quality })]),
            "{ev:?}"
        );
    }

    #[test]
    fn steady_at_low_load_never_changes() {
        let mut l = warm(&QLR);
        assert!(drive(&mut l, 10_000, 0.3, 0.1, 0).is_empty());
        assert!(steady(&l, Tier::Quality));
    }

    #[test]
    fn preventive_demotion_sequence() {
        let mut l = warm(&QLR);
        // EWMA from 0 crosses 0.8 in ~140 hops at 0.85, then 50 confirmations.
        let mut hop = 0;
        loop {
            hop += 1;
            l.observe(0.85, None, 0);
            if matches!(l.step(), Step::Shadow { .. }) {
                break;
            }
            assert!(hop < 300, "no shadow phase started");
        }
        assert_eq!(
            l.step(),
            Step::Shadow {
                live: Tier::Quality,
                shadow: Tier::Light,
                kind: ShadowKind::Demote
            }
        );
        // The pair runs 0.15 over budget a hop: 0.75 hops of backlog by the
        // end of the warm-up, 1.2 after the crossfade, inside the cushion.
        let mut events = Vec::new();
        for _ in 0..WARMUP {
            assert!(matches!(l.step(), Step::Shadow { .. }));
            if let Some(e) = l.observe(0.85, Some(0.3), 0) {
                events.push(e);
            }
        }
        assert!(
            matches!(
                events[..],
                [Event::Demoted {
                    to: Tier::Light,
                    lag: 0,
                    ..
                }]
            ),
            "{events:?}"
        );
        for h in 0..XFADE_DEMOTE {
            assert_eq!(
                l.step(),
                Step::Crossfade {
                    from: Tier::Quality,
                    to: Tier::Light,
                    hop: h,
                    of: XFADE_DEMOTE
                }
            );
            assert!(l.observe(0.85, Some(0.3), 0).is_none());
        }
        assert!(steady(&l, Tier::Light));
        assert!(
            (l.load() - 0.3).abs() < 1e-6,
            "load seeded from the shadow median"
        );
    }

    #[test]
    fn a_warm_up_that_would_overrun_the_cushion_escalates_to_raw() {
        let mut l = warm(&QLR);
        let mut hop = 0;
        while !matches!(l.step(), Step::Shadow { .. }) {
            hop += 1;
            l.observe(0.95, None, 0);
            assert!(hop < 300, "no shadow phase started");
        }
        // 0.25 over budget a hop: the warm-up plus the crossfade would
        // build two hops of backlog. The projected backlog crosses the
        // limit at the fourth hop and the ladder takes the emergency path
        // instead (raw, then a trial of light right away).
        let mut events = Vec::new();
        for _ in 0..WARMUP {
            if let Some(e) = l.observe(0.95, Some(0.3), 0) {
                events.push(e);
            }
        }
        assert!(
            matches!(events[..], [Event::Demoted { to: Tier::Raw, .. }]),
            "{events:?}"
        );
        drive(&mut l, XFADE_EMERGENCY, 0.95, 0.0, 0);
        // Raw is live; light rejoins from the very next hop.
        assert!(
            matches!(
                l.step(),
                Step::Steady { live: Tier::Raw }
                    | Step::Shadow {
                        live: Tier::Raw,
                        shadow: Tier::Light,
                        kind: ShadowKind::Rejoin,
                    }
            ),
            "{:?}",
            l.step()
        );
        assert_eq!(
            l.step(),
            Step::Shadow {
                live: Tier::Raw,
                shadow: Tier::Light,
                kind: ShadowKind::Rejoin,
            }
        );
    }

    #[test]
    fn single_spike_does_not_demote() {
        let mut l = warm(&QLR);
        drive(&mut l, 400, 0.5, 0.1, 0);
        l.observe(10.0, None, 0);
        assert!(drive(&mut l, 400, 0.5, 0.1, 0).is_empty());
        assert!(steady(&l, Tier::Quality));
    }

    #[test]
    fn trial_start_needs_low_load_and_no_lag() {
        let mut l = warm(&QLR).with_dwell_base(50);
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Light));
        // Past the dwell, but the load climbs to 0.75: the one trial that
        // starts while the load is still under 0.7 fails (candidate 1.5),
        // and none starts after that.
        let ev = drive(&mut l, 400, 0.75, 1.5, 0);
        assert!(
            ev.iter()
                .all(|(_, e)| matches!(e, Event::TrialFailed { .. })),
            "{ev:?}"
        );
        assert!(steady(&l, Tier::Light));
        // Load back under 0.7, but a lag of one every hop: still no trial.
        drive(&mut l, 400, 0.3, 0.0, 1);
        assert!(steady(&l, Tier::Light));
        // Both gates open: the trial starts on the next hop.
        l.observe(0.3, None, 0);
        assert!(matches!(
            l.step(),
            Step::Shadow {
                shadow: Tier::Quality,
                kind: ShadowKind::Trial,
                ..
            }
        ));
    }

    #[test]
    fn non_finite_costs_count_as_the_clamp() {
        // A NaN live cost is treated as 2.0: the EWMA climbs, the
        // preventive rule fires, and the warm-up pair at 2.3 escalates to
        // raw through the backlog model.
        let mut l = warm(&QLR);
        let ev = drive(&mut l, 400, f32::NAN, 0.3, 0);
        assert!(
            matches!(ev.first(), Some((_, Event::Demoted { .. }))),
            "{ev:?}"
        );
        // A NaN shadow cost during a trial is a hopeless candidate.
        let mut l = warm(&QLR).with_dwell_base(50);
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        drive(&mut l, 50, 0.3, 0.3, 0);
        let ev = drive(&mut l, 5, 0.3, f32::NAN, 0);
        assert!(matches!(ev[..], [(_, Event::TrialFailed { .. })]), "{ev:?}");
        assert!(steady(&l, Tier::Light));
    }

    #[test]
    fn light_to_raw_preventive_on_the_three_tier_ladder() {
        let mut l = warm(&QLR);
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Light));
        // Quality trials fail from here on.
        drive(&mut l, RECENT_PROMOTION, 0.3, 1.5, 0);
        // 0.85 is under the 0.90 raw threshold: light holds (a quality
        // trial started while the load was still rising fails, nothing
        // else happens).
        let ev = drive(&mut l, 1000, 0.85, 1.5, 0);
        assert!(
            ev.iter()
                .all(|(_, e)| matches!(e, Event::TrialFailed { .. })),
            "{ev:?}"
        );
        assert!(steady(&l, Tier::Light));
        let ev = drive(&mut l, 1000, 0.95, 0.0, 0);
        assert!(
            matches!(
                ev[..],
                [(
                    _,
                    Event::Demoted {
                        to: Tier::Raw,
                        lag: 0,
                        ..
                    }
                )]
            ),
            "{ev:?}"
        );
    }

    #[test]
    fn a_demote_warm_up_aborted_by_lag_escalates_to_raw() {
        let mut l = warm(&QLR);
        let mut hop = 0;
        while !matches!(l.step(), Step::Shadow { .. }) {
            hop += 1;
            l.observe(0.85, None, 0);
            assert!(hop < 300, "no shadow phase started");
        }
        // Two warm-up hops go fine, then the lag reading hits LAG_ABORT.
        assert!(l.observe(0.85, Some(0.3), 0).is_none());
        assert!(l.observe(0.85, Some(0.3), 0).is_none());
        let ev = l.observe(0.85, Some(0.3), LAG_ABORT);
        assert!(
            matches!(
                ev,
                Some(Event::Demoted {
                    to: Tier::Raw,
                    lag: LAG_ABORT,
                    ..
                })
            ),
            "{ev:?}"
        );
    }

    #[test]
    fn prevent_to_raw_needs_higher_load() {
        let mut l = warm(&[Tier::Light, Tier::Raw]);
        assert!(drive(&mut l, 1000, 0.85, 0.0, 0).is_empty());
        assert!(steady(&l, Tier::Light));
        let ev = drive(&mut l, 1000, 0.95, 0.0, 0);
        assert!(
            matches!(
                ev[..],
                [(
                    _,
                    Event::Demoted {
                        to: Tier::Raw,
                        lag: 0,
                        ..
                    }
                )]
            ),
            "{ev:?}"
        );
        assert!(steady(&l, Tier::Raw));
    }

    #[test]
    fn single_lag_spike_that_drains_is_not_an_emergency() {
        let mut l = warm(&QLR);
        drive(&mut l, 100, 0.3, 0.1, 0);
        for lag in [3, 2, 1, 0] {
            assert!(l.observe(1.0, None, lag).is_none());
        }
        assert!(steady(&l, Tier::Quality));
    }

    #[test]
    fn a_large_lag_is_an_emergency_at_once() {
        // A worker kept off the CPU for 40 ms: no hold, no cost condition,
        // the cost reading of the hop before the stall was normal.
        let mut l = warm(&QLR);
        drive(&mut l, 100, 0.5, 0.0, 0);
        let ev = l.observe(0.5, None, LAG_PANIC);
        assert!(
            matches!(
                ev,
                Some(Event::Demoted {
                    to: Tier::Raw,
                    lag: LAG_PANIC,
                    ..
                })
            ),
            "{ev:?}"
        );
        // Inside the grace the hold still applies (GRACE_LAG).
        let mut l = Ladder::new(&QLR);
        assert!(l.observe(0.5, None, LAG_PANIC).is_none());
    }

    #[test]
    fn emergency_needs_a_high_recent_cost() {
        // A stall at cost 0.77 drains slowly: the lag reading holds at 1 for
        // several hops, but that is the preventive rule's band, not an
        // overload.
        let mut l = warm(&QLR);
        assert!(drive(&mut l, 8, 0.77, 0.0, 1).is_empty());
        assert!(steady(&l, Tier::Quality));
    }

    #[test]
    fn emergency_on_lag_held_not_on_shrinking() {
        let mut l = warm(&QLR);
        for lag in [1, 0, 1, 0, 2, 1, 0] {
            assert!(l.observe(1.0, None, lag).is_none());
        }
        assert!(steady(&l, Tier::Quality));
        assert!(l.observe(1.0, None, 1).is_none());
        assert!(l.observe(1.0, None, 1).is_none());
        let ev = l.observe(1.0, None, 1);
        assert!(
            matches!(
                ev,
                Some(Event::Demoted {
                    to: Tier::Raw,
                    lag: 1,
                    ..
                })
            ),
            "{ev:?}"
        );
        assert_eq!(
            l.step(),
            Step::Crossfade {
                from: Tier::Quality,
                to: Tier::Raw,
                hop: 0,
                of: XFADE_EMERGENCY
            }
        );
    }

    #[test]
    fn emergency_then_immediate_light_rejoin() {
        let mut l = warm(&QLR);
        overload(&mut l, 1.2);
        // Lag drained: the very next step warms Light up to rejoin, no
        // verdict, so raw is live for WARMUP plus the crossfade only.
        assert_eq!(
            l.step(),
            Step::Shadow {
                live: Tier::Raw,
                shadow: Tier::Light,
                kind: ShadowKind::Rejoin
            }
        );
        let ev = drive(&mut l, WARMUP, 0.0, 0.3, 0);
        assert!(
            matches!(ev[..], [(_, Event::Promoted { to: Tier::Light })]),
            "{ev:?}"
        );
        // The rejoin crossfade is the short one; the load is seeded with
        // the warm-up's median and the live cost then moves it.
        drive(&mut l, XFADE_REJOIN, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Light));
        assert!((l.load() - 0.3).abs() < 0.05, "{}", l.load());
    }

    #[test]
    fn emergency_from_light_waits_dwell() {
        let mut l = warm(&QLR);
        // Get to Light first via an emergency + trial.
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Light));
        // Now Light fails.
        overload(&mut l, 1.5);
        // No trial before the dwell: light's base is RAW_RETRY_BASE, and
        // light was entered by promotion less than RECENT_PROMOTION ago, so
        // the emergency doubled it to 1000.
        assert!(drive(&mut l, 2 * RAW_RETRY_BASE - 1, 0.0, 0.3, 0).is_empty());
        assert!(steady(&l, Tier::Raw));
        drive(&mut l, 2, 0.0, 0.3, 0);
        assert!(matches!(
            l.step(),
            Step::Shadow {
                shadow: Tier::Light,
                kind: ShadowKind::Trial,
                ..
            }
        ));
    }

    #[test]
    fn trial_verdict_by_median() {
        let mut l = warm(&QLR).with_dwell_base(100);
        // Start in Light.
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Light));
        // Dwell passes, a trial of Quality starts.
        drive(&mut l, 100, 0.3, 0.3, 0);
        assert!(matches!(
            l.step(),
            Step::Shadow {
                shadow: Tier::Quality,
                kind: ShadowKind::Trial,
                ..
            }
        ));
        // 16 expensive samples, 14 cheap: median 0.9 -> fail.
        drive(&mut l, TRIAL_COLD, 0.3, 0.5, 0);
        let mut ev = Vec::new();
        for i in 0..TRIAL {
            let c = if i % 2 == 0 || i >= 28 { 0.9 } else { 0.2 };
            if let Some(e) = l.observe(0.3, Some(c), 0) {
                ev.push(e);
            }
        }
        assert!(
            matches!(
                ev[..],
                [Event::TrialFailed {
                    tier: Tier::Quality,
                    ..
                }]
            ),
            "{ev:?}"
        );
        assert_eq!(l.next_trial_at(), 200, "dwell doubled");
        assert!(steady(&l, Tier::Light));
        // Next trial after 200 hops: 16 cheap, 14 expensive -> promote.
        // Cheap hops drain the backlog between expensive hops, and the
        // recent pair cost leaves room for the full promotion fade.
        drive(&mut l, 200, 0.3, 0.3, 0);
        assert!(matches!(
            l.step(),
            Step::Shadow {
                shadow: Tier::Quality,
                ..
            }
        ));
        drive(&mut l, TRIAL_COLD, 0.3, 0.5, 0);
        let mut ev = Vec::new();
        for i in 0..TRIAL {
            let c = if i % 2 == 0 || i == 29 { 0.5 } else { 0.9 };
            if let Some(e) = l.observe(0.3, Some(c), 0) {
                ev.push(e);
            }
        }
        assert!(
            matches!(ev[..], [Event::Promoted { to: Tier::Quality }]),
            "{ev:?}"
        );
        drive(&mut l, XFADE_PROMOTE, 0.5, 0.5, 0);
        assert!(steady(&l, Tier::Quality));
    }

    #[test]
    fn trial_aborts_on_lag() {
        let mut l = warm(&QLR).with_dwell_base(50);
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Light));
        drive(&mut l, 50, 0.3, 0.3, 0);
        let in_trial = |l: &Ladder| {
            matches!(
                l.step(),
                Step::Shadow {
                    kind: ShadowKind::Trial,
                    ..
                }
            )
        };
        assert!(in_trial(&l));
        // Two hops at lag 1 are tolerated, three in a row are not.
        drive(&mut l, 5, 0.3, 0.6, 0);
        assert!(l.observe(0.3, Some(0.6), 1).is_none());
        assert!(l.observe(0.3, Some(0.6), 1).is_none());
        assert!(l.observe(0.3, Some(0.6), 0).is_none());
        assert!(in_trial(&l));
        assert!(l.observe(0.3, Some(0.6), 1).is_none());
        assert!(l.observe(0.3, Some(0.6), 1).is_none());
        let ev = l.observe(0.3, Some(0.6), 1);
        assert!(
            matches!(
                ev,
                Some(Event::TrialFailed {
                    tier: Tier::Quality,
                    ..
                })
            ),
            "{ev:?}"
        );
        assert!(steady(&l, Tier::Light));
        // Cut short by lag: a silent probe, the dwell stays.
        assert_eq!(l.next_trial_at(), 50);
        // A lag of two aborts at once.
        drive(&mut l, 50, 0.3, 0.3, 0);
        assert!(in_trial(&l));
        let ev = l.observe(0.3, Some(0.6), 2);
        assert!(
            matches!(
                ev,
                Some(Event::TrialFailed {
                    tier: Tier::Quality,
                    ..
                })
            ),
            "{ev:?}"
        );
        assert_eq!(l.next_trial_at(), 50, "cut short: no backoff");
    }

    #[test]
    fn trial_aborts_on_candidate_cost_after_cold_hops() {
        let mut l = warm(&QLR).with_dwell_base(50);
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        drive(&mut l, 50, 0.3, 0.3, 0);
        assert!(matches!(
            l.step(),
            Step::Shadow {
                kind: ShadowKind::Trial,
                ..
            }
        ));
        // Two cold hops at 2x are not judged (backlog 0.3, 0.5 with the
        // live 0.2), then two judged hops at 1.0 abort: the candidate
        // cannot run alone, although the pair's backlog (0.9, projected
        // 1.1) still fits.
        assert!(l.observe(0.2, Some(1.1), 0).is_none());
        assert!(l.observe(0.2, Some(1.0), 0).is_none());
        assert!(l.observe(0.2, Some(1.0), 0).is_none());
        let ev = l.observe(0.2, Some(1.0), 0);
        assert!(
            matches!(
                ev,
                Some(Event::TrialFailed {
                    tier: Tier::Quality,
                    cost,
                }) if (cost - 1.0).abs() < 1e-6
            ),
            "{ev:?}"
        );
        assert!(steady(&l, Tier::Light));
        // A candidate at 0.75 next to a live 0.3 does not fit: the pair
        // runs over budget by 0.05 a hop and the modelled backlog ends the
        // trial (judged on the warm hops: 0.75) long before its 30 hops.
        let wait = l.next_trial_at();
        drive(&mut l, wait, 0.3, 0.3, 0);
        assert!(l.observe(0.3, Some(1.0), 0).is_none());
        assert!(l.observe(0.3, Some(0.9), 0).is_none());
        let ev = drive(&mut l, TRIAL, 0.3, 0.75, 0);
        assert!(
            matches!(ev[..], [(hop, Event::TrialFailed { cost, .. })] if (cost - 0.75).abs() < 1e-6 && hop < 20),
            "{ev:?}"
        );
        // And one at 0.6 (pair 0.9, the backlog drains) is promoted.
        let wait = l.next_trial_at();
        drive(&mut l, wait, 0.3, 0.3, 0);
        let ev = drive(&mut l, TRIAL_HOPS, 0.3, 0.6, 0);
        assert!(
            matches!(ev[..], [(_, Event::Promoted { to: Tier::Quality })]),
            "{ev:?}"
        );
    }

    #[test]
    fn a_hopeless_candidate_is_dropped_on_its_cold_hop() {
        let mut l = warm(&QLR).with_dwell_base(50);
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        drive(&mut l, 50, 0.3, 0.3, 0);
        assert!(matches!(
            l.step(),
            Step::Shadow {
                kind: ShadowKind::Trial,
                ..
            }
        ));
        // A cold hop at 2.0 next to the live 0.3 builds 1.3 hops of
        // backlog at once: the trial ends there, reporting that hop, and
        // the live tier runs alone again before a zero is audible.
        let ev = l.observe(0.3, Some(2.0), 0);
        assert!(
            matches!(ev, Some(Event::TrialFailed { cost, .. }) if (cost - 2.0).abs() < 1e-6),
            "{ev:?}"
        );
        assert!(steady(&l, Tier::Light));
    }

    #[test]
    fn dwell_caps_at_max_and_hold_resets() {
        // Quality's dwell (a model tier, the full cap) doubles on every
        // verdict failure up to DWELL_MAX; the tier above raw is capped
        // lower (see the_tier_above_raw_backs_off_to_twenty_seconds_at_most).
        let mut l = warm(&QLR).with_dwell_base(DWELL_BASE);
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Light));
        for _ in 0..8 {
            // Every trial runs its length and fails on the verdict
            // (candidate at 0.7 next to the live 0.3: pair 1.0, over 0.7).
            let wait = l.next_trial_at();
            drive(&mut l, wait + TRIAL_HOPS + 1, 0.3, 0.7, 0);
            assert!(l.next_trial_at() <= DWELL_MAX);
        }
        assert_eq!(l.next_trial_at(), DWELL_MAX);
        // A trial finally succeeds; holding the top tier for HOLD_RESET
        // hops forgives the backoff, so the next failure starts from base.
        drive(
            &mut l,
            DWELL_MAX + TRIAL_HOPS + XFADE_PROMOTE + 1,
            0.3,
            0.3,
            0,
        );
        assert!(steady(&l, Tier::Quality));
        drive(&mut l, HOLD_RESET + 1, 0.3, 0.3, 0);
        overload(&mut l, 1.5);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Light));
        assert_eq!(l.next_trial_at(), DWELL_BASE);
    }

    #[test]
    fn dwell_is_kept_per_target_tier() {
        // In light after an emergency; quality trials fail three times, so
        // quality's backoff is eight times the base. A starvation spike then
        // knocks light out: light is retried after ITS dwell (the base,
        // doubled once by the oscillation guard since light was entered by
        // promotion less than RECENT_PROMOTION hops ago), not quality's.
        let mut l = warm(&QLR).with_dwell_base(100);
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Light));
        for expected in [200, 400, 800] {
            let wait = l.next_trial_at() + TRIAL_HOPS + 1;
            drive(&mut l, wait, 0.3, 0.7, 0); // pair 1.0: full trial, verdict fails
            assert_eq!(l.next_trial_at(), expected, "quality backoff");
        }
        assert!(steady(&l, Tier::Light));
        let ev = l.observe(0.3, None, LAG_PANIC);
        assert!(
            matches!(ev, Some(Event::Demoted { to: Tier::Raw, .. })),
            "{ev:?}"
        );
        drive(&mut l, XFADE_EMERGENCY, 0.3, 0.0, 0);
        assert_eq!(l.next_trial_at(), 200, "light's own backoff, not quality's");
        drive(&mut l, 201, 0.0, 0.0, 0);
        assert!(matches!(
            l.step(),
            Step::Shadow {
                shadow: Tier::Light,
                kind: ShadowKind::Trial,
                ..
            }
        ));
    }

    #[test]
    fn a_probe_cut_short_by_load_keeps_its_dwell() {
        // Raw live after light failed; light's probes land inside load
        // bursts (candidate 1.5) and are cut short: the wait stays 5 s each
        // time, so light is back a few seconds after the load is gone.
        let mut l = warm(&QLR);
        overload(&mut l, 1.2);
        drive(&mut l, REJOIN_HOPS, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Light));
        drive(&mut l, RECENT_PROMOTION, 0.3, 1.5, 0); // quality probes fail
        overload(&mut l, 1.5);
        drive(&mut l, XFADE_EMERGENCY, 0.0, 0.0, 0);
        assert_eq!(l.next_trial_at(), RAW_RETRY_BASE);
        for _ in 0..3 {
            let ev = drive(&mut l, RAW_RETRY_BASE + TRIAL_HOPS, 0.0, 1.5, 0);
            assert!(
                matches!(
                    ev[..],
                    [(
                        _,
                        Event::TrialFailed {
                            tier: Tier::Light,
                            ..
                        }
                    )]
                ),
                "{ev:?}"
            );
            assert_eq!(
                l.next_trial_at(),
                RAW_RETRY_BASE,
                "no backoff for a cut-short probe"
            );
        }
        // The load is gone: the next probe promotes.
        let ev = drive(&mut l, RAW_RETRY_BASE + TRIAL_HOPS, 0.0, 0.3, 0);
        assert!(
            matches!(ev[..], [(_, Event::Promoted { to: Tier::Light })]),
            "{ev:?}"
        );
    }

    #[test]
    fn a_tier_that_holds_a_minute_earns_its_dwell_back() {
        let mut l = warm(&[Tier::Quality, Tier::Raw]).with_dwell_base(100);
        // A cheap panic: quality rejoins at once.
        drive(&mut l, 100, 0.5, 0.0, 0);
        l.observe(0.5, None, LAG_PANIC);
        drive(&mut l, XFADE_PANIC + REJOIN_HOPS, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Quality));
        // Knocked out within RECENT_PROMOTION twice: the guard doubles the
        // dwell each time (200, 400) and the rejoin waits for it.
        for expected in [200, 400] {
            drive(&mut l, 100, 0.5, 0.0, 0);
            l.observe(0.5, None, LAG_PANIC);
            drive(&mut l, XFADE_PANIC, 0.5, 0.0, 0);
            assert_eq!(l.next_trial_at(), expected);
            drive(
                &mut l,
                expected + TRIAL_HOPS + XFADE_PROMOTE + 1,
                0.0,
                0.3,
                0,
            );
            assert!(steady(&l, Tier::Quality));
        }
        // Holding for a minute after the promotion forgives the backoff:
        // the next (held, not cheap) emergency waits the base dwell.
        drive(&mut l, RECENT_PROMOTION + 1, 0.3, 0.0, 0);
        overload(&mut l, 1.5);
        drive(&mut l, XFADE_EMERGENCY, 1.5, 0.0, 0);
        assert_eq!(l.next_trial_at(), 100);
    }

    #[test]
    fn a_load_that_comes_and_goes_recovers_quickly_after_it_stops() {
        // Six bursts that knock every model tier out, idle gaps between
        // them. The lag is simulated: a model tier behind budget builds it,
        // raw drains it. After the last burst light must be back within a
        // retry period and quality within its (doubled) dwell.
        let mut l = warm(&QLR);
        let mut lag = 0.0f32;
        let hop_cost = |l: &Ladder, burst: bool| -> f32 {
            match (l.live(), burst) {
                (Tier::Quality, true) => 1.3,
                (Tier::Light, true) => 1.1,
                (Tier::Raw, _) => 0.0,
                (Tier::Quality, false) => 0.4,
                (Tier::Light, false) => 0.15,
            }
        };
        let mut run = |l: &mut Ladder, hops: u32, burst: bool| {
            for _ in 0..hops {
                let c = hop_cost(l, burst);
                let shadow = match l.step() {
                    Step::Steady { .. } => None,
                    Step::Shadow { shadow, .. } | Step::Crossfade { to: shadow, .. } => {
                        Some(match (shadow, burst) {
                            (Tier::Quality, true) => 1.3,
                            (Tier::Light, true) => 1.1,
                            (Tier::Raw, _) => 0.0,
                            (Tier::Quality, false) => 0.4,
                            (Tier::Light, false) => 0.15,
                        })
                    }
                };
                let pair = c + shadow.unwrap_or(0.0);
                lag = if l.live() == Tier::Raw && shadow.is_none() {
                    0.0
                } else {
                    (lag + pair - 1.0).max(0.0)
                };
                l.observe(c, shadow, lag as u32);
            }
        };
        for _ in 0..6 {
            run(&mut l, 400, true);
            run(&mut l, 500, false);
        }
        run(&mut l, 400, true);
        // After the last burst.
        let mut light_at = None;
        let mut quality_at = None;
        for h in 0..(DWELL_MAX + TRIAL_HOPS + XFADE_PROMOTE) {
            run(&mut l, 1, false);
            if light_at.is_none() && l.live() == Tier::Light {
                light_at = Some(h);
            }
            if l.live() == Tier::Quality {
                quality_at = Some(h);
                break;
            }
        }
        let light_at = light_at.expect("light never came back");
        assert!(
            light_at <= RAW_RETRY_MAX + TRIAL_HOPS + XFADE_PROMOTE + REJOIN_HOPS,
            "light back after {light_at} hops"
        );
        let quality_at = quality_at.expect("quality never came back");
        assert!(
            quality_at <= 4 * DWELL_BASE + TRIAL_HOPS + XFADE_PROMOTE,
            "quality back after {quality_at} hops"
        );
    }

    #[test]
    fn the_tier_above_raw_backs_off_to_twenty_seconds_at_most() {
        // Light knocked out within a minute of every rejoin, five times.
        let mut l = warm(&QLR);
        overload(&mut l, 1.2);
        let mut waits = Vec::new();
        for _ in 0..5 {
            let wait = l.next_trial_at();
            drive(
                &mut l,
                wait + TRIAL_HOPS + XFADE_PROMOTE + REJOIN_HOPS + 1,
                0.0,
                0.3,
                0,
            );
            assert!(steady(&l, Tier::Light), "{}", l.describe());
            drive(&mut l, 100, 0.3, 0.0, 0);
            l.observe(0.3, None, LAG_PANIC);
            drive(&mut l, XFADE_PANIC, 0.3, 0.0, 0);
            waits.push(l.next_trial_at());
        }
        assert_eq!(waits, vec![1000, 2000, 2000, 2000, 2000]);
    }

    #[test]
    fn oscillation_guard_doubles_dwell() {
        let mut l = warm(&[Tier::Quality, Tier::Raw]).with_dwell_base(100);
        overload(&mut l, 1.5);
        // Promote back (candidate cheap).
        drive(&mut l, 100 + TRIAL_HOPS + XFADE_PROMOTE + 1, 0.0, 0.3, 0);
        assert!(steady(&l, Tier::Quality));
        // Fails again within RECENT_PROMOTION: dwell doubles.
        overload(&mut l, 1.5);
        assert_eq!(l.next_trial_at(), 200);
    }

    #[test]
    fn reset_mid_phase_lands_on_lower_tier_and_keeps_counters() {
        let mut l = warm(&QLR).with_dwell_base(50);
        // Mid demotion warm-up.
        let mut hop = 0;
        while !matches!(l.step(), Step::Shadow { .. }) {
            l.observe(0.9, None, 0);
            hop += 1;
            assert!(hop < 300);
        }
        l.observe(0.9, Some(0.3), 0);
        l.reset();
        assert!(steady(&l, Tier::Light));
        let before = l.next_trial_at();
        // Run until a promotion crossfade back to Quality is in flight.
        hop = 0;
        while !matches!(
            l.step(),
            Step::Crossfade {
                to: Tier::Quality,
                ..
            }
        ) {
            drive(&mut l, 1, 0.3, 0.3, 0);
            hop += 1;
            assert!(hop < 500, "no promotion crossfade started");
        }
        l.reset();
        assert!(steady(&l, Tier::Light));
        assert_eq!(l.next_trial_at(), before);
    }

    #[test]
    fn pinned_never_moves() {
        let mut p = Pinned(Tier::Raw);
        for lag in [0, 3, 0] {
            assert!(p.observe(1.5, None, lag).is_none());
            assert_eq!(p.step(), Step::Steady { live: Tier::Raw });
        }
        assert_eq!(p.live(), Tier::Raw);
    }

    #[test]
    fn two_tier_ladder_trials_quality_after_dwell() {
        let mut l = warm(&[Tier::Quality, Tier::Raw]).with_dwell_base(10);
        overload(&mut l, 1.5);
        // The failed tier is the one just above raw: no immediate trial.
        assert!(drive(&mut l, 9, 0.0, 0.3, 0).is_empty());
        assert!(steady(&l, Tier::Raw));
        drive(&mut l, 2, 0.0, 0.3, 0);
        assert!(matches!(
            l.step(),
            Step::Shadow {
                shadow: Tier::Quality,
                kind: ShadowKind::Trial,
                ..
            }
        ));
    }

    #[test]
    fn lag_at_bottom_is_ignored() {
        let mut l = warm(&[Tier::Quality, Tier::Raw]);
        overload(&mut l, 1.5);
        assert!(drive(&mut l, 100, 0.0, 0.0, 5).is_empty());
        assert!(steady(&l, Tier::Raw));
    }

    #[test]
    #[should_panic]
    fn ladder_must_not_be_empty() {
        let _ = Ladder::new(&[]);
    }
}
