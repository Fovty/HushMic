//! Honest, alignment-free sample indicators (raw vs filtered 10 s buffers).
//!
//! A live A/B capture is two INDEPENDENT `pw-record` streams: measured ~100
//! ms out of lag with ~0 frame-to-frame correlation. So nothing here
//! compares raw[i] to filtered[i] — every number is a per-stream
//! DISTRIBUTION statistic (own noise floor, own loudest-speech level), which
//! a time offset cannot corrupt. Frame-based (50 ms); every returned value
//! is finite; a take without enough voice-band dynamic range reports
//! voice_measurable=false rather than a fabricated number. (That gate keys on
//! dynamic range, NOT true voicing — see MIN_SPEECH_DYNAMICS_DB.)

use crate::abtest::types::{SampleMetrics, SAMPLE_RATE};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// 50 ms analysis frames at the contract's 48 kHz.
const FRAME_LEN: usize = 2400;
/// Voice-band energy comes from one zero-padded FFT per frame (next power of
/// two above FRAME_LEN); deterministic, no windowing needed for band RMS.
const FFT_LEN: usize = 4096;
const VOICE_LO_HZ: f32 = 300.0;
const VOICE_HI_HZ: f32 = 4000.0;
/// dB floor for silent frames — keeps log10 finite on all-zero input.
const DB_MIN: f32 = -120.0;
/// Noise floor = this percentile of a stream's own frame levels.
const FLOOR_PCT: f32 = 10.0;
/// Realistic measurement floor: the model gates pauses to ~digital silence
/// (−134 dBFS observed), so the filtered floor is clamped here before the
/// reduction is computed — otherwise it prints physically meaningless
/// sub-silence numbers. The UI reads this to label a clamped filtered floor
/// "silent" (and the reduction "≥") rather than the literal −75.0.
pub(crate) const SILENT_FLOOR_DBFS: f32 = -75.0;
/// "Speech level" = median voice-band dBFS of a stream's loudest this
/// fraction of frames (the speech-active portion of a normal take).
const SPEECH_TOP_FRAC: f32 = 0.30;
/// Voice retention is only reported when the raw take's loudest voice-band
/// frames rise at least this far above its own voice-band floor. This is a
/// DYNAMIC-RANGE heuristic, not a voicing detector: it rejects steady noise,
/// but intermittent voice-band sources with pauses (keyboard typing, music,
/// a TV) can still pass and be labelled "voice". Hardening it needs a real
/// voicing feature tuned on real audio, deferred as a future improvement.
const MIN_SPEECH_DYNAMICS_DB: f32 = 10.0;
/// Fewer than ~2 s of audio: nothing is reportable.
const MIN_FRAMES: usize = 40;

/// Per-frame levels of one channel: full-band and voice-band RMS in dBFS.
struct FrameDb {
    full: f32,
    voice: f32,
}

/// Compute the two summary indicators from the raw and filtered 48 kHz mono
/// takes. Deterministic, pure, alignment-free. The two buffers need NOT be
/// time-aligned or the same length.
pub fn compute(raw: &[f32], filtered: &[f32]) -> SampleMetrics {
    let nr = raw.len() / FRAME_LEN;
    let nf = filtered.len() / FRAME_LEN;
    if nr < MIN_FRAMES || nf < MIN_FRAMES {
        return SampleMetrics::default();
    }
    let fft = FftPlanner::new().plan_fft_forward(FFT_LEN);
    let raw_db = analyze(&raw[..nr * FRAME_LEN], fft.as_ref());
    let filt_db = analyze(&filtered[..nf * FRAME_LEN], fft.as_ref());

    // Background: each stream's OWN full-band noise floor (low percentile),
    // filtered clamped to the realistic display floor before subtracting.
    let raw_floor = percentile(
        &raw_db.iter().map(|f| f.full).collect::<Vec<_>>(),
        FLOOR_PCT,
    );
    let filt_floor = percentile(
        &filt_db.iter().map(|f| f.full).collect::<Vec<_>>(),
        FLOOR_PCT,
    );
    let filt_floor_disp = filt_floor.max(SILENT_FLOOR_DBFS);

    // Voice: median voice-band level of each stream's loudest frames (the
    // speech-active portion). Comparing these distributions catches ducking
    // without needing the two streams aligned in time.
    let raw_voice: Vec<f32> = raw_db.iter().map(|f| f.voice).collect();
    let filt_voice: Vec<f32> = filt_db.iter().map(|f| f.voice).collect();
    let raw_speech = median_of_top(&raw_voice, SPEECH_TOP_FRAC);
    let filt_speech = median_of_top(&filt_voice, SPEECH_TOP_FRAC);
    let raw_voice_floor = percentile(&raw_voice, FLOOR_PCT);

    SampleMetrics {
        // Measurable only when the RAW take has speech dynamics AND the
        // FILTERED take actually carries voice-band signal above the silence
        // floor. Without the second half, a filtered leg that captured silence
        // (a stalled/mis-routed A/B capture) yields filt(−120) − raw(−12) and
        // prints a fabricated "Voice N dB quieter". Genuine over-suppression
        // still sits above SILENT_FLOOR_DBFS (see ducked_voice test).
        voice_measurable: raw_speech - raw_voice_floor >= MIN_SPEECH_DYNAMICS_DB
            && filt_speech > SILENT_FLOOR_DBFS,
        background_reduction_db: sanitize((raw_floor - filt_floor_disp).max(0.0)),
        raw_floor_dbfs: sanitize(raw_floor),
        filtered_floor_dbfs: sanitize(filt_floor_disp),
        voice_retention_db: sanitize(filt_speech - raw_speech),
        raw_speech_dbfs: sanitize(raw_speech),
        filtered_speech_dbfs: sanitize(filt_speech),
    }
}

/// Value at `pct` (0..100) of `vals`, ascending. Empty → DB_MIN.
fn percentile(vals: &[f32], pct: f32) -> f32 {
    if vals.is_empty() {
        return DB_MIN;
    }
    let mut v = vals.to_vec();
    v.sort_by(f32::total_cmp);
    let idx = ((pct / 100.0) * (v.len() - 1) as f32).round() as usize;
    v[idx.min(v.len() - 1)]
}

/// Median of the loudest `frac` (0..1) of `vals` — a stream's typical
/// speech level. Empty → DB_MIN.
fn median_of_top(vals: &[f32], frac: f32) -> f32 {
    if vals.is_empty() {
        return DB_MIN;
    }
    let mut v = vals.to_vec();
    v.sort_by(f32::total_cmp);
    let take = ((frac * v.len() as f32).round() as usize).clamp(1, v.len());
    median(v[v.len() - take..].to_vec())
}

/// Per-frame full-band RMS (time domain) and voice-band RMS (one-sided
/// Parseval over the 300 Hz–4 kHz FFT bins: E ≈ 2·Σ|X[k]|²/N, normalized to
/// the frame's sample count so both levels share the same dBFS reference).
fn analyze(samples: &[f32], fft: &dyn Fft<f32>) -> Vec<FrameDb> {
    let hz_per_bin = SAMPLE_RATE as f32 / FFT_LEN as f32;
    let k_lo = (VOICE_LO_HZ / hz_per_bin).ceil() as usize;
    let k_hi = (VOICE_HI_HZ / hz_per_bin).floor() as usize;
    let mut buf = vec![Complex::new(0.0f32, 0.0); FFT_LEN];
    let mut scratch = vec![Complex::new(0.0f32, 0.0); fft.get_inplace_scratch_len()];
    samples
        .chunks_exact(FRAME_LEN)
        .map(|frame| {
            let sum_sq: f64 = frame.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
            let full = power_db(sum_sq / FRAME_LEN as f64);
            for (b, &s) in buf.iter_mut().zip(frame) {
                *b = Complex::new(s, 0.0);
            }
            for b in &mut buf[FRAME_LEN..] {
                *b = Complex::new(0.0, 0.0);
            }
            fft.process_with_scratch(&mut buf, &mut scratch);
            let band: f64 = buf[k_lo..=k_hi]
                .iter()
                .map(|c| f64::from(c.norm_sqr()))
                .sum();
            let voice = power_db(2.0 * band / FFT_LEN as f64 / FRAME_LEN as f64);
            FrameDb { full, voice }
        })
        .collect()
}

/// Mean-square power → dB, floored so silence maps to DB_MIN, not −inf.
fn power_db(mean_sq: f64) -> f32 {
    if mean_sq <= 0.0 {
        return DB_MIN;
    }
    ((10.0 * mean_sq.log10()) as f32).max(DB_MIN)
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return DB_MIN;
    }
    v.sort_by(f32::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Final guarantee of the contract: metrics are finite and bounded.
fn sanitize(db: f32) -> f32 {
    if db.is_finite() {
        db.clamp(-200.0, 200.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic noise source (LCG, PCG-style multiplier); uniform in
    /// [-1, 1).
    struct Lcg(u64);

    impl Lcg {
        fn next_f32(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        }
    }

    /// White noise normalized to an exact RMS level in dBFS.
    fn noise(len: usize, rms_db: f32, seed: u64) -> Vec<f32> {
        let mut lcg = Lcg(seed);
        let mut v: Vec<f32> = (0..len).map(|_| lcg.next_f32()).collect();
        let rms = (v.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>() / len as f64).sqrt();
        let scale = f64::from(10.0f32.powf(rms_db / 20.0)) / rms;
        for s in &mut v {
            *s = (f64::from(*s) * scale) as f32;
        }
        v
    }

    /// 440+1320 Hz tone bursts (300 ms on / 300 ms off) at `speech_db` RMS
    /// over `noise_db` white noise: a crude but frame-consistent "speech
    /// over background". `noise_att`/`speech_att` scale the two parts to
    /// synthesize a filtered take.
    fn take(
        len: usize,
        speech_db: f32,
        noise_db: f32,
        seed: u64,
        speech_att_db: f32,
        noise_att_db: f32,
    ) -> Vec<f32> {
        let bg = noise(len, noise_db, seed);
        let comp_amp = f64::from(10.0f32.powf(speech_db / 20.0));
        let s_att = f64::from(10.0f32.powf(speech_att_db / 20.0));
        let n_att = f64::from(10.0f32.powf(noise_att_db / 20.0));
        let burst = 48_000 * 3 / 10;
        (0..len)
            .map(|i| {
                let voiced = (i / burst) % 2 == 0;
                let tone = if voiced {
                    let t = i as f64 / 48_000.0;
                    comp_amp
                        * ((std::f64::consts::TAU * 440.0 * t).sin()
                            + (std::f64::consts::TAU * 1320.0 * t).sin())
                } else {
                    0.0
                };
                (tone * s_att + f64::from(bg[i]) * n_att) as f32
            })
            .collect()
    }

    #[test]
    fn noise_only_is_not_voice_measurable() {
        let raw = noise(48_000 * 5, -30.0, 1);
        // Filtered = raw attenuated 20 dB.
        let filtered: Vec<f32> = raw.iter().map(|s| s * 0.1).collect();
        let m = compute(&raw, &filtered);
        // Steady noise has no speech dynamics → the voice card must not
        // fabricate a number.
        assert!(!m.voice_measurable, "{m:?}");
        // Background floor still drops ~20 dB (raw −30 → filtered −50).
        assert!(
            (m.background_reduction_db - 20.0).abs() < 2.0,
            "background {m:?}"
        );
        assert!((m.raw_floor_dbfs - -30.0).abs() < 2.0, "{m:?}");
    }

    #[test]
    fn preserved_voice_measures_near_zero_retention() {
        // Bursts −12 dBFS over −35 noise; filtered keeps bursts, noise −25.
        let raw = take(48_000 * 6, -12.0, -35.0, 7, 0.0, 0.0);
        let filtered = take(48_000 * 6, -12.0, -35.0, 7, 0.0, -25.0);
        let m = compute(&raw, &filtered);
        assert!(m.voice_measurable, "{m:?}");
        assert!(m.voice_retention_db.abs() < 2.0, "retention {m:?}");
        assert!(m.background_reduction_db > 10.0, "background {m:?}");
    }

    #[test]
    fn ducked_voice_shows_negative_retention() {
        // Filtered ducks the speech itself by 15 dB (over-suppression).
        let raw = take(48_000 * 6, -12.0, -35.0, 9, 0.0, 0.0);
        let filtered = take(48_000 * 6, -12.0, -35.0, 9, -15.0, -25.0);
        let m = compute(&raw, &filtered);
        assert!(m.voice_measurable, "{m:?}");
        assert!(m.voice_retention_db < -10.0, "retention {m:?}");
    }

    #[test]
    fn filtered_silence_is_not_voice_measurable() {
        // Real speech in the raw take but the FILTERED leg captured digital
        // silence (a stalled / mis-routed A/B capture on modern PipeWire). The
        // voice card must fall back to "not measurable" instead of fabricating
        // a huge "Voice N dB quieter" number from filt(−120) − raw(−12).
        let raw = take(48_000 * 6, -12.0, -35.0, 21, 0.0, 0.0);
        let filtered = vec![0.0f32; raw.len()];
        let m = compute(&raw, &filtered);
        assert!(
            !m.voice_measurable,
            "a silent filtered leg must not be voice-measurable: {m:?}"
        );
    }

    #[test]
    fn alignment_free_a_time_shift_does_not_change_retention() {
        // The whole point: filtered is raw rolled 4000 samples (~83 ms), the
        // exact lag the old per-frame code choked on. Distributions are
        // identical, so retention must be ~0 and background ~0.
        let raw = take(48_000 * 6, -12.0, -35.0, 11, 0.0, 0.0);
        let shift = 4000;
        let mut filtered = raw[shift..].to_vec();
        filtered.extend_from_slice(&raw[..shift]);
        let m = compute(&raw, &filtered);
        assert!(m.voice_measurable, "{m:?}");
        assert!(m.voice_retention_db.abs() < 1.0, "retention {m:?}");
        assert!(m.background_reduction_db < 1.0, "background {m:?}");
    }

    #[test]
    fn gated_filtered_floor_is_clamped_not_sub_silence() {
        // Filtered gated to near-digital-silence (the real DPDFNet pause
        // behaviour): the reduction must be bounded by the −75 dBFS display
        // floor, never a −134 dBFS sub-silence blowup.
        let raw = noise(48_000 * 4, -30.0, 13);
        let filtered = vec![1e-7f32; raw.len()];
        let m = compute(&raw, &filtered);
        assert_eq!(m.filtered_floor_dbfs, SILENT_FLOOR_DBFS, "{m:?}");
        assert!(
            (m.background_reduction_db - 45.0).abs() < 2.0,
            "background {m:?}"
        );
    }

    #[test]
    fn silence_and_short_and_empty_are_not_measurable() {
        for buf in [
            vec![0.0f32; 96_000],        // silence
            vec![0.5f32; FRAME_LEN * 3], // 150 ms < MIN_FRAMES
            Vec::new(),                  // empty
        ] {
            let m = compute(&buf, &buf);
            assert!(!m.voice_measurable, "{m:?}");
            for v in [
                m.background_reduction_db,
                m.raw_floor_dbfs,
                m.voice_retention_db,
            ] {
                assert!(v.is_finite(), "{m:?}");
            }
        }
    }
}
