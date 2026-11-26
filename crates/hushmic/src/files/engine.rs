//! The engine a file pass runs: the model and inference the Settings
//! choose, at full strength. Gentle, Medium and Strong are mixes of this
//! pass and the original (exact to float rounding, measured), so one pass
//! serves every strength, the hold and Save.

use hushmic_denoiser::{Denoiser, Inference, StreamDenoiser};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct EngineSpec {
    pub model: PathBuf,
    pub inference: Inference,
}

impl EngineSpec {
    /// The model `id` (its file stem, as in the config) in `model_dir`.
    pub fn new(model_dir: &Path, id: &str, inference: Inference) -> EngineSpec {
        EngineSpec {
            model: model_dir.join(format!("{id}.onnx")),
            inference,
        }
    }

    /// A fresh engine at Maximum (a new denoiser has no attenuation limit).
    pub fn build(&self) -> Result<StreamDenoiser, String> {
        Denoiser::from_file_with(&self.model, self.inference)
            .map(StreamDenoiser::new)
            .map_err(|e| format!("{}: {e}", self.model.display()))
    }
}

/// Commit ONNX Runtime from `dylib`: the native engine's fallback, and the
/// engine on CPUs without AVX2. Fine when another caller already did.
pub fn init_runtime(dylib: &Path) -> Result<(), String> {
    hushmic_denoiser::init_runtime(dylib)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
