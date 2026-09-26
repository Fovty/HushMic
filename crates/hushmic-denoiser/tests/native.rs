//! Native engine (feature `native`): parity against ONNX through the full
//! Denoiser (STFT -> model -> iSTFT), engine selection, fallbacks, reset
//! determinism, and the vendored sources against upstream's checksums.
//! The asset-driven tests need the dev models plus their
//! `<model>.weights.f32` (scripts/setup-assets.sh) and self-skip without.

mod common;

use common::{init_dev_runtime, model_path, repo_root};
use hushmic_denoiser::{Denoiser, Engine, Inference, HOP};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const MODELS: [&str; 2] = ["dpdfnet8_48khz_hr", "dpdfnet2_48khz_hr"];
const FIXTURES: [&str; 3] = [
    "noisy_public_48k.flac",
    "noisy_cafe_48k.flac",
    "noisy_keyboard_48k.flac",
];
/// STFT/OLA + model-state warm-up, as in the parity suite.
const SKIP: usize = 4 * HOP;

/// The model path with its weights beside it, runtime up; None to skip.
fn assets(model: &str) -> Option<PathBuf> {
    let mp = model_path(&format!("{model}.onnx"))?;
    let weights = mp.with_file_name(format!("{model}.weights.f32"));
    if !weights.exists() {
        if std::env::var("HUSHMIC_ASSERT_ASSETS").as_deref() == Ok("1") {
            panic!("{} missing but HUSHMIC_ASSERT_ASSETS=1", weights.display());
        }
        eprintln!("skipping: native weights not provisioned");
        return None;
    }
    init_dev_runtime()?;
    Some(mp)
}

/// INT8 needs AVX2 and FMA; elsewhere the INT8 assertions become fallback
/// assertions.
fn cpu_has_int8() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

fn read_flac(name: &str) -> Vec<f32> {
    let mut r =
        claxon::FlacReader::open(repo_root().join("tests/fixtures").join(name)).expect("open flac");
    r.samples()
        .map(|s| s.expect("flac sample") as f32 / 32768.0)
        .collect()
}

fn stream(d: &mut Denoiser, input: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(input.len());
    let mut hop_in = [0f32; HOP];
    let mut hop_out = [0f32; HOP];
    for h in 0..input.len() / HOP {
        hop_in.copy_from_slice(&input[h * HOP..(h + 1) * HOP]);
        d.process_hop(&hop_in, &mut hop_out).expect("process");
        out.extend_from_slice(&hop_out);
    }
    out
}

/// Waveform agreement of `x` with `reference`, in dB.
fn snr_db(reference: &[f32], x: &[f32]) -> f64 {
    let (mut sig, mut err) = (0f64, 0f64);
    for (r, v) in reference[SKIP..].iter().zip(&x[SKIP..]) {
        sig += (*r as f64).powi(2);
        err += (*r as f64 - *v as f64).powi(2);
    }
    if err == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (sig / err).log10()
}

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

/// A scratch directory unique to this test.
fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hushmic-native-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn native_matches_onnx_through_the_full_denoiser() {
    for model in MODELS {
        let Some(mp) = assets(model) else { return };
        for fixture in FIXTURES {
            let noisy = read_flac(fixture);
            let reference = stream(&mut Denoiser::from_file(&mp).unwrap(), &noisy);
            let mut engines = vec![(Inference::NativeFp32, Engine::NativeFp32, 70.0)];
            if cpu_has_int8() {
                // INT8 is lossy by design: reported, and gated only against
                // breakage (measured about 39 to 45 dB).
                engines.push((Inference::NativeInt8, Engine::NativeInt8, 35.0));
            }
            for (inference, engine, floor) in engines {
                let mut d = Denoiser::from_file_with(&mp, inference).unwrap();
                assert_eq!(d.engine(), engine, "{model}: {:?}", d.fallback_reason());
                let out = stream(&mut d, &noisy);
                assert!(out.iter().all(|v| v.is_finite()));
                let snr = snr_db(&reference, &out);
                eprintln!("{model} {fixture} {engine}: SNR vs onnx {snr:.1} dB");
                assert!(snr > floor, "{model} {fixture} {engine}: {snr:.1} dB");
            }
        }
    }
}

#[test]
fn auto_picks_int8_where_the_cpu_has_it() {
    for model in MODELS {
        let Some(mp) = assets(model) else { return };
        let d = Denoiser::from_file_with(&mp, Inference::Auto).unwrap();
        if cpu_has_int8() {
            assert_eq!(d.engine(), Engine::NativeInt8);
            assert_eq!(d.fallback_reason(), None);
        } else {
            assert_eq!(d.engine(), Engine::Onnx);
            assert!(d.fallback_reason().unwrap().contains("AVX2"));
        }
    }
}

#[test]
fn onnx_on_request_and_by_default() {
    let Some(mp) = assets(MODELS[1]) else { return };
    let d = Denoiser::from_file_with(&mp, Inference::Onnx).unwrap();
    assert_eq!(d.engine(), Engine::Onnx);
    assert_eq!(d.fallback_reason(), None);
    let d = Denoiser::from_file(&mp).unwrap();
    assert_eq!(d.engine(), Engine::Onnx);
    assert_eq!(d.fallback_reason(), None);
}

#[test]
fn native_seeds_the_same_initial_state_as_onnx() {
    // The first hops of silence depend only on the initial state; a wrong
    // normalization seed would show up as a large difference here.
    for model in MODELS {
        let Some(mp) = assets(model) else { return };
        let silence = vec![0f32; 20 * HOP];
        let a = stream(&mut Denoiser::from_file(&mp).unwrap(), &silence);
        let mut n = Denoiser::from_file_with(&mp, Inference::NativeFp32).unwrap();
        let b = stream(&mut n, &silence);
        let max = a
            .iter()
            .zip(&b)
            .map(|(x, y)| (x - y).abs())
            .fold(0f32, f32::max);
        assert!(max < 1e-5, "{model}: max diff {max}");
    }
}

#[test]
fn reset_and_fresh_instances_are_deterministic() {
    for model in MODELS {
        let Some(mp) = assets(model) else { return };
        let noisy = read_flac("noisy_keyboard_48k.flac");
        let clip = &noisy[..300 * HOP];
        for inference in [Inference::NativeFp32, Inference::NativeInt8] {
            let mut d = Denoiser::from_file_with(&mp, inference).unwrap();
            let first = stream(&mut d, clip);
            d.reset();
            let again = stream(&mut d, clip);
            let fresh = stream(&mut Denoiser::from_file_with(&mp, inference).unwrap(), clip);
            assert_eq!(bits(&first), bits(&again), "{model} {inference:?}: reset");
            assert_eq!(bits(&first), bits(&fresh), "{model} {inference:?}: fresh");
        }
    }
}

/// A fallback must be the plain ONNX engine, bit for bit, with its reason.
fn assert_onnx_fallback(reference: &Path, d: &mut Denoiser, why: &str) {
    assert_eq!(d.engine(), Engine::Onnx);
    let reason = d.fallback_reason().expect("a fallback carries its reason");
    assert!(reason.contains(why), "{reason}");
    let noisy = read_flac("noisy_public_48k.flac");
    let clip = &noisy[..100 * HOP];
    let a = stream(&mut Denoiser::from_file(reference).unwrap(), clip);
    assert_eq!(bits(&a), bits(&stream(d, clip)));
}

#[test]
fn missing_damaged_or_truncated_weights_fall_back_to_onnx() {
    let model = MODELS[1];
    let Some(mp) = assets(model) else { return };
    let tmp = scratch("weights");
    let onnx = tmp.join(format!("{model}.onnx"));
    std::os::unix::fs::symlink(&mp, &onnx).unwrap();
    let weights = tmp.join(format!("{model}.weights.f32"));

    let mut d = Denoiser::from_file_with(&onnx, Inference::Auto).unwrap();
    assert_onnx_fallback(&mp, &mut d, "native weights not installed");

    // Right size, one flipped bit: the checksum must catch it.
    let mut bytes = std::fs::read(mp.with_file_name(format!("{model}.weights.f32"))).unwrap();
    bytes[1000] ^= 0x01;
    std::fs::write(&weights, &bytes).unwrap();
    let mut d = Denoiser::from_file_with(&onnx, Inference::NativeFp32).unwrap();
    assert_onnx_fallback(&mp, &mut d, "do not match the expected checksum");

    std::fs::write(&weights, &bytes[..bytes.len() - 4]).unwrap();
    let mut d = Denoiser::from_file_with(&onnx, Inference::NativeInt8).unwrap();
    assert_onnx_fallback(&mp, &mut d, "wrong size");

    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn a_model_is_recognized_by_content_not_by_name() {
    // The light model under the quality model's file name: recognized as
    // the light model, so it wants the light model's weights.
    let Some(light) = assets(MODELS[1]) else {
        return;
    };
    let Some(quality) = assets(MODELS[0]) else {
        return;
    };
    let tmp = scratch("rename");
    let onnx = tmp.join(format!("{}.onnx", MODELS[0]));
    std::os::unix::fs::symlink(&light, &onnx).unwrap();
    std::os::unix::fs::symlink(
        quality.with_file_name(format!("{}.weights.f32", MODELS[0])),
        tmp.join(format!("{}.weights.f32", MODELS[0])),
    )
    .unwrap();
    let mut d = Denoiser::from_file_with(&onnx, Inference::NativeFp32).unwrap();
    assert_onnx_fallback(&light, &mut d, &format!("{}.weights.f32", MODELS[1]));
    std::fs::remove_dir_all(&tmp).unwrap();
}

/// `SHA256SUMS` lines as (digest, relative path).
fn upstream_sums() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("native/artifacts/v1");
    std::fs::read_to_string(dir.join("SHA256SUMS"))
        .unwrap()
        .lines()
        .map(|l| {
            let (sum, rel) = l.split_once("  ").expect("sha256sum format");
            (sum.to_string(), rel.to_string())
        })
        .collect()
}

fn sha256_file(p: &Path) -> String {
    Sha256::digest(std::fs::read(p).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn vendored_sources_match_upstream_checksums() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("native/artifacts/v1");
    let sums = upstream_sums();
    assert_eq!(sums.len(), 8, "upstream lists 8 files");
    let mut checked = 0;
    for (sum, rel) in &sums {
        if rel.ends_with("weights.f32") {
            continue; // not vendored; see the next test
        }
        assert_eq!(&sha256_file(&dir.join(rel)), sum, "{rel}");
        checked += 1;
    }
    assert_eq!(checked, 6);
}

#[test]
fn weight_pins_match_upstream_checksums() {
    let script = std::fs::read_to_string(repo_root().join("scripts/setup-assets.sh")).unwrap();
    for (sum, rel) in upstream_sums() {
        let Some(model) = rel.strip_suffix("/weights.f32") else {
            continue;
        };
        assert!(
            script.contains(&format!("[{model}]=\"{sum}\"")),
            "setup-assets.sh must pin {model} weights to {sum}"
        );
        if let Some(mp) = model_path(&format!("{model}.onnx")) {
            let w = mp.with_file_name(format!("{model}.weights.f32"));
            if w.exists() {
                assert_eq!(sha256_file(&w), sum, "{}", w.display());
            }
        }
    }
}
