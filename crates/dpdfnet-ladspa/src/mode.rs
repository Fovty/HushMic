//! Runtime mode (process / bypass / mute) and the mute gain ramp.

use crate::stft::HOP;

/// Samples the mute gain ramp spans: one hop = 10 ms @48 kHz.
pub const MUTE_RAMP_SAMPLES: usize = HOP;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Process,
    Bypass,
    Mute,
}

impl Mode {
    /// LADSPA control value -> mode: round to nearest, clamp to 0..=2.
    /// Non-finite values fall back to Process (the safe default).
    pub fn from_control(v: f32) -> Mode {
        if !v.is_finite() {
            return Mode::Process;
        }
        match v.round().clamp(0.0, 2.0) as u8 {
            1 => Mode::Bypass,
            2 => Mode::Mute,
            _ => Mode::Process,
        }
    }
}

/// Time-domain output gain with a linear per-sample ramp. The first target
/// ever set snaps instantly (a chain born muted must not leak even a faded
/// hop); later changes ramp over `MUTE_RAMP_SAMPLES`.
pub struct GainRamp {
    gain: f32,
    target: f32,
    primed: bool,
}

impl GainRamp {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        GainRamp {
            gain: 1.0,
            target: 1.0,
            primed: false,
        }
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.target = if muted { 0.0 } else { 1.0 };
        if !self.primed {
            self.gain = self.target;
            self.primed = true;
        }
    }

    /// Reset to the unprimed state (fresh session): the next `set_muted`
    /// snaps again.
    pub fn reset(&mut self) {
        self.gain = 1.0;
        self.target = 1.0;
        self.primed = false;
    }

    /// Apply the gain to a hop, stepping toward the target per sample.
    pub fn process(&mut self, hop: &mut [f32]) {
        const STEP: f32 = 1.0 / MUTE_RAMP_SAMPLES as f32;
        for s in hop.iter_mut() {
            if self.gain != self.target {
                let d = self.target - self.gain;
                if d.abs() <= STEP {
                    self.gain = self.target; // land exactly, then hold
                } else {
                    self.gain += STEP * d.signum();
                }
            }
            *s *= self.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_control_rounds_and_clamps() {
        assert_eq!(Mode::from_control(0.0), Mode::Process);
        assert_eq!(Mode::from_control(0.4), Mode::Process);
        assert_eq!(Mode::from_control(0.6), Mode::Bypass);
        assert_eq!(Mode::from_control(1.0), Mode::Bypass);
        assert_eq!(Mode::from_control(2.0), Mode::Mute);
        assert_eq!(Mode::from_control(2.7), Mode::Mute);
        assert_eq!(Mode::from_control(-3.0), Mode::Process);
        assert_eq!(Mode::from_control(7.0), Mode::Mute);
        assert_eq!(Mode::from_control(f32::NAN), Mode::Process);
    }

    #[test]
    fn first_set_snaps_born_muted_leaks_nothing() {
        // Privacy invariant: a chain spawned with Mode=2 in its conf must
        // emit exact zeros from the very first sample - no fade-out of real
        // mic audio.
        let mut g = GainRamp::new();
        g.set_muted(true);
        let mut hop = [1.0f32; MUTE_RAMP_SAMPLES];
        g.process(&mut hop);
        assert!(
            hop.iter().all(|&s| s == 0.0),
            "born-muted hop must be silent"
        );
    }

    #[test]
    fn later_mute_ramps_then_holds_exact_zero() {
        let mut g = GainRamp::new();
        g.set_muted(false); // primes at unity
        let mut hop = [1.0f32; MUTE_RAMP_SAMPLES];
        g.process(&mut hop);
        assert!(hop.iter().all(|&s| s == 1.0), "unmuted passes through");

        g.set_muted(true);
        let mut ramp = [1.0f32; MUTE_RAMP_SAMPLES];
        g.process(&mut ramp);
        // monotone non-increasing fade, bounded per-sample step
        for w in ramp.windows(2) {
            assert!(w[1] <= w[0] + 1e-6, "fade must be monotone");
            assert!(
                (w[0] - w[1]).abs() <= 1.5 / MUTE_RAMP_SAMPLES as f32,
                "per-sample step bound exceeded"
            );
        }
        // after the ramp budget: exactly zero, and stays there
        let mut next = [1.0f32; MUTE_RAMP_SAMPLES];
        g.process(&mut next);
        assert!(next.iter().all(|&s| s == 0.0), "must reach exact 0");
    }

    #[test]
    fn unmute_ramps_back_to_unity() {
        let mut g = GainRamp::new();
        g.set_muted(true); // snaps to 0
        g.set_muted(false);
        let mut a = [1.0f32; MUTE_RAMP_SAMPLES];
        g.process(&mut a);
        assert!(a[0] < 0.01, "unmute starts near zero");
        let mut b = [1.0f32; MUTE_RAMP_SAMPLES];
        g.process(&mut b);
        assert!(b.iter().all(|&s| s == 1.0), "must return to exact unity");
    }

    #[test]
    fn reset_unprimes_so_next_set_snaps() {
        let mut g = GainRamp::new();
        g.set_muted(false); // primed at unity
        g.reset();
        g.set_muted(true); // must snap, not ramp
        let mut hop = [1.0f32; MUTE_RAMP_SAMPLES];
        g.process(&mut hop);
        assert!(hop.iter().all(|&s| s == 0.0), "post-reset set must snap");
    }
}
