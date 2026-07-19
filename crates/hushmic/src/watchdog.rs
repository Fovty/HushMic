use std::sync::mpsc::Sender;
use std::time::Duration;

/// A single watchdog beat. The main loop re-checks the child/node on each one.
pub struct Tick;

/// Emits a [`Tick`] roughly every `secs` so the main loop can re-check the
/// child/node and re-instantiate it if it died (suspend / daemon restart).
///
/// The thread exits cleanly once the receiver is dropped (main loop ended).
pub fn spawn(tx: Sender<Tick>, secs: u64) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(secs));
        if tx.send(Tick).is_err() {
            break;
        }
    });
}

/// Exponential backoff for watchdog re-enable attempts (ticks: 0,1,3,7,15,31, cap 60).
pub struct Backoff {
    fails: u32,
    waited: u32,
}
impl Backoff {
    pub fn new() -> Self {
        Self {
            fails: 0,
            waited: 0,
        }
    }
    fn delay(&self) -> u32 {
        if self.fails == 0 {
            0
        } else {
            ((1u32 << self.fails.min(6)) - 1).min(60)
        }
    }
    /// Call once per tick when a re-enable is warranted; true => attempt now.
    pub fn should_attempt(&mut self) -> bool {
        if self.waited >= self.delay() {
            true
        } else {
            self.waited += 1;
            false
        }
    }
    /// Record the attempt outcome (resets the wait counter).
    pub fn record(&mut self, success: bool) {
        self.waited = 0;
        if success {
            self.fails = 0;
        } else {
            self.fails = self.fails.saturating_add(1);
        }
    }
}
impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// Consecutive definitive ticks required before an automatic switch —
/// USB re-enumeration flaps, and a chain restart is an audible gap.
const DEBOUNCE_TICKS: u32 = 2;
/// Minimum ticks between automatic switches (6 × 5 s tick = 30 s): a
/// flapping device degenerates to slow toggling, never a restart storm.
const SWITCH_COOLDOWN_TICKS: u32 = 6;

/// An automatic chain switch the main loop should execute (via the normal
/// `enable()`, whose mic resolution produces the right conf either way).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Switch {
    /// The preferred mic disappeared: restart to follow the system default.
    Fallback,
    /// The preferred mic is back: restart onto it.
    Return,
}

/// Mic-recovery decision state machine — pure, fed once per watchdog tick.
/// `config.mic` is never touched; this only decides when to restart the
/// (healthy) chain because the *preferred* mic went away or came back.
pub struct Recovery {
    absent_ticks: u32,
    present_ticks: u32,
    cooldown: u32,
}

impl Recovery {
    pub fn new() -> Self {
        Recovery {
            absent_ticks: 0,
            present_ticks: 0,
            cooldown: 0,
        }
    }

    /// One tick of facts:
    /// - `preferred_selected`: config names a mic at all
    /// - `active_is_preferred`: the running chain was rendered with it
    /// - `preferred_present`: the mic is in the snapshot; `None` = the
    ///   probe failed — freezes the counters (unknown is not gone)
    /// - `in_grace`: chain freshly spawned; don't judge it yet
    ///
    /// Returns the switch to execute (internally resets and starts the
    /// cooldown when it fires).
    pub fn observe(
        &mut self,
        preferred_selected: bool,
        active_is_preferred: bool,
        preferred_present: Option<bool>,
        in_grace: bool,
    ) -> Option<Switch> {
        // The cooldown is time, not evidence: it advances on every tick,
        // including frozen and grace ones.
        if self.cooldown > 0 {
            self.cooldown -= 1;
        }
        if !preferred_selected || in_grace {
            self.absent_ticks = 0;
            self.present_ticks = 0;
            return None;
        }
        // Probe failed: unknown is not gone — freeze, never reset.
        let present = preferred_present?;
        match (active_is_preferred, present) {
            // Chain on the preferred mic, mic gone: count toward fallback.
            (true, false) => {
                self.present_ticks = 0;
                self.absent_ticks += 1;
                if self.absent_ticks >= DEBOUNCE_TICKS && self.cooldown == 0 {
                    self.absent_ticks = 0;
                    self.cooldown = SWITCH_COOLDOWN_TICKS;
                    return Some(Switch::Fallback);
                }
            }
            // Chain on the fallback, mic back: count toward return.
            (false, true) => {
                self.absent_ticks = 0;
                self.present_ticks += 1;
                if self.present_ticks >= DEBOUNCE_TICKS && self.cooldown == 0 {
                    self.present_ticks = 0;
                    self.cooldown = SWITCH_COOLDOWN_TICKS;
                    return Some(Switch::Return);
                }
            }
            // Consistent state: nothing brewing in either direction.
            _ => {
                self.absent_ticks = 0;
                self.present_ticks = 0;
            }
        }
        None
    }
}

impl Default for Recovery {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_schedule() {
        let mut b = Backoff::new();
        assert!(b.should_attempt()); // fails=0 -> immediate
        b.record(false); // fail #1 -> delay 1 tick
        assert!(!b.should_attempt()); // wait
        assert!(b.should_attempt()); // then attempt
        b.record(false); // fail #2 -> delay 3 ticks
        assert!(!b.should_attempt());
        assert!(!b.should_attempt());
        assert!(!b.should_attempt());
        assert!(b.should_attempt());
        b.record(true); // success resets
        assert!(b.should_attempt()); // back to immediate
    }

    // --- Recovery state machine ---------------------------------------
    // Shorthand: chain healthy, a mic is selected, no grace.
    fn absent(r: &mut Recovery) -> Option<Switch> {
        r.observe(true, true, Some(false), false)
    }
    fn back(r: &mut Recovery) -> Option<Switch> {
        r.observe(true, false, Some(true), false)
    }

    #[test]
    fn fallback_fires_after_two_definitive_absent_ticks() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r), None); // 1st: debounce
        assert_eq!(absent(&mut r), Some(Switch::Fallback));
    }

    #[test]
    fn return_fires_after_two_definitive_present_ticks() {
        let mut r = Recovery::new();
        assert_eq!(back(&mut r), None);
        assert_eq!(back(&mut r), Some(Switch::Return));
    }

    #[test]
    fn consistent_state_resets_the_count() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r), None);
        // mic observed present again while chain is on it: all is well
        assert_eq!(r.observe(true, true, Some(true), false), None);
        assert_eq!(absent(&mut r), None); // count restarted
        assert_eq!(absent(&mut r), Some(Switch::Fallback));
    }

    #[test]
    fn probe_failure_freezes_but_does_not_reset() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r), None);
        assert_eq!(r.observe(true, true, None, false), None); // frozen
        assert_eq!(absent(&mut r), Some(Switch::Fallback)); // 2nd definitive
    }

    #[test]
    fn no_decision_without_a_preferred_mic() {
        let mut r = Recovery::new();
        for _ in 0..10 {
            assert_eq!(r.observe(false, false, Some(true), false), None);
        }
        // and it also resets accumulated state
        assert_eq!(absent(&mut r), None);
        assert_eq!(r.observe(false, true, Some(false), false), None);
        assert_eq!(absent(&mut r), None);
        assert_eq!(absent(&mut r), Some(Switch::Fallback));
    }

    #[test]
    fn startup_grace_resets_and_blocks() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r), None);
        assert_eq!(r.observe(true, true, Some(false), true), None); // grace
        assert_eq!(absent(&mut r), None); // count restarted
        assert_eq!(absent(&mut r), Some(Switch::Fallback));
    }

    #[test]
    fn direction_flip_resets_the_count() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r), None);
        // chain now on fallback and mic present: opposite direction
        assert_eq!(back(&mut r), None);
        assert_eq!(back(&mut r), Some(Switch::Return));
    }

    #[test]
    fn cooldown_gates_the_next_switch_to_six_ticks() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r), None);
        assert_eq!(absent(&mut r), Some(Switch::Fallback)); // cooldown starts
                                                            // The mic comes straight back: debounce is satisfied quickly, but
                                                            // the switch must wait out the cooldown window.
        let mut fired_at = None;
        for i in 1..=SWITCH_COOLDOWN_TICKS + 2 {
            if back(&mut r) == Some(Switch::Return) {
                fired_at = Some(i);
                break;
            }
        }
        assert_eq!(fired_at, Some(SWITCH_COOLDOWN_TICKS));
    }

    #[test]
    fn each_switch_rearms_the_cooldown() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r), None);
        assert_eq!(absent(&mut r), Some(Switch::Fallback));
        for _ in 0..SWITCH_COOLDOWN_TICKS - 1 {
            assert_eq!(back(&mut r), None);
        }
        assert_eq!(back(&mut r), Some(Switch::Return));
        // and the third flip is gated again
        for _ in 0..SWITCH_COOLDOWN_TICKS - 1 {
            assert_eq!(absent(&mut r), None);
        }
        assert_eq!(absent(&mut r), Some(Switch::Fallback));
    }
}
