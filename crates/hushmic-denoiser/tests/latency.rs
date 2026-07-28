//! Measured latency pins: the declared latency is a structural constant —
//! 480 samples of STFT framing plus the models' 4-hop group delay (1920).
//! These tests MEASURE the DSP rather than trusting arithmetic; if a model or
//! DSP change shifts the real latency, they fail and force LATENCY_SAMPLES to
//! be re-derived. (The framing half alone is pinned by the STFT unit tests;
//! the plugin's one-hop output prefill on top is asserted via the hushmic
//! crate's conf-test constant, 2880 = 2400 + 480.)

mod common;

use common::{dev_denoiser, repo_root};
use hushmic_denoiser::{Mode, HOP, LATENCY_SAMPLES};

fn argmax(out: &[f32]) -> (usize, f32) {
    let mut best = (0usize, 0f32);
    for (i, &v) in out.iter().enumerate() {
        if v.abs() > best.1 {
            best = (i, v.abs());
        }
    }
    best
}

/// The bypass path (delayed-noisy ring through the same synthesis) is a
/// deterministic delay line: an impulse comes out bit-strength intact,
/// exactly LATENCY_SAMPLES later. This is also the time-domain proof that
/// the ring alignment matches the framing.
#[test]
fn bypass_path_delay_is_exact() {
    for model in ["dpdfnet8_48khz_hr.onnx", "dpdfnet2_48khz_hr.onnx"] {
        let Some(mut den) = dev_denoiser(model) else {
            eprintln!("skipping bypass_path_delay_is_exact: {model} not provisioned");
            continue;
        };
        assert_eq!(den.latency_samples(), LATENCY_SAMPLES);
        den.set_mode(Mode::Bypass);
        let total = 80 * HOP;
        let impulse_at = 40 * HOP + 123;
        let mut out = Vec::with_capacity(total);
        for h in 0..(total / HOP) {
            let mut hop_in = [0f32; HOP];
            for (j, v) in hop_in.iter_mut().enumerate() {
                if h * HOP + j == impulse_at {
                    *v = 1.0;
                }
            }
            let mut hop_out = [0f32; HOP];
            let _ = den.process_hop(&hop_in, &mut hop_out);
            out.extend_from_slice(&hop_out);
        }
        let (peak, amp) = argmax(&out);
        assert_eq!(
            peak - impulse_at,
            LATENCY_SAMPLES,
            "{model}: bypass delay must be exactly {LATENCY_SAMPLES} samples"
        );
        assert!(
            (amp - 1.0).abs() < 1e-3,
            "{model}: bypass must pass the impulse intact, got {amp}"
        );
    }
}

fn read_flac_mono_f32(p: &std::path::Path) -> Vec<f32> {
    let mut r = claxon::FlacReader::open(p).expect("open flac");
    r.samples()
        .map(|s| s.expect("flac sample") as f32 / 32768.0)
        .collect()
}

/// The neural path's group delay, measured with REAL noisy speech (a
/// synthetic probe gets eaten by the suppressor): the cleaned output must
/// cross-correlate with the input within a few samples of LATENCY_SAMPLES.
/// This is what makes the declared constant a measurement, not arithmetic.
#[test]
fn enhanced_path_group_delay_matches_bypass() {
    let fixture = repo_root().join("tests/fixtures/noisy_public_48k.flac");
    for model in ["dpdfnet8_48khz_hr.onnx", "dpdfnet2_48khz_hr.onnx"] {
        let Some(mut den) = dev_denoiser(model) else {
            eprintln!("skipping enhanced_path_group_delay: {model} not provisioned");
            continue;
        };
        let input = read_flac_mono_f32(&fixture);
        let total = (input.len() / HOP) * HOP;
        let mut out = vec![0f32; total];
        for h in 0..(total / HOP) {
            let mut hop_in = [0f32; HOP];
            hop_in.copy_from_slice(&input[h * HOP..(h + 1) * HOP]);
            let mut hop_out = [0f32; HOP];
            let _ = den.process_hop(&hop_in, &mut hop_out);
            out[h * HOP..(h + 1) * HOP].copy_from_slice(&hop_out);
        }
        // bounded window keeps debug-mode runtime sane; 8 s of speech is
        // plenty for an unambiguous correlation peak
        let skip = 20 * HOP;
        let n = (8 * 48_000).min(total - skip - 6 * HOP);
        let mut best = (0usize, f64::MIN);
        for lag in 0..(6 * HOP) {
            let mut acc = 0f64;
            for i in (skip..(skip + n)).step_by(2) {
                acc += (input[i] as f64) * (out[i + lag] as f64);
            }
            if acc > best.1 {
                best = (lag, acc);
            }
        }
        let eout: f64 = out[skip..skip + n]
            .iter()
            .map(|v| (*v as f64).powi(2))
            .sum();
        assert!(
            eout > 1.0,
            "{model}: cleaned output carries no energy — bad probe"
        );
        let lag = best.0 as isize;
        assert!(
            (lag - LATENCY_SAMPLES as isize).abs() <= 8,
            "{model}: enhanced-path group delay {lag} drifted from {LATENCY_SAMPLES}"
        );
    }
}
