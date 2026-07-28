//! Real-time-factor probe: p95 per-hop inference cost against the 10 ms
//! budget. Dev tool; set HUSHMIC_MODEL_PATH (and ORT_DYLIB_PATH unless the
//! repo's bundled runtime is provisioned).

use hushmic_denoiser::{Denoiser, HOP};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    if std::env::var("ORT_DYLIB_PATH").is_err() {
        let bundled =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/lib/libonnxruntime.so");
        if bundled.exists() {
            hushmic_denoiser::init_runtime(&bundled).unwrap();
        }
    }
    let model = std::env::var("HUSHMIC_MODEL_PATH").unwrap();
    let mut eng = Denoiser::from_file(&model).unwrap();
    let zero = [0f32; HOP];
    let mut out = [0f32; HOP];
    for _ in 0..50 {
        eng.process_hop(&zero, &mut out).unwrap();
    } // warmup
    let mut times = Vec::with_capacity(2000);
    for _ in 0..2000 {
        let t = Instant::now();
        eng.process_hop(&zero, &mut out).unwrap();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = times[(times.len() as f64 * 0.95) as usize];
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    println!(
        "mean={mean:.3} ms p95={p95:.3} ms budget=10 ms RTF_p95={:.3}",
        p95 / 10.0
    );
}
