//! Native inference engine (feature `native`): the DPDFNet authors' C
//! runtime, vendored under native/ and compiled by build.rs. It has the
//! same one-hop contract as the ONNX graph — `spec` `[481,2]` interleaved
//! plus `state_in` in, `spec_e` plus `state_out` out — so everything around
//! the model is untouched.
//!
//! Only the versioned C API is bound: `<model>_get_api(1)` returns an
//! immutable function table. The generated C is specific to one exported
//! ONNX graph, so a model file is recognized by its SHA-256 (the export the
//! C was generated from, recorded in the vendored manifest), and the packed
//! weights next to it must hash to the value the table carries. Every
//! mismatch is an `Err` with a reason, which the caller turns into an ONNX
//! fallback.

use crate::stft::SPEC_LEN;
use sha2::{Digest, Sha256};
use std::ffi::{c_char, c_int, CStr};
use std::io::Read;
use std::path::Path;
use std::ptr::NonNull;

/// Which engine runs a model; see [`Denoiser::from_file_with`].
///
/// [`Denoiser::from_file_with`]: crate::Denoiser::from_file_with
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Inference {
    /// Native INT8 where the CPU supports it (x86-64 with AVX2 and FMA)
    /// and the model's weights are installed and intact, otherwise ONNX.
    #[default]
    Auto,
    /// ONNX Runtime only.
    Onnx,
    /// Native with INT8 matrices (AVX2 and FMA required), else ONNX.
    NativeInt8,
    /// Native in full FP32. Mainly for comparisons: without AVX2 it runs
    /// the portable scalar code, which is slower than ONNX.
    NativeFp32,
}

impl Inference {
    /// `auto`, `onnx`, `native-int8` or `native-fp32`.
    pub fn parse(word: &str) -> Option<Inference> {
        Some(match word {
            "auto" => Inference::Auto,
            "onnx" => Inference::Onnx,
            "native-int8" => Inference::NativeInt8,
            "native-fp32" => Inference::NativeFp32,
            _ => return None,
        })
    }

    /// The word [`Inference::parse`] accepts.
    pub fn word(self) -> &'static str {
        match self {
            Inference::Auto => "auto",
            Inference::Onnx => "onnx",
            Inference::NativeInt8 => "native-int8",
            Inference::NativeFp32 => "native-fp32",
        }
    }
}

/// The engine a [`Denoiser`](crate::Denoiser) actually runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Onnx,
    NativeInt8,
    NativeFp32,
}

impl Engine {
    pub fn is_native(self) -> bool {
        self != Engine::Onnx
    }
}

impl std::fmt::Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Engine::Onnx => "onnx",
            Engine::NativeInt8 => "native int8",
            Engine::NativeFp32 => "native fp32",
        })
    }
}

// native_api.h
const ABI_VERSION: u32 = 1;
const PRESET_FP32: u32 = 0;
const PRESET_INT8_SELECTIVE: u32 = 1;

#[repr(C)]
struct CModel {
    _opaque: [u8; 0],
}

/// Mirror of `dpdf_native_api_v1` (frozen layout).
#[repr(C)]
struct ApiV1 {
    abi_version: u32,
    struct_size: u32,
    model_name: *const c_char,
    weights_sha256: *const c_char,
    sample_rate: u32,
    hop_size: u32,
    spectrum_size: usize,
    state_size: usize,
    weight_count: usize,
    preset_supported: Option<unsafe extern "C" fn(u32) -> c_int>,
    create: Option<unsafe extern "C" fn(*const f32, usize, u32) -> *mut CModel>,
    init_state: Option<unsafe extern "C" fn(*mut f32) -> c_int>,
    process: Option<
        unsafe extern "C" fn(*mut CModel, *const f32, *const f32, *mut f32, *mut f32) -> c_int,
    >,
    destroy: Option<unsafe extern "C" fn(*mut CModel)>,
    owned_bytes: Option<unsafe extern "C" fn(*const CModel) -> usize>,
}

extern "C" {
    fn dpdfnet8_48khz_hr_get_api(abi_version: u32) -> *const ApiV1;
    fn dpdfnet2_48khz_hr_get_api(abi_version: u32) -> *const ApiV1;
}

/// One compiled-in model: its getter and the SHA-256 of the ONNX export
/// its C code was generated from.
struct Registered {
    get_api: unsafe extern "C" fn(u32) -> *const ApiV1,
    onnx_sha256: &'static str,
}

const REGISTRY: [Registered; 2] = [
    Registered {
        get_api: dpdfnet8_48khz_hr_get_api,
        onnx_sha256: env!("HUSHMIC_NATIVE_ONNX_SHA256_dpdfnet8_48khz_hr"),
    },
    Registered {
        get_api: dpdfnet2_48khz_hr_get_api,
        onnx_sha256: env!("HUSHMIC_NATIVE_ONNX_SHA256_dpdfnet2_48khz_hr"),
    },
];

/// The checked function table: every entry present, sizes as HushMic's
/// STFT needs them.
struct Api {
    name: &'static str,
    weights_sha256: &'static str,
    state_size: usize,
    weight_count: usize,
    preset_supported: unsafe extern "C" fn(u32) -> c_int,
    create: unsafe extern "C" fn(*const f32, usize, u32) -> *mut CModel,
    init_state: unsafe extern "C" fn(*mut f32) -> c_int,
    process: unsafe extern "C" fn(*mut CModel, *const f32, *const f32, *mut f32, *mut f32) -> c_int,
    destroy: unsafe extern "C" fn(*mut CModel),
}

fn static_str(p: *const c_char) -> Option<&'static str> {
    // SAFETY: non-null table strings are C literals that live as long as
    // the library, which is linked in.
    (!p.is_null())
        .then(|| unsafe { CStr::from_ptr(p) }.to_str().ok())
        .flatten()
}

impl Api {
    fn get(r: &Registered) -> Result<Api, String> {
        // SAFETY: the getter only reads its argument.
        let raw = unsafe { (r.get_api)(ABI_VERSION) };
        // SAFETY: non-null means a pointer to the library's static table.
        let t = unsafe { raw.as_ref() }.ok_or("the native runtime does not offer ABI v1")?;
        if t.abi_version != ABI_VERSION || (t.struct_size as usize) < std::mem::size_of::<ApiV1>() {
            return Err(format!(
                "native ABI mismatch (version {}, table {} bytes)",
                t.abi_version, t.struct_size
            ));
        }
        if t.sample_rate != crate::SAMPLE_RATE
            || t.hop_size as usize != crate::HOP
            || t.spectrum_size != SPEC_LEN
        {
            return Err(format!(
                "native model shape mismatch ({} Hz, hop {}, spectrum {})",
                t.sample_rate, t.hop_size, t.spectrum_size
            ));
        }
        let incomplete = || "native function table is incomplete".to_string();
        Ok(Api {
            name: static_str(t.model_name).ok_or_else(incomplete)?,
            weights_sha256: static_str(t.weights_sha256)
                .filter(|s| s.len() == 64)
                .ok_or_else(incomplete)?,
            state_size: t.state_size,
            weight_count: t.weight_count,
            preset_supported: t.preset_supported.ok_or_else(incomplete)?,
            create: t.create.ok_or_else(incomplete)?,
            init_state: t.init_state.ok_or_else(incomplete)?,
            process: t.process.ok_or_else(incomplete)?,
            destroy: t.destroy.ok_or_else(incomplete)?,
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

/// One native model instance: owns the C model (weights and scratch
/// arena). A process call mutates the arena, so it takes `&mut self`.
pub(crate) struct NativeModel {
    api: Api,
    ptr: NonNull<CModel>,
    pub(crate) engine: Engine,
}

// SAFETY: the C model has no thread affinity, and the only call that
// touches it (process, via `run`) takes `&mut self`, so it can never run
// concurrently; shared references only read the table's constants. This
// keeps `Denoiser` `Send + Sync` as with ONNX.
unsafe impl Send for NativeModel {}
unsafe impl Sync for NativeModel {}

impl NativeModel {
    /// Load the native build of the model in `onnx` with the engine
    /// `inference` asks for (never `Onnx`). Returns the model and its
    /// initial state, or why it cannot run natively.
    pub(crate) fn load(
        onnx: &Path,
        inference: Inference,
    ) -> Result<(NativeModel, Vec<f32>), String> {
        NativeModel::load_with(onnx, inference, None)
    }

    /// `load`, with `supported` standing in for the runtime's preset probe
    /// (tests exercise the unsupported-CPU path with it).
    pub(crate) fn load_with(
        onnx: &Path,
        inference: Inference,
        supported: Option<fn(u32) -> bool>,
    ) -> Result<(NativeModel, Vec<f32>), String> {
        let (preset, engine) = match inference {
            Inference::Auto | Inference::NativeInt8 => (PRESET_INT8_SELECTIVE, Engine::NativeInt8),
            Inference::NativeFp32 => (PRESET_FP32, Engine::NativeFp32),
            Inference::Onnx => return Err("ONNX requested".into()),
        };
        let sha = file_sha256(onnx).map_err(|e| format!("{}: {e}", onnx.display()))?;
        let registered = REGISTRY
            .iter()
            .find(|r| r.onnx_sha256 == sha)
            .ok_or("no native build of this model file")?;
        let api = Api::get(registered)?;
        let ok = match supported {
            Some(probe) => probe(preset),
            // SAFETY: pure capability query.
            None => unsafe { (api.preset_supported)(preset) != 0 },
        };
        if !ok {
            return Err(if preset == PRESET_INT8_SELECTIVE {
                "CPU lacks AVX2/FMA for native int8".into()
            } else {
                "native fp32 unsupported by this build".into()
            });
        }
        let weights = read_weights(&weights_path(onnx, api.name), &api)?;
        // SAFETY: `weights` holds exactly weight_count floats; create
        // copies what it keeps, so the buffer may drop afterwards.
        let raw = unsafe { (api.create)(weights.as_ptr(), weights.len(), preset) };
        drop(weights);
        let ptr = NonNull::new(raw).ok_or("native model creation failed")?;
        let model = NativeModel { api, ptr, engine };
        let mut init_state = vec![0f32; model.api.state_size];
        // SAFETY: init_state writes exactly state_size floats.
        if unsafe { (model.api.init_state)(init_state.as_mut_ptr()) } != 0 {
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
        // the table at load; state sizes checked above. The four buffers
        // are distinct Rust borrows, so none overlap.
        let rc = unsafe {
            (self.api.process)(
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
        // SAFETY: ptr came from this table's create and is destroyed once.
        unsafe { (self.api.destroy)(self.ptr.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn inference_words_round_trip() {
        for i in [
            Inference::Auto,
            Inference::Onnx,
            Inference::NativeInt8,
            Inference::NativeFp32,
        ] {
            assert_eq!(Inference::parse(i.word()), Some(i));
        }
        assert_eq!(Inference::parse("native"), None);
        assert_eq!(Inference::parse("native-fp16"), None);
        assert_eq!(Inference::parse("ONNX"), None);
        assert_eq!(Inference::default(), Inference::Auto);
    }

    #[test]
    fn every_model_offers_a_complete_v1_table() {
        let mut names = Vec::new();
        for r in &REGISTRY {
            let api = Api::get(r).expect("v1 table");
            assert_eq!(api.weights_sha256.len(), 64);
            assert!(api.state_size > 577 && api.weight_count > 0);
            assert_eq!(r.onnx_sha256.len(), 64);
            names.push(api.name);
        }
        assert_eq!(names, ["dpdfnet8_48khz_hr", "dpdfnet2_48khz_hr"]);
    }

    #[test]
    fn other_abi_versions_are_refused() {
        for r in &REGISTRY {
            for v in [0, 2, u32::MAX] {
                // SAFETY: the getter only reads its argument.
                assert!(unsafe { (r.get_api)(v) }.is_null(), "version {v}");
            }
        }
    }

    #[test]
    fn fp32_is_always_available_and_unknown_presets_are_not() {
        for r in &REGISTRY {
            let api = Api::get(r).unwrap();
            // SAFETY: pure capability queries.
            unsafe {
                assert_ne!((api.preset_supported)(PRESET_FP32), 0);
                assert_eq!((api.preset_supported)(7), 0);
                assert!((api.create)(std::ptr::null(), 0, 7).is_null());
            }
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

    #[test]
    fn an_unsupported_int8_preset_is_an_error_not_a_downgrade() {
        let Some(onnx) = dev_model("dpdfnet2_48khz_hr.onnx") else {
            eprintln!("skipping: dev assets not provisioned");
            return;
        };
        let e =
            NativeModel::load_with(&onnx, Inference::Auto, Some(|p| p != PRESET_INT8_SELECTIVE))
                .err()
                .expect("must not load");
        assert_eq!(e, "CPU lacks AVX2/FMA for native int8");
        // FP32 is a separate request and still loads.
        let (m, _) = NativeModel::load_with(
            &onnx,
            Inference::NativeFp32,
            Some(|p| p != PRESET_INT8_SELECTIVE),
        )
        .expect("fp32 loads");
        assert_eq!(m.engine, Engine::NativeFp32);
    }
}
