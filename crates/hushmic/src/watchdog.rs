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

/// How long a chosen microphone must stay gone (or back) before the chain
/// switches: two looks at least this far apart. USB re-enumeration flaps
/// for a moment; an unplug is told within a few seconds (UX panel round
/// 2, A5).
pub const MIC_SETTLE: Duration = Duration::from_millis(1500);
/// The least time between two automatic mic switches: a flapping device
/// degenerates to slow toggling, never a restart storm.
pub const MIC_COOLDOWN: Duration = Duration::from_secs(30);

/// Mic-recovery decision state machine — pure, fed on every graph read
/// with its time. `config.mic` is never touched; this only decides when
/// to restart the (healthy) chain because the *preferred* mic went away
/// or came back. Time, not ticks: the reads come at the 5 s tick and in
/// between (a device added or removed), and neither may hurry the
/// debounce or the cooldown.
pub struct Recovery {
    /// Since when the mic is seen gone (the chain on it) or back (the
    /// chain on the fallback), and which way.
    pending: Option<(Switch, std::time::Instant)>,
    /// The last switch, for the cooldown.
    last_switch: Option<std::time::Instant>,
}

impl Recovery {
    pub fn new() -> Self {
        Recovery {
            pending: None,
            last_switch: None,
        }
    }

    /// One read's facts at `now`:
    /// - `preferred_selected`: config names a mic at all
    /// - `active_is_preferred`: the running chain was rendered with it
    /// - `preferred_present`: the mic is in the snapshot; `None` = the
    ///   probe failed — freezes (unknown is not gone)
    /// - `in_grace`: chain freshly spawned; don't judge it yet
    ///
    /// Returns the switch to execute: the mic gone (or back) on looks at
    /// least [`MIC_SETTLE`] apart, and [`MIC_COOLDOWN`] after the last one.
    pub fn observe(
        &mut self,
        preferred_selected: bool,
        active_is_preferred: bool,
        preferred_present: Option<bool>,
        in_grace: bool,
        now: std::time::Instant,
    ) -> Option<Switch> {
        if !preferred_selected || in_grace {
            self.pending = None;
            return None;
        }
        // Probe failed: unknown is not gone — freeze, never reset.
        let present = preferred_present?;
        let want = match (active_is_preferred, present) {
            // Chain on the preferred mic, mic gone: toward the fallback.
            (true, false) => Switch::Fallback,
            // Chain on the fallback, mic back: toward the return.
            (false, true) => Switch::Return,
            // Consistent state: nothing brewing in either direction.
            _ => {
                self.pending = None;
                return None;
            }
        };
        let since = match self.pending {
            Some((w, t)) if w == want => t,
            _ => {
                self.pending = Some((want, now));
                return None;
            }
        };
        let cooled = self
            .last_switch
            .is_none_or(|t| now.saturating_duration_since(t) >= MIC_COOLDOWN);
        if now.saturating_duration_since(since) >= MIC_SETTLE && cooled {
            self.pending = None;
            self.last_switch = Some(now);
            return Some(want);
        }
        None
    }

    /// When the next look decides a pending switch (the caller reads the
    /// graph then); None with nothing pending.
    pub fn decides_at(&self) -> Option<std::time::Instant> {
        let (_, since) = self.pending?;
        let settled = since + MIC_SETTLE;
        Some(match self.last_switch {
            Some(t) => settled.max(t + MIC_COOLDOWN),
            None => settled,
        })
    }
}

impl Default for Recovery {
    fn default() -> Self {
        Self::new()
    }
}

/// Longest wait between settings restarts while the default keeps moving
/// (the cooldown doubles from [`SWITCH_COOLDOWN_TICKS`] up to this: 5 min).
const PROFILE_COOLDOWN_MAX_TICKS: u32 = 60;
/// Ticks with the chain on the device it follows (no change pending)
/// after which a running cooldown is over and the next one is back to its
/// base length (2 min).
const PROFILE_QUIET_TICKS: u32 = 24;

/// Settings restarts toward one device that keep resolving another one
/// before [`ProfileFollow`] stops trying (a device the tick sees but a
/// start cannot land on, e.g. one WirePlumber will not make effective).
const PROFILE_MAX_MISSES: u32 = 3;

/// What a follow-default chain should do about the device it captures
/// from (see [`ProfileFollow`]).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ProfileAction {
    /// Nothing to do (or not yet: debounce, cooldown, unknown).
    Keep,
    /// The chain follows another device whose settings equal the running
    /// ones: no restart, the settings just belong to that device now.
    Retarget,
    /// The chain follows another device with other settings: restart.
    Restart,
    /// Restarts toward this device keep landing elsewhere: stop, keep the
    /// running device (say so once). Re-armed when the device changes.
    GiveUp,
}

/// One tick of facts for [`ProfileFollow::observe`].
#[derive(Debug, Clone, Copy)]
pub struct ProfileFacts<'a> {
    /// A running chain follows the default source: enabled, not down, not
    /// pinned to a mic. False resets the machine.
    pub following: bool,
    /// The chain was just (re)spawned: don't judge it yet.
    pub in_grace: bool,
    /// The device whose settings the running chain was rendered with, as
    /// ITS resolution saw it (None = the defaults, no device).
    pub running: Option<&'a str>,
    /// The device the chain captures from now; outer None = unknown (no
    /// snapshot, no usable default) — freezes, never counts as a change.
    pub observed: Option<Option<&'a str>>,
    /// `observed`'s settings equal the running chain's.
    pub same_settings: bool,
}

/// Settings-follow decision state machine — pure, fed once per watchdog
/// tick. A chain that follows the default source re-links on its own when
/// the default moves, but its model and strength were rendered for the
/// device it started on. A changed device must be seen on
/// [`DEBOUNCE_TICKS`] consecutive ticks; restarts are then spaced by a
/// cooldown that doubles while the default keeps flapping (autosuspend,
/// a headset switching profiles, EasyEffects coming and going) and is
/// cleared once things have been quiet for two minutes. Every comparison is against what the
/// running chain resolved, so a restart that resolved another device than
/// the tick saw is judged again later, at the next cooldown, and given up
/// after [`PROFILE_MAX_MISSES`] such restarts — never an endless loop.
#[derive(Debug)]
pub struct ProfileFollow {
    pending: Option<Option<String>>,
    seen: u32,
    cooldown: u32,
    next_cooldown: u32,
    quiet: u32,
    /// The device the last restarts aimed at, how many of them in a row
    /// did not land there, and the device given up on.
    target: Option<Option<String>>,
    misses: u32,
    gave_up: Option<Option<String>>,
}

impl ProfileFollow {
    pub fn new() -> Self {
        ProfileFollow {
            pending: None,
            seen: 0,
            cooldown: 0,
            next_cooldown: SWITCH_COOLDOWN_TICKS,
            quiet: 0,
            target: None,
            misses: 0,
            gave_up: None,
        }
    }

    pub fn observe(&mut self, f: ProfileFacts) -> ProfileAction {
        // Time, not evidence: advances on every tick.
        self.cooldown = self.cooldown.saturating_sub(1);
        self.quiet = self.quiet.saturating_add(1);
        if self.quiet >= PROFILE_QUIET_TICKS {
            // Settled for a while: the next change is news, not a flap.
            self.next_cooldown = SWITCH_COOLDOWN_TICKS;
            self.cooldown = 0;
        }
        if !f.following || f.in_grace {
            self.pending = None;
            self.seen = 0;
            return ProfileAction::Keep;
        }
        // Unknown is not a change: freeze.
        let Some(observed) = f.observed else {
            return ProfileAction::Keep;
        };
        if observed == f.running {
            // Landed (or never left): whatever was given up is moot.
            self.pending = None;
            self.seen = 0;
            self.target = None;
            self.misses = 0;
            self.gave_up = None;
            return ProfileAction::Keep;
        }
        let observed = observed.map(str::to_string);
        if self.gave_up.as_ref() == Some(&observed) {
            return ProfileAction::Keep;
        }
        self.gave_up = None;
        // Any disagreement is activity: only a stretch of ticks without
        // one brings the cooldown back to its base.
        self.quiet = 0;
        if self.pending.as_ref() == Some(&observed) {
            self.seen += 1;
        } else {
            self.pending = Some(observed.clone());
            self.seen = 1;
        }
        if self.seen < DEBOUNCE_TICKS {
            return ProfileAction::Keep;
        }
        if f.same_settings {
            self.pending = None;
            self.seen = 0;
            return ProfileAction::Retarget;
        }
        if self.cooldown > 0 {
            // Stays pending: fires once the cooldown is over if the
            // device is still the same.
            return ProfileAction::Keep;
        }
        self.pending = None;
        self.seen = 0;
        if self.target.as_ref() == Some(&observed) {
            if self.misses >= PROFILE_MAX_MISSES {
                self.gave_up = Some(observed);
                self.target = None;
                self.misses = 0;
                return ProfileAction::GiveUp;
            }
            self.misses += 1;
        } else {
            self.target = Some(observed);
            self.misses = 1;
        }
        self.cooldown = self.next_cooldown;
        self.next_cooldown = (self.next_cooldown * 2).min(PROFILE_COOLDOWN_MAX_TICKS);
        ProfileAction::Restart
    }
}

/// The user switched the system default away from our node while this run
/// holds it ("Set as default microphone"): the configured default names a
/// real device on [`DEBOUNCE_TICKS`] consecutive ticks. Pure; the caller
/// then releases the takeover so the switch sticks.
#[derive(Debug, Default)]
pub struct DefaultRelease {
    seen: u32,
}

impl DefaultRelease {
    /// `taken`: this run holds the default; `moved`: the configured
    /// default is a real device (not ours); `in_grace`: the chain was just
    /// (re)spawned. True = release now.
    pub fn observe(&mut self, taken: bool, moved: bool, in_grace: bool) -> bool {
        if !taken || !moved || in_grace {
            self.seen = 0;
            return false;
        }
        self.seen += 1;
        if self.seen >= DEBOUNCE_TICKS {
            self.seen = 0;
            return true;
        }
        false
    }
}

impl Default for ProfileFollow {
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
    // Shorthand: chain healthy, a mic is selected, no grace, at t s.
    fn at(t: f64) -> std::time::Instant {
        use std::sync::OnceLock;
        static T0: OnceLock<std::time::Instant> = OnceLock::new();
        *T0.get_or_init(std::time::Instant::now) + Duration::from_secs_f64(t)
    }
    fn absent(r: &mut Recovery, t: f64) -> Option<Switch> {
        r.observe(true, true, Some(false), false, at(t))
    }
    fn back(r: &mut Recovery, t: f64) -> Option<Switch> {
        r.observe(true, false, Some(true), false, at(t))
    }

    #[test]
    fn fallback_fires_once_the_mic_stays_gone() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r, 0.0), None); // the first look: debounce
        assert_eq!(r.decides_at(), Some(at(1.5)));
        assert_eq!(absent(&mut r, 1.0), None, "a moment is a flap");
        assert_eq!(absent(&mut r, 1.5), Some(Switch::Fallback));
        assert_eq!(r.decides_at(), None);
    }

    #[test]
    fn return_fires_once_the_mic_stays_back() {
        let mut r = Recovery::new();
        assert_eq!(back(&mut r, 0.0), None);
        assert_eq!(back(&mut r, 1.6), Some(Switch::Return));
    }

    #[test]
    fn consistent_state_resets_the_count() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r, 0.0), None);
        // mic observed present again while chain is on it: all is well
        assert_eq!(r.observe(true, true, Some(true), false, at(1.0)), None);
        assert_eq!(absent(&mut r, 2.0), None); // count restarted
        assert_eq!(absent(&mut r, 3.0), None);
        assert_eq!(absent(&mut r, 3.5), Some(Switch::Fallback));
    }

    #[test]
    fn probe_failure_freezes_but_does_not_reset() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r, 0.0), None);
        assert_eq!(r.observe(true, true, None, false, at(1.0)), None); // frozen
        assert_eq!(absent(&mut r, 2.0), Some(Switch::Fallback));
    }

    #[test]
    fn no_decision_without_a_preferred_mic() {
        let mut r = Recovery::new();
        for i in 0..10 {
            assert_eq!(
                r.observe(false, false, Some(true), false, at(i as f64)),
                None
            );
        }
        // and it also resets accumulated state
        assert_eq!(absent(&mut r, 10.0), None);
        assert_eq!(r.observe(false, true, Some(false), false, at(11.0)), None);
        assert_eq!(absent(&mut r, 12.0), None);
        assert_eq!(absent(&mut r, 14.0), Some(Switch::Fallback));
    }

    #[test]
    fn startup_grace_resets_and_blocks() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r, 0.0), None);
        assert_eq!(r.observe(true, true, Some(false), true, at(2.0)), None); // grace
        assert_eq!(absent(&mut r, 3.0), None); // count restarted
        assert_eq!(absent(&mut r, 5.0), Some(Switch::Fallback));
    }

    #[test]
    fn direction_flip_resets_the_count() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r, 0.0), None);
        // chain now on fallback and mic present: opposite direction
        assert_eq!(back(&mut r, 2.0), None);
        assert_eq!(back(&mut r, 4.0), Some(Switch::Return));
    }

    #[test]
    fn cooldown_gates_the_next_switch() {
        let mut r = Recovery::new();
        assert_eq!(absent(&mut r, 0.0), None);
        assert_eq!(absent(&mut r, 2.0), Some(Switch::Fallback)); // cooldown starts
                                                                 // The mic comes straight back: the debounce is satisfied quickly,
                                                                 // but the switch waits out the cooldown, however many looks come.
        assert_eq!(back(&mut r, 3.0), None);
        assert_eq!(r.decides_at(), Some(at(32.0)));
        for i in 0..50 {
            assert_eq!(back(&mut r, 5.0 + i as f64 * 0.5), None);
        }
        assert_eq!(back(&mut r, 32.0), Some(Switch::Return));
        // and the next flip is gated again
        assert_eq!(absent(&mut r, 33.0), None);
        assert_eq!(absent(&mut r, 61.9), None);
        assert_eq!(absent(&mut r, 62.0), Some(Switch::Fallback));
    }

    // --- ProfileFollow ------------------------------------------------
    fn facts<'a>(
        running: Option<&'a str>,
        observed: Option<&'a str>,
        same: bool,
    ) -> ProfileFacts<'a> {
        ProfileFacts {
            following: true,
            in_grace: false,
            running,
            observed: Some(observed),
            same_settings: same,
        }
    }

    #[test]
    fn a_new_default_restarts_after_the_debounce() {
        let mut p = ProfileFollow::new();
        let f = facts(Some("rode"), Some("jabra"), false);
        assert_eq!(p.observe(f), ProfileAction::Keep);
        assert_eq!(p.observe(f), ProfileAction::Restart);
        // The restart resolved the Jabra: settled.
        assert_eq!(
            p.observe(facts(Some("jabra"), Some("jabra"), true)),
            ProfileAction::Keep
        );
    }

    #[test]
    fn equal_settings_retarget_without_a_restart_or_cooldown() {
        let mut p = ProfileFollow::new();
        let f = facts(Some("jabra"), Some("webcam"), true);
        assert_eq!(p.observe(f), ProfileAction::Keep);
        assert_eq!(p.observe(f), ProfileAction::Retarget);
        // A real change right after is not held back by a cooldown.
        let g = facts(Some("webcam"), Some("rode"), false);
        assert_eq!(p.observe(g), ProfileAction::Keep);
        assert_eq!(p.observe(g), ProfileAction::Restart);
    }

    #[test]
    fn a_one_tick_blip_never_restarts() {
        let mut p = ProfileFollow::new();
        for _ in 0..10 {
            assert_eq!(
                p.observe(facts(Some("rode"), Some("jabra"), false)),
                ProfileAction::Keep
            );
            assert_eq!(
                p.observe(facts(Some("rode"), Some("rode"), true)),
                ProfileAction::Keep
            );
        }
    }

    #[test]
    fn changing_candidates_restart_the_count() {
        let mut p = ProfileFollow::new();
        assert_eq!(
            p.observe(facts(Some("rode"), Some("a"), false)),
            ProfileAction::Keep
        );
        assert_eq!(
            p.observe(facts(Some("rode"), Some("b"), false)),
            ProfileAction::Keep
        );
        assert_eq!(
            p.observe(facts(Some("rode"), Some("b"), false)),
            ProfileAction::Restart
        );
    }

    #[test]
    fn unknown_freezes_and_not_following_resets() {
        let mut p = ProfileFollow::new();
        let f = facts(Some("rode"), Some("jabra"), false);
        assert_eq!(p.observe(f), ProfileAction::Keep);
        let unknown = ProfileFacts {
            observed: None,
            ..f
        };
        assert_eq!(p.observe(unknown), ProfileAction::Keep);
        assert_eq!(p.observe(f), ProfileAction::Restart, "the count survived");
        // Pinned / off / down, and a fresh spawn: the count starts over.
        let mut p = ProfileFollow::new();
        for reset in [
            ProfileFacts {
                following: false,
                ..f
            },
            ProfileFacts {
                in_grace: true,
                ..f
            },
        ] {
            assert_eq!(p.observe(f), ProfileAction::Keep);
            assert_eq!(p.observe(reset), ProfileAction::Keep);
        }
        assert_eq!(p.observe(f), ProfileAction::Keep);
        assert_eq!(p.observe(f), ProfileAction::Restart);
    }

    #[test]
    fn the_defaults_count_as_a_device() {
        // Started while the default was unknown (defaults), then a device
        // with a profile shows up — and the way back.
        let mut p = ProfileFollow::new();
        let f = facts(None, Some("rode"), false);
        p.observe(f);
        assert_eq!(p.observe(f), ProfileAction::Restart);
    }

    /// Returns the ticks at which restarts fired.
    fn flap(p: &mut ProfileFollow, ticks: u32) -> Vec<u32> {
        // The default alternates every 2 ticks (just long enough to pass
        // the debounce); every restart lands on what it saw.
        let mut running = "x";
        let mut fired = vec![];
        for t in 0..ticks {
            let observed = if (t / 2) % 2 == 0 { "y" } else { "x" };
            if p.observe(facts(Some(running), Some(observed), false)) == ProfileAction::Restart {
                running = observed;
                fired.push(t);
            }
        }
        fired
    }

    #[test]
    fn a_flapping_default_backs_off_then_recovers() {
        let mut p = ProfileFollow::new();
        let fired = flap(&mut p, 200);
        // 1000 s of flapping: a handful of restarts, not one per change.
        assert!(fired.len() <= 6, "{fired:?}");
        let gaps: Vec<u32> = fired.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.iter().all(|g| *g >= SWITCH_COOLDOWN_TICKS), "{gaps:?}");
        assert!(gaps.windows(2).all(|w| w[1] >= w[0]), "doubling: {gaps:?}");
        assert!(
            gaps.iter().all(|g| *g <= PROFILE_COOLDOWN_MAX_TICKS + 4),
            "{gaps:?}"
        );
        // Quiet long enough: no cooldown left, and the next one is back
        // to its base.
        for _ in 0..PROFILE_QUIET_TICKS {
            p.observe(facts(Some("x"), Some("x"), true));
        }
        assert_eq!(p.cooldown, 0);
        let f = facts(Some("x"), Some("y"), false);
        p.observe(f);
        assert_eq!(p.observe(f), ProfileAction::Restart);
        let g = facts(Some("y"), Some("x"), false);
        let mut waited = 0;
        while p.observe(g) != ProfileAction::Restart {
            waited += 1;
        }
        assert_eq!(waited + 1, SWITCH_COOLDOWN_TICKS);
    }

    #[test]
    fn a_resolution_mismatch_backs_off_then_gives_up() {
        // The restart keeps resolving "x" although the tick sees "y".
        let mut p = ProfileFollow::new();
        let f = facts(Some("x"), Some("y"), false);
        let acts: Vec<(u32, ProfileAction)> = (0..400)
            .map(|t| (t, p.observe(f)))
            .filter(|(_, a)| *a != ProfileAction::Keep)
            .collect();
        // Base, then doubling gaps; the fourth attempt gives up instead,
        // once, and nothing follows for as long as nothing changes.
        assert_eq!(
            acts,
            vec![
                (1, ProfileAction::Restart),
                (7, ProfileAction::Restart),
                (19, ProfileAction::Restart),
                (43, ProfileAction::GiveUp),
            ]
        );
        // Another device is news again…
        let g = facts(Some("x"), Some("z"), false);
        p.observe(g);
        assert_eq!(p.observe(g), ProfileAction::Restart);
        // …and so is "y" once it has been something else in between.
        let mut p = ProfileFollow::new();
        for _ in 0..50 {
            p.observe(f);
        }
        p.observe(facts(Some("x"), Some("x"), true));
        for _ in 0..PROFILE_QUIET_TICKS {
            p.observe(facts(Some("x"), Some("x"), true));
        }
        p.observe(f);
        assert_eq!(p.observe(f), ProfileAction::Restart);
    }

    /// The tick's snapshot predates a takeover that completes later in
    /// the same tick: that tick reports `taken` false, so a stale default
    /// in the snapshot never counts toward a release.
    #[test]
    fn the_takeover_tick_does_not_count() {
        let mut r = DefaultRelease::default();
        // Takeover tick (snapshot still shows the old default).
        assert!(!r.observe(false, true, false));
        // Next tick: the default is ours in the snapshot.
        assert!(!r.observe(true, false, false));
        // A stale snapshot on the takeover tick plus one real sighting is
        // only one: two held ticks are needed.
        let mut r = DefaultRelease::default();
        assert!(!r.observe(false, true, false));
        assert!(!r.observe(true, true, false));
        assert!(r.observe(true, true, false));
    }

    #[test]
    fn a_switch_away_releases_after_the_debounce() {
        let mut r = DefaultRelease::default();
        assert!(!r.observe(true, true, false));
        assert!(r.observe(true, true, false));
        // A one-tick blip, not holding the default, a fresh spawn: no.
        let mut r = DefaultRelease::default();
        for reset in [
            (true, false, false),
            (false, true, false),
            (true, true, true),
        ] {
            assert!(!r.observe(true, true, false));
            assert!(!r.observe(reset.0, reset.1, reset.2));
        }
        assert!(!r.observe(true, true, false));
        assert!(r.observe(true, true, false));
    }
}
