//! The strength (attenuation limit) is a blend after the model: the output
//! is `a * noisy + (1 - a) * enhanced`, `a = 10^(-dB/20)`, with the noisy
//! reference delayed by exactly the latency through the same synthesis. So
//! audio cleaned at one strength, with its input, gives it at any other
//! without the model: `out' = in + (1 - a') / (1 - a) * (out - in)`. The
//! app's Mic check re-renders a replay this way when the strength moves;
//! this pins it against the real engine.

mod common;

use common::dev_denoiser;
use hushmic_denoiser::{HOP, LATENCY_SAMPLES};

fn alpha(db: f32) -> f32 {
    10f32.powf(-db / 20.0).min(1.0)
}

/// Speech-like tone bursts over steady noise (a fixed generator: the same
/// input for every run).
fn input(len: usize) -> Vec<f32> {
    let mut seed = 0x2545_f491_u32;
    (0..len)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed as f32 / u32::MAX as f32 - 0.5) * 0.05;
            let t = i as f32 / 48_000.0;
            let on = (t * 2.0).fract() < 0.6;
            let voice = if on {
                0.2 * (t * 220.0 * std::f32::consts::TAU).sin()
                    + 0.1 * (t * 660.0 * std::f32::consts::TAU).sin()
            } else {
                0.0
            };
            voice + noise
        })
        .collect()
}

fn clean(model: &str, db: f32, input: &[f32]) -> Option<Vec<f32>> {
    let mut den = dev_denoiser(model)?;
    den.set_attenuation_limit_db(db);
    let mut out = Vec::with_capacity(input.len());
    for hop in input.as_chunks::<HOP>().0 {
        let mut o = [0f32; HOP];
        let _ = den.process_hop(hop, &mut o);
        out.extend_from_slice(&o);
    }
    Some(out)
}

#[test]
fn a_take_cleaned_at_one_strength_gives_any_other() {
    let model = "dpdfnet2_48khz_hr.onnx";
    let raw = input(150 * HOP);
    let (Some(at12), Some(at24), Some(at6)) = (
        clean(model, 12.0, &raw),
        clean(model, 24.0, &raw),
        clean(model, 6.0, &raw),
    ) else {
        eprintln!("skipping: {model} not provisioned");
        return;
    };
    // the input as the output's moments (the tap's clean_lag)
    let lag = LATENCY_SAMPLES;
    let from = lag + 20 * HOP;
    for (want, db) in [(&at24, 24.0), (&at6, 6.0)] {
        let k = (1.0 - alpha(db)) / (1.0 - alpha(12.0));
        let (mut err, mut sig) = (0f64, 0f64);
        for i in from..raw.len() {
            let r = raw[i - lag];
            let got = r + k * (at12[i] - r);
            err += f64::from(got - want[i]).powi(2);
            sig += f64::from(want[i]).powi(2);
        }
        let snr = 10.0 * (sig / err.max(1e-30)).log10();
        assert!(snr > 60.0, "{db} dB from 12 dB: {snr:.1} dB");
    }
}
