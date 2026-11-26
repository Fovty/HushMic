//! The mute gate on the host's thread. The engine takes a mode change at
//! its next hop, so without the gate a mute (push-to-talk released, the
//! mute shortcut) would still play what is already queued, up to the
//! plugin's latency, and an unmute would play what was captured while
//! muted. The gate closes within the cycle that sees the mute (a 5 ms
//! ramp, then exact zeros) and opens after an unmute only where the
//! output comes from input captured after it, the plugin's latency later
//! (a 5 ms ramp, no click). Bypass and strength never touch it.
//!
//! Output index `n` of the stream carries input index `n - latency` (the
//! alignment ledger holds the latency exact), which is how the gate knows
//! where the audio captured after an unmute begins. The latency is the one
//! in force, not the declared one: a host cycle longer than designed
//! grows the ledger's lead (by about a cycle), and the gate's delay with
//! it ([`MuteGate::set_delay`], every cycle). The models also reach
//! back: an output hop holds traces of the hops just before it (measured:
//! three hops for both shipped models, at -31 to -64 dBFS), so the gate
//! waits [`ENGINE_REACH`] longer and the first 40 ms after an unmute stay
//! silent rather than let any of what was said while muted through.

/// Samples the gate ramps over (5 ms at 48 kHz).
pub const RAMP: usize = 240;

/// How far an output hop reaches back into earlier input beyond the
/// latency: three hops measured for both shipped models, one spare.
pub const ENGINE_REACH: usize = 4 * hushmic_denoiser::HOP;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MuteGate {
    gain: f32,
    muted: bool,
    /// The first output index of the last unmuting cycle (its input is
    /// the first captured after the unmute); none: never muted.
    unmuted_at: Option<u64>,
    /// From an input sample to the output that holds no trace of earlier
    /// input: the latency in force plus the models' reach.
    delay: u64,
}

impl Default for MuteGate {
    fn default() -> Self {
        Self::new()
    }
}

impl MuteGate {
    /// Open, as a fresh stream starts.
    pub const fn new() -> MuteGate {
        MuteGate {
            gain: 1.0,
            muted: false,
            unmuted_at: None,
            delay: 0,
        }
    }

    /// The cycle whose first output index is `at` is muted or not. After an
    /// unmute the gate opens from the output of this cycle's input on.
    pub fn set_muted(&mut self, muted: bool, at: u64) {
        if self.muted && !muted {
            self.unmuted_at = Some(at);
        }
        self.muted = muted;
    }

    /// The delay in force (latency plus reach), set every cycle before
    /// [`MuteGate::apply`]. It only grows within a stream, as the ledger's
    /// lead does; a stream that restarts with a shorter one dropped what
    /// was queued, so the longer delay only keeps the gate shut longer.
    pub fn set_delay(&mut self, delay: u64) {
        self.delay = self.delay.max(delay);
    }

    /// The output index from which the gate may open.
    fn open_at(&self) -> u64 {
        self.unmuted_at.map_or(0, |u| u.saturating_add(self.delay))
    }

    /// Gate one cycle's output, whose first sample has output index `at`.
    /// RT-safe: arithmetic only.
    pub fn apply(&mut self, out: &mut [f32], at: u64) {
        let end = at.saturating_add(out.len() as u64);
        let open_at = self.open_at();
        if !self.muted && self.gain == 1.0 && at >= open_at {
            return;
        }
        if self.gain == 0.0 && (self.muted || end <= open_at) {
            out.fill(0.0);
            return;
        }
        let step = 1.0 / RAMP as f32;
        for (i, o) in out.iter_mut().enumerate() {
            let open = !self.muted && at + i as u64 >= open_at;
            self.gain = if open {
                (self.gain + step).min(1.0)
            } else {
                (self.gain - step).max(0.0)
            };
            *o = if self.gain == 0.0 {
                0.0
            } else {
                *o * self.gain
            };
        }
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAT: u64 = 4_000;

    fn ones(n: usize) -> Vec<f32> {
        vec![1.0; n]
    }

    #[test]
    fn a_mute_ramps_down_within_the_cycle_then_is_exact_zero() {
        let mut g = MuteGate::new();
        let mut out = ones(480);
        g.apply(&mut out, 0);
        assert!(out.iter().all(|&v| v == 1.0), "open: untouched");
        g.set_delay(LAT);
        g.set_muted(true, 480);
        let mut out = ones(480);
        g.apply(&mut out, 480);
        assert!(out[0] < 1.0 && out[0] > 0.99, "the ramp starts at once");
        assert!(out.windows(2).all(|w| w[1] <= w[0]), "down, never up");
        assert!(
            out[RAMP..].iter().all(|&v| v.to_bits() == 0),
            "exact +0.0 after 5 ms"
        );
        let mut out = ones(64);
        g.apply(&mut out, 960);
        assert!(out.iter().all(|&v| v.to_bits() == 0));
    }

    #[test]
    fn an_unmute_opens_only_on_audio_captured_after_it() {
        let mut g = MuteGate::new();
        g.set_delay(LAT);
        g.set_muted(true, 0);
        let mut out = ones(480);
        g.apply(&mut out, 0);
        // Unmuted in the cycle starting at 960: its input comes out at
        // 960 + LAT; everything before is what was captured while muted.
        g.set_muted(false, 960);
        let mut at = 480;
        let mut first_open = None;
        while at < 960 + LAT + 2_000 {
            let mut out = ones(128);
            g.apply(&mut out, at);
            for (i, &v) in out.iter().enumerate() {
                let n = at + i as u64;
                if n < 960 + LAT {
                    assert_eq!(v.to_bits(), 0, "captured while muted, at {n}");
                } else if first_open.is_none() && v > 0.0 {
                    first_open = Some(n);
                }
                if n >= 960 + LAT && n < 960 + LAT + RAMP as u64 {
                    let k = (n - 960 - LAT + 1) as f32 / RAMP as f32;
                    assert!(v <= k + 1e-5, "ramp at {n}: {v} > {k}");
                }
            }
            at += 128;
        }
        assert_eq!(first_open, Some(960 + LAT));
        assert_eq!(g.gain(), 1.0, "fully open after the ramp");
    }

    #[test]
    fn mute_and_unmute_within_a_ramp_never_jump() {
        let mut g = MuteGate::new();
        g.set_muted(true, 0);
        let mut out = ones(100);
        g.apply(&mut out, 0);
        g.set_muted(false, 100);
        let mut out2 = ones(400);
        g.apply(&mut out2, 100);
        let all: Vec<f32> = out.into_iter().chain(out2).collect();
        let step = 1.0 / RAMP as f32 + 1e-6;
        assert!(
            all.windows(2).all(|w| (w[1] - w[0]).abs() <= step),
            "no jump"
        );
        assert_eq!(g.gain(), 1.0);
    }

    #[test]
    fn a_delay_that_grows_after_the_unmute_keeps_the_gate_shut_longer() {
        let mut g = MuteGate::new();
        g.set_delay(LAT);
        g.set_muted(true, 0);
        let mut out = ones(2048);
        g.apply(&mut out, 0);
        g.set_muted(false, 2048);
        // The host's cycle grew: the ledger's lead (and the delay) with it.
        let grown = LAT + 2016;
        let mut at = 2048;
        while at < 2048 + grown + 4096 {
            g.set_delay(grown);
            let mut out = ones(2048);
            g.apply(&mut out, at);
            for (i, &v) in out.iter().enumerate() {
                if at + (i as u64) < 2048 + grown {
                    assert_eq!(v.to_bits(), 0, "open early at {}", at + i as u64);
                }
            }
            at += 2048;
        }
        assert_eq!(g.gain(), 1.0);
        // A shorter delay later (a restarted stream) never opens earlier.
        g.set_delay(LAT);
        assert_eq!(g.open_at(), 2048 + grown);
    }

    #[test]
    fn bypass_and_process_leave_it_open() {
        let mut g = MuteGate::new();
        g.set_delay(LAT);
        for k in 0..10u64 {
            g.set_muted(false, k * 480);
            let mut out = ones(480);
            g.apply(&mut out, k * 480);
            assert!(out.iter().all(|&v| v == 1.0));
        }
    }
}
