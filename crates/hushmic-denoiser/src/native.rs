//! Native inference engine (feature `native`): the DPDFNet authors'
//! optimized C runtime (W7A8: 7-bit weights, 8-bit activations), vendored
//! under native/ and compiled by build.rs. It has the same one-hop contract
//! as the ONNX graph — `spec` `[481,2]` interleaved plus `state_in` in,
//! `spec_e` plus `state_out` out — so everything around the model is
//! untouched.
//!
//! The public C API of `dpdfnet.h` (ABI 1) is bound, plus one private
//! symbol per model, `dpdfnet{8,2}_private_has_avx_vnni`: the predicate
//! the runtime uses at creation to pick its AVX-VNNI kernels, so the engine
//! can report which kernel set runs. It is hidden in the static archive,
//! not exported from the plugin. The runtime is
//! specific to the exported ONNX graphs it was made from, so a model file is
//! recognized by its SHA-256 (recorded in the vendored manifests), and the
//! packed weights next to it must hash to the value the runtime reports.
//! Every mismatch is an `Err` with a reason, which the caller turns into an
//! ONNX fallback.

use crate::stft::SPEC_LEN;
use sha2::{Digest, Sha256};
use std::ffi::{c_char, CStr};
use std::io::Read;
use std::path::Path;
use std::ptr::NonNull;

/// Which engine runs a model; see [`Denoiser::from_file_with`].
///
/// [`Denoiser::from_file_with`]: crate::Denoiser::from_file_with
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Inference {
    /// The native engine where the CPU supports it (x86-64 with AVX2 and
    /// FMA) and the model's weights are installed and intact, otherwise
    /// ONNX.
    #[default]
    Auto,
    /// ONNX Runtime only.
    Onnx,
    /// The native engine (W7A8; the name is kept from the earlier INT8
    /// runtime), else ONNX.
    NativeInt8,
    /// The earlier runtime's full FP32 path. The current runtime has none,
    /// so this always falls back to ONNX and says why.
    #[deprecated(note = "the native runtime has no FP32 path; this always runs ONNX")]
    NativeFp32,
}

impl Inference {
    /// `auto`, `onnx` or `native`. The earlier words `native-int8` and
    /// `native-w7a8` (the native engine) and `native-fp32` are still
    /// accepted.
    #[allow(deprecated)]
    pub fn parse(word: &str) -> Option<Inference> {
        Some(match word {
            "auto" => Inference::Auto,
            "onnx" => Inference::Onnx,
            "native" | "native-int8" | "native-w7a8" => Inference::NativeInt8,
            "native-fp32" => Inference::NativeFp32,
            _ => return None,
        })
    }

    /// The word [`Inference::parse`] accepts.
    #[allow(deprecated)]
    pub fn word(self) -> &'static str {
        match self {
            Inference::Auto => "auto",
            Inference::Onnx => "onnx",
            Inference::NativeInt8 => "native",
            Inference::NativeFp32 => "native-fp32",
        }
    }
}

/// The engine a [`Denoiser`](crate::Denoiser) actually runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Onnx,
    /// The native engine (W7A8).
    NativeInt8,
    /// Never reported by the current runtime.
    #[deprecated(note = "the native runtime has no FP32 path")]
    NativeFp32,
}

impl Engine {
    pub fn is_native(self) -> bool {
        self != Engine::Onnx
    }
}

impl std::fmt::Display for Engine {
    #[allow(deprecated)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Engine::Onnx => "onnx",
            Engine::NativeInt8 => "native w7a8",
            Engine::NativeFp32 => "native fp32",
        })
    }
}

// dpdfnet.h
const ABI_VERSION: u32 = 1;

#[repr(C)]
struct CModel {
    _opaque: [u8; 0],
}

/// Mirror of `dpdfnet_model_info`.
#[repr(C)]
struct ModelInfo {
    abi_version: u32,
    name: *const c_char,
    weights_sha256: *const c_char,
    sample_rate: u32,
    hop_samples: u32,
    spectrum_floats: usize,
    state_floats: usize,
    weight_floats: usize,
}

/// `dpdfnet_model_id`, a C enum.
type ModelId = std::ffi::c_int;

#[cfg(hushmic_native_runtime)]
mod ffi {
    use super::{CModel, ModelId, ModelInfo};
    use std::ffi::c_int;
    extern "C" {
        pub fn dpdfnet_get_model_info(id: ModelId) -> *const ModelInfo;
        pub fn dpdfnet_cpu_supported() -> c_int;
        pub fn dpdfnet_create(id: ModelId, weights: *const f32, count: usize) -> *mut CModel;
        pub fn dpdfnet_init_state(id: ModelId, state: *mut f32) -> c_int;
        pub fn dpdfnet_process(
            model: *mut CModel,
            spectrum: *const f32,
            state: *const f32,
            output: *mut f32,
            next_state: *mut f32,
        ) -> c_int;
        pub fn dpdfnet_destroy(model: *mut CModel);
        // Not public API: the predicate each model's quantized matrices use
        // at creation to pick the AVX-VNNI kernels over the AVX2 ones (0
        // when the VNNI kernels are not built in). Bound so the engine can
        // say which kernels really run; a rename upstream fails the link.
        pub fn dpdfnet8_private_has_avx_vnni() -> c_int;
        pub fn dpdfnet2_private_has_avx_vnni() -> c_int;
    }
}

/// The runtime is x86-64 Linux only; elsewhere no model is available and
/// the engine is never created.
#[cfg(not(hushmic_native_runtime))]
#[allow(clippy::missing_safety_doc)]
mod ffi {
    use super::{CModel, ModelId, ModelInfo};
    use std::ffi::c_int;
    pub unsafe fn dpdfnet_get_model_info(_: ModelId) -> *const ModelInfo {
        std::ptr::null()
    }
    pub unsafe fn dpdfnet_cpu_supported() -> c_int {
        0
    }
    pub unsafe fn dpdfnet_create(_: ModelId, _: *const f32, _: usize) -> *mut CModel {
        std::ptr::null_mut()
    }
    pub unsafe fn dpdfnet_init_state(_: ModelId, _: *mut f32) -> c_int {
        -1
    }
    pub unsafe fn dpdfnet_process(
        _: *mut CModel,
        _: *const f32,
        _: *const f32,
        _: *mut f32,
        _: *mut f32,
    ) -> c_int {
        -1
    }
    pub unsafe fn dpdfnet_destroy(_: *mut CModel) {}
    pub unsafe fn dpdfnet8_private_has_avx_vnni() -> c_int {
        0
    }
    pub unsafe fn dpdfnet2_private_has_avx_vnni() -> c_int {
        0
    }
}

/// One compiled-in model: its runtime ID and the SHA-256 of the ONNX
/// export the runtime was made from.
struct Registered {
    id: ModelId,
    onnx_sha256: &'static str,
    has_avx_vnni: fn() -> bool,
}

const REGISTRY: [Registered; 2] = [
    Registered {
        id: 8,
        onnx_sha256: env!("HUSHMIC_NATIVE_ONNX_SHA256_dpdfnet8_48khz_hr"),
        // SAFETY: a CPU feature query without arguments.
        has_avx_vnni: || unsafe { ffi::dpdfnet8_private_has_avx_vnni() != 0 },
    },
    Registered {
        id: 2,
        onnx_sha256: env!("HUSHMIC_NATIVE_ONNX_SHA256_dpdfnet2_48khz_hr"),
        // SAFETY: a CPU feature query without arguments.
        has_avx_vnni: || unsafe { ffi::dpdfnet2_private_has_avx_vnni() != 0 },
    },
];

/// The checked model metadata: sizes as HushMic's STFT needs them.
struct Api {
    id: ModelId,
    name: &'static str,
    weights_sha256: &'static str,
    state_size: usize,
    weight_count: usize,
}

fn static_str(p: *const c_char) -> Option<&'static str> {
    // SAFETY: non-null metadata strings are C literals that live as long
    // as the library, which is linked in.
    (!p.is_null())
        .then(|| unsafe { CStr::from_ptr(p) }.to_str().ok())
        .flatten()
}

impl Api {
    fn get(r: &Registered) -> Result<Api, String> {
        if !cfg!(hushmic_native_runtime) {
            return Err("native engine not built for this platform".into());
        }
        // SAFETY: a lookup; unknown IDs return NULL.
        let raw = unsafe { ffi::dpdfnet_get_model_info(r.id) };
        // SAFETY: non-null means a pointer to the library's static metadata.
        let t = unsafe { raw.as_ref() }.ok_or("the native runtime does not know this model")?;
        if t.abi_version != ABI_VERSION {
            return Err(format!("native ABI mismatch (version {})", t.abi_version));
        }
        if t.sample_rate != crate::SAMPLE_RATE
            || t.hop_samples as usize != crate::HOP
            || t.spectrum_floats != SPEC_LEN
        {
            return Err(format!(
                "native model shape mismatch ({} Hz, hop {}, spectrum {})",
                t.sample_rate, t.hop_samples, t.spectrum_floats
            ));
        }
        let incomplete = || "native model metadata is incomplete".to_string();
        Ok(Api {
            id: r.id,
            name: static_str(t.name).ok_or_else(incomplete)?,
            weights_sha256: static_str(t.weights_sha256)
                .filter(|s| s.len() == 64)
                .ok_or_else(incomplete)?,
            state_size: t.state_floats,
            weight_count: t.weight_floats,
        })
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of a file, streamed.
fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match f.read(&mut buf)? {
            0 => break,
            n => h.update(&buf[..n]),
        }
    }
    Ok(hex(&h.finalize()))
}

/// Where a model's packed weights live: `<model name>.weights.f32` next to
/// its ONNX file.
pub(crate) fn weights_path(onnx: &Path, model_name: &str) -> std::path::PathBuf {
    onnx.with_file_name(format!("{model_name}.weights.f32"))
}

/// Read the weights and verify them against the table before decoding:
/// exact size, then SHA-256.
fn read_weights(path: &Path, api: &Api) -> Result<Vec<f32>, String> {
    let shown = path.display();
    let mut f = std::fs::File::open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!("native weights not installed ({shown})"),
        _ => format!("native weights unreadable ({shown}: {e})"),
    })?;
    let want = api.weight_count * 4;
    let len = f
        .metadata()
        .map_err(|e| format!("native weights unreadable ({shown}: {e})"))?
        .len();
    if len != want as u64 {
        return Err(format!(
            "native weights have the wrong size ({shown}: {len} bytes, expected {want})"
        ));
    }
    let mut weights = vec![0f32; api.weight_count];
    // SAFETY: a f32 buffer viewed as its bytes; any bit pattern is a valid
    // f32, and the view is dropped before `weights` is used again.
    let bytes = unsafe { std::slice::from_raw_parts_mut(weights.as_mut_ptr().cast::<u8>(), want) };
    f.read_exact(bytes)
        .map_err(|e| format!("native weights unreadable ({shown}: {e})"))?;
    if hex(&Sha256::digest(&*bytes)) != api.weights_sha256 {
        return Err(format!(
            "native weights do not match the expected checksum ({shown})"
        ));
    }
    if cfg!(target_endian = "big") {
        for w in weights.iter_mut() {
            *w = f32::from_bits(u32::from_le(w.to_bits()));
        }
    }
    Ok(weights)
}

/// One native model instance: owns the C context (packed weights and
/// scratch). A process call mutates the scratch, so it takes `&mut self`.
pub(crate) struct NativeModel {
    api: Api,
    ptr: NonNull<CModel>,
    pub(crate) engine: Engine,
    /// The kernel set the runtime picked at creation.
    pub(crate) kernels: &'static str,
}

// SAFETY: the C context has no thread affinity, and the only call that
// touches it (process, via `run`) takes `&mut self`, so it can never run
// concurrently; shared references only read the metadata. This keeps
// `Denoiser` `Send + Sync` as with ONNX.
unsafe impl Send for NativeModel {}
unsafe impl Sync for NativeModel {}

impl NativeModel {
    /// Load the native build of the model in `onnx` (`inference` is never
    /// `Onnx`). Returns the model and its initial state, or why it cannot
    /// run natively.
    pub(crate) fn load(
        onnx: &Path,
        inference: Inference,
    ) -> Result<(NativeModel, Vec<f32>), String> {
        NativeModel::load_with(onnx, inference, None)
    }

    /// `load`, with `supported` standing in for the runtime's CPU probe
    /// (tests exercise the unsupported-CPU path with it).
    #[allow(deprecated)]
    pub(crate) fn load_with(
        onnx: &Path,
        inference: Inference,
        supported: Option<fn() -> bool>,
    ) -> Result<(NativeModel, Vec<f32>), String> {
        match inference {
            Inference::Auto | Inference::NativeInt8 => {}
            Inference::NativeFp32 => return Err("native fp32 is not in this runtime".into()),
            Inference::Onnx => return Err("ONNX requested".into()),
        }
        let sha = file_sha256(onnx).map_err(|e| format!("{}: {e}", onnx.display()))?;
        let registered = REGISTRY
            .iter()
            .find(|r| r.onnx_sha256 == sha)
            .ok_or("no native build of this model file")?;
        let api = Api::get(registered)?;
        let ok = match supported {
            Some(probe) => probe(),
            // SAFETY: pure capability query.
            None => unsafe { ffi::dpdfnet_cpu_supported() != 0 },
        };
        if !ok {
            return Err("CPU lacks AVX2/FMA for the native engine".into());
        }
        let weights = read_weights(&weights_path(onnx, api.name), &api)?;
        // SAFETY: `weights` holds exactly weight_count floats, checked
        // against the expected SHA-256; create copies what it keeps, so the
        // buffer may drop afterwards.
        let raw = unsafe { ffi::dpdfnet_create(api.id, weights.as_ptr(), weights.len()) };
        drop(weights);
        let ptr = NonNull::new(raw).ok_or("native model creation failed")?;
        // The same query creation just used to pick the kernels.
        let vnni = (registered.has_avx_vnni)();
        let model = NativeModel {
            api,
            ptr,
            engine: Engine::NativeInt8,
            kernels: if vnni { "avx-vnni" } else { "avx2" },
        };
        // The runtime's seeds (normalization state), not zeros; every reset
        // copies this vector back.
        let mut init_state = vec![0f32; model.api.state_size];
        // SAFETY: init_state writes exactly state_floats floats.
        if unsafe { ffi::dpdfnet_init_state(model.api.id, init_state.as_mut_ptr()) } != 0 {
            return Err("native state initialisation failed".into());
        }
        Ok((model, init_state))
    }

    pub(crate) fn state_size(&self) -> usize {
        self.api.state_size
    }

    pub(crate) fn run(
        &mut self,
        spec: &[f32; SPEC_LEN],
        state_in: &[f32],
        spec_e: &mut [f32; SPEC_LEN],
        state_out: &mut Vec<f32>,
    ) -> Result<(), String> {
        let n = self.api.state_size;
        if state_in.len() != n {
            return Err(format!(
                "state has {} elements, expected {n}",
                state_in.len()
            ));
        }
        // Allocates only for a caller's short buffer; the Denoiser's is
        // already state_size.
        state_out.resize(n, 0.0);
        // SAFETY: spectrum sizes are SPEC_LEN by type and checked against
        // the metadata at load; state sizes checked above. The four buffers
        // are distinct Rust borrows, so none overlap. No allocation inside.
        let rc = unsafe {
            ffi::dpdfnet_process(
                self.ptr.as_ptr(),
                spec.as_ptr(),
                state_in.as_ptr(),
                spec_e.as_mut_ptr(),
                state_out.as_mut_ptr(),
            )
        };
        if rc != 0 {
            return Err(format!("native process returned {rc}"));
        }
        Ok(())
    }
}

impl Drop for NativeModel {
    fn drop(&mut self) {
        // SAFETY: ptr came from dpdfnet_create and is destroyed once.
        unsafe { ffi::dpdfnet_destroy(self.ptr.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(hushmic_native_runtime)]
    use std::path::PathBuf;

    #[test]
    #[allow(deprecated)]
    fn inference_words_round_trip() {
        for i in [
            Inference::Auto,
            Inference::Onnx,
            Inference::NativeInt8,
            Inference::NativeFp32,
        ] {
            assert_eq!(Inference::parse(i.word()), Some(i));
        }
        // The earlier word still selects the native engine.
        assert_eq!(Inference::parse("native-int8"), Some(Inference::NativeInt8));
        assert_eq!(Inference::parse("native-w7a8"), Some(Inference::NativeInt8));
        assert_eq!(Inference::parse("native-fp16"), None);
        assert_eq!(Inference::parse("ONNX"), None);
        assert_eq!(Inference::default(), Inference::Auto);
    }

    #[test]
    fn engine_words() {
        assert_eq!(Engine::NativeInt8.to_string(), "native w7a8");
        assert_eq!(Engine::Onnx.to_string(), "onnx");
    }

    #[cfg(hushmic_native_runtime)]
    #[test]
    fn every_model_offers_complete_metadata() {
        let mut names = Vec::new();
        for r in &REGISTRY {
            let api = Api::get(r).expect("metadata");
            assert_eq!(api.weights_sha256.len(), 64);
            assert!(api.state_size > 577 && api.weight_count > 0);
            assert_eq!(r.onnx_sha256.len(), 64);
            names.push(api.name);
        }
        assert_eq!(names, ["dpdfnet8_48khz_hr", "dpdfnet2_48khz_hr"]);
    }

    #[cfg(hushmic_native_runtime)]
    #[test]
    fn unknown_models_and_bad_arguments_are_refused() {
        // SAFETY: lookups and argument checks; nothing is created.
        unsafe {
            for id in [0, 1, 3, 7, -1] {
                assert!(ffi::dpdfnet_get_model_info(id).is_null(), "id {id}");
                assert_ne!(ffi::dpdfnet_init_state(id, [0f32; 4].as_mut_ptr()), 0);
            }
            assert!(ffi::dpdfnet_create(2, std::ptr::null(), 0).is_null());
            assert!(ffi::dpdfnet_create(2, [0f32; 4].as_ptr(), 4).is_null());
            assert_ne!(ffi::dpdfnet_init_state(2, std::ptr::null_mut()), 0);
            ffi::dpdfnet_destroy(std::ptr::null_mut());
        }
    }

    #[test]
    fn a_foreign_model_file_is_not_run_natively() {
        let dir = std::env::temp_dir().join(format!("hushmic-native-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let onnx = dir.join("dpdfnet8_48khz_hr.onnx");
        std::fs::write(&onnx, b"not the exported graph").unwrap();
        let e = NativeModel::load(&onnx, Inference::Auto).err().unwrap();
        assert_eq!(e, "no native build of this model file");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(hushmic_native_runtime)]
    fn dev_model(name: &str) -> Option<PathBuf> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/models")
            .join(name);
        let w = weights_path(&p, name.trim_end_matches(".onnx"));
        let present = p.exists() && w.exists();
        if !present && std::env::var("HUSHMIC_ASSERT_ASSETS").as_deref() == Ok("1") {
            panic!(
                "{} or its weights missing but HUSHMIC_ASSERT_ASSETS=1",
                p.display()
            );
        }
        present.then_some(p)
    }

    #[cfg(hushmic_native_runtime)]
    #[test]
    #[allow(deprecated)]
    fn an_unsupported_cpu_or_fp32_request_is_an_error_not_a_downgrade() {
        let Some(onnx) = dev_model("dpdfnet2_48khz_hr.onnx") else {
            eprintln!("skipping: dev assets not provisioned");
            return;
        };
        let e = NativeModel::load_with(&onnx, Inference::Auto, Some(|| false))
            .err()
            .expect("must not load");
        assert_eq!(e, "CPU lacks AVX2/FMA for the native engine");
        let e = NativeModel::load(&onnx, Inference::NativeFp32)
            .err()
            .expect("no fp32 path");
        assert_eq!(e, "native fp32 is not in this runtime");
    }

    /// The state a native model starts from (and every reset restores) is
    /// the runtime's seeds: the same normalization seeds the ONNX export
    /// carries in its metadata, zeros elsewhere.
    #[cfg(all(hushmic_native_runtime, feature = "load-dynamic"))]
    #[test]
    fn native_seeds_equal_the_onnx_seeds() {
        let rt =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/lib/libonnxruntime.so");
        if !rt.exists() || unsafe { ffi::dpdfnet_cpu_supported() } == 0 {
            eprintln!("skipping: runtime or AVX2/FMA missing");
            return;
        }
        crate::runtime::init_runtime(rt).expect("runtime");
        for name in ["dpdfnet8_48khz_hr.onnx", "dpdfnet2_48khz_hr.onnx"] {
            let Some(onnx) = dev_model(name) else {
                eprintln!("skipping: dev assets not provisioned");
                return;
            };
            let (_, native) = NativeModel::load(&onnx, Inference::Auto).expect("native");
            let reference = crate::model::Model::load(&onnx).expect("onnx").init_state;
            assert_eq!(native.len(), reference.len(), "{name}");
            assert!(native.iter().any(|&v| v != 0.0), "{name}: all zeros");
            let max = native
                .iter()
                .zip(&reference)
                .map(|(a, b)| (a - b).abs())
                .fold(0f32, f32::max);
            assert!(max <= 1e-5, "{name}: seeds differ by {max}");
        }
    }

    /// SHA-256 over little-endian f32 bits.
    #[cfg(hushmic_native_runtime)]
    fn bits_sha256(v: &[f32]) -> String {
        let mut h = Sha256::new();
        for x in v {
            h.update(x.to_le_bytes());
        }
        hex(&h.finalize())
    }

    /// The running glibc's version (`gnu_get_libc_version`), if glibc.
    #[cfg(hushmic_native_runtime)]
    fn glibc_version() -> Option<String> {
        #[cfg(target_env = "gnu")]
        {
            extern "C" {
                fn gnu_get_libc_version() -> *const c_char;
            }
            // SAFETY: returns a static NUL-terminated string.
            static_str(unsafe { gnu_get_libc_version() }).map(str::to_string)
        }
        #[cfg(not(target_env = "gnu"))]
        None
    }

    /// Golden output: the first 300 hops of a committed fixture through the
    /// native engine from its initial state, hashed bit for bit. Upstream
    /// guarantees the AVX-VNNI kernels give the same bits as the AVX2 ones;
    /// this checks it (recorded on AVX2, verified equal on an AVX-VNNI
    /// i7-13700KF). The spectra going in are hashed separately, so a
    /// mismatch says whether the FFT or the runtime moved.
    ///
    /// The bits also depend on the C library's libm (the runtime calls
    /// expf/tanhf and friends; the STFT window uses sinf): glibc 2.44 gives
    /// other hashes than 2.35/2.39 on the same binary and CPU. So the exact
    /// compare runs only on a glibc listed below (the dev box and both CI
    /// unit-test images); elsewhere the hashes are printed and the compare
    /// is skipped. The libm-independent check that runs everywhere is the
    /// ONNX parity floor in tests/native.rs
    /// (`native_matches_onnx_through_the_full_denoiser`).
    #[cfg(hushmic_native_runtime)]
    #[test]
    fn golden_output_is_the_same_on_every_kernel_set() {
        const HOPS: usize = 300;
        /// (glibc, spectra, dpdfnet8, dpdfnet2).
        const GOLDEN: [(&str, &str, &str, &str); 2] = [
            // ubuntu:22.04 (GitLab unit-tests), gcc 11.4
            (
                "2.35",
                "ef70b390c176bcd482650b8763c898b232cf28255a69b57a3b4d5e72b98d9678",
                "fc259a4715c2a9240bc894e1f7ddfce2efb5028a9e9e9aa4cab1690585741770",
                "b8c3bfa9af0582a2c16b196b5309d495a6e4476c0ae5e5b6fcc8f426552d55f7",
            ),
            // ubuntu 24.04 (dev box, GitHub ubuntu-latest), gcc 13.3
            (
                "2.39",
                "ef70b390c176bcd482650b8763c898b232cf28255a69b57a3b4d5e72b98d9678",
                "fc259a4715c2a9240bc894e1f7ddfce2efb5028a9e9e9aa4cab1690585741770",
                "b8c3bfa9af0582a2c16b196b5309d495a6e4476c0ae5e5b6fcc8f426552d55f7",
            ),
        ];
        // SAFETY: a pure capability query.
        if unsafe { ffi::dpdfnet_cpu_supported() } == 0 {
            eprintln!("skipping: CPU without AVX2/FMA");
            return;
        }
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/noisy_public_48k.flac");
        let audio: Vec<f32> = claxon::FlacReader::open(&fixture)
            .expect("fixture")
            .samples()
            .take(HOPS * crate::HOP)
            .map(|s| s.expect("flac sample") as f32 / 32768.0)
            .collect();
        assert_eq!(audio.len(), HOPS * crate::HOP);
        let mut analysis = crate::stft::Analysis::new();
        let mut spectra = Vec::with_capacity(HOPS * SPEC_LEN);
        let mut spec = [0f32; SPEC_LEN];
        for hop in audio.as_chunks::<{ crate::HOP }>().0 {
            analysis.push_hop(hop, &mut spec);
            spectra.extend_from_slice(&spec);
        }
        let spectra_sha = bits_sha256(&spectra);
        let glibc = glibc_version();
        let recorded = GOLDEN.iter().find(|g| Some(g.0) == glibc.as_deref());
        for (i, name) in ["dpdfnet8_48khz_hr.onnx", "dpdfnet2_48khz_hr.onnx"]
            .into_iter()
            .enumerate()
        {
            let Some(onnx) = dev_model(name) else {
                eprintln!("skipping: dev assets not provisioned");
                return;
            };
            let (mut m, mut state) = NativeModel::load(&onnx, Inference::Auto).expect("native");
            let mut next = vec![0f32; state.len()];
            let mut out = [0f32; SPEC_LEN];
            let mut all = Vec::with_capacity(HOPS * SPEC_LEN + state.len());
            for s in spectra.as_chunks::<SPEC_LEN>().0 {
                m.run(s, &state, &mut out, &mut next).expect("process");
                std::mem::swap(&mut state, &mut next);
                all.extend_from_slice(&out);
            }
            all.extend_from_slice(&state);
            let got = bits_sha256(&all);
            eprintln!(
                "{name} ({}, glibc {}): {got}; spectra {spectra_sha}",
                m.kernels,
                glibc.as_deref().unwrap_or("?")
            );
            let Some(&(_, spectra_want, m8, m2)) = recorded else {
                eprintln!("different libm, exact compare skipped");
                continue;
            };
            assert_eq!(spectra_sha, spectra_want, "the input spectra changed (FFT)");
            let want = [m8, m2][i];
            assert_eq!(got, want, "{name} on {} kernels", m.kernels);
        }
    }
}
