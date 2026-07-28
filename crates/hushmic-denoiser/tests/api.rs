//! Public-API behavior tests: the contracts an embedding application relies
//! on, exercised end-to-end on the real models (self-skip when the dev assets
//! are not provisioned).

mod common;

use common::{dev_denoiser, init_dev_runtime, model_path};
use hushmic_denoiser::{Denoiser, Error, Mode, StreamDenoiser, HOP, LATENCY_SAMPLES};

const MODEL: &str = "dpdfnet8_48khz_hr.onnx";

/// Deterministic pseudo-noise (LCG); no RNG dependency, reproducible failures.
fn test_signal(len: usize) -> Vec<f32> {
    let mut x = 0x2545f491u64;
    (0..len)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((x >> 33) as u32 as f32 / u32::MAX as f32) * 0.5 - 0.25
        })
        .collect()
}

fn run_hops(den: &mut Denoiser, input: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(input.len());
    let mut hop_in = [0f32; HOP];
    let mut hop_out = [0f32; HOP];
    for h in 0..input.len() / HOP {
        hop_in.copy_from_slice(&input[h * HOP..(h + 1) * HOP]);
        den.process_hop(&hop_in, &mut hop_out).expect("process");
        out.extend_from_slice(&hop_out);
    }
    out
}

#[test]
fn fresh_denoiser_defaults() {
    let Some(den) = dev_denoiser(MODEL) else {
        eprintln!("skipping fresh_denoiser_defaults: assets not provisioned");
        return;
    };
    assert_eq!(den.mode(), Mode::Process);
    assert!(
        den.attenuation_limit_db().is_infinite(),
        "default: unlimited"
    );
    assert_eq!(den.latency_samples(), LATENCY_SAMPLES);
}

#[test]
fn attenuation_limit_getter_round_trips() {
    let Some(mut den) = dev_denoiser(MODEL) else {
        eprintln!("skipping attenuation_limit_getter_round_trips: assets not provisioned");
        return;
    };
    den.set_attenuation_limit_db(42.5);
    assert_eq!(den.attenuation_limit_db(), 42.5);
    den.set_mode(Mode::Bypass);
    assert_eq!(den.mode(), Mode::Bypass);
}

/// Privacy invariant at the public API: mute as the FIRST mode change on a
/// fresh instance must be silent from the very first sample — no fade-out of
/// real audio.
#[test]
fn born_muted_leaks_nothing() {
    let Some(mut den) = dev_denoiser(MODEL) else {
        eprintln!("skipping born_muted_leaks_nothing: assets not provisioned");
        return;
    };
    den.set_mode(Mode::Mute);
    let loud = [0.9f32; HOP];
    let mut out = [1.0f32; HOP];
    for _ in 0..8 {
        den.process_hop(&loud, &mut out).expect("process");
        assert!(
            out.iter().all(|&s| s == 0.0),
            "muted output must be exact zeros"
        );
    }
}

/// reset() keeps configuration in force: a denoiser reset while muted stays
/// silent without the caller re-applying the mode.
#[test]
fn reset_while_muted_stays_silent() {
    let Some(mut den) = dev_denoiser(MODEL) else {
        eprintln!("skipping reset_while_muted_stays_silent: assets not provisioned");
        return;
    };
    den.set_mode(Mode::Mute);
    let loud = [0.9f32; HOP];
    let mut out = [1.0f32; HOP];
    den.process_hop(&loud, &mut out).expect("process");
    den.reset();
    assert_eq!(den.mode(), Mode::Mute, "mode survives reset");
    den.process_hop(&loud, &mut out).expect("process");
    assert!(
        out.iter().all(|&s| s == 0.0),
        "reset-while-muted must not leak a single sample"
    );
}

#[test]
fn from_memory_matches_from_file() {
    let (Some(mut a), Some(mp)) = (dev_denoiser(MODEL), model_path(MODEL)) else {
        eprintln!("skipping from_memory_matches_from_file: assets not provisioned");
        return;
    };
    let bytes = std::fs::read(&mp).expect("read model");
    let mut b = Denoiser::from_memory(&bytes).expect("from_memory");
    let sig = test_signal(30 * HOP);
    assert_eq!(
        run_hops(&mut a, &sig),
        run_hops(&mut b, &sig),
        "same model via file and memory must be bit-identical"
    );
}

/// StreamDenoiser must be a pure repackaging of process_hop: pseudo-random
/// chunk sizes produce bit-identical output to the hop-by-hop path (both
/// sides are fresh instances of the same model with identical session
/// options, so inference is deterministic).
#[test]
fn stream_chunking_is_invariant() {
    let (Some(mut hop_path), Some(stream_den)) = (dev_denoiser(MODEL), dev_denoiser(MODEL)) else {
        eprintln!("skipping stream_chunking_is_invariant: assets not provisioned");
        return;
    };
    let sig = test_signal(64 * HOP);
    let expect = run_hops(&mut hop_path, &sig);

    let mut stream = StreamDenoiser::new(stream_den);
    let mut got = Vec::with_capacity(sig.len());
    let mut x = 0x9e3779b9u64; // seeded chunk-size sequence, 1..=4096
    let mut pos = 0;
    while pos < sig.len() {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        let n = (1 + (x >> 33) % 4096) as usize;
        let end = (pos + n).min(sig.len());
        got.extend_from_slice(stream.process(&sig[pos..end]));
        pos = end;
    }
    assert!(
        stream.take_error().is_none(),
        "clean run must report no error"
    );
    // the stream may still hold a partial hop; compare the emitted prefix
    assert!(!got.is_empty());
    assert_eq!(
        got[..],
        expect[..got.len()],
        "chunked output must be bit-identical"
    );
    assert!(
        expect.len() - got.len() < HOP,
        "stream may only trail by a partial hop"
    );
}

/// Chunk invariance across a mid-stream set_mode: the switch lands at the
/// same hop boundary on both paths, so outputs stay bit-identical.
#[test]
fn stream_mode_switch_at_hop_boundary_is_invariant() {
    let (Some(mut hop_path), Some(stream_den)) = (dev_denoiser(MODEL), dev_denoiser(MODEL)) else {
        eprintln!("skipping stream_mode_switch: assets not provisioned");
        return;
    };
    let sig = test_signal(32 * HOP);
    let switch_at = 16 * HOP;

    let mut expect = run_hops(&mut hop_path, &sig[..switch_at]);
    hop_path.set_mode(Mode::Bypass);
    expect.extend(run_hops(&mut hop_path, &sig[switch_at..]));

    let mut stream = StreamDenoiser::new(stream_den);
    let mut got = Vec::new();
    got.extend_from_slice(stream.process(&sig[..switch_at]));
    stream.denoiser_mut().set_mode(Mode::Bypass);
    got.extend_from_slice(stream.process(&sig[switch_at..]));

    assert_eq!(got, expect, "mode switch at a hop boundary must match");
}

/// Chunk invariance across a mid-stream reset() at an aligned boundary.
#[test]
fn stream_reset_at_hop_boundary_is_invariant() {
    let (Some(mut hop_path), Some(stream_den)) = (dev_denoiser(MODEL), dev_denoiser(MODEL)) else {
        eprintln!("skipping stream_reset: assets not provisioned");
        return;
    };
    let sig = test_signal(24 * HOP);
    let reset_at = 12 * HOP;

    let mut expect = run_hops(&mut hop_path, &sig[..reset_at]);
    hop_path.reset();
    expect.extend(run_hops(&mut hop_path, &sig[reset_at..]));

    let mut stream = StreamDenoiser::new(stream_den);
    let mut got = Vec::new();
    got.extend_from_slice(stream.process(&sig[..reset_at]));
    stream.reset();
    got.extend_from_slice(stream.process(&sig[reset_at..]));

    assert_eq!(got, expect, "reset at a hop boundary must match");
}

/// pending() = latency + fill to the next hop boundary; feeding exactly that
/// many zeros drains every real sample (the example's tail-drain contract).
#[test]
fn pending_drains_the_full_tail() {
    let Some(den) = dev_denoiser(MODEL) else {
        eprintln!("skipping pending_drains_the_full_tail: assets not provisioned");
        return;
    };
    let n = 5 * HOP + 137; // deliberately not hop-aligned
    let sig = test_signal(n);
    let mut stream = StreamDenoiser::new(den);
    let mut emitted = stream.process(&sig).len();
    assert_eq!(stream.pending(), LATENCY_SAMPLES + (HOP - n % HOP) % HOP);
    emitted += stream.process(&vec![0f32; stream.pending()]).len();
    assert!(
        emitted >= n + LATENCY_SAMPLES,
        "pending() zeros must flush all real content: emitted {emitted}, need {}",
        n + LATENCY_SAMPLES
    );
}

/// A bad model path with a working runtime is a Model error, not Runtime.
#[test]
fn missing_model_is_a_model_error() {
    if init_dev_runtime().is_none() {
        eprintln!("skipping missing_model_is_a_model_error: assets not provisioned");
        return;
    }
    match Denoiser::from_file("/nonexistent/no-such-model.onnx") {
        Err(Error::Model(msg)) => assert!(!msg.is_empty()),
        Err(e) => panic!("expected Error::Model, got {e:?}"),
        Ok(_) => panic!("expected Error::Model, got a working Denoiser"),
    }
}
