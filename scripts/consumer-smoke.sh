#!/usr/bin/env bash
# Prove hushmic-denoiser works as an EXTERNAL dependency: build a scratch
# cargo project outside the workspace that depends on the crate by path, run
# it against the dev assets, and assert it produces sane denoised audio.
# This is the test for what issue #6 actually asked for — "can someone else's
# project embed this" — which in-workspace tests can't answer (they share the
# workspace's feature unification, lockfile, and build-time environment).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODEL="$REPO_ROOT/assets/models/dpdfnet8_48khz_hr.onnx"
DYLIB="$REPO_ROOT/assets/lib/libonnxruntime.so"

if [ ! -f "$MODEL" ] || [ ! -f "$DYLIB" ]; then
  if [ "${HUSHMIC_ASSERT_ASSETS:-}" = "1" ]; then
    echo "consumer-smoke: FAIL — assets missing but HUSHMIC_ASSERT_ASSETS=1" >&2
    exit 1
  fi
  echo "consumer-smoke: SKIP (dev assets not provisioned; run scripts/setup-assets.sh)"
  exit 0
fi

DIR="$(mktemp -d)"
trap 'rm -rf "$DIR"' EXIT

mkdir -p "$DIR/src"
cat > "$DIR/Cargo.toml" <<EOF
[package]
name = "denoiser-smoke"
version = "0.0.0"
edition = "2021"

[dependencies]
hushmic-denoiser = { path = "$REPO_ROOT/crates/hushmic-denoiser" }

[workspace]
EOF

cat > "$DIR/src/main.rs" <<'EOF'
use hushmic_denoiser::{init_runtime, Denoiser, StreamDenoiser, HOP};

fn main() {
    let model = std::env::var("SMOKE_MODEL").expect("SMOKE_MODEL");
    let dylib = std::env::var("SMOKE_DYLIB").expect("SMOKE_DYLIB");
    init_runtime(&dylib).expect("runtime");
    let den = Denoiser::from_file(&model).expect("model");
    let mut stream = StreamDenoiser::new(den);

    // deterministic pseudo-noise, processed in an awkward chunk size
    let mut x = 0x2545f491u64;
    let sig: Vec<f32> = (0..48_000)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((x >> 33) as u32 as f32 / u32::MAX as f32) * 0.4 - 0.2
        })
        .collect();
    let mut out = Vec::new();
    for chunk in sig.chunks(1000) {
        out.extend_from_slice(stream.process(chunk));
    }
    out.extend_from_slice(stream.process(&vec![0f32; stream.pending()]));
    assert!(stream.take_error().is_none(), "no inference errors expected");
    assert!(out.len() >= sig.len(), "output must cover the input");
    assert!(out.iter().all(|s| s.is_finite()), "output must be finite");
    let energy_in: f32 = sig.iter().map(|s| s * s).sum();
    let energy_out: f32 = out.iter().map(|s| s * s).sum();
    assert!(
        energy_out < energy_in * 0.5,
        "a denoiser fed pure noise must attenuate it: in={energy_in} out={energy_out}"
    );
    // one hop-level call too, for the low-level API
    let mut d = stream.into_inner();
    let (inp, mut outp) = ([0.1f32; HOP], [0f32; HOP]);
    d.process_hop(&inp, &mut outp).expect("hop");
    println!("consumer-smoke: OK ({} samples, energy {energy_in:.1} -> {energy_out:.1})", out.len());
}
EOF

echo "consumer-smoke: building scratch consumer project..."
( cd "$DIR" && SMOKE_MODEL="$MODEL" SMOKE_DYLIB="$DYLIB" cargo run --quiet --release )
