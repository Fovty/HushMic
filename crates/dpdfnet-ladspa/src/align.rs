//! RT-side alignment ledger for the async inference path (issue #10).
//!
//! The engine's OLA/ring alignment lives on the worker thread and cannot
//! desync (the engine is strictly hop-in/hop-out 1:1; transient inference
//! errors already substitute aligned zero frames internally). What the RT
//! side must guarantee is *stream* alignment: the emitted signal is the
//! engine stream delayed by exactly [`OUTPUT_LEAD`] samples, with samples
//! that miss their cycle *replaced* by zeros — never inserted — so the
//! chain's latency never drifts from the declared constant.
//!
//! Pure arithmetic over sample counts: no I/O, no atomics, no allocation.
//!
//! Input-ring overflow (a worker stall beyond ~680 ms of queued audio) is
//! NOT ledger territory: the plugin restarts the stream instead — a
//! non-blocking engine reset plus a fresh `Aligner`, resyncing to *now*
//! rather than replaying seconds-stale audio into a live call.

/// The graph quantum the design is margined for (hushmic pins the graph to
/// this via `node.force-quantum`; one 10 ms hop, WebRTC's native request).
/// A hop multiple, so cumulative pushed samples stay hop-aligned and the
/// margin needs no residue term.
pub const DESIGN_QUANTUM: usize = 480;

use hushmic_denoiser::HOP;

/// Worker stall headroom (40 ms): scheduling jitter plus transient
/// compute inflation the cushion absorbs before a zero is substituted.
/// Sustained overload still substitutes silence.
pub const STALL_HEADROOM: usize = 1920;

/// The plugin-side output margin: output for the input pushed in a cycle
/// is popped in that same callback, but the worker produces it only
/// during the following cycle period — so the cushion must cover one full
/// quantum, plus the stall headroom.
pub const OUTPUT_LEAD: usize = DESIGN_QUANTUM + STALL_HEADROOM;

/// The output lead a host cycle of `quantum` samples needs for the same
/// stall headroom at every cycle phase: the quantum itself plus the
/// largest residue that can sit in the input ring at a callback, short of
/// a whole hop (cumulative pushes of a non-hop-multiple quantum leave up
/// to `HOP - gcd(quantum, HOP)` samples the worker cannot process yet).
/// Exactly [`OUTPUT_LEAD`] at the design quantum; never smaller.
pub fn required_lead(quantum: usize) -> usize {
    let residue = HOP - gcd(quantum, HOP);
    (quantum + residue + STALL_HEADROOM).max(OUTPUT_LEAD)
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a.max(1)
    } else {
        gcd(b, a % b)
    }
}

/// Total plugin latency: the engine's measured algorithmic latency (one
/// hop of STFT framing + the models' four-hop group delay, pinned by
/// hushmic-denoiser's latency tests) plus the async output lead.
/// hushmic's `controller::LATENCY_SAMPLES` pins the same number on the
/// conf/doctor side.
pub const PLUGIN_LATENCY_SAMPLES: usize = hushmic_denoiser::LATENCY_SAMPLES + OUTPUT_LEAD;

/// What one `run()` callback should emit and discard. Always satisfies
/// `lead_zeros + real + tail_zeros == want` and `discard + real <= available`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct PopPlan {
    /// Late arrivals to pop and throw away first (their slots in the output
    /// stream were already zero-filled in earlier cycles).
    pub discard: usize,
    /// Silence to emit before real samples (startup prefill, quantum
    /// growth, or an input-overflow gap).
    pub lead_zeros: usize,
    /// Real samples to pop and emit.
    pub real: usize,
    /// Zero-fill for samples not yet produced; recorded as debt and
    /// discarded when they eventually arrive.
    pub tail_zeros: usize,
}

/// The ledger. One per plugin instance, reset on `activate()`.
#[derive(Debug)]
pub struct Aligner {
    /// Silence still owed before real samples.
    lead: usize,
    /// Zeros substituted for samples that are late but WILL arrive; the
    /// first `debt` samples later found in the ring are discarded so the
    /// stream returns to exactly nominal alignment.
    debt: usize,
    /// The lead the largest quantum seen requires (`required_lead`); a
    /// larger-than-designed quantum grows the lead once (audio stays
    /// clean, actual latency exceeds the declared value — the hushmic
    /// doctor warns about the metadata override that causes this).
    total: usize,
}

impl Aligner {
    pub fn new() -> Aligner {
        Aligner {
            lead: OUTPUT_LEAD,
            debt: 0,
            total: OUTPUT_LEAD,
        }
    }

    /// Back to the startup state (fresh prefill, no debt).
    pub fn reset(&mut self) {
        *self = Aligner::new();
    }

    /// Plan one callback's pops for `want` output samples with `available`
    /// samples sitting in the output ring.
    pub fn plan(&mut self, want: usize, available: usize) -> PopPlan {
        let need = required_lead(want);
        if need > self.total {
            // Margin structurally short for this quantum: extend the lead
            // once by the delta instead of substituting zeros every cycle.
            self.lead += need - self.total;
            self.total = need;
        }
        let discard = self.debt.min(available);
        self.debt -= discard;
        let avail = available - discard;
        let lead_zeros = self.lead.min(want);
        self.lead -= lead_zeros;
        let real = avail.min(want - lead_zeros);
        let tail_zeros = want - lead_zeros - real;
        self.debt += tail_zeros;
        PopPlan {
            discard,
            lead_zeros,
            real,
            tail_zeros,
        }
    }
}

impl Default for Aligner {
    fn default() -> Aligner {
        Aligner::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_pin_the_declared_latency() {
        // hushmic's controller::LATENCY_SAMPLES pins the same number for
        // the conf delay node and the doctor; a drift here must fail.
        assert_eq!(OUTPUT_LEAD, 2400);
        assert_eq!(PLUGIN_LATENCY_SAMPLES, 4800);
        assert_eq!(hushmic_denoiser::LATENCY_SAMPLES, 2400);
    }

    /// A pure worker/ring simulator: the engine is identity over a ramp
    /// (sample value = 1-based input index, so value 0.0 is unambiguously
    /// "substituted silence"), production can lag behind pushes, and the
    /// emitted stream is checked sample-exactly.
    struct Sim {
        aligner: Aligner,
        input_idx: usize, // next 1-based ramp value to push
        produced: Vec<f32>,
        consumed: usize,
        emitted: Vec<f32>,
    }

    impl Sim {
        fn new() -> Sim {
            Sim {
                aligner: Aligner::new(),
                input_idx: 1,
                produced: Vec::new(),
                consumed: 0,
                emitted: Vec::new(),
            }
        }

        /// Push `q` input samples (the engine "produces" them when
        /// `produce()` is called), then pop `q` per the ledger's plan.
        fn push(&mut self, q: usize) {
            for _ in 0..q {
                self.produced.push(self.input_idx as f32);
                self.input_idx += 1;
            }
        }

        /// One callback: `available` = produced-but-not-consumed, capped
        /// at `ready` (samples the worker has actually finished).
        fn pop(&mut self, q: usize, ready: usize) {
            let available = ready.min(self.produced.len() - self.consumed);
            let plan = self.aligner.plan(q, available);
            assert_eq!(plan.lead_zeros + plan.real + plan.tail_zeros, q);
            assert!(plan.discard + plan.real <= available);
            self.consumed += plan.discard;
            self.emitted
                .extend(std::iter::repeat_n(0.0, plan.lead_zeros));
            for _ in 0..plan.real {
                self.emitted.push(self.produced[self.consumed]);
                self.consumed += 1;
            }
            self.emitted
                .extend(std::iter::repeat_n(0.0, plan.tail_zeros));
        }

        fn cycle(&mut self, q: usize) {
            self.push(q);
            self.pop(q, usize::MAX);
        }

        /// Every non-silence emitted sample must sit exactly `latency`
        /// positions after its input, from `from` on.
        fn assert_alignment(&self, latency: usize, from: usize) {
            for (p, &v) in self.emitted.iter().enumerate().skip(from) {
                if v != 0.0 {
                    assert_eq!(
                        p,
                        (v as usize - 1) + latency,
                        "value {v} at position {p}, want latency {latency}"
                    );
                }
            }
        }
    }

    #[test]
    fn startup_emits_exactly_the_lead_then_real_samples() {
        let mut s = Sim::new();
        for _ in 0..=OUTPUT_LEAD / DESIGN_QUANTUM {
            s.cycle(DESIGN_QUANTUM);
        }
        assert!(s.emitted[..OUTPUT_LEAD].iter().all(|&v| v == 0.0));
        assert_eq!(s.emitted[OUTPUT_LEAD], 1.0, "first real sample");
        s.assert_alignment(OUTPUT_LEAD, 0);
    }

    #[test]
    fn forty_ms_worker_delay_preserves_every_sample() {
        let mut s = Sim::new();
        for _ in 0..20 {
            s.push(DESIGN_QUANTUM);
            // Model a worker whose output trails input by four 10 ms hops.
            let ready = s.produced.len().saturating_sub(4 * DESIGN_QUANTUM);
            s.pop(DESIGN_QUANTUM, ready.saturating_sub(s.consumed));
        }
        assert!(s.emitted[OUTPUT_LEAD..].iter().all(|&v| v != 0.0));
        s.assert_alignment(OUTPUT_LEAD, 0);
    }

    #[test]
    fn a_stall_substitutes_zeros_then_realigns_exactly() {
        let mut s = Sim::new();
        for _ in 0..=OUTPUT_LEAD / DESIGN_QUANTUM {
            s.cycle(480);
        }
        // Worker stalls hard: only 180 samples sit in the ring — the
        // standing cushion is exhausted and 300 slots must be substituted.
        s.push(480);
        s.pop(480, 180);
        // Recovered next cycle: everything ready again.
        for _ in 0..4 {
            s.cycle(480);
        }
        let zeros = s.emitted[OUTPUT_LEAD..]
            .iter()
            .filter(|&&v| v == 0.0)
            .count();
        assert_eq!(zeros, 300, "exactly the substituted samples are silence");
        s.assert_alignment(OUTPUT_LEAD, 0);
    }

    #[test]
    fn quantum_growth_extends_the_lead_once() {
        let mut s = Sim::new();
        for _ in 0..4 {
            s.cycle(480);
        }
        for _ in 0..=OUTPUT_LEAD / DESIGN_QUANTUM {
            s.cycle(1024); // metadata override beyond the design quantum
        }
        for _ in 0..4 {
            s.cycle(480); // shrinking back does NOT remove the extension
        }
        let grown = required_lead(1024);
        assert_eq!(grown, 1024 + (480 - 32) + STALL_HEADROOM);
        // Everything emitted after the growth point is aligned to the
        // grown latency; silence in between is the inserted extension.
        let growth_point = 4 * 480 + OUTPUT_LEAD;
        s.assert_alignment(grown, growth_point + (grown - OUTPUT_LEAD));
        let zeros: usize = s.emitted[..].iter().filter(|&&v| v == 0.0).count();
        assert_eq!(zeros, grown);
    }

    #[test]
    fn stall_and_growth_compose_without_desync() {
        let mut s = Sim::new();
        for _ in 0..3 {
            s.cycle(480);
        }
        s.push(480);
        s.pop(480, 100); // stall: only 100 samples ready
        for _ in 0..3 {
            s.cycle(700); // odd, larger quantum
        }
        for _ in 0..5 {
            s.cycle(480);
        }
        let latency = required_lead(700);
        // After the turbulence settles, alignment is exact at the grown
        // latency: check from the point where all inserted/substituted
        // silence is behind us.
        let settle = s.emitted.len() - 3 * 480;
        s.assert_alignment(latency, settle);
        // And zero-substitution never inserted samples: stream length is
        // exactly what was demanded.
        assert_eq!(s.emitted.len(), 4 * 480 + 3 * 700 + 5 * 480);
    }

    #[test]
    fn required_lead_keeps_the_headroom_at_every_phase() {
        // Design quantum and its divisors: no growth.
        for q in [480, 240, 160, 32, 1] {
            assert_eq!(required_lead(q), OUTPUT_LEAD, "q={q}");
        }
        // Non-divisors leave a residue in the input ring: the cushion
        // must cover it. 1024 leaves up to 448 samples (gcd 32).
        assert_eq!(required_lead(1024), 1024 + 448 + STALL_HEADROOM);
        // A hop multiple leaves none.
        assert_eq!(required_lead(960), 960 + STALL_HEADROOM);
        // Simulate a hop-granular worker that runs up to the whole stall
        // headroom behind: with the required lead no cycle at any phase
        // substitutes a zero.
        for q in [64usize, 100, 480, 512, 700, 1024, 8192] {
            for behind in [0usize, STALL_HEADROOM] {
                let mut s = Sim::new();
                let cycles = required_lead(q) / q + 2 * HOP / gcd(q, HOP);
                for _ in 0..cycles {
                    let pushed = s.produced.len();
                    s.push(q);
                    let done = (pushed.saturating_sub(behind) / HOP) * HOP;
                    s.pop(q, done.saturating_sub(s.consumed));
                }
                let zeros = s.emitted.iter().filter(|&&v| v == 0.0).count();
                assert_eq!(
                    zeros,
                    required_lead(q),
                    "q={q} behind={behind}: only the lead is silence"
                );
            }
        }
    }

    #[test]
    fn previous_callback_simulation_rejects_the_old_1024_lead() {
        let q = 1024;
        let old_lead = q + STALL_HEADROOM;
        let mut s = Sim::new();
        // Suppress automatic growth while injecting the previous lead.
        s.aligner.total = required_lead(q);
        s.aligner.lead = old_lead;
        for _ in 0..2 * HOP / gcd(q, HOP) + 4 {
            let previous = s.produced.len();
            s.push(q);
            let done = previous.saturating_sub(STALL_HEADROOM) / HOP * HOP;
            s.pop(q, done.saturating_sub(s.consumed));
        }
        let zeros = s.emitted.iter().filter(|&&v| v == 0.0).count();
        assert!(
            zeros > old_lead,
            "old lead unexpectedly covered every phase"
        );
    }

    #[test]
    fn plan_is_safe_at_the_edges() {
        let mut a = Aligner::new();
        // Nothing available at all: everything is lead/tail zeros.
        let p = a.plan(480, 0);
        assert_eq!(p.discard + p.real, 0);
        assert_eq!(p.lead_zeros + p.tail_zeros, 480);
        // Zero-size callback is a no-op.
        let p = a.plan(0, 100);
        assert_eq!(
            p,
            PopPlan {
                discard: 0,
                lead_zeros: 0,
                real: 0,
                tail_zeros: 0
            }
        );
        // Huge availability never over-pops.
        let p = a.plan(480, usize::MAX / 2);
        assert_eq!(p.lead_zeros + p.real + p.tail_zeros, 480);
    }
}
