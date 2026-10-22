//! Native engine (feature `native`): parity against ONNX through the full
//! Denoiser (STFT -> model -> iSTFT), engine selection, fallbacks, reset
//! determinism, and the vendored sources against their recorded checksums.
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

/// The native engine needs AVX2 and FMA; elsewhere its assertions become
/// fallback assertions.
fn cpu_has_native() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// A flag of the first CPU in /proc/cpuinfo.
fn cpu_flag(flag: &str) -> bool {
    std::fs::read_to_string("/proc/cpuinfo")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("flags"))
        .is_some_and(|l| l.split_whitespace().any(|f| f == flag))
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
    if !cpu_has_native() {
        eprintln!("skipping: CPU without AVX2/FMA");
        return;
    }
    for model in MODELS {
        let Some(mp) = assets(model) else { return };
        for fixture in FIXTURES {
            let noisy = read_flac(fixture);
            let reference = stream(&mut Denoiser::from_file(&mp).unwrap(), &noisy);
            let mut d = Denoiser::from_file_with(&mp, Inference::NativeInt8).unwrap();
            assert_eq!(
                d.engine(),
                Engine::NativeInt8,
                "{model}: {:?}",
                d.fallback_reason()
            );
            let out = stream(&mut d, &noisy);
            assert!(out.iter().all(|v| v.is_finite()));
            let snr = snr_db(&reference, &out);
            eprintln!("{model} {fixture}: SNR vs onnx {snr:.1} dB");
            // W7A8 is lossy by design: reported, and gated only against
            // breakage. Measured 35.4 to 38.8 dB on these fixtures (EPYC
            // Zen 2, ONNX Runtime 1.27; the earlier INT8 runtime gave 39 to
            // 45 dB, so its 35 dB floor no longer leaves a margin). The
            // floor sits 5 dB below the lowest value, for the reference
            // side's ISA-dependent rounding; a real break (state, weights,
            // alignment) lands far below it.
            assert!(snr > 30.0, "{model} {fixture}: {snr:.1} dB");
        }
    }
}

#[test]
fn auto_picks_native_where_the_cpu_has_it() {
    for model in MODELS {
        let Some(mp) = assets(model) else { return };
        let d = Denoiser::from_file_with(&mp, Inference::Auto).unwrap();
        if cpu_has_native() {
            assert_eq!(d.engine(), Engine::NativeInt8);
            assert_eq!(d.engine().to_string(), "native w7a8");
            assert_eq!(d.fallback_reason(), None);
            // The runtime picks AVX-VNNI kernels only where the CPU has the
            // instructions; never claim them on a CPU without.
            let kernels = d.native_kernels().unwrap();
            assert!(kernels == "avx2" || kernels == "avx-vnni", "{kernels}");
            if !cpu_flag("avx_vnni") {
                assert_eq!(kernels, "avx2");
            }
        } else {
            assert_eq!(d.native_kernels(), None);
            assert_eq!(d.engine(), Engine::Onnx);
            let why = d.fallback_reason().unwrap();
            assert!(why.contains("AVX2") || why.contains("not built"), "{why}");
        }
    }
}

#[test]
fn processing_fits_in_a_one_mib_stack() {
    // Upstream asks for at least 1 MiB of stack on the processing thread
    // (kernel scratch lives there). Run real hops on exactly that much, so
    // a future runtime that needs more fails here, not in the plugin.
    if !cpu_has_native() {
        return;
    }
    for model in MODELS {
        let Some(mp) = assets(model) else { return };
        let mut d = Denoiser::from_file_with(&mp, Inference::Auto).unwrap();
        assert_eq!(d.engine(), Engine::NativeInt8, "{:?}", d.fallback_reason());
        let noisy = read_flac("noisy_public_48k.flac");
        let clip = noisy[..50 * HOP].to_vec();
        let out = std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(move || stream(&mut d, &clip))
            .unwrap()
            .join()
            .expect("processing on a 1 MiB stack");
        assert!(out.iter().all(|v| v.is_finite()), "{model}");
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
fn reset_and_fresh_instances_are_deterministic() {
    if !cpu_has_native() {
        return;
    }
    for model in MODELS {
        let Some(mp) = assets(model) else { return };
        let noisy = read_flac("noisy_keyboard_48k.flac");
        let clip = &noisy[..300 * HOP];
        let mut d = Denoiser::from_file_with(&mp, Inference::NativeInt8).unwrap();
        assert_eq!(d.engine(), Engine::NativeInt8);
        let first = stream(&mut d, clip);
        d.reset();
        let again = stream(&mut d, clip);
        let fresh = stream(
            &mut Denoiser::from_file_with(&mp, Inference::NativeInt8).unwrap(),
            clip,
        );
        assert_eq!(bits(&first), bits(&again), "{model}: reset");
        assert_eq!(bits(&first), bits(&fresh), "{model}: fresh");
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
#[allow(deprecated)]
fn an_fp32_request_runs_onnx_and_says_why() {
    let Some(mp) = assets(MODELS[1]) else { return };
    let mut d = Denoiser::from_file_with(&mp, Inference::NativeFp32).unwrap();
    assert_onnx_fallback(&mp, &mut d, "native fp32 is not in this runtime");
}

#[test]
fn missing_damaged_or_truncated_weights_fall_back_to_onnx() {
    if !cpu_has_native() {
        return; // the CPU check comes first
    }
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
    let mut d = Denoiser::from_file_with(&onnx, Inference::Auto).unwrap();
    assert_onnx_fallback(&mp, &mut d, "do not match the expected checksum");

    std::fs::write(&weights, &bytes[..bytes.len() - 4]).unwrap();
    let mut d = Denoiser::from_file_with(&onnx, Inference::NativeInt8).unwrap();
    assert_onnx_fallback(&mp, &mut d, "wrong size");

    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn a_model_is_recognized_by_content_not_by_name() {
    if !cpu_has_native() {
        return;
    }
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
    let mut d = Denoiser::from_file_with(&onnx, Inference::Auto).unwrap();
    assert_onnx_fallback(&light, &mut d, &format!("{}.weights.f32", MODELS[1]));
    std::fs::remove_dir_all(&tmp).unwrap();
}

fn native_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("native")
}

/// `sha256sum` lines of a file under native/, as (digest, relative path).
fn sums(file: &str) -> Vec<(String, String)> {
    std::fs::read_to_string(native_dir().join(file))
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
fn vendored_sources_are_unmodified() {
    // UPSTREAM.sha256 was taken from the upstream checkout; every vendored
    // file except the README is listed, and nothing else is there.
    let listed = sums("UPSTREAM.sha256");
    for (sum, rel) in &listed {
        assert_eq!(&sha256_file(&native_dir().join(rel)), sum, "{rel}");
    }
    let mut present = Vec::new();
    let mut dirs = vec![native_dir()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                dirs.push(p);
            } else {
                let rel = p.strip_prefix(native_dir()).unwrap();
                present.push(rel.to_str().unwrap().to_string());
            }
        }
    }
    present.retain(|r| r != "README.md" && r != "UPSTREAM.sha256");
    present.sort();
    let mut names: Vec<String> = listed.into_iter().map(|(_, r)| r).collect();
    names.sort();
    assert_eq!(present, names);
}

#[test]
fn weight_pins_match_upstream_checksums() {
    let script = std::fs::read_to_string(repo_root().join("scripts/setup-assets.sh")).unwrap();
    let pins = sums("models/SHA256SUMS");
    assert_eq!(pins.len(), 2);
    for (sum, rel) in pins {
        let model = rel
            .strip_prefix("models/")
            .and_then(|r| r.strip_suffix("/weights.f32"))
            .expect("models/<model>/weights.f32");
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
