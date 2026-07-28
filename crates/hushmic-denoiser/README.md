# hushmic-denoiser

The noise suppression engine behind [HushMic](https://github.com/Fovty/HushMic),
usable from any Rust application. Feed it 48 kHz mono samples, get cleaned
speech back. Runs DPDFNet on the CPU and makes no network calls.

```rust
use hushmic_denoiser::{Denoiser, StreamDenoiser};

let denoiser = Denoiser::from_file("dpdfnet8_48khz_hr.onnx")?;
let mut stream = StreamDenoiser::new(denoiser);

// in your capture loop, any chunk size; output trails input by 50 ms
let cleaned: &[f32] = stream.process(&mic_chunk);
```

`Denoiser` works on fixed 480-sample hops (10 ms) and is `Send`, so you can
create it at startup and move it into your audio thread. `StreamDenoiser`
wraps it for arbitrary chunk sizes. Bypass and mute modes, an attenuation
limit and latency reporting are covered in the rustdoc.

## Installation

Not on crates.io yet. Use it as a git dependency, pinned to the latest
release tag:

```toml
[dependencies]
hushmic-denoiser = { git = "https://github.com/Fovty/HushMic", tag = "v0.6.0" }
```

## What you need at runtime

Two things, neither of which the crate downloads for you.

**A model file.** The DPDFNet models are by
[Ceva](https://github.com/ceva-ip/DPDFNet) (Apache-2.0), 11 to 15 MB. Get
`dpdfnet8_48khz_hr.onnx` (best quality) or `dpdfnet2_48khz_hr.onnx` (lighter)
from the [HushMic release assets](https://github.com/Fovty/HushMic/releases);
checksums are in `sha256sums.txt`. The
[`dpdfnet` PyPI package](https://pypi.org/project/dpdfnet/) carries the same
files. Ship the model with your app and pass its path to
`Denoiser::from_file`, or embed it with `include_bytes!` and use
`Denoiser::from_memory`.

**ONNX Runtime.** By default the crate loads `libonnxruntime.so` at runtime,
resolved in this order:

1. `init_runtime(path)`, for apps that bundle their own copy. Call it before
   creating the first `Denoiser`; watch for `RuntimeInit::AlreadyInitialized`
   if other code in your process also uses ONNX Runtime.
2. The `ORT_DYLIB_PATH` environment variable (empty counts as unset).
3. The bare soname `libonnxruntime.so`: a copy next to your executable wins,
   then the normal dynamic linker search, which finds a distro package.

Prefer static linking or ort's downloaded binaries? Disable the default
feature and configure [`ort`](https://crates.io/crates/ort) yourself; cargo
merges the features:

```toml
hushmic-denoiser = { git = "https://github.com/Fovty/HushMic", tag = "v0.6.0", default-features = false }
ort = { version = "=2.0.0-rc.12", features = ["download-binaries"] }
```

## Example

```sh
cargo run --release --example denoise_wav -- dpdfnet8_48khz_hr.onnx noisy.wav cleaned.wav
```

Reads a 48 kHz mono WAV, writes the cleaned audio as a sample-aligned 16-bit
WAV.

## License

MIT OR Apache-2.0, like the rest of HushMic. The DPDFNet models and reference
implementation are by [Ceva](https://github.com/ceva-ip/DPDFNet)
(Apache-2.0).
