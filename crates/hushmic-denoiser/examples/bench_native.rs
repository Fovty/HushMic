//! Dev probe (feature `native`): load time and full Denoiser hop cost
//! (STFT + model + iSTFT) per engine — ONNX, native fp32 and native int8 —
//! for both models. Rounds rotate the engine order so drift (thermal,
//! neighbours) spreads evenly. One `load ...` and one `bench ...` line per
//! model and engine, for scripts.
//!
//! Env: HUSHMIC_MODEL_DIR (default: the repo's assets/models, which holds
//! the weights too), HUSHMIC_BENCH_HOPS (timed hops per engine and round,
//! default 500), HUSHMIC_BENCH_ROUNDS (default 3), HUSHMIC_BENCH_PACED=1
//! (one hop per 10 ms, like the live path, instead of back to back),
//! HUSHMIC_BENCH_FLAC (mono 48 kHz input, looped; default a sine),
//! HUSHMIC_BENCH_MODELS (comma list of file stems).

use hushmic_denoiser::{Denoiser, Inference, HOP};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    if std::env::var("ORT_DYLIB_PATH").is_err() {
        let bundled = root.join("assets/lib/libonnxruntime.so");
        if bundled.exists() {
            hushmic_denoiser::init_runtime(&bundled).unwrap();
        }
    }
    let model_dir = std::env::var_os("HUSHMIC_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("assets/models"));
    let hops: usize = env_or("HUSHMIC_BENCH_HOPS", 500);
    let rounds: usize = env_or("HUSHMIC_BENCH_ROUNDS", 3);
    let paced = std::env::var("HUSHMIC_BENCH_PACED").as_deref() == Ok("1");
    let models = std::env::var("HUSHMIC_BENCH_MODELS")
        .unwrap_or_else(|_| "dpdfnet8_48khz_hr,dpdfnet2_48khz_hr".into());
    let audio: Vec<f32> = match std::env::var("HUSHMIC_BENCH_FLAC") {
        Ok(p) => claxon::FlacReader::open(p)
            .unwrap()
            .samples()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect(),
        Err(_) => (0..HOP).map(|i| ((i as f32) * 0.37).sin() * 0.1).collect(),
    };

    let engines = [
        Inference::Onnx,
        Inference::NativeFp32,
        Inference::NativeInt8,
    ];
    for model in models.split(',') {
        let path = model_dir.join(format!("{model}.onnx"));
        let mut dens: Vec<(Inference, Denoiser)> = engines
            .iter()
            .map(|&inference| {
                let t = Instant::now();
                let d = Denoiser::from_file_with(&path, inference).unwrap();
                println!(
                    "load model={model} engine={} active=\"{}\" ms={:.1}{}",
                    inference.word(),
                    d.engine(),
                    t.elapsed().as_secs_f64() * 1000.0,
                    d.fallback_reason()
                        .map(|r| format!(" fallback=\"{r}\""))
                        .unwrap_or_default()
                );
                (inference, d)
            })
            .collect();
        let mut times: Vec<Vec<f64>> = vec![Vec::new(); dens.len()];
        let mut input = [0f32; HOP];
        let mut out = [0f32; HOP];
        let mut pos = 0usize;
        for round in 0..rounds {
            for k in 0..dens.len() {
                let i = (k + round) % dens.len();
                let d = &mut dens[i].1;
                d.reset();
                // Warm-up: the first hops of a session are not the steady state.
                for _ in 0..50 {
                    for s in input.iter_mut() {
                        *s = audio[pos];
                        pos = (pos + 1) % audio.len();
                    }
                    d.process_hop(&input, &mut out).unwrap();
                }
                let mut next = Instant::now();
                for _ in 0..hops {
                    for s in input.iter_mut() {
                        *s = audio[pos];
                        pos = (pos + 1) % audio.len();
                    }
                    if paced {
                        next += Duration::from_millis(10);
                        std::thread::sleep(next.saturating_duration_since(Instant::now()));
                    }
                    let t = Instant::now();
                    d.process_hop(&input, &mut out).unwrap();
                    times[i].push(t.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
        for (i, (inference, d)) in dens.iter().enumerate() {
            let mut t = std::mem::take(&mut times[i]);
            t.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let q = |p: f64| t[((t.len() as f64 * p) as usize).min(t.len() - 1)];
            let mean = t.iter().sum::<f64>() / t.len() as f64;
            println!(
                "bench model={model} engine={} active=\"{}\" paced={} hops={} \
                 mean_ms={mean:.3} p50_ms={:.3} p95_ms={:.3} p99_ms={:.3} max_ms={:.3}",
                inference.word(),
                d.engine(),
                paced as u8,
                t.len(),
                q(0.5),
                q(0.95),
                q(0.99),
                t[t.len() - 1]
            );
        }
    }
}
