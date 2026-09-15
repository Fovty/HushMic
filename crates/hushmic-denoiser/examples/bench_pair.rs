//! Dev probe: per-hop cost of the quality model alone, of the light model
//! alone, and of both alternating every hop (the adaptive engine's trial
//! and warm-up phases). Set HUSHMIC_MODEL_DIR (and ORT_DYLIB_PATH unless the
//! repo's bundled runtime is provisioned).

use hushmic_denoiser::{Denoiser, HOP};
use std::path::PathBuf;
use std::time::Instant;

fn stats(mut times: Vec<f64>) -> (f64, f64) {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    (mean, times[(times.len() as f64 * 0.95) as usize])
}

fn main() {
    if std::env::var("ORT_DYLIB_PATH").is_err() {
        let bundled =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/lib/libonnxruntime.so");
        if bundled.exists() {
            hushmic_denoiser::init_runtime(&bundled).unwrap();
        }
    }
    let dir = PathBuf::from(std::env::var("HUSHMIC_MODEL_DIR").unwrap());
    let mut q = Denoiser::from_file(dir.join("dpdfnet8_48khz_hr.onnx")).unwrap();
    let mut l = Denoiser::from_file(dir.join("dpdfnet2_48khz_hr.onnx")).unwrap();
    // Real audio when HUSHMIC_BENCH_FLAC names a mono 48 kHz FLAC (streamed
    // hop by hop, looping), else a sine.
    let audio: Vec<f32> = match std::env::var("HUSHMIC_BENCH_FLAC") {
        Ok(p) => claxon::FlacReader::open(p)
            .unwrap()
            .samples()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect(),
        Err(_) => (0..HOP).map(|i| ((i as f32) * 0.37).sin() * 0.1).collect(),
    };
    let mut pos = 0usize;
    let mut next = |input: &mut [f32; HOP]| {
        for s in input.iter_mut() {
            *s = audio[pos];
            pos = (pos + 1) % audio.len();
        }
    };
    let mut input = [0f32; HOP];
    let mut out = [0f32; HOP];
    // Cold start: the very first hops of a fresh session.
    let mut cold = Vec::new();
    for _ in 0..10 {
        next(&mut input);
        let t = Instant::now();
        q.process_hop(&input, &mut out).unwrap();
        cold.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "quality cold first 10 hops (ms): {:?}",
        cold.iter()
            .map(|v| (v * 10.0).round() / 10.0)
            .collect::<Vec<_>>()
    );
    for _ in 0..100 {
        next(&mut input);
        q.process_hop(&input, &mut out).unwrap();
        l.process_hop(&input, &mut out).unwrap();
    }
    let n = 500;
    let mut tq = Vec::new();
    for _ in 0..n {
        next(&mut input);
        let t = Instant::now();
        q.process_hop(&input, &mut out).unwrap();
        tq.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let mut tl = Vec::new();
    for _ in 0..n {
        next(&mut input);
        let t = Instant::now();
        l.process_hop(&input, &mut out).unwrap();
        tl.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let mut pq = Vec::new();
    let mut pl = Vec::new();
    for _ in 0..n {
        next(&mut input);
        let t = Instant::now();
        q.process_hop(&input, &mut out).unwrap();
        pq.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        l.process_hop(&input, &mut out).unwrap();
        pl.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    // A trial: the light model has been live alone for a while, then the
    // idle quality model runs its first hops next to it. Repeated a few
    // times so the cold hops' spread shows.
    for round in 0..3 {
        for _ in 0..300 {
            next(&mut input);
            l.process_hop(&input, &mut out).unwrap();
        }
        let mut resumed = Vec::new();
        for _ in 0..6 {
            next(&mut input);
            let t = Instant::now();
            q.process_hop(&input, &mut out).unwrap();
            resumed.push(t.elapsed().as_secs_f64() * 1000.0);
            l.process_hop(&input, &mut out).unwrap();
        }
        println!(
            "quality resumed after 3 s idle, round {round}, first 6 hops (ms): {:?}",
            resumed
                .iter()
                .map(|v| (v * 10.0).round() / 10.0)
                .collect::<Vec<_>>()
        );
    }
    let (a, b) = stats(tq);
    println!("quality alone      mean={a:.2} ms p95={b:.2}");
    let (a, b) = stats(tl);
    println!("light alone        mean={a:.2} ms p95={b:.2}");
    let (a, b) = stats(pq);
    println!("quality in pair    mean={a:.2} ms p95={b:.2}");
    let (a, b) = stats(pl);
    println!("light in pair      mean={a:.2} ms p95={b:.2}");
}
