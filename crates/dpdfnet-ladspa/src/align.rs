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

/// Worker stall headroom (20 ms): scheduling jitter plus transient
/// compute inflation the cushion absorbs before a zero is substituted.
/// Sized from the issue-#10 field data (a 4x-inflated 28 ms dpdfnet8 hop
/// consumes 18 ms of cushion).
pub const STALL_HEADROOM: usize = 960;

/// The plugin-side output margin: output for the input pushed in a cycle
/// is popped in that same callback, but the worker produces it only
/// during the following cycle period — so the cushion must cover one full
/// quantum, plus the stall headroom.
pub const OUTPUT_LEAD: usize = DESIGN_QUANTUM + STALL_HEADROOM;

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
    /// Largest `want` seen; a larger-than-designed quantum grows the lead
    /// once (audio stays clean, actual latency exceeds the declared value
    /// — the hushmic doctor warns about the metadata override that causes
    /// this).
    max_quantum: usize,
}

impl Aligner {
    pub fn new() -> Aligner {
        Aligner {
            lead: OUTPUT_LEAD,
            debt: 0,
            max_quantum: DESIGN_QUANTUM,
        }
    }

    /// Back to the startup state (fresh prefill, no debt).
    pub fn reset(&mut self) {
        *self = Aligner::new();
    }

    /// Plan one callback's pops for `want` output samples with `available`
    /// samples sitting in the output ring.
    pub fn plan(&mut self, want: usize, available: usize) -> PopPlan {
        if want > self.max_quantum {
            // Margin structurally short for this quantum: extend the lead
            // once by the delta instead of substituting zeros every cycle.
            self.lead += want - self.max_quantum;
            self.max_quantum = want;
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
        assert_eq!(OUTPUT_LEAD, 1440);
        assert_eq!(PLUGIN_LATENCY_SAMPLES, 3840);
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
        for _ in 0..6 {
            s.cycle(DESIGN_QUANTUM);
        }
        assert!(s.emitted[..OUTPUT_LEAD].iter().all(|&v| v == 0.0));
        assert_eq!(s.emitted[OUTPUT_LEAD], 1.0, "first real sample");
        s.assert_alignment(OUTPUT_LEAD, 0);
    }

    #[test]
    fn a_stall_substitutes_zeros_then_realigns_exactly() {
        let mut s = Sim::new();
        for _ in 0..4 {
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
        for _ in 0..6 {
            s.cycle(1024); // metadata override beyond the design quantum
        }
        for _ in 0..4 {
            s.cycle(480); // shrinking back does NOT remove the extension
        }
        let grown = OUTPUT_LEAD + (1024 - 480);
        // Everything emitted after the growth point is aligned to the
        // grown latency; silence in between is the inserted extension.
        let growth_point = 4 * 480 + OUTPUT_LEAD;
        s.assert_alignment(grown, growth_point + (1024 - 480));
        let zeros: usize = s.emitted[..].iter().filter(|&&v| v == 0.0).count();
        assert_eq!(zeros, OUTPUT_LEAD + (1024 - 480));
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
        let latency = OUTPUT_LEAD + (700 - 480);
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
