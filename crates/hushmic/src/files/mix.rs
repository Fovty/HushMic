//! Every strength from the one pass: the plugin's strength is a cap on the
//! suppression, which it applies as a blend of the model's output with the
//! (aligned) original, `(1 − α)·clean + α·original` with α = 10^(−dB/20),
//! and the synthesis after it is linear. So the pass runs once at Maximum
//! and Gentle, Medium, Strong, the hold (α = 1) and Save are this mix.
//!
//! Changes ramp: a strength change over 10 ms, the hold over Mic check's
//! 20 ms crossfade, so neither clicks.

use crate::miccheck::player::CROSSFADE;

/// A strength change ramps over 10 ms.
pub const STRENGTH_RAMP: usize = 480;
/// The hold ramps over 20 ms, as Mic check's switch does.
pub const HOLD_RAMP: usize = CROSSFADE;

/// The original's share at a strength of `attn_db` (the plugin's dB cap):
/// exactly what the plugin's limiter blends, including no blend at all
/// for an unlimited cap or a share too small to matter.
pub fn alpha(attn_db: f32) -> f32 {
    if !attn_db.is_finite() || attn_db >= 200.0 {
        return 0.0;
    }
    let a = 10f32.powf(-attn_db / 20.0).min(1.0);
    if a > 1e-6 {
        a
    } else {
        0.0
    }
}

/// The mix of the cleaned sound and the original, per frame.
#[derive(Clone, Debug)]
pub struct Blend {
    /// The strength's share of the original.
    strength: f32,
    held: bool,
    /// The ramp: from, to, frames done, frames in all.
    from: f32,
    to: f32,
    done: usize,
    len: usize,
}

impl Blend {
    pub fn new(strength_alpha: f32) -> Blend {
        Blend {
            strength: strength_alpha,
            held: false,
            from: strength_alpha,
            to: strength_alpha,
            done: 0,
            len: 0,
        }
    }

    /// The share of the original right now.
    pub fn current(&self) -> f32 {
        if self.done >= self.len {
            return self.to;
        }
        self.from + (self.to - self.from) * (self.done as f32 / self.len as f32)
    }

    fn ramp_to(&mut self, to: f32, len: usize) {
        self.from = self.current();
        self.to = to;
        self.done = 0;
        self.len = len;
    }

    /// The share of the original once the ramp is through.
    pub fn target(&self) -> f32 {
        self.to
    }

    /// Held, and the ramp to the original is through: the original alone.
    pub fn held_fully(&self) -> bool {
        self.held && self.current() >= 1.0
    }

    /// Finish a ramp at once (while nothing plays, there is nothing to
    /// ramp).
    pub fn settle(&mut self) {
        self.done = self.len;
    }

    /// A new strength (its α); heard at once unless the original is held.
    pub fn set_strength(&mut self, a: f32) {
        self.strength = a;
        if !self.held {
            self.ramp_to(a, STRENGTH_RAMP);
        }
    }

    /// Hold (the original alone) or let go.
    pub fn hold(&mut self, on: bool) {
        if self.held != on {
            self.held = on;
            self.ramp_to(if on { 1.0 } else { self.strength }, HOLD_RAMP);
        }
    }

    /// Mix one block, planar. `clean` may have one channel for a mono
    /// recording stored as stereo: it goes to every channel of `orig`.
    pub fn mix(&mut self, clean: &[Vec<f32>], orig: &[Vec<f32>]) -> Vec<Vec<f32>> {
        let n = orig.first().map_or(0, Vec::len);
        let mut out = vec![Vec::with_capacity(n); orig.len()];
        for i in 0..n {
            let a = self.current();
            for (c, o) in out.iter_mut().enumerate() {
                let x = clean.get(c).or(clean.first()).map_or(0.0, |ch| ch[i]);
                o.push((1.0 - a) * x + a * orig[c][i]);
            }
            if self.done < self.len {
                self.done += 1;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::Strength;

    #[test]
    fn the_presets_blend_as_the_plugin_does() {
        let a = |s: Strength| alpha(s.attn_limit().unwrap());
        assert!((a(Strength::Gentle) - 0.501).abs() < 1e-3);
        assert!((a(Strength::Medium) - 0.251).abs() < 1e-3);
        assert!((a(Strength::Strong) - 0.0631).abs() < 1e-4);
        assert!(a(Strength::Maximum) < 1e-4);
        assert_eq!(alpha(f32::INFINITY), 0.0);
        assert_eq!(alpha(0.0), 1.0);
        assert_eq!(alpha(-6.0), 1.0, "a negative cap is the original");
    }

    #[test]
    fn changes_ramp_and_the_hold_wins_over_the_strength() {
        let mut b = Blend::new(0.0);
        let clean = vec![vec![0.0f32; 2000]];
        let orig = vec![vec![1.0f32; 2000]];
        // The mix is the original's share: 0 → 0.5 over 10 ms.
        b.set_strength(0.5);
        let y = b.mix(&clean, &orig);
        assert_eq!(y[0][0], 0.0);
        assert!((y[0][STRENGTH_RAMP / 2] - 0.25).abs() < 1e-3);
        assert_eq!(y[0][STRENGTH_RAMP], 0.5);
        // Held: to the original over 20 ms; a strength meanwhile waits.
        b.hold(true);
        b.set_strength(0.25);
        let y = b.mix(&clean, &orig);
        assert!((y[0][HOLD_RAMP / 2] - 0.75).abs() < 1e-3);
        assert_eq!(y[0][HOLD_RAMP], 1.0);
        b.hold(false);
        let y = b.mix(&clean, &orig);
        assert_eq!(y[0][HOLD_RAMP], 0.25);
        // Every step is small: no click.
        assert!(y[0].windows(2).all(|w| (w[1] - w[0]).abs() < 1e-3));
    }

    #[test]
    fn a_mono_store_feeds_every_channel() {
        let mut b = Blend::new(0.5);
        let y = b.mix(&[vec![1.0, 1.0]], &[vec![0.0, 0.0], vec![1.0, 1.0]]);
        assert_eq!(y, vec![vec![0.5, 0.5], vec![1.0, 1.0]]);
    }
}
