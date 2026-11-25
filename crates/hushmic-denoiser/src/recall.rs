//! Talker recall across long pauses.
//!
//! DPDFNet decides which voice to keep from its recurrent memory of the last
//! several seconds. After ~10 s of silence that memory of the near talker has
//! faded, and the next voice — even a distant, reverberant one — is accepted as
//! the talker for its whole phrase. So: remember the state (norms included)
//! while the model passes speech, and put it back every 2 s of silent output,
//! never on a sounding hop (stale conv buffers would clip the onset), and on
//! reset (so a stream restart keeps the talker). Silent output with loud input
//! counts too: a background talker who keeps going never pauses 2 s, and is
//! accepted within a minute unless the memory is re-asserted under them. The
//! output-side tail is left alone (see [`OUTPUT_TAIL`]).

use crate::stft::{FREQ_BINS, HOP, SPEC_LEN};

/// Model output below this is the model's silence (room tone comes out ~-119 dB).
const SILENT_DB: f32 = -75.0;
/// Model output above this is speech worth remembering.
const SPEECH_DB: f32 = -60.0;
/// Re-assert the talker every 2 s so its memory never fades across a long pause.
const RESTORE_AFTER_HOPS: u32 = 200;
/// State tail (DF temporal conv + 3 decoder convs, same in dpdfnet2/8): a
/// speech-time copy replays the last speech for ~3 hops, so never restore it.
const OUTPUT_TAIL: usize = 30720 + 2886 + 2880 + 4810;

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
            snapshot: vec![0f32; state_size.saturating_sub(OUTPUT_TAIL)],
            has_snapshot: false,
            quiet_hops: 0,
        }
    }

    /// Call with the freshly initialised state: puts the remembered talker back.
    pub(crate) fn reset(&mut self, state: &mut [f32]) {
        if self.has_snapshot {
            state[..self.snapshot.len()].copy_from_slice(&self.snapshot);
        }
        self.quiet_hops = 0;
    }

    /// Call after each good model hop with the new state and the model's output
    /// level (before any attenuation blend). Returns true when it restored.
    pub(crate) fn after_hop(&mut self, state: &mut [f32], out_db: f32) -> bool {
        let n = self.snapshot.len();
        if state.len() != n + OUTPUT_TAIL || n == 0 {
            return false;
        }
        let mut restored = false;
        if out_db < SILENT_DB {
            self.quiet_hops = self.quiet_hops.saturating_add(1);
            if self.has_snapshot && self.quiet_hops.is_multiple_of(RESTORE_AFTER_HOPS) {
                state[..n].copy_from_slice(&self.snapshot);
                restored = true;
            }
        } else {
            self.quiet_hops = 0;
        }
        if out_db > SPEECH_DB {
            self.snapshot.copy_from_slice(&state[..n]);
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

    const QUIET: f32 = -119.0; // the model's silence
    const N: usize = 4 + OUTPUT_TAIL;

    fn quiet(r: &mut Recall, s: &mut [f32], hops: u32) -> bool {
        (0..hops).any(|_| r.after_hop(s, QUIET))
    }

    #[test]
    fn restores_prefix_during_silence_not_on_onset() {
        let mut r = Recall::new(N);
        let mut s = vec![1.0; N];
        assert!(!r.after_hop(&mut s, -40.0)); // speech: remembered
        s = vec![2.0; N]; // state drifts during silence
        assert!(!quiet(&mut r, &mut s, RESTORE_AFTER_HOPS - 1));
        assert!(r.after_hop(&mut s, QUIET), "2 s of quiet");
        assert_eq!(s[..4], [1.0; 4], "norms included");
        assert!(s[4..].iter().all(|&v| v == 2.0), "output tail left alone");
        s = vec![2.0; N];
        assert!(!quiet(&mut r, &mut s, RESTORE_AFTER_HOPS - 1));
        assert!(r.after_hop(&mut s, QUIET), "again at 4 s");
        s = vec![3.0; N];
        assert!(!r.after_hop(&mut s, -50.0), "not on the onset");
        assert!(s.iter().all(|&v| v == 3.0));
    }

    #[test]
    fn no_restore_without_snapshot_or_before_the_count() {
        let mut r = Recall::new(N);
        let mut s = vec![1.0; N];
        assert!(
            !quiet(&mut r, &mut s, 2 * RESTORE_AFTER_HOPS),
            "nothing remembered"
        );
        r.after_hop(&mut s, -40.0);
        assert!(
            !quiet(&mut r, &mut s, RESTORE_AFTER_HOPS - 1),
            "pause too short"
        );
        r.after_hop(&mut s, -50.0); // sound resets the count
        assert!(!quiet(&mut r, &mut s, RESTORE_AFTER_HOPS - 1));
    }

    #[test]
    fn state_without_room_for_the_tail_is_left_alone() {
        let mut r = Recall::new(4);
        let mut s = vec![1.0; 4];
        r.after_hop(&mut s, -40.0);
        s = vec![2.0; 4];
        assert!(!quiet(&mut r, &mut s, 2 * RESTORE_AFTER_HOPS));
        r.reset(&mut s);
        assert_eq!(s, [2.0; 4]);
    }

    #[test]
    fn reset_restores_and_keeps_the_snapshot() {
        let mut r = Recall::new(N);
        let mut s = vec![1.0; N];
        r.after_hop(&mut s, -40.0);
        s = vec![0.0; N]; // re-initialised state
        r.reset(&mut s);
        assert_eq!(s[..4], [1.0; 4]);
        assert!(s[4..].iter().all(|&v| v == 0.0), "tail stays initialised");
        s = vec![0.0; N];
        r.reset(&mut s);
        assert_eq!(s[..4], [1.0; 4], "snapshot kept");
        assert!(!quiet(&mut r, &mut s, RESTORE_AFTER_HOPS - 1));
        r.reset(&mut s);
        assert!(
            !quiet(&mut r, &mut s, RESTORE_AFTER_HOPS - 1),
            "count zeroed"
        );
    }
}
