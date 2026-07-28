# hushmic-denoiser

Real-time DPDFNet noise suppression for voice — the DSP engine behind
[HushMic](https://github.com/Fovty/HushMic), usable from any Rust application:
48 kHz mono frames in, cleaned frames out. CPU-only, no network access, ever.

```rust
use hushmic_denoiser::{Denoiser, StreamDenoiser};

let denoiser = Denoiser::from_file("dpdfnet8_48khz_hr.onnx")?;
let mut stream = StreamDenoiser::new(denoiser);

// in your capture loop — any chunk size, output trails input by 50 ms:
let cleaned: &[f32] = stream.process(&mic_chunk);
```

`Denoiser` processes fixed 480-sample hops (10 ms) and is `Send` — construct it
at startup, move it into your audio thread. `StreamDenoiser` wraps it for
arbitrary chunk sizes. Bypass/mute modes, an attenuation limit, and instance
latency reporting are on the API; see the rustdoc.

## Using it (git dependency)

Not on crates.io yet — the API gets a stabilization pass first. Until then,
pin the latest release tag (the snippet below always names it):

```toml
[dependencies]
hushmic-denoiser = { git = "https://github.com/Fovty/HushMic", tag = "v0.5.1" }
```

## What you need at runtime

**1. A DPDFNet model.** The models are Apache-2.0, by
[Ceva's DPDFNet project](https://github.com/ceva-ip/DPDFNet), 11–15 MB, and are
not bundled in this crate. Download `dpdfnet8_48khz_hr.onnx` (best quality) or
`dpdfnet2_48khz_hr.onnx` (lighter) from the
[HushMic release assets](https://github.com/Fovty/HushMic/releases) — checksums
are in each release's `sha256sums.txt` — or fetch them via the upstream
[`dpdfnet` PyPI package](https://pypi.org/project/dpdfnet/). Ship the file with
your app and pass its path to `Denoiser::from_file`, or embed it with
`include_bytes!` and use `Denoiser::from_memory`.

**2. ONNX Runtime.** With the default `load-dynamic` feature the crate loads
`libonnxruntime.so` at runtime, resolved in this order:

1. `init_runtime(path)` — call it before creating the first `Denoiser` if your
   app bundles its own ONNX Runtime (recommended for distribution; note the
   `RuntimeInit::AlreadyInitialized` signal if another component beat you to it);
2. the `ORT_DYLIB_PATH` environment variable (empty counts as unset);
3. the bare soname `libonnxruntime.so`: a copy sitting next to your
   executable wins, then the default dynamic-linker search (distro package).

Prefer static linking or ort's downloaded binaries instead? Disable the
default feature and configure [`ort`](https://crates.io/crates/ort) yourself —
cargo unifies the features:

```toml
hushmic-denoiser = { git = "https://github.com/Fovty/HushMic", tag = "v0.5.1", default-features = false }
ort = { version = "=2.0.0-rc.12", features = ["download-binaries"] }
```

## Example

```sh
cargo run --release --example denoise_wav -- dpdfnet8_48khz_hr.onnx noisy.wav cleaned.wav
```

(48 kHz mono WAV in, sample-aligned 16-bit WAV out.)

## Attribution & license

The DPDFNet models and reference implementation are by
[Ceva](https://github.com/ceva-ip/DPDFNet) (Apache-2.0). This crate is
MIT OR Apache-2.0, like the rest of HushMic.
