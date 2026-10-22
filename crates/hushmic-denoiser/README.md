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
hushmic-denoiser = { git = "https://github.com/Fovty/HushMic", tag = "v0.10.4" }
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

ONNX Runtime 1.18 or newer works (the crate uses API version 18). Once a
model runs on ONNX Runtime, the crate keeps its environment until the process
exits instead of releasing it at exit, which crashed runtimes before 1.23.

Prefer static linking or ort's downloaded binaries? Disable the default
feature and configure [`ort`](https://crates.io/crates/ort) yourself; cargo
merges the features:

```toml
hushmic-denoiser = { git = "https://github.com/Fovty/HushMic", tag = "v0.10.4", default-features = false }
ort = { version = "=2.0.0-rc.12", features = ["download-binaries"] }
```

## Native engine (optional)

The `native` feature adds a second engine: the DPDFNet authors' optimized
C runtime, included in this crate (`native/`) and compiled by `build.rs`.
It is x86-64 Linux only and needs a C compiler that also assembles `.S`
files; upstream tests GCC only, and a clang build prints a warning. The
AVX-VNNI kernels are built only when the toolchain supports them (GCC 11
and binutils 2.36 or newer), otherwise the AVX2 kernels run everywhere.
On CPUs with AVX2 and FMA it runs the models with W7A8 matrix math (7-bit
weights, 8-bit activations; AVX-VNNI where the CPU has it) in under half
the CPU time of ONNX Runtime, and about a quarter to a third with AVX-VNNI. Its output agrees with ONNX's to about 35
to 39 dB (waveform SNR).

```toml
hushmic-denoiser = { git = "https://github.com/Fovty/HushMic", tag = "v0.10.4", features = ["native"] }
```

```rust
use hushmic_denoiser::{Denoiser, Inference};

let denoiser = Denoiser::from_file_with("dpdfnet8_48khz_hr.onnx", Inference::Auto)?;
println!("running on {}", denoiser.engine()); // "native w7a8" or "onnx"
println!("kernels: {:?}", denoiser.native_kernels()); // Some("avx2"), Some("avx-vnni") or None
```

The engine needs the model's packed weights, `<model>.weights.f32`, next
to the `.onnx` file. Every HushMic release since 0.10.0 attaches them
(listed in `sha256sums.txt`), and `scripts/setup-assets.sh` in the HushMic
repository downloads and verifies them. The `.onnx` file is still needed:
it identifies the model, and ONNX Runtime takes over whenever the native
engine cannot run (no AVX2/FMA, missing or damaged weights, an
unrecognized model file); `Denoiser::fallback_reason` says why. The thread
that processes audio needs at least 1 MiB of stack (a spawned Rust thread
has 2 MiB). `Inference::NativeInt8` asks for the native engine explicitly
(the name is from the earlier INT8 runtime); `Inference::NativeFp32` is
deprecated and runs ONNX.

## Example

```sh
cargo run --release --example denoise_wav -- dpdfnet8_48khz_hr.onnx noisy.wav cleaned.wav
```

Reads a 48 kHz mono WAV, writes the cleaned audio as a sample-aligned 16-bit
WAV.

## License

MIT OR Apache-2.0, like the rest of HushMic. The DPDFNet models, reference
implementation and native runtime are by
[Ceva](https://github.com/ceva-ip/DPDFNet) (Apache-2.0); `native/README.md`
lists the included files and their origin.
