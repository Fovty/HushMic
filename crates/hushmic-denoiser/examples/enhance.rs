//! Repo-internal offline enhancer: run a 48 kHz mono WAV through the engine.
//!
//!   cargo run --release --example enhance -p hushmic-denoiser -- in.wav out.wav
//!
//! Streams hop-by-hop with NO latency alignment — the raw engine output
//! framing that scripts/gen-parity-fixtures.py and docs/demo/make-demos.py
//! depend on (the committed goldens are byte-comparable to this output).
//! Application-style, latency-aligned file denoising is what the
//! `denoise_wav` example shows instead.
//!
//! Model path:   $HUSHMIC_MODEL_PATH  (else <repo>/assets/models/dpdfnet8_48khz_hr.onnx)
//! ORT runtime:  $ORT_DYLIB_PATH      (else <repo>/assets/lib/libonnxruntime.so)

use hushmic_denoiser::{Denoiser, HOP};
use std::path::PathBuf;

fn read_wav_mono_f32(p: &str) -> (Vec<f32>, u32) {
    let mut r = hound::WavReader::open(p).expect("open input wav");
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
    // Downmix to mono by taking the first channel (input is expected to be mono already).
    let mono: Vec<f32> = if spec.channels == 1 {
        s
    } else {
        s.iter().step_by(spec.channels as usize).copied().collect()
    };
    (mono, spec.sample_rate)
}

fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64).sqrt() as f32
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn model_path() -> PathBuf {
    if let Ok(p) = std::env::var("HUSHMIC_MODEL_PATH") {
        return PathBuf::from(p);
    }
    repo_root().join("assets/models/dpdfnet8_48khz_hr.onnx")
}

/// Dev-tree runtime bring-up: honor ORT_DYLIB_PATH, else the repo's bundled
/// runtime (the library's own fallback would try the system soname, which a
/// development checkout should not depend on).
fn init_dev_runtime() {
    if std::env::var("ORT_DYLIB_PATH").is_err() {
        let bundled = repo_root().join("assets/lib/libonnxruntime.so");
        if bundled.exists() {
            hushmic_denoiser::init_runtime(&bundled)
                .unwrap_or_else(|e| panic!("bundled runtime: {e}"));
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: enhance <in.wav> <out.wav>");
        std::process::exit(2);
    }
    let in_path = &args[1];
    let out_path = &args[2];

    let (noisy, sr) = read_wav_mono_f32(in_path);
    if sr != 48_000 {
        eprintln!(
            "[enhance] WARNING: input sample rate is {sr} Hz, expected 48000 Hz; \
             the model assumes 48 kHz and output will be played back at 48 kHz."
        );
    }

    init_dev_runtime();
    let model = model_path();
    let mut eng = Denoiser::from_file(&model)
        .unwrap_or_else(|e| panic!("engine init ({}): {e}", model.display()));

    let mut out = Vec::with_capacity(noisy.len());
    let mut hop_in = [0f32; HOP];
    let mut hop_out = [0f32; HOP];
    let hops = noisy.len() / HOP;
    for h in 0..hops {
        hop_in.copy_from_slice(&noisy[h * HOP..(h + 1) * HOP]);
        eng.process_hop(&hop_in, &mut hop_out).expect("process_hop");
        out.extend_from_slice(&hop_out);
    }

    // Write enhanced output as 48 kHz mono 16-bit PCM (universally playable).
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(out_path, spec).expect("create output wav");
    for &s in &out {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        w.write_sample(v).expect("write sample");
    }
    w.finalize().expect("finalize output wav");

    eprintln!(
        "[enhance] {} -> {} : {} hops, in_rms={:.5} out_rms={:.5}",
        in_path,
        out_path,
        hops,
        rms(&noisy),
        rms(&out)
    );
}
