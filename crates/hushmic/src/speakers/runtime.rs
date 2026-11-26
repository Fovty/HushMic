//! The speakers cleaner's decisions, kept pure: from the saved settings,
//! one graph snapshot and whether the chain process lives, what to do
//! (start, restart onto another output, stop, nothing). The owner
//! ([`super::Speakers`]) carries the steps out.

use super::graph::Graph;
use super::SINK;
use std::time::{Duration, Instant};

/// How long a fresh chain may take before a snapshot without its sink
/// counts against it.
pub const START_GRACE: Duration = Duration::from_secs(6);

/// How long a changed output must hold before the chain moves to it: a
/// device list read while a device appears or vanishes can be stale.
pub const SETTLE: Duration = Duration::from_millis(1500);

/// How long the sink may be missing from the graph (process alive)
/// before the chain is restarted: PipeWire restarted under it.
pub const MISSING: Duration = Duration::from_secs(3);

/// The longest wait between two start attempts after failures.
pub const BACKOFF_CAP: Duration = Duration::from_secs(60);

/// After this many failed starts in a row the cleaner is "failing" (the
/// status and the doctor say so; attempts go on at the capped pace).
pub const FAILING_AFTER: u32 = 3;

/// The output a chain started now should be pinned to; None = it follows
/// the system default. Pure.
///
/// - A chosen output that is there is pinned.
/// - A chosen output that is gone stays pinned (the session manager plays
///   on the default meanwhile and moves back when it returns), unless the
///   default is HushMic Speakers itself: that fallback would be a loop.
/// - Following the default needs no pin, except while the default is
///   HushMic Speakers (the user picked it as the system output): then the
///   last real default seen, else the first real output.
/// - Without a snapshot the chosen output stands (unknown is not gone).
pub fn wanted_target(
    output: Option<&str>,
    graph: Option<&Graph>,
    last_real_default: Option<&str>,
) -> Option<String> {
    let Some(g) = graph else {
        return output.map(str::to_string);
    };
    // never one whose sound reaches the microphone HushMic captures
    if let Some(o) = output.filter(|o| *o != SINK && !g.is_feeder(o)) {
        if g.has_sink(o) || !g.user_default_is_ours() {
            return Some(o.to_string());
        }
    }
    if !g.user_default_is_ours() {
        // following the default, unless that plays into the microphone
        if g.default_sink().is_some_and(|d| g.is_feeder(d)) {
            return g.sinks.first().map(|s| s.name.clone());
        }
        return None;
    }
    last_real_default
        .filter(|d| g.has_sink(d))
        .map(str::to_string)
        .or_else(|| g.sinks.first().map(|s| s.name.clone()))
}

/// What one look decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Nothing,
    /// Start a chain pinned to this output (None: following the default).
    Start(Option<String>),
    /// Replace the running chain with one on this output.
    Restart {
        target: Option<String>,
        why: Why,
    },
    Stop,
}

/// Why a running chain is replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The output it should play on changed: the owner re-targets the
    /// running chain's playback stream (its sink stays, and with it the
    /// apps playing into it), restarting only when that cannot be done.
    Output,
    /// Its sink left the graph while the process lives (PipeWire restarted).
    SinkGone,
    /// A setting the running chain cannot take (the inference engine).
    Settings,
}

/// What a look is given.
pub struct Facts<'a> {
    /// HushMic is on and the cleaner is switched on.
    pub on: bool,
    /// The saved output (None: the system default).
    pub output: Option<&'a str>,
    /// This look's snapshot (None: PipeWire did not answer).
    pub graph: Option<&'a Graph>,
    /// The chain process is alive.
    pub alive: bool,
    pub now: Instant,
}

/// The decisions' memory.
#[derive(Debug, Default)]
pub struct Runtime {
    /// Some(target) while a chain runs on that target.
    running: Option<Option<String>>,
    started_at: Option<Instant>,
    /// The last real system default output seen (memory only: HushMic
    /// never wrote the default, so nothing is left to restore).
    last_real_default: Option<String>,
    /// A wanted target that differs from the running one, and since when.
    pending: Option<(Option<String>, Instant)>,
    /// Since when the sink is missing while the process lives.
    missing_since: Option<Instant>,
    /// Failed starts (or deaths) in a row.
    failures: u32,
    /// No attempt before this.
    next_attempt: Option<Instant>,
    /// The sink was seen up since the last start.
    seen_up: bool,
}

impl Runtime {
    pub fn new() -> Runtime {
        Runtime::default()
    }

    /// Decide from one look.
    pub fn observe(&mut self, f: &Facts) -> Step {
        // The real default the user had: never the session manager's
        // momentary fallback while their default is HushMic Speakers.
        if let Some(g) = f.graph.filter(|g| !g.user_default_is_ours()) {
            if let Some(d) = g.real_default() {
                if self.last_real_default.as_deref() != Some(d) {
                    self.last_real_default = Some(d.to_string());
                }
            }
        }
        if !f.on {
            if self.running.take().is_some() {
                self.reset_run();
                self.failures = 0;
                self.next_attempt = None;
                return Step::Stop;
            }
            self.failures = 0;
            self.next_attempt = None;
            return Step::Nothing;
        }
        let wanted = wanted_target(f.output, f.graph, self.last_real_default.as_deref());
        let Some(running) = self.running.clone() else {
            return self.start_if_due(wanted, f.now);
        };
        if !f.alive {
            eprintln!("[hushmic] the speakers chain exited");
            self.running = None;
            self.reset_run();
            self.failed(f.now);
            return self.start_if_due(wanted, f.now);
        }
        let Some(g) = f.graph else {
            return Step::Nothing;
        };
        let in_grace = self
            .started_at
            .is_some_and(|t| f.now.saturating_duration_since(t) < START_GRACE);
        if g.sink_up {
            self.missing_since = None;
            if !self.seen_up {
                self.seen_up = true;
                self.failures = 0;
                self.next_attempt = None;
            }
        } else if !in_grace {
            let since = *self.missing_since.get_or_insert(f.now);
            if f.now.saturating_duration_since(since) >= MISSING {
                if self.next_attempt.is_some_and(|t| f.now < t) {
                    return Step::Nothing;
                }
                if self.seen_up {
                    eprintln!("[hushmic] HushMic Speakers left the graph; restarting it");
                } else {
                    eprintln!("[hushmic] HushMic Speakers did not appear; restarting it");
                    self.failed(f.now);
                }
                return Step::Restart {
                    target: wanted,
                    why: Why::SinkGone,
                };
            }
        }
        if wanted == running {
            self.pending = None;
            return Step::Nothing;
        }
        match &self.pending {
            Some((p, since)) if *p == wanted => {
                if f.now.saturating_duration_since(*since) >= SETTLE {
                    self.pending = None;
                    return Step::Restart {
                        target: wanted,
                        why: Why::Output,
                    };
                }
            }
            _ => self.pending = Some((wanted, f.now)),
        }
        Step::Nothing
    }

    /// A settings change (the switch or the chosen output) wants the chain
    /// on `target` now, without the settle wait.
    pub fn reconfigure(&mut self, on: bool, output: Option<&str>, graph: Option<&Graph>) -> Step {
        if !on {
            self.failures = 0;
            self.next_attempt = None;
            return if self.running.take().is_some() {
                self.reset_run();
                Step::Stop
            } else {
                Step::Nothing
            };
        }
        // a deliberate change starts over: no waiting out a backoff
        self.failures = 0;
        self.next_attempt = None;
        let wanted = wanted_target(output, graph, self.last_real_default.as_deref());
        match &self.running {
            None => Step::Start(wanted),
            Some(r) if *r == wanted => Step::Nothing,
            Some(_) => {
                self.pending = None;
                Step::Restart {
                    target: wanted,
                    why: Why::Output,
                }
            }
        }
    }

    /// The owner carried out a start (or restart) on `target`.
    pub fn started(&mut self, target: Option<String>, ok: bool, now: Instant) {
        self.reset_run();
        if ok {
            self.running = Some(target);
            self.started_at = Some(now);
        } else {
            self.running = None;
            self.failed(now);
        }
    }

    /// The owner moved the running chain to `target` without a restart
    /// (its playback stream re-targeted): the same run goes on.
    pub fn retargeted(&mut self, target: Option<String>) {
        if self.running.is_some() {
            self.running = Some(target);
            self.pending = None;
        }
    }

    /// The owner stopped the chain on its own (quit, HushMic turned off).
    pub fn stopped(&mut self) {
        self.running = None;
        self.reset_run();
    }

    /// The target the running chain was started on; None while none runs.
    pub fn running_target(&self) -> Option<Option<&str>> {
        self.running.as_ref().map(|t| t.as_deref())
    }

    /// The running chain's sink was seen up since it started.
    pub fn seen_up(&self) -> bool {
        self.seen_up
    }

    /// A down chain: try now, not at the next backoff step.
    pub fn retry_now(&mut self) {
        self.next_attempt = None;
    }

    /// When the next start may be tried (after a failure).
    pub fn next_attempt(&self) -> Option<Instant> {
        self.next_attempt
    }

    /// Failed starts in a row.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Several starts in a row failed.
    pub fn failing(&self) -> bool {
        self.failures >= FAILING_AFTER
    }

    /// A real default from before this run (WirePlumber's history) for
    /// when none was seen yet: the user's default was already HushMic
    /// Speakers when HushMic started.
    pub fn seed_last_real_default(&mut self, name: Option<String>) {
        if self.last_real_default.is_none() {
            self.last_real_default = name.filter(|n| n != SINK && !n.is_empty());
        }
    }

    /// The last real system default output seen.
    pub fn last_real_default(&self) -> Option<&str> {
        self.last_real_default.as_deref()
    }

    fn start_if_due(&mut self, wanted: Option<String>, now: Instant) -> Step {
        if self.next_attempt.is_some_and(|t| now < t) {
            return Step::Nothing;
        }
        Step::Start(wanted)
    }

    fn failed(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        let secs = 1u64 << self.failures.min(6);
        self.next_attempt = Some(now + Duration::from_secs(secs).min(BACKOFF_CAP));
    }

    fn reset_run(&mut self) {
        self.started_at = None;
        self.pending = None;
        self.missing_since = None;
        self.seen_up = false;
    }
}
