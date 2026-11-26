//! One pass serves every strength: the Maximum pass mixed with the original
//! equals the engine run at Gentle, Medium and Strong, to float rounding.
//! Self-skips without the repo's models (scripts/setup-assets.sh), unless
//! HUSHMIC_ASSERT_ASSETS=1.

use hushmic::appstate::Strength;
use hushmic::files::decode::Decoder;
use hushmic::files::engine::{init_runtime, EngineSpec};
use hushmic::files::mix::{alpha, Blend};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The engine over the whole of `x` at `attn_db`, latency taken off.
fn render(spec: &EngineSpec, x: &[f32], attn_db: Option<f32>) -> Vec<f32> {
    let mut e = spec.build().unwrap();
    if let Some(db) = attn_db {
        e.denoiser_mut().set_attenuation_limit_db(db);
    }
    let lat = e.denoiser().latency_samples();
    let mut y = e.process(x).to_vec();
    let z = vec![0f32; e.pending()];
    y.extend_from_slice(e.process(&z));
    y.drain(..lat);
    y.truncate(x.len());
    y
}

#[test]
fn the_maximum_pass_mixed_with_the_original_is_every_strength() {
    let models = root().join("assets/models");
    let rt = root().join("assets/lib/libonnxruntime.so");
    if !(models.join("dpdfnet2_48khz_hr.onnx").exists() && rt.exists()) {
        assert!(
            std::env::var("HUSHMIC_ASSERT_ASSETS").as_deref() != Ok("1"),
            "assets missing"
        );
        eprintln!("SKIP: models not provisioned");
        return;
    }
    init_runtime(&rt).unwrap();
    let spec = EngineSpec::new(
        &models,
        "dpdfnet2_48khz_hr",
        hushmic_denoiser::Inference::Auto,
    );
    let mut d = Decoder::open(&root().join("tests/fixtures/noisy_public_48k.flac"), None).unwrap();
    let mut x = Vec::new();
    while let Some(b) = d.next_block().unwrap() {
        x.extend_from_slice(&b[0]);
    }
    let max = render(&spec, &x, None);
    // The plugin's limiter has no original to blend for its first four
    // frames (its delay line fills): those 40 ms are the plain output.
    let from = 4 * hushmic_denoiser::HOP;
    for s in [Strength::Gentle, Strength::Medium, Strength::Strong] {
        let db = s.attn_limit().unwrap();
        let direct = render(&spec, &x, Some(db));
        let mixed = Blend::new(alpha(db)).mix(std::slice::from_ref(&max), std::slice::from_ref(&x));
        let (mut e, mut p) = (0f64, 0f64);
        for (a, b) in mixed[0][from..].iter().zip(&direct[from..]) {
            e += (*a as f64 - *b as f64).powi(2);
            p += (*b as f64).powi(2);
        }
        let rel = 10.0 * (e / p).log10();
        eprintln!("{s:?}: the mix is {rel:.0} dB from the direct render");
        assert!(rel < -100.0, "{s:?}: {rel:.1} dB");
    }
}
