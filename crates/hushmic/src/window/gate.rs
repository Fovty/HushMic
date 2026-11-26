//! The first-start beam gate (the lead's ruling, round 2). The beam belongs
//! to the first start: it falls only when that start (from Starting or
//! Off) ends in filtering. A first start that ends in bypass or mute uses
//! it up, and so does opening the window on a running engine; it is used up
//! once per install (`ui.beam_used`, kept by the daemon). Later switch-ons
//! play the morph, Filtering → Raw the reverse, mute fades the call side,
//! unmute is instant.
//!
//! The gate only reacts to the state: it has no way to change it, so "never
//! force the state" holds by construction.

use crate::appstate::Vis;

/// What a state change plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Play {
    Nothing,
    /// The beam falls onto the Mark, then the morph and the lit edge.
    Beam,
    /// Unfiltered → filtered: the Mark's bars lie down into the line.
    Morph,
    /// Filtered → raw: the reverse.
    Unmorph,
    /// The call side's columns fade to silence.
    MuteFade,
}

#[derive(Debug)]
pub struct Gate {
    beam_used: bool,
    reduced_motion: bool,
    /// The light theme draws no additive light: no beam there.
    light: bool,
    last: Option<Vis>,
    /// The beam was used up and the daemon has not been told yet.
    newly_used: bool,
}

impl Gate {
    /// `beam_used` from the daemon's `ui.beam_used`.
    pub fn new(beam_used: bool) -> Self {
        Gate {
            beam_used,
            reduced_motion: false,
            light: false,
            last: None,
            newly_used: false,
        }
    }

    /// The desktop's motion setting and the theme, as they are now.
    pub fn set_look(&mut self, reduced_motion: bool, light: bool) {
        self.reduced_motion = reduced_motion;
        self.light = light;
    }

    fn use_up(&mut self) {
        if !self.beam_used {
            self.beam_used = true;
            self.newly_used = true;
        }
    }

    /// The window opened on an engine in `vis`.
    pub fn open(&mut self, vis: Vis) {
        self.last = Some(vis);
        if !matches!(vis, Vis::Starting | Vis::Off) {
            self.use_up();
        }
    }

    /// The state went from `prev` to `next`: what to play.
    pub fn change(&mut self, prev: Vis, next: Vis) -> Play {
        self.last = Some(next);
        let start = matches!(prev, Vis::Starting | Vis::Off);
        if start && !matches!(next, Vis::Starting | Vis::Off) {
            let beam =
                next == Vis::Filtering && !self.beam_used && !self.reduced_motion && !self.light;
            self.use_up();
            if beam {
                return Play::Beam;
            }
        }
        if self.reduced_motion {
            return Play::Nothing;
        }
        match (prev, next) {
            (Vis::Muted, Vis::Filtering) => Play::Nothing, // unmute is instant
            (Vis::Filtering, Vis::Filtering) => Play::Nothing,
            (_, Vis::Filtering) => Play::Morph,
            (Vis::Filtering, Vis::Raw) => Play::Unmorph,
            (Vis::Filtering | Vis::Raw, Vis::Muted) => Play::MuteFade,
            _ => Play::Nothing,
        }
    }

    /// Feed the state every frame: the first call opens, later ones play a
    /// change (Nothing while it holds).
    pub fn observe(&mut self, vis: Vis) -> Play {
        match self.last {
            None => {
                self.open(vis);
                Play::Nothing
            }
            Some(prev) if prev == vis => Play::Nothing,
            Some(prev) => self.change(prev, vis),
        }
    }

    pub fn beam_used(&self) -> bool {
        self.beam_used
    }

    /// True once after the beam was used up: send `Action::UiBeamUsed`.
    pub fn take_newly_used(&mut self) -> bool {
        std::mem::take(&mut self.newly_used)
    }
}

#[cfg(test)]
mod tests {
    use super::{Gate, Play};
    use crate::appstate::Vis::*;

    fn fresh() -> Gate {
        Gate::new(false)
    }

    #[test]
    fn the_first_start_into_filtering_plays_the_beam_once() {
        let mut g = fresh();
        g.open(Starting);
        assert_eq!(g.change(Starting, Filtering), Play::Beam);
        assert_eq!(g.change(Filtering, Raw), Play::Unmorph);
        assert_eq!(g.change(Raw, Filtering), Play::Morph);
        // off and on again later in the same install: the morph, not the beam
        assert_eq!(g.change(Filtering, Off), Play::Nothing);
        assert_eq!(g.change(Off, Starting), Play::Nothing);
        assert_eq!(g.change(Starting, Filtering), Play::Morph);
    }

    #[test]
    fn a_first_start_in_bypass_or_mute_stays_put_and_uses_the_beam_up() {
        for end in [Raw, Muted] {
            let mut g = fresh();
            g.open(Starting);
            assert_eq!(g.change(Starting, end), Play::Nothing);
            assert!(g.beam_used());
            let back = if end == Muted {
                Play::Nothing
            } else {
                Play::Morph
            };
            assert_eq!(g.change(end, Filtering), back, "{end:?}");
        }
    }

    #[test]
    fn opening_on_a_running_engine_uses_the_beam_up_but_off_does_not() {
        let mut g = fresh();
        g.open(Filtering);
        assert!(g.beam_used());
        let mut g = fresh();
        g.open(Off);
        assert!(!g.beam_used());
        assert_eq!(g.change(Off, Filtering), Play::Beam);
    }

    #[test]
    fn unmute_is_instant_and_mute_fades_the_call_side() {
        let mut g = fresh();
        g.open(Filtering);
        assert_eq!(g.change(Filtering, Muted), Play::MuteFade);
        assert_eq!(g.change(Muted, Filtering), Play::Nothing);
    }

    #[test]
    fn a_later_launch_never_shows_the_beam_again() {
        let mut g = Gate::new(true);
        g.open(Starting);
        assert_eq!(g.change(Starting, Filtering), Play::Morph);
    }

    #[test]
    fn reduced_motion_and_light_skip_the_beam_but_still_use_it_up() {
        let mut g = fresh();
        g.set_look(true, false);
        assert_eq!(g.change(Starting, Filtering), Play::Nothing);
        assert!(g.beam_used());
        let mut g = fresh();
        g.set_look(false, true);
        assert_eq!(g.change(Starting, Filtering), Play::Morph);
        assert!(g.beam_used());
    }

    #[test]
    fn observe_opens_then_plays_changes_and_reports_the_use_once() {
        let mut g = fresh();
        assert_eq!(g.observe(Starting), Play::Nothing);
        assert_eq!(g.observe(Starting), Play::Nothing);
        assert!(!g.take_newly_used());
        assert_eq!(g.observe(Filtering), Play::Beam);
        assert!(g.take_newly_used(), "tell the daemon");
        assert!(!g.take_newly_used(), "once");
        assert_eq!(g.observe(Filtering), Play::Nothing);
        // Already used when the window opened: nothing to report.
        let mut g = Gate::new(true);
        g.observe(Filtering);
        assert!(!g.take_newly_used());
    }

    /// The gate never asks for a state: it only answers with what to play
    /// (its API has no way to change one). A whole first run in bypass
    /// plays nothing and leaves the beam used.
    #[test]
    fn a_saved_bypass_start_never_turns_on_by_itself() {
        let mut g = fresh();
        assert_eq!(g.observe(Off), Play::Nothing);
        assert_eq!(g.observe(Starting), Play::Nothing);
        assert_eq!(g.observe(Raw), Play::Nothing);
        assert!(g.beam_used());
        // The user switches on later: the morph, never the beam.
        assert_eq!(g.observe(Filtering), Play::Morph);
    }
}
