use crate::stft::SPEC_LEN;
use std::collections::VecDeque;

// DPDFNet's attenuation-limit is OFFLINE-ONLY upstream; hushmic deliberately
// ports it to the streaming path. The 4-frame noisy delay aligns
// the retained noisy floor with the model's group delay — validated by
// `delay_alignment_blends_correct_frame`.
pub const NOISY_FRAME_OFFSET: usize = 4;

/// Hops the bypass mix ramps over (~40 ms @ 480-sample hops). Per-hop steps
/// are smoothed by the synthesis OLA windowing.
pub const BYPASS_RAMP_HOPS: usize = 4;

pub struct AttnLimiter {
    alpha: f32, // residual noisy fraction; 0 = fully enhanced, 1 = fully noisy
    enabled: bool,
    ring: VecDeque<[f32; SPEC_LEN]>,
    bypass_mix: f32,    // 0 = attn-blended output, 1 = pure delayed noisy
    bypass_target: f32, // stepped toward per apply() call
}

impl AttnLimiter {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            alpha: 0.0,
            enabled: false,
            ring: VecDeque::with_capacity(NOISY_FRAME_OFFSET + 1),
            bypass_mix: 0.0,
            bypass_target: 0.0,
        }
    }

    /// Bypass = fade the output to the delayed noisy spectrum the ring
    /// already maintains (it stays model-group-delay aligned). Ramped per
    /// apply() call over `BYPASS_RAMP_HOPS`.
    pub fn set_bypass(&mut self, bypassed: bool) {
        self.bypass_target = if bypassed { 1.0 } else { 0.0 };
    }

    /// dB cap -> alpha = 10^(-dB/20). dB <= 0 disables suppression (alpha=1, pure noisy).
    /// A very large dB (e.g. >= 80) effectively disables blending (alpha ~ 0, pure enhanced).
    pub fn set_db(&mut self, db: f32) {
        if !db.is_finite() || db >= 200.0 {
            self.enabled = false; // unlimited suppression: skip the blend entirely
            self.alpha = 0.0;
            return;
        }
        self.alpha = 10f32.powf(-db / 20.0);
        // db <= 0 yields alpha > 1 (over-unity); clamp to pure noisy per contract
        self.alpha = self.alpha.min(1.0);
        // alpha ~ 0 means no noisy floor; treat as disabled to save work
        self.enabled = self.alpha > 1e-6;
    }

    pub fn reset(&mut self) {
        self.ring.clear();
        self.bypass_mix = 0.0; // fresh session fades in from processed
    }

    pub fn apply(&mut self, noisy: &[f32; SPEC_LEN], enhanced: &mut [f32; SPEC_LEN]) {
        // step the bypass mix once per hop; land exactly on the target
        if self.bypass_mix != self.bypass_target {
            let step = 1.0 / BYPASS_RAMP_HOPS as f32;
            let d = self.bypass_target - self.bypass_mix;
            if d.abs() <= step {
                self.bypass_mix = self.bypass_target;
            } else {
                self.bypass_mix += step * d.signum();
            }
        }
        // maintain a delay line of the last NOISY_FRAME_OFFSET noisy frames
        self.ring.push_back(*noisy);
        let delayed = if self.ring.len() > NOISY_FRAME_OFFSET {
            self.ring.pop_front()
        } else {
            None // not primed yet -> no noisy reference available
        };
        let Some(d) = delayed else {
            return; // no noisy reference: pure enhanced whatever the mode
        };
        if self.enabled {
            let a = self.alpha;
            for i in 0..SPEC_LEN {
                enhanced[i] = a * d[i] + (1.0 - a) * enhanced[i];
            }
        }
        // bypass blends last so full bypass wins over the attn blend
        if self.bypass_mix > 0.0 {
            let m = self.bypass_mix;
            for i in 0..SPEC_LEN {
                enhanced[i] = m * d[i] + (1.0 - m) * enhanced[i];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stft::SPEC_LEN;

    #[test]
    fn zero_db_returns_noisy_after_delay() {
        let mut a = AttnLimiter::new();
        a.set_db(0.0); // alpha = 1.0 -> output equals the delayed noisy reference
        let mut noisy = [0f32; SPEC_LEN];
        for (i, v) in noisy.iter_mut().enumerate() {
            *v = i as f32;
        }
        // push NOISY_FRAME_OFFSET frames so the delay line is primed
        for _ in 0..NOISY_FRAME_OFFSET {
            let mut enh = [0f32; SPEC_LEN];
            a.apply(&noisy, &mut enh);
        }
        let mut enh = [7f32; SPEC_LEN];
        a.apply(&noisy, &mut enh);
        // with alpha=1 and a constant noisy frame, output == noisy
        assert!(
            (enh[10] - noisy[10]).abs() < 1e-4,
            "0 dB must pass noisy through"
        );
    }

    #[test]
    fn large_db_returns_enhanced() {
        let mut a = AttnLimiter::new();
        a.set_db(100.0); // alpha = 10^(-100/20) = 1e-5 -> tiny noisy floor
        let noisy = [1000f32; SPEC_LEN];
        // prime the delay line so the 5th apply actually blends the noisy reference
        for _ in 0..NOISY_FRAME_OFFSET {
            let mut scratch = [0f32; SPEC_LEN];
            a.apply(&noisy, &mut scratch);
        }
        let mut enh = [3f32; SPEC_LEN];
        a.apply(&noisy, &mut enh);
        // blended = 1e-5*1000 + (1-1e-5)*3 ≈ 3.01; exercises the tiny-alpha blend
        assert!(
            (enh[10] - 3.01).abs() < 0.05,
            "100 dB must keep ~enhanced (tiny noisy floor)"
        );
    }

    /// Prime the ring and complete the bypass ramp with constant input.
    fn primed_bypassed(a: &mut AttnLimiter, noisy: &[f32; SPEC_LEN]) {
        a.set_bypass(true);
        for _ in 0..(NOISY_FRAME_OFFSET + BYPASS_RAMP_HOPS) {
            let mut enh = [0f32; SPEC_LEN];
            a.apply(noisy, &mut enh);
        }
    }

    #[test]
    fn bypass_converges_to_delayed_noisy() {
        let mut a = AttnLimiter::new(); // attn disabled (default)
        let noisy = [42.0f32; SPEC_LEN];
        primed_bypassed(&mut a, &noisy);
        let mut enh = [-7.0f32; SPEC_LEN];
        a.apply(&noisy, &mut enh);
        assert!(
            enh.iter().all(|&v| v == 42.0),
            "full bypass must output the delayed noisy spectrum exactly"
        );
    }

    #[test]
    fn bypass_overrides_attn_blend() {
        let mut a = AttnLimiter::new();
        a.set_db(6.0); // attn blend active - must not matter under full bypass
        let noisy = [42.0f32; SPEC_LEN];
        primed_bypassed(&mut a, &noisy);
        let mut enh = [-7.0f32; SPEC_LEN];
        a.apply(&noisy, &mut enh);
        assert!(
            enh.iter().all(|&v| v == 42.0),
            "bypass beats the attn blend"
        );
    }

    #[test]
    fn bypass_ramp_steps_are_bounded() {
        let mut a = AttnLimiter::new();
        let noisy = [1.0f32; SPEC_LEN];
        // prime the ring first, still in processed mode
        for _ in 0..NOISY_FRAME_OFFSET {
            let mut enh = [0f32; SPEC_LEN];
            a.apply(&noisy, &mut enh);
        }
        a.set_bypass(true);
        // enhanced sentinel 0, delayed noisy 1 -> output IS the current mix
        let step_bound = 1.0 / BYPASS_RAMP_HOPS as f32 + 1e-6;
        let mut prev = 0.0f32;
        for _ in 0..(BYPASS_RAMP_HOPS + 2) {
            let mut enh = [0f32; SPEC_LEN];
            a.apply(&noisy, &mut enh);
            let mix = enh[0];
            assert!(mix >= prev - 1e-6, "mix must ramp monotonically up");
            assert!(mix - prev <= step_bound, "per-hop step bound exceeded");
            prev = mix;
        }
        assert!((prev - 1.0).abs() < 1e-6, "ramp must reach full bypass");
    }

    #[test]
    fn bypass_off_ramps_back_to_processed() {
        let mut a = AttnLimiter::new();
        let noisy = [1.0f32; SPEC_LEN];
        primed_bypassed(&mut a, &noisy);
        a.set_bypass(false);
        for _ in 0..(BYPASS_RAMP_HOPS + 1) {
            let mut enh = [0f32; SPEC_LEN];
            a.apply(&noisy, &mut enh);
        }
        let mut enh = [5.0f32; SPEC_LEN];
        a.apply(&noisy, &mut enh);
        assert!(
            enh.iter().all(|&v| v == 5.0),
            "attn-disabled processed output must return to pure enhanced"
        );
    }

    #[test]
    fn bypass_before_ring_primed_falls_back_to_processed() {
        let mut a = AttnLimiter::new();
        a.set_bypass(true);
        let noisy = [42.0f32; SPEC_LEN];
        // fewer than NOISY_FRAME_OFFSET applies: no delayed reference yet
        for t in 0..NOISY_FRAME_OFFSET {
            let mut enh = [5.0f32; SPEC_LEN];
            a.apply(&noisy, &mut enh);
            assert!(
                enh.iter().all(|&v| v == 5.0),
                "unprimed bypass (t={t}) must pass the processed signal"
            );
        }
    }

    #[test]
    fn delay_alignment_blends_correct_frame() {
        use crate::stft::SPEC_LEN;
        let mut a = AttnLimiter::new();
        a.set_db(6.0); // alpha = 10^(-0.3) ≈ 0.5012
        let alpha = 10f32.powf(-6.0 / 20.0);
        // Feed frames whose every bin == frame index, so we can detect which past
        // noisy frame got blended in. enhanced is a constant sentinel each call.
        let mut last = [0f32; SPEC_LEN];
        for t in 0..(NOISY_FRAME_OFFSET + 3) {
            let noisy = [t as f32; SPEC_LEN];
            let mut enh = [1000.0f32; SPEC_LEN];
            a.apply(&noisy, &mut enh);
            last = enh;
            if t < NOISY_FRAME_OFFSET {
                // not yet primed -> pure enhanced (no blend)
                assert!(
                    (last[0] - 1000.0).abs() < 1e-3,
                    "frame {t} should be unblended"
                );
            }
        }
        // On the last call (t = OFFSET+2), the delayed noisy is frame (t - OFFSET) = 2.
        let t = NOISY_FRAME_OFFSET + 2;
        let expected = alpha * ((t - NOISY_FRAME_OFFSET) as f32) + (1.0 - alpha) * 1000.0;
        assert!(
            (last[0] - expected).abs() < 0.05,
            "delayed-blend misaligned: got {}, expected {}",
            last[0],
            expected
        );
    }
}
