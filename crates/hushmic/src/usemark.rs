//! When an app began using HushMic's microphone, on the engine meter's
//! clock: the window cuts the meter's history there
//! ([`crate::meter::EngineMeter::history_since`]), so Live never draws
//! sound no app heard.
//!
//! The engine runs while anything records `hushmic_source`: an app, but
//! also Mic check's keep-alive or a volume control's level meter. A run
//! that one of those started holds sound no app heard; a run that began
//! after the last graph snapshot without an app was started by the app
//! and is all heard. The daemon learns who records from its graph
//! snapshots (every few seconds), so a small thread watches the meter's
//! header for runs (a few atomic loads a look): it bounds when the
//! current run began, and a new run wakes the daemon for a snapshot at
//! once, so the app is seen within moments, not at the next tick.
//!
//! The mark errs late, never early: a run that was going before the last
//! snapshot without an app is cut at the snapshot that first shows one
//! (up to a tick of heard sound is lost, never unheard sound shown). It
//! lives as long as the use and the source host: a new host starts over.

use crate::meter::{self, IDLE_AFTER};
use hushmic_shm::meter::MeterReader;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the watcher looks at the meter.
pub const LOOK_EVERY: Duration = Duration::from_millis(250);
/// A meter file that could not be opened is tried again after this.
const RETRY_EVERY: Duration = Duration::from_secs(1);

/// One run of the engine meter: one chain's file, one plugin instance,
/// one epoch (no reset, no idle gap).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub path: PathBuf,
    pub instance: u64,
    pub epoch: u32,
}

/// The runs the watcher saw (pure; times are CLOCK_MONOTONIC ms).
#[derive(Debug, Default)]
pub struct Runs {
    /// The run flowing at the last look, and the look before it began.
    current: Option<(Run, u64)>,
    last_look: u64,
}

impl Runs {
    /// One look at `now`: the run flowing then, if any. True when it is a
    /// new one.
    pub fn look(&mut self, now: u64, run: Option<Run>) -> bool {
        let new = match run {
            None => {
                self.current = None;
                false
            }
            Some(r) if self.current.as_ref().is_some_and(|(c, _)| *c == r) => false,
            Some(r) => {
                self.current = Some((r, self.last_look));
                true
            }
        };
        self.last_look = now;
        new
    }

    /// The current run began after this: the look before it was seen, or,
    /// with none flowing at the last look, that look (a run since then is
    /// younger still).
    pub fn began_after(&self) -> u64 {
        self.current
            .as_ref()
            .map_or(self.last_look, |(_, after)| *after)
    }
}

/// The mark (pure): fed with every graph snapshot.
#[derive(Debug, Default)]
pub struct UseMark {
    mark: Option<u64>,
    /// When the last snapshot without an app came in.
    unused_at: u64,
    /// The source host the mark belongs to.
    host: Option<u64>,
}

impl UseMark {
    /// A snapshot that came in at `now`: whether an app records HushMic,
    /// whether something else does too (and runs: [`crate::apps::others_running`]),
    /// the source host's generation, and [`Runs::began_after`].
    pub fn observe(
        &mut self,
        now: u64,
        in_use: bool,
        others: bool,
        host: Option<u64>,
        run_after: u64,
    ) {
        if !in_use {
            self.mark = None;
            self.unused_at = now;
            self.host = host;
            return;
        }
        if self.mark.is_some() && self.host == host {
            return;
        }
        // The run began after the last snapshot without an app, and
        // nothing but apps records: an app started it, and all of it was
        // heard (0 cuts nothing). Else it was going before, or a Mic check
        // or a level meter may have started it just before the app came:
        // cut where the app was first seen.
        self.mark = Some(if run_after > self.unused_at && !others {
            0
        } else {
            now
        });
        self.host = host;
    }

    /// CLOCK_MONOTONIC ms before which the meter's hops reached no app
    /// (0: none of the current run), while an app uses HushMic.
    pub fn mark(&self) -> Option<u64> {
        self.mark
    }
}

/// The watcher thread: looks at the running chain's meter every
/// [`LOOK_EVERY`], and sleeps while no chain runs.
pub struct Watch {
    runs: Arc<Mutex<Runs>>,
    chain: Arc<AtomicBool>,
    thread: Option<std::thread::Thread>,
}

impl Watch {
    /// `new_run` is called (on the watcher's thread) when a run begins.
    pub fn spawn(new_run: Box<dyn Fn() + Send>) -> Watch {
        let runs = Arc::new(Mutex::new(Runs::default()));
        let chain = Arc::new(AtomicBool::new(false));
        let (shared, running) = (Arc::clone(&runs), Arc::clone(&chain));
        let thread = std::thread::Builder::new()
            .name("hushmic-usemark".into())
            .spawn(move || {
                let mut eye = Eye::default();
                loop {
                    if !running.load(Ordering::Acquire) {
                        // No chain, no run: say so once, then sleep until
                        // one is started.
                        shared
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .look(hushmic_shm::monotonic_ms(), None);
                        while !running.load(Ordering::Acquire) {
                            std::thread::park();
                        }
                    }
                    let run = eye.look();
                    let now = hushmic_shm::monotonic_ms();
                    let new = shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .look(now, run);
                    if new {
                        new_run();
                    }
                    std::thread::sleep(LOOK_EVERY);
                }
            });
        Watch {
            runs,
            chain,
            thread: thread.ok().map(|t| t.thread().clone()),
        }
    }

    /// Whether a chain runs now (the daemon says so before each wait).
    pub fn chain(&self, running: bool) {
        if self.chain.swap(running, Ordering::AcqRel) != running && running {
            if let Some(t) = &self.thread {
                t.unpark();
            }
        }
    }

    /// [`Runs::began_after`], as of the last look.
    pub fn began_after(&self) -> u64 {
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .began_after()
    }
}

/// The watcher's reader of the running chain's meter.
#[derive(Default)]
struct Eye {
    reader: Option<(PathBuf, MeterReader)>,
    /// A path that could not be opened, and when.
    failed: Option<(PathBuf, u64)>,
}

impl Eye {
    /// The run flowing now, if any.
    fn look(&mut self) -> Option<Run> {
        let path = meter::dir()
            .map(|d| meter::path_for_generation(&d, crate::diagnostics::engine_generation()))?;
        let now = hushmic_shm::monotonic_ms();
        if self.reader.as_ref().is_none_or(|(p, _)| *p != path) {
            self.reader = None;
            let retry = RETRY_EVERY.as_millis() as u64;
            if self
                .failed
                .as_ref()
                .is_some_and(|(p, at)| *p == path && now.saturating_sub(*at) < retry)
            {
                return None;
            }
            match MeterReader::open(&path) {
                Ok(r) => {
                    self.failed = None;
                    self.reader = Some((path.clone(), r));
                }
                Err(_) => {
                    self.failed = Some((path, now));
                    return None;
                }
            }
        }
        let (path, r) = self.reader.as_ref()?;
        if !r.valid() {
            // Being set up again, or replaced: open it anew next time.
            self.reader = None;
            return None;
        }
        let last = r.last_publish_ms();
        let flowing = last > 0 && now.saturating_sub(last) <= IDLE_AFTER.as_millis() as u64;
        flowing.then(|| {
            let (instance, epoch) = r.run();
            Run {
                path: path.clone(),
                instance,
                epoch,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(epoch: u32) -> Option<Run> {
        Some(Run {
            path: PathBuf::from("/run/meter-3"),
            instance: 1,
            epoch,
        })
    }

    #[test]
    fn a_run_began_after_the_look_that_did_not_see_it() {
        let mut r = Runs::default();
        assert!(!r.look(1_000, None));
        assert_eq!(r.began_after(), 1_000, "none yet: any run is younger");
        assert!(r.look(1_250, run(0)));
        assert!(!r.look(1_500, run(0)));
        assert_eq!(r.began_after(), 1_000);
        // An idle gap or a reset: a new run.
        assert!(r.look(1_750, run(1)));
        assert_eq!(r.began_after(), 1_500);
        assert!(!r.look(2_000, None));
        assert_eq!(r.began_after(), 2_000);
    }

    #[test]
    fn a_run_an_app_started_is_heard_whole() {
        let mut m = UseMark::default();
        m.observe(5_000, false, false, Some(1), 4_000);
        assert_eq!(m.mark(), None);
        // The engine stood still at 5 s; the app's run began after that.
        m.observe(10_000, true, false, Some(1), 7_250);
        assert_eq!(m.mark(), Some(0));
        // Later snapshots keep it.
        m.observe(15_000, true, false, Some(1), 7_250);
        assert_eq!(m.mark(), Some(0));
    }

    #[test]
    fn a_run_another_reader_may_have_started_is_cut_where_the_app_was_seen() {
        let mut m = UseMark::default();
        m.observe(5_000, false, false, Some(1), 4_000);
        // A level meter started the run at 5.3 s and the app came at 5.5 s,
        // both before the next snapshot: the run is younger than the
        // snapshot without an app, but not all of it was heard.
        m.observe(5_800, true, true, Some(1), 5_250);
        assert_eq!(m.mark(), Some(5_800));
    }

    #[test]
    fn a_run_that_was_going_is_cut_where_the_app_was_seen() {
        let mut m = UseMark::default();
        // Mic check runs the engine from 3 s; no app at 5 s.
        m.observe(5_000, false, false, Some(1), 3_000);
        m.observe(10_000, true, false, Some(1), 3_000);
        assert_eq!(m.mark(), Some(10_000));
        m.observe(15_000, true, false, Some(1), 3_000);
        assert_eq!(m.mark(), Some(10_000), "kept while the use lasts");
        // The app leaves and comes back during the same run.
        m.observe(20_000, false, false, Some(1), 3_000);
        assert_eq!(m.mark(), None);
        m.observe(25_000, true, false, Some(1), 3_000);
        assert_eq!(m.mark(), Some(25_000));
    }

    #[test]
    fn a_new_source_host_starts_the_mark_over() {
        let mut m = UseMark::default();
        m.observe(5_000, false, false, Some(1), 3_000);
        m.observe(10_000, true, false, Some(1), 3_000);
        assert_eq!(m.mark(), Some(10_000));
        // The host was replaced; the apps are back on the new one, whose
        // chain's run began after the last snapshot without an app.
        m.observe(30_000, true, false, Some(2), 28_000);
        assert_eq!(m.mark(), Some(0));
        // Replaced again, and this time a run was already going.
        m.observe(40_000, true, false, Some(3), 2_000);
        assert_eq!(m.mark(), Some(40_000));
    }

    #[test]
    fn the_first_snapshot_of_a_run_seen_at_once_errs_late() {
        // The daemon just started and its very first look already saw the
        // run: nothing says when it began.
        let mut r = Runs::default();
        r.look(1_000, run(0));
        let mut m = UseMark::default();
        m.observe(1_100, true, false, Some(1), r.began_after());
        assert_eq!(m.mark(), Some(1_100));
    }
}
