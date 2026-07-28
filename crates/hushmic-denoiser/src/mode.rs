//! Runtime mode (process / bypass / mute) and the mute gain ramp.

use crate::stft::HOP;

/// Samples the mute gain ramp spans: one hop = 10 ms @48 kHz.
pub const MUTE_RAMP_SAMPLES: usize = HOP;

/// What leaves the denoiser. The engine keeps running in every mode (its
/// recurrent state stays warm, so transitions are instant and glitch-free).
///
/// `non_exhaustive`: modes have been added before (`Mute` arrived after
/// `Bypass`); downstream matches need a catch-all arm.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Denoised output (the default).
    Process,
    /// The unprocessed input, latency-aligned with the denoised path.
    Bypass,
    /// Exact digital silence, entered/left through a short fade.
    Mute,
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

    /// Reset directly INTO a mute state, still unprimed: the gain lands on
    /// the target instantly (a reset-while-muted stream must not leak even
    /// one sample) and the next `set_muted` still snaps.
    pub fn reset_to(&mut self, muted: bool) {
        let g = if muted { 0.0 } else { 1.0 };
        self.gain = g;
        self.target = g;
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
    fn reset_to_muted_is_silent_and_still_snaps() {
        let mut g = GainRamp::new();
        g.set_muted(false); // primed at unity mid-session
        g.reset_to(true);
        let mut hop = [1.0f32; MUTE_RAMP_SAMPLES];
        g.process(&mut hop);
        assert!(
            hop.iter().all(|&s| s == 0.0),
            "reset into mute must not leak a fade"
        );
        g.set_muted(false); // next set after reset_to must snap, not ramp
        let mut hop = [1.0f32; MUTE_RAMP_SAMPLES];
        g.process(&mut hop);
        assert!(hop.iter().all(|&s| s == 1.0), "post-reset_to set must snap");
    }

    #[test]
    fn reset_to_unmuted_unprimes_so_next_set_snaps() {
        let mut g = GainRamp::new();
        g.set_muted(false); // primed at unity
        g.reset_to(false);
        g.set_muted(true); // must snap, not ramp
        let mut hop = [1.0f32; MUTE_RAMP_SAMPLES];
        g.process(&mut hop);
        assert!(hop.iter().all(|&s| s == 0.0), "post-reset set must snap");
    }
}
