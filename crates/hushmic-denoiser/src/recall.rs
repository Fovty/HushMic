//! Talker recall across long pauses.
//!
//! DPDFNet decides which voice to keep from its recurrent memory of the last
//! several seconds. After ~10 s of silence that memory of the near talker has
//! faded, and the next voice — even a distant, reverberant one — is accepted as
//! the talker for its whole phrase. So: remember the recurrent part of the state
//! while the model passes speech, and after a long silence put it back on the
//! first sounding frame. The input-norm part (`state[..NORM_STATE]`) is left to
//! keep tracking the room.

use crate::stft::{FREQ_BINS, HOP, SPEC_LEN};

/// erb_norm (481) + spec_norm (96) running means at the head of the DPDFNet
/// state; the recurrent and conv memory follows.
const NORM_STATE: usize = 481 + 96;
/// Model output below this is the model's silence (room tone comes out ~-119 dB).
const SILENT_DB: f32 = -75.0;
/// Model output above this is speech worth remembering.
const SPEECH_DB: f32 = -60.0;
/// 2 s of silent output before the next sound triggers a restore.
const RESTORE_AFTER_HOPS: u32 = 200;

/// Hop level in dBFS of the time signal a spectrum frame resynthesizes to:
/// with the Vorbis window, sum(w^2) = HOP, so mean |X_k|^2 / HOP ~ sample power.
pub(crate) fn frame_db(spec: &[f32; SPEC_LEN]) -> f32 {
    let sum: f32 = spec.iter().map(|v| v * v).sum();
    let mean_bin = 2.0 * sum / (2 * (FREQ_BINS - 1)) as f32; // one-sided -> full spectrum
    10.0 * (mean_bin / HOP as f32 + 1e-12).log10()
}

pub(crate) struct Recall {
    snapshot: Vec<f32>,
    has_snapshot: bool,
    quiet_hops: u32,
}

impl Recall {
    pub(crate) fn new(state_size: usize) -> Recall {
        Recall {
            snapshot: vec![0f32; state_size.saturating_sub(NORM_STATE)],
            has_snapshot: false,
            quiet_hops: 0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.has_snapshot = false;
        self.quiet_hops = 0;
    }

    /// Call after each good model hop with the new state and the model's output
    /// level (before any attenuation blend). Returns true when it restored.
    pub(crate) fn after_hop(&mut self, state: &mut [f32], out_db: f32) -> bool {
        if state.len() != NORM_STATE + self.snapshot.len() || self.snapshot.is_empty() {
            return false;
        }
        let mut restored = false;
        if out_db < SILENT_DB {
            self.quiet_hops = self.quiet_hops.saturating_add(1);
        } else {
            if self.has_snapshot && self.quiet_hops >= RESTORE_AFTER_HOPS {
                state[NORM_STATE..].copy_from_slice(&self.snapshot);
                restored = true;
            }
            self.quiet_hops = 0;
        }
        if out_db > SPEECH_DB {
            self.snapshot.copy_from_slice(&state[NORM_STATE..]);
            self.has_snapshot = true;
        }
        restored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stft::Analysis;

    #[test]
    fn frame_db_reads_sample_power() {
        // uniform noise in [-a, a] has power a^2 / 3; a = 0.1 -> -24.8 dB
        let mut an = Analysis::new();
        let mut spec = [0f32; SPEC_LEN];
        let mut x = 1u32;
        let mut hop = [0f32; HOP];
        let mut acc = 0.0;
        for i in 0..50 {
            for s in &mut hop {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                *s = (x as f32 / u32::MAX as f32 * 2.0 - 1.0) * 0.1;
            }
            an.push_hop(&hop, &mut spec);
            if i >= 2 {
                acc += 10f32.powf(frame_db(&spec) / 10.0);
            }
        }
        let db = 10.0 * (acc / 48.0).log10();
        assert!((db + 24.8).abs() < 0.5, "{db}");
    }

    fn state(v: f32) -> Vec<f32> {
        vec![v; NORM_STATE + 4]
    }

    #[test]
    fn restores_memory_not_norms_after_long_silence() {
        let mut r = Recall::new(NORM_STATE + 4);
        let mut s = state(1.0);
        assert!(!r.after_hop(&mut s, -40.0)); // speech: remembered
        for _ in 0..RESTORE_AFTER_HOPS {
            s = state(2.0); // memory drifts during silence
            assert!(!r.after_hop(&mut s, -119.0));
        }
        s = state(3.0);
        assert!(r.after_hop(&mut s, -50.0)); // first sound after 2 s
        assert!(s[..NORM_STATE].iter().all(|&v| v == 3.0), "norms untouched");
        assert!(s[NORM_STATE..].iter().all(|&v| v == 1.0), "memory restored");
    }

    #[test]
    fn short_pause_and_no_speech_do_not_restore() {
        let mut r = Recall::new(NORM_STATE + 4);
        let mut s = state(1.0);
        for _ in 0..RESTORE_AFTER_HOPS {
            r.after_hop(&mut s, -119.0);
        }
        assert!(!r.after_hop(&mut s, -50.0), "nothing remembered yet");
        r.after_hop(&mut s, -40.0);
        for _ in 0..RESTORE_AFTER_HOPS - 1 {
            r.after_hop(&mut s, -119.0);
        }
        assert!(!r.after_hop(&mut s, -50.0), "pause too short");
        r.reset();
        for _ in 0..RESTORE_AFTER_HOPS {
            r.after_hop(&mut s, -119.0);
        }
        assert!(!r.after_hop(&mut s, -50.0), "reset forgets the snapshot");
    }
}
