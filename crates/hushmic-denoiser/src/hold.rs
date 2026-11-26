//! Voice hold: the model keeps to the talker it last passed.
//!
//! DPDFNet decides which voice is the talker from its recurrent memory of
//! the last seconds. While that memory holds the near talker, a voice from
//! across the room is removed like noise. About ten seconds after the near
//! talker stops, the memory has faded, and the next voice is taken for the
//! talker, however far away it is. A far voice that never stops wears the
//! memory down the same way within a minute.
//!
//! The hold remembers the model's state while speech comes out, and puts
//! it back every two seconds for as long as nothing comes out: in a pause,
//! and under a voice the model is removing. Only a hop the model answered
//! with silence is touched, so nothing that is being passed is disturbed,
//! and the first voice the model does pass becomes the one it remembers.
//!
//! Off by default: a [`crate::Denoiser`] that never turns it on computes
//! exactly what it did without it.
//!
//! The idea and the first implementation are travis-south's (HushMic
//! issue 34).

use crate::stft::{FREQ_BINS, HOP, SPEC_LEN};

/// The model's output below this is its silence (it answers a quiet room
/// near -120 dBFS).
const SILENT_DB: f32 = -75.0;
/// The model's output above this is speech it passed: worth remembering.
const SPEECH_DB: f32 = -60.0;
/// The memory is put back every two seconds of silence.
const RESTORE_EVERY_HOPS: u32 = 200;
/// The end of the state that is never put back: the buffers of the
/// output side (the deep-filter convolution over time and the three
/// decoder convolutions, the same in dpdfnet2 and dpdfnet8). Copied from a
/// moment of speech they would play its last hops again.
const OUTPUT_TAIL: usize = 30720 + 2886 + 2880 + 4810;

/// The level, in dBFS, of the audio one spectrum frame stands for: with
/// the analysis window in use the squared window sums to `HOP`, so the
/// mean of |X|² over the full spectrum, divided by `HOP`, is the mean
/// square of the samples.
pub(crate) fn frame_db(spec: &[f32; SPEC_LEN]) -> f32 {
    let sum: f32 = spec.iter().map(|v| v * v).sum();
    // one-sided spectrum: every bin counts twice in the full one
    let mean_bin = 2.0 * sum / (2 * (FREQ_BINS - 1)) as f32;
    10.0 * (mean_bin / HOP as f32 + 1e-12).log10()
}

pub(crate) struct VoiceHold {
    on: bool,
    /// The state without its output tail, as it stood at the last hop of
    /// passed speech.
    remembered: Vec<f32>,
    has_memory: bool,
    silent_hops: u32,
}

impl VoiceHold {
    /// For a model whose state holds `state_size` values. A state too
    /// small to have the output tail is left alone for good.
    pub(crate) fn new(state_size: usize) -> VoiceHold {
        VoiceHold {
            on: false,
            remembered: vec![0f32; state_size.saturating_sub(OUTPUT_TAIL)],
            has_memory: false,
            silent_hops: 0,
        }
    }

    pub(crate) fn is_on(&self) -> bool {
        self.on
    }

    /// Turning it off forgets the talker: turned on again it starts as new.
    pub(crate) fn set_on(&mut self, on: bool) {
        self.on = on;
        if !on {
            self.has_memory = false;
            self.silent_hops = 0;
        }
    }

    /// After the state was set back to its start (a stream that begins
    /// again): the talker's memory goes back in, so the same voice is
    /// still the talker.
    pub(crate) fn after_reset(&mut self, state: &mut [f32]) {
        self.silent_hops = 0;
        if self.on && self.has_memory && self.fits(state) {
            state[..self.remembered.len()].copy_from_slice(&self.remembered);
        }
    }

    /// After a hop the model computed: `state` is its new state, `out_db`
    /// the level of what it let through (before the strength limit mixes
    /// the original back in). True when the memory was put back.
    pub(crate) fn after_hop(&mut self, state: &mut [f32], out_db: f32) -> bool {
        if !self.on || !self.fits(state) {
            return false;
        }
        let n = self.remembered.len();
        let mut restored = false;
        if out_db < SILENT_DB {
            self.silent_hops = self.silent_hops.saturating_add(1);
            if self.has_memory && self.silent_hops.is_multiple_of(RESTORE_EVERY_HOPS) {
                state[..n].copy_from_slice(&self.remembered);
                restored = true;
            }
        } else {
            self.silent_hops = 0;
        }
        if out_db > SPEECH_DB {
            self.remembered.copy_from_slice(&state[..n]);
            self.has_memory = true;
        }
        restored
    }

    fn fits(&self, state: &[f32]) -> bool {
        let n = self.remembered.len();
        n > 0 && state.len() == n + OUTPUT_TAIL
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stft::Analysis;

    const QUIET: f32 = -119.0;
    const SPEECH: f32 = -40.0;
    const N: usize = 4 + OUTPUT_TAIL;

    fn held(state_size: usize) -> VoiceHold {
        let mut h = VoiceHold::new(state_size);
        h.set_on(true);
        h
    }

    /// `hops` silent hops; whether any of them put the memory back.
    fn silence(h: &mut VoiceHold, s: &mut [f32], hops: u32) -> bool {
        (0..hops).fold(false, |any, _| h.after_hop(s, QUIET) || any)
    }

    #[test]
    fn frame_db_is_the_level_of_the_samples() {
        // uniform noise in [-a, a] has the mean square a²/3: a = 0.1 is -24.8 dB
        let mut analysis = Analysis::new();
        let mut spec = [0f32; SPEC_LEN];
        let mut hop = [0f32; HOP];
        let mut x = 1u32;
        let mut power = 0.0;
        for i in 0..50 {
            for s in &mut hop {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                *s = (x as f32 / u32::MAX as f32 * 2.0 - 1.0) * 0.1;
            }
            analysis.push_hop(&hop, &mut spec);
            if i >= 2 {
                power += 10f32.powf(frame_db(&spec) / 10.0);
            }
        }
        let db = 10.0 * (power / 48.0).log10();
        assert!((db + 24.8).abs() < 0.5, "{db}");
    }

    #[test]
    fn off_it_never_touches_the_state() {
        let mut h = VoiceHold::new(N);
        assert!(!h.is_on());
        let mut s = vec![1.0; N];
        assert!(!h.after_hop(&mut s, SPEECH));
        s = vec![2.0; N];
        assert!(!silence(&mut h, &mut s, 3 * RESTORE_EVERY_HOPS));
        h.after_reset(&mut s);
        assert!(s.iter().all(|&v| v == 2.0));
    }

    #[test]
    fn the_memory_goes_back_every_two_seconds_of_silence() {
        let mut h = held(N);
        let mut s = vec![1.0; N];
        assert!(
            !h.after_hop(&mut s, SPEECH),
            "speech is remembered, not restored"
        );
        s = vec![2.0; N]; // the state drifts while nothing is said
        assert!(!silence(&mut h, &mut s, RESTORE_EVERY_HOPS - 1));
        assert!(h.after_hop(&mut s, QUIET), "at two seconds");
        assert_eq!(s[..4], [1.0; 4]);
        assert!(s[4..].iter().all(|&v| v == 2.0), "the output tail stays");
        s = vec![2.0; N];
        assert!(!silence(&mut h, &mut s, RESTORE_EVERY_HOPS - 1));
        assert!(h.after_hop(&mut s, QUIET), "and at four");
    }

    #[test]
    fn a_hop_that_sounds_is_never_restored_and_starts_the_count_again() {
        let mut h = held(N);
        let mut s = vec![1.0; N];
        h.after_hop(&mut s, SPEECH);
        s = vec![2.0; N];
        assert!(!silence(&mut h, &mut s, RESTORE_EVERY_HOPS - 1));
        // between silence and speech: neither restored nor remembered
        assert!(!h.after_hop(&mut s, -70.0));
        assert!(s.iter().all(|&v| v == 2.0));
        assert!(
            !silence(&mut h, &mut s, RESTORE_EVERY_HOPS - 1),
            "counted from zero"
        );
        assert!(h.after_hop(&mut s, QUIET));
        assert_eq!(
            s[..4],
            [1.0; 4],
            "the speech from before, not the hop between"
        );
    }

    #[test]
    fn the_voice_that_is_passed_becomes_the_one_remembered() {
        let mut h = held(N);
        let mut s = vec![1.0; N];
        h.after_hop(&mut s, SPEECH);
        s = vec![5.0; N]; // someone else, and the model lets them through
        h.after_hop(&mut s, SPEECH);
        s = vec![2.0; N];
        assert!(silence(&mut h, &mut s, RESTORE_EVERY_HOPS));
        assert_eq!(s[..4], [5.0; 4]);
    }

    #[test]
    fn nothing_is_put_back_before_anything_was_said() {
        let mut h = held(N);
        let mut s = vec![2.0; N];
        assert!(!silence(&mut h, &mut s, 3 * RESTORE_EVERY_HOPS));
        h.after_reset(&mut s);
        assert!(s.iter().all(|&v| v == 2.0));
    }

    #[test]
    fn a_reset_keeps_the_talker_and_turning_it_off_forgets_them() {
        let mut h = held(N);
        let mut s = vec![1.0; N];
        h.after_hop(&mut s, SPEECH);
        s = vec![0.0; N]; // the state at its start again
        h.after_reset(&mut s);
        assert_eq!(s[..4], [1.0; 4]);
        assert!(s[4..].iter().all(|&v| v == 0.0), "the tail starts fresh");
        assert!(
            !silence(&mut h, &mut s, RESTORE_EVERY_HOPS - 1),
            "counted from zero"
        );
        h.set_on(false);
        h.set_on(true);
        s = vec![0.0; N];
        h.after_reset(&mut s);
        assert!(s.iter().all(|&v| v == 0.0), "forgotten");
        assert!(!silence(&mut h, &mut s, 3 * RESTORE_EVERY_HOPS));
    }

    #[test]
    fn a_state_of_another_shape_is_left_alone() {
        // too small for the tail
        let mut h = held(4);
        let mut s = vec![1.0; 4];
        h.after_hop(&mut s, SPEECH);
        s = vec![2.0; 4];
        assert!(!silence(&mut h, &mut s, 3 * RESTORE_EVERY_HOPS));
        h.after_reset(&mut s);
        assert_eq!(s, [2.0; 4]);
        // not the size it was made for
        let mut h = held(N);
        let mut s = vec![1.0; N + 8];
        h.after_hop(&mut s, SPEECH);
        assert!(!silence(&mut h, &mut s, 3 * RESTORE_EVERY_HOPS));
    }
}
