use dpdfnet_ladspa::engine::Engine;
use dpdfnet_ladspa::stft::HOP;
use std::path::PathBuf;

fn read_wav_mono_f32(p: &str) -> Vec<f32> {
    let mut r = hound::WavReader::open(p).expect("open wav");
    let spec = r.spec();
    let s: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|x| x.unwrap()).collect(),
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|x| x.unwrap() as f32 / max)
                .collect()
        }
    };
    if spec.channels == 1 {
        s
    } else {
        s.iter().step_by(spec.channels as usize).copied().collect()
    }
}

fn pearson(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let ma = a.iter().sum::<f32>() / n as f32;
    let mb = b.iter().sum::<f32>() / n as f32;
    let mut num = 0f64;
    let mut da = 0f64;
    let mut db = 0f64;
    for i in 0..n {
        let (x, y) = ((a[i] - ma) as f64, (b[i] - mb) as f64);
        num += x * y;
        da += x * x;
        db += y * y;
    }
    (num / (da.sqrt() * db.sqrt())) as f32
}

fn read_flac_mono_f32(p: &std::path::Path) -> Vec<f32> {
    let mut r = claxon::FlacReader::open(p).expect("open flac");
    let info = r.streaminfo();
    assert_eq!(info.sample_rate, 48_000, "fixture must be 48 kHz");
    assert_eq!(info.channels, 1, "fixture must be mono");
    assert_eq!(info.bits_per_sample, 16, "fixture must be int16");
    r.samples()
        .map(|s| s.expect("flac sample") as f32 / 32768.0)
        .collect()
}

/// End-to-end audio-path pin that runs EVERYWHERE, including CI: the fixtures
/// are small int16 FLACs committed to the repo, built from public sources
/// (LibriVox voice + Commons fan noise) by scripts/gen-parity-fixtures.py, and
/// the golden is the validated engine's own streaming output over them. Both
/// streams share the engine's inherent latency, so they align 1:1 (unlike the
/// offline golden below, which needs a one-hop shift). A passthrough or
/// state-desync bug scores <= ~0.1 here (the fan bed dominates the noisy
/// input at any alignment), so 0.99 has huge margin while still tolerating
/// cross-CPU float differences in ORT.
#[test]
fn matches_committed_public_golden() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let model = root.join("assets/models/dpdfnet8_48khz_hr.onnx");
    if !model.exists() {
        // bare checkout without scripts/setup-assets.sh; CI always provisions
        eprintln!("skipping matches_committed_public_golden: model not provisioned");
        return;
    }
    let noisy = read_flac_mono_f32(&root.join("tests/fixtures/noisy_public_48k.flac"));
    let golden = read_flac_mono_f32(&root.join("tests/fixtures/golden_public_dpdfnet8.flac"));

    let mut eng = Engine::new(&model).expect("engine");
    let mut out = Vec::with_capacity(noisy.len());
    let mut hop_in = [0f32; HOP];
    let mut hop_out = [0f32; HOP];
    for h in 0..noisy.len() / HOP {
        hop_in.copy_from_slice(&noisy[h * HOP..(h + 1) * HOP]);
        eng.process_hop(&hop_in, &mut hop_out).expect("process");
        out.extend_from_slice(&hop_out);
    }
    let skip = 4 * HOP; // STFT/OLA + model-state warm-up
    let corr = pearson(&out[skip..], &golden[skip..]);
    eprintln!("public parity correlation vs committed golden: {corr}");
    assert!(
        corr > 0.99,
        "engine output correlation vs committed golden too low: {corr}"
    );
}

#[test]
fn matches_golden_reference() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let model = root.join("assets/models/dpdfnet8_48khz_hr.onnx");
    let noisy_path = root.join("tests/fixtures/noisy_fan_48k.wav");
    let golden_path = root.join("tests/fixtures/golden_fan_dpdfnet8.wav");

    // The golden reference and its noisy input are large (~12 MB) binary WAVs kept
    // out of the repo (gitignored, not published, not fetched by setup-assets.sh),
    // so a clean checkout (e.g. CI) has neither. Skip rather than fail there: the
    // engine-load + ONNX inference path is still covered by model::tests in CI, and
    // this golden-correlation check runs wherever the fixtures are provisioned.
    if !model.exists() || !noisy_path.exists() || !golden_path.exists() {
        eprintln!(
            "skipping matches_golden_reference: model/fixtures not provisioned \
             (local-only golden-parity test)"
        );
        return;
    }

    let noisy = read_wav_mono_f32(noisy_path.to_str().unwrap());
    let golden = read_wav_mono_f32(golden_path.to_str().unwrap());

    let mut eng = Engine::new(&model).expect("engine");
    let mut out = Vec::with_capacity(noisy.len());
    let mut hop_in = [0f32; HOP];
    let mut hop_out = [0f32; HOP];
    let hops = noisy.len() / HOP;
    for h in 0..hops {
        hop_in.copy_from_slice(&noisy[h * HOP..(h + 1) * HOP]);
        eng.process_hop(&hop_in, &mut hop_out).expect("process");
        out.extend_from_slice(&hop_out);
    }
    // Skip the first 4 hops on both streams for STFT/OLA warm-up. The causal engine
    // uses an N_FFT/2 leading-pad analysis (center=True equivalent, verified by
    // stft_cola.rs as exactly one hop of pass-through delay); the *offline* golden
    // reference's istft trims that pad, so the engine output lags the golden by
    // exactly one hop. Advance the engine stream by that one-hop latency before
    // correlating. Expect near-identical output (rustfft vs numpy fft only).
    let skip = 4 * HOP;
    let latency = HOP; // causal STFT center-pad vs the offline (trimmed) golden
    let corr = pearson(&out[skip + latency..], &golden[skip..]);
    eprintln!("parity correlation vs golden: {corr}");
    assert!(
        corr > 0.99,
        "engine output correlation vs golden too low: {corr}"
    );
}
