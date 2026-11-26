//! The default-source takeover's PipeWire calls, off the main loop.
//!
//! Reading and writing the "default" metadata waits for the session
//! manager: with WirePlumber busy a call takes seconds, with it stalled it
//! never answers. On the main loop that held mute, push-to-talk and the
//! tray; under one short shared budget a slow box (about 1 s per call)
//! never fit a read and a write, so HushMic never became the default. So
//! an attempt runs here, one at a time, each call capped at [`CALL_CAP`],
//! and its outcome comes back to the main loop, which owns the takeover's
//! state and applies it ([`crate::controller::Controller::takeover_finished`]).
//!
//! The only step that changes the session is the write. It runs only
//! while the chain that asked is still the one that wants it, and while it
//! is being sent the controller's teardown waits for it
//! ([`Worker::cancel`]), so a quit's restore always goes out after it. The
//! read-back that confirms the write is not waited for: from the send on,
//! a teardown sees a provisional "sent, not confirmed" outcome.
//!
//! Before the write, the user's default goes to the disk as the crash
//! breadcrumb. That waits for the disk, without a cap (a breadcrumb that
//! may not be there after a crash is none), so it comes before the
//! attempt asks whether it may write: a teardown never waits for the
//! disk, and an attempt that finds itself unwanted when the disk answers
//! sends nothing and takes its breadcrumb away again. So once a teardown
//! has returned, no write of this worker is sent any more; one that was
//! being sent was waited for, and the teardown's restore went out after
//! it.

use crate::pipewire;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long one PipeWire call of an attempt may take.
pub const CALL_CAP: Duration = Duration::from_secs(5);

/// How long a whole attempt may take (read, a dump when there is no
/// configured default, the write and its read-back).
pub const ATTEMPT_CAP: Duration = Duration::from_secs(15);

/// The caps an attempt runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub call: Duration,
    pub attempt: Duration,
}

impl Default for Caps {
    fn default() -> Caps {
        Caps {
            call: CALL_CAP,
            attempt: ATTEMPT_CAP,
        }
    }
}

/// What an attempt starts from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Job {
    /// Nothing taken yet: record the user's default, then write ours.
    Capture,
    /// A write whose read-back got no answer: it may have landed, and the
    /// user may have picked a device since. Look first, then write again.
    Retry {
        /// What this takeover found (and restores on quit).
        prior: Option<String>,
    },
}

/// How a write and its read-back ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    Landed,
    /// No answer in time: it may still land.
    NoAnswer,
    /// Answered and not applied (a restricted client), or the write failed.
    Dropped,
    /// Not sent: writing the breadcrumb took what was left of the attempt.
    Unsent,
}

/// What an attempt did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A read got no answer: nothing recorded, nothing written.
    NoAnswer,
    /// The chain that asked no longer wants it: nothing written.
    Cancelled,
    /// The user's default was recorded (and persisted for crash recovery)
    /// and ours written, unless `write` says it was not sent.
    Captured { prior: Option<String>, write: Write },
    /// The earlier write had landed after all: nothing written.
    Confirmed,
    /// The default is a real device other than the one this takeover
    /// found: the user's pick, which stands. Nothing written.
    Picked(String),
    /// The earlier write was lost: written again.
    Rewritten(Write),
}

impl Outcome {
    /// The session manager did not answer (as opposed to answering and
    /// dropping the write).
    pub fn unanswered(&self) -> bool {
        matches!(
            self,
            Outcome::NoAnswer
                | Outcome::Captured {
                    write: Write::NoAnswer,
                    ..
                }
                | Outcome::Rewritten(Write::NoAnswer)
        )
    }
}

/// An attempt's outcome, with the chain generation that asked for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    pub generation: u64,
    pub outcome: Outcome,
}

/// How an attempt tells the worker where it is.
pub trait Gate {
    /// Asked right before the write, once the breadcrumb is on the disk.
    /// From a yes until [`Gate::sent`] (or the attempt's end), the worker
    /// counts the attempt as writing, and [`Worker::cancel`] waits for
    /// that: one call, capped.
    fn may_write(&mut self) -> bool;
    /// The write has been sent; what remains is its read-back.
    /// `provisional` is what a teardown takes from now on, if it comes
    /// before the attempt ends.
    fn sent(&mut self, provisional: Outcome);
}

/// Called by an attempt right before it writes the breadcrumb (tests: a
/// disk that is slow to take it).
pub type Hold = Arc<dyn Fn() + Send + Sync>;

/// One attempt: blocking, every call capped.
pub fn attempt(job: &Job, caps: Caps, hold: Option<&Hold>, gate: &mut dyn Gate) -> Outcome {
    let deadline = Instant::now() + caps.attempt;
    let budget = || pipewire::remaining(deadline).map(|r| r.min(caps.call));
    let Some(current) = budget().and_then(pipewire::read_default_source) else {
        return Outcome::NoAnswer;
    };
    let prior = match job {
        Job::Capture => match current {
            // Never record our own node as the user's default: a crashed
            // run's persisted copy is the better answer (startup recovery
            // keeps its file when WirePlumber was not up yet), else the
            // newest real mic of WirePlumber's own history (a session that
            // ended before WirePlumber saved the restore, UX panel A26),
            // plugged in or not (A118 SF-T1)
            Some(name) if name == "hushmic_source" => {
                crate::controller::read_persisted_prior_default()
                    .or_else(|| pipewire::prior_from_history(&pipewire::wp_source_history()))
            }
            // No CONFIGURED default is common: the EFFECTIVE default the
            // session manager computed is the device in use, a far better
            // restore target than nothing.
            None => {
                let Some(b) = budget() else {
                    return Outcome::NoAnswer;
                };
                pipewire::pw_dump_within(b)
                    .and_then(|d| pipewire::parse_default_source(&d))
                    .filter(|n| n != "hushmic_source")
                    .or_else(crate::controller::read_persisted_prior_default)
            }
            other => other,
        },
        Job::Retry { prior } => {
            match current.as_deref() {
                Some("hushmic_source") => return Outcome::Confirmed,
                Some(picked)
                    if crate::config::can_own_profile(picked)
                        && Some(picked) != prior.as_deref() =>
                {
                    return Outcome::Picked(picked.to_string());
                }
                // Still what this takeover found (or unset): lost.
                _ => {}
            }
            prior.clone()
        }
    };
    // Survive a crash while the takeover is active: the breadcrumb is
    // written before the default points at us. And before the gate is
    // asked: it waits for the disk, which a teardown must not wait for.
    let in_time = budget().is_some();
    let made = *job == Job::Capture && {
        if let Some(hold) = hold {
            hold();
        }
        crate::controller::persist_prior_default(prior.as_deref().unwrap_or(""))
    };
    if !gate.may_write() {
        // Nothing is taken, so nothing would clear the breadcrumb this
        // attempt made. One that was there before it stays: a crashed
        // run's copy of the user's device.
        if made {
            crate::controller::clear_persisted_prior_default();
        }
        return Outcome::Cancelled;
    }
    if *job == Job::Capture && in_time && budget().is_none() {
        // The disk took what was left of the attempt.
        return outcome(job, &prior, Write::Unsent);
    }
    let ended = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::TimedOut => Write::NoAnswer,
        _ => Write::Dropped,
    };
    let write = match pipewire::write_default_source("hushmic_source", &budget) {
        Ok(()) => {
            gate.sent(outcome(job, &prior, Write::NoAnswer));
            match pipewire::confirm_default_source("hushmic_source", &budget) {
                Ok(()) => Write::Landed,
                Err(e) => ended(e),
            }
        }
        Err(e) => ended(e),
    };
    outcome(job, &prior, write)
}

/// What a write under `job` amounts to.
fn outcome(job: &Job, prior: &Option<String>, write: Write) -> Outcome {
    match job {
        Job::Capture => Outcome::Captured {
            prior: prior.clone(),
            write,
        },
        Job::Retry { .. } => Outcome::Rewritten(write),
    }
}

#[derive(Default)]
struct Slot {
    busy: bool,
    /// From the attempt's go-ahead to write until the write is sent.
    writing: bool,
    /// The sent write's outcome until its read-back ends ([`Gate::sent`]).
    provisional: Option<Done>,
    /// A teardown took the provisional outcome and dealt with the write:
    /// what the read-back finds is for nobody.
    settled: bool,
    done: Option<Done>,
}

struct Shared {
    slot: Mutex<Slot>,
    cv: Condvar,
    /// The chain generation whose attempt may write; 0: none.
    wanted: AtomicU64,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What an attempt's body is (the default runs [`attempt`]; tests swap it).
type Body = Box<dyn Fn(&Job, &mut dyn Gate) -> Outcome + Send>;

/// The worker's side of [`Gate`] for the attempt of `generation`.
struct SlotGate<'a> {
    shared: &'a Shared,
    generation: u64,
}

impl Gate for SlotGate<'_> {
    fn may_write(&mut self) -> bool {
        let mut slot = self.shared.lock();
        slot.writing = self.shared.wanted.load(Ordering::Acquire) == self.generation;
        slot.writing
    }

    fn sent(&mut self, provisional: Outcome) {
        let mut slot = self.shared.lock();
        slot.writing = false;
        slot.provisional = Some(Done {
            generation: self.generation,
            outcome: provisional,
        });
        self.shared.cv.notify_all();
    }
}

/// The thread that runs attempts, one at a time.
pub struct Worker {
    tx: mpsc::Sender<(u64, Job)>,
    shared: Arc<Shared>,
}

impl Worker {
    /// `done` is called (on the worker thread) after each attempt; the
    /// outcome waits in [`Worker::take`].
    pub fn spawn(done: Box<dyn Fn() + Send>, caps: Caps, hold: Option<Hold>) -> Worker {
        Worker::with_body(
            done,
            Box::new(move |job, gate| attempt(job, caps, hold.as_ref(), gate)),
        )
    }

    fn with_body(done: Box<dyn Fn() + Send>, body: Body) -> Worker {
        let (tx, rx) = mpsc::channel::<(u64, Job)>();
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot::default()),
            cv: Condvar::new(),
            wanted: AtomicU64::new(0),
        });
        let theirs = shared.clone();
        let _ = std::thread::Builder::new()
            .name("hushmic-takeover".into())
            .spawn(move || {
                for (generation, job) in rx {
                    let s = &*theirs;
                    let outcome = body(
                        &job,
                        &mut SlotGate {
                            shared: s,
                            generation,
                        },
                    );
                    {
                        let mut slot = s.lock();
                        slot.writing = false;
                        slot.provisional = None;
                        slot.busy = false;
                        let outcome = if std::mem::take(&mut slot.settled) {
                            Outcome::Cancelled
                        } else {
                            outcome
                        };
                        slot.done = Some(Done {
                            generation,
                            outcome,
                        });
                        s.cv.notify_all();
                    }
                    done();
                }
            });
        Worker { tx, shared }
    }

    /// An attempt is out, or its outcome has not been taken yet.
    pub fn busy(&self) -> bool {
        let slot = self.shared.lock();
        slot.busy || slot.done.is_some()
    }

    /// Start an attempt for the chain spawned under `generation`. Callers
    /// check [`Worker::busy`] first: one attempt at a time.
    pub fn start(&self, generation: u64, job: Job) {
        self.shared.lock().busy = true;
        self.shared.wanted.store(generation, Ordering::Release);
        if self.tx.send((generation, job)).is_err() {
            self.shared.lock().busy = false;
        }
    }

    /// The finished attempt's outcome, once.
    pub fn take(&self) -> Option<Done> {
        self.shared.lock().done.take()
    }

    /// No attempt may write from now on. One that is sending its write is
    /// waited for (up to `limit`), and what it did is returned: its outcome,
    /// or while its read-back runs, the provisional one. The caller deals
    /// with that write (a restore goes out after it), so whatever the
    /// read-back then finds, the attempt ends as cancelled. One still
    /// reading, or at its breadcrumb, ends without writing, later.
    pub fn cancel(&self, limit: Duration) -> Option<Done> {
        let slot = self.shared.lock();
        self.shared.wanted.store(0, Ordering::Release);
        let mut slot = self
            .shared
            .cv
            .wait_timeout_while(slot, limit, |s| s.writing)
            .unwrap_or_else(|e| e.into_inner())
            .0;
        if let Some(done) = slot.done.take() {
            return Some(done);
        }
        let provisional = slot.provisional.take();
        slot.settled |= provisional.is_some();
        provisional
    }

    /// Wait (up to `limit`) for the attempt that is out and take its
    /// outcome (`--enable-once`, tests).
    pub fn wait(&self, limit: Duration) -> Option<Done> {
        let slot = self.shared.lock();
        let mut slot = self
            .shared
            .cv
            .wait_timeout_while(slot, limit, |s| s.busy && s.done.is_none())
            .unwrap_or_else(|e| e.into_inner())
            .0;
        slot.done.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured() -> Outcome {
        Outcome::Captured {
            prior: Some("usb_mic".into()),
            write: Write::Landed,
        }
    }

    #[test]
    fn a_teardown_waits_for_a_write_under_way_and_sees_what_it_did() {
        let (writing_tx, writing_rx) = mpsc::channel::<()>();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate_rx = Mutex::new(gate_rx);
        let w = Worker::with_body(
            Box::new(|| {}),
            Box::new(move |_, gate| {
                if !gate.may_write() {
                    return Outcome::Cancelled;
                }
                let _ = writing_tx.send(());
                let _ = gate_rx.lock().unwrap().recv();
                captured()
            }),
        );
        w.start(7, Job::Capture);
        writing_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let _ = gate_tx.send(());
        });
        let t = Instant::now();
        let done = w.cancel(Duration::from_secs(5));
        assert!(t.elapsed() >= Duration::from_millis(150), "did not wait");
        assert_eq!(
            done,
            Some(Done {
                generation: 7,
                outcome: captured()
            })
        );
        release.join().unwrap();
        assert!(!w.busy());
    }

    #[test]
    fn a_teardown_does_not_wait_for_the_read_back_of_a_sent_write() {
        let (sent_tx, sent_rx) = mpsc::channel::<()>();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate_rx = Mutex::new(gate_rx);
        let w = Worker::with_body(
            Box::new(|| {}),
            Box::new(move |_, gate| {
                if !gate.may_write() {
                    return Outcome::Cancelled;
                }
                gate.sent(Outcome::Captured {
                    prior: Some("usb_mic".into()),
                    write: Write::NoAnswer,
                });
                let _ = sent_tx.send(());
                // The read-back, until the gate opens.
                let _ = gate_rx.lock().unwrap().recv();
                captured()
            }),
        );
        w.start(5, Job::Capture);
        sent_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let t = Instant::now();
        let done = w.cancel(Duration::from_secs(5));
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "waited for the read-back"
        );
        assert_eq!(
            done,
            Some(Done {
                generation: 5,
                outcome: Outcome::Captured {
                    prior: Some("usb_mic".into()),
                    write: Write::NoAnswer
                }
            })
        );
        // What the read-back finds is for nobody: the teardown has dealt
        // with the write, and a restore of its own may be what it reads.
        gate_tx.send(()).unwrap();
        assert_eq!(
            w.wait(Duration::from_secs(5)),
            Some(Done {
                generation: 5,
                outcome: Outcome::Cancelled
            })
        );
        assert!(!w.busy());
    }

    #[test]
    fn an_attempt_still_reading_is_told_not_to_write() {
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate_rx = Mutex::new(gate_rx);
        let w = Worker::with_body(
            Box::new(|| {}),
            Box::new(move |_, gate| {
                // The read, until the gate opens.
                let _ = gate_rx.lock().unwrap().recv();
                if !gate.may_write() {
                    return Outcome::Cancelled;
                }
                captured()
            }),
        );
        w.start(3, Job::Capture);
        let t = Instant::now();
        assert_eq!(w.cancel(Duration::from_secs(5)), None);
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "a read is not waited for"
        );
        gate_tx.send(()).unwrap();
        assert_eq!(
            w.wait(Duration::from_secs(5)),
            Some(Done {
                generation: 3,
                outcome: Outcome::Cancelled
            })
        );
    }

    #[test]
    fn one_attempt_at_a_time_until_its_outcome_is_taken() {
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let w = Worker::with_body(
            Box::new(move || {
                let _ = done_tx.send(());
            }),
            Box::new(|job, _| match job {
                Job::Capture => Outcome::NoAnswer,
                Job::Retry { .. } => Outcome::Confirmed,
            }),
        );
        assert!(!w.busy());
        w.start(1, Job::Capture);
        assert!(w.busy());
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(w.busy(), "busy until the outcome is taken");
        let done = w.take().unwrap();
        assert_eq!(done.outcome, Outcome::NoAnswer);
        assert!(done.outcome.unanswered());
        assert!(!w.busy());
        assert_eq!(w.take(), None);
    }
}
