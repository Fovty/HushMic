//! Writes the running chain's Mode and Attenuation Limit ports off the main
//! loop.
//!
//! The chain takes mode and strength from its control file the moment the
//! daemon stores them (`hushmic_shm::control`); the ports are written too,
//! best effort, so graph tools show the truth and a plugin that does not
//! honour the file (an older one) still follows. Writing a port takes a
//! pw-dump and a pw-cli, and both wait for the session manager, so they
//! run here: one thread, the newest wish only (a burst of push-to-talk
//! presses becomes one write), each call bounded.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How long one PipeWire call of a port write may take.
const CALL_BUDGET: Duration = Duration::from_secs(10);

/// What the ports of one chain should carry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Want {
    /// The chain generation the wish is for.
    pub generation: u64,
    pub mode: Option<u8>,
    pub attn_db: Option<f32>,
}

impl Want {
    /// `newer` on top of what is still pending: the same chain keeps the
    /// fields `newer` leaves alone; another chain starts over.
    fn merge(self, newer: Want) -> Want {
        if self.generation != newer.generation {
            return newer;
        }
        Want {
            generation: newer.generation,
            mode: newer.mode.or(self.mode),
            attn_db: newer.attn_db.or(self.attn_db),
        }
    }
}

/// A port write that did not land: the chain it was for.
pub type OnFailure = Box<dyn Fn(u64) + Send>;

/// What the running chain needs after one of its port writes did not land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missed {
    /// Its plugin takes the control file, which already holds the value.
    Nothing,
    /// Its plugin has not started yet and maps the control file when it
    /// does: look again later, never restart for it (an idle chain would
    /// restart on every write the session manager is too slow for).
    Wait,
    /// Write the ports again: the node may not have been up yet.
    Retry,
    /// The ports are the only way in and they did not take: a restart
    /// renders the values instead.
    Restart,
}

/// The verdict for a chain whose port write did not land. `honoured`: its
/// plugin set the control file's reader word. `has_file`: the chain was
/// given a control file. `ran`: evidence its plugin has run (a hop in its
/// meter, or the graph showing it running), so a reader word still unset
/// means a plugin that ignores the file. `starting`: within the spawn's
/// grace, when the node may not be there yet.
pub fn missed(honoured: bool, has_file: bool, ran: bool, starting: bool) -> Missed {
    if honoured {
        Missed::Nothing
    } else if has_file && !ran {
        Missed::Wait
    } else if starting {
        Missed::Retry
    } else {
        Missed::Restart
    }
}

/// The port the writer writes, and how (the default is pw-cli; tests swap
/// it). The third argument says whether the wish is still for the running
/// chain; a write checks it right before it lands.
type Write = Box<dyn Fn(&str, &str, &dyn Fn() -> bool) -> bool + Send>;

#[derive(Default)]
struct Pending {
    want: Option<Want>,
    quit: bool,
}

pub struct PortWriter {
    shared: Arc<(Mutex<Pending>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The generation of the chain that runs now (0: none), shared between
/// the controller (which sets it at every spawn and stop) and the writer
/// (which drops wishes for any other chain).
pub type Current = Arc<AtomicU64>;

impl PortWriter {
    pub fn spawn(on_failure: OnFailure, current: Current) -> PortWriter {
        PortWriter::with_write(
            on_failure,
            current,
            Box::new(|control, value, still| {
                crate::pipewire::set_chain_param_within(control, value, CALL_BUDGET, still)
            }),
        )
    }

    fn with_write(on_failure: OnFailure, current: Current, write: Write) -> PortWriter {
        let shared = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let theirs = shared.clone();
        let thread = std::thread::Builder::new()
            .name("hushmic-ports".into())
            .spawn(move || run(&theirs, &current, &write, &*on_failure))
            .ok();
        PortWriter { shared, thread }
    }

    /// Queue `want`; returns at once.
    pub fn write(&self, want: Want) {
        let (lock, cv) = &*self.shared;
        let mut p = lock.lock().unwrap_or_else(|e| e.into_inner());
        p.want = Some(match p.want {
            Some(old) => old.merge(want),
            None => want,
        });
        cv.notify_one();
    }
}

impl Drop for PortWriter {
    fn drop(&mut self) {
        let (lock, cv) = &*self.shared;
        lock.lock().unwrap_or_else(|e| e.into_inner()).quit = true;
        cv.notify_one();
        // Not joined: a write stuck in PipeWire must not hold up a quit;
        // the thread ends at its next look at `quit`.
        drop(self.thread.take());
    }
}

fn run(
    shared: &(Mutex<Pending>, Condvar),
    current: &AtomicU64,
    write: &Write,
    failed: &dyn Fn(u64),
) {
    let (lock, cv) = shared;
    loop {
        let want = {
            let mut p = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if p.quit {
                    return;
                }
                if let Some(w) = p.want.take() {
                    break w;
                }
                p = cv.wait(p).unwrap_or_else(|e| e.into_inner());
            }
        };
        // A wish for a chain that has been replaced is dropped: the new
        // chain was started with the current values, and an old wish
        // landing on its ports would undo a newer change.
        let still = || current.load(Ordering::Acquire) == want.generation;
        let mut ok = true;
        if let Some(m) = want.mode.filter(|_| still()) {
            ok &= write("Mode", &m.to_string(), &still);
        }
        if let Some(db) = want.attn_db.filter(|_| still()) {
            ok &= write(
                crate::pipewire::ATTN_CONTROL,
                &crate::pipewire::attn_value(db),
                &still,
            );
        }
        if !ok && still() {
            failed(want.generation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_pending_wish_takes_the_newest_values_of_its_chain() {
        let a = Want {
            generation: 3,
            mode: Some(2),
            attn_db: Some(12.0),
        };
        let b = Want {
            generation: 3,
            mode: Some(0),
            attn_db: None,
        };
        assert_eq!(
            a.merge(b),
            Want {
                generation: 3,
                mode: Some(0),
                attn_db: Some(12.0)
            }
        );
        let other = Want {
            generation: 4,
            mode: None,
            attn_db: Some(30.0),
        };
        assert_eq!(a.merge(other), other, "another chain starts over");
    }

    #[test]
    fn writes_coalesce_while_one_is_stuck_and_failures_name_their_chain() {
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate = Mutex::new(gate_rx);
        let (seen_tx, seen_rx) = mpsc::channel::<(String, String)>();
        let seen = Mutex::new(seen_tx);
        let (fail_tx, fail_rx) = mpsc::channel::<u64>();
        let fail = Mutex::new(fail_tx);
        let current: Current = Arc::new(AtomicU64::new(7));
        let w = PortWriter::with_write(
            Box::new(move |g| {
                let _ = fail.lock().unwrap().send(g);
            }),
            current.clone(),
            Box::new(move |control, value, still| {
                let _ = seen.lock().unwrap().send((control.into(), value.into()));
                // Held until the test lets it go: a write stuck in PipeWire.
                let _ = gate.lock().unwrap().recv();
                // A replaced chain: nothing lands, nothing to report.
                if !still() {
                    return true;
                }
                value != "1"
            }),
        );
        let mode = |generation, m| Want {
            generation,
            mode: Some(m),
            attn_db: None,
        };
        w.write(mode(7, 2));
        let first = seen_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first, ("Mode".into(), "2".into()));
        // While it is stuck: three wishes, of which only the last is written.
        w.write(mode(7, 0));
        w.write(mode(7, 2));
        w.write(mode(7, 1));
        gate_tx.send(()).unwrap();
        let next = seen_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(next, ("Mode".into(), "1".into()));
        gate_tx.send(()).unwrap();
        assert_eq!(fail_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 7);
        assert!(seen_rx.recv_timeout(Duration::from_millis(200)).is_err());
        // The chain is replaced while a write for it is under way, and a
        // wish for the old chain is queued: neither lands nor fails.
        w.write(mode(7, 2));
        assert_eq!(
            seen_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            ("Mode".into(), "2".into())
        );
        w.write(mode(7, 1));
        current.store(8, Ordering::Release);
        gate_tx.send(()).unwrap();
        assert!(
            seen_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the queued wish for chain 7 was written to chain 8"
        );
        assert!(fail_rx.recv_timeout(Duration::from_millis(300)).is_err());
        drop(w);
    }

    #[test]
    fn a_control_file_nobody_has_mapped_yet_waits_instead_of_restarting() {
        // The plugin maps the file when it first starts (its first link);
        // until then its reader word is unset. That alone proves nothing:
        // an idle chain must not restart for a write that did not land.
        assert_eq!(missed(false, true, false, false), Missed::Wait);
        assert_eq!(missed(false, true, false, true), Missed::Wait);
        // Mapped: the file already carries the value.
        assert_eq!(missed(true, true, true, false), Missed::Nothing);
        assert_eq!(missed(true, true, false, false), Missed::Nothing);
        // Its plugin has run and never took the file (an older plugin, or
        // one that could not map it): the ports are the only way in.
        assert_eq!(missed(false, true, true, false), Missed::Restart);
        assert_eq!(missed(false, true, true, true), Missed::Retry);
        // No control file at all: as before, retry while the chain starts,
        // restart after.
        assert_eq!(missed(false, false, false, true), Missed::Retry);
        assert_eq!(missed(false, false, false, false), Missed::Restart);
        assert_eq!(missed(false, false, true, false), Missed::Restart);
    }
}
