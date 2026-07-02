//! ONNX model wrapper for DPDFNet (single-thread CPU via the `ort` crate).
//!
//! Loads the `dpdfnet8_48khz_hr.onnx` graph, seeds the recurrent state from the
//! model's custom metadata (`erb_norm_init` + `spec_norm_init`), and runs one hop:
//!   inputs : `spec` `[1,1,481,2]` (f32 interleaved re/im) + `state_in` `[state_size]`
//!   outputs: `spec_e` `[1,1,481,2]` + `state_out` `[state_size]`

use crate::stft::{FREQ_BINS, SPEC_LEN};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::TensorRef;
use std::path::Path;
use std::sync::Once;

const DEFAULT_DYLIB: &str = env!("HUSHMIC_DEFAULT_DYLIB");
static RUNTIME_INIT: Once = Once::new();
static RUNTIME_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Point ort at the bundled libonnxruntime and commit the environment.
/// Idempotent; returns whether the runtime dylib actually loaded. Callers must
/// not touch `Session::builder()` on `false`: without a committed environment
/// ort's API bootstrap would retry the load itself and PANIC on failure,
/// bypassing the engine=None graceful-silence path a missing dylib should hit.
pub fn ensure_runtime() -> bool {
    RUNTIME_INIT.call_once(|| {
        let dylib = std::env::var("ORT_DYLIB_PATH").unwrap_or_else(|_| DEFAULT_DYLIB.to_string());
        // init_from dlopens the dylib and runs ort's version check; commit() installs the env.
        // NB: in rc.12 `init_from` returns `Result<EnvironmentBuilder>` and `commit()` returns
        // `bool` (true if installed, false if an env was already committed) -- it is not a Result.
        match ort::init_from(&dylib) {
            Ok(builder) => {
                let _ = builder.commit();
                RUNTIME_OK.store(true, std::sync::atomic::Ordering::Release);
            }
            Err(e) => eprintln!("[dpdfnet-ladspa] ort init_from({dylib}) failed: {e}"),
        }
    });
    RUNTIME_OK.load(std::sync::atomic::Ordering::Acquire)
}

pub struct Model {
    session: Session,
    pub state_size: usize,
    pub init_state: Vec<f32>,
}

fn parse_csv_f32(s: &str) -> Vec<f32> {
    s.split(',')
        .filter_map(|t| t.trim().parse::<f32>().ok())
        .collect()
}

impl Model {
    pub fn load(model_path: &Path) -> Result<Model, String> {
        if !ensure_runtime() {
            return Err("ONNX Runtime unavailable (libonnxruntime failed to load; \
                        see ORT_DYLIB_PATH)"
                .to_string());
        }
        let session = Session::builder()
            .map_err(|e| e.to_string())?
            .with_execution_providers([ort::ep::CPU::default().build()])
            .map_err(|e| e.to_string())?
            .with_intra_threads(1)
            .map_err(|e| e.to_string())?
            .with_inter_threads(1)
            .map_err(|e| e.to_string())?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| e.to_string())?
            .commit_from_file(model_path)
            .map_err(|e| format!("commit_from_file({}): {e}", model_path.display()))?;

        let meta = session.metadata().map_err(|e| e.to_string())?;

        // state_size: the model exports it as authoritative custom metadata. We prefer this over
        // introspecting `session.inputs()[1]`'s declared shape -- it is equally authoritative and
        // avoids any ambiguity from a symbolic/dynamic declared dimension.
        let state_size: usize = meta
            .custom("state_size")
            .and_then(|s| s.trim().parse().ok())
            .or_else(|| {
                // Fallback: read the rank-1 size from the `state_in` input's declared shape.
                session
                    .inputs()
                    .get(1)
                    .and_then(|outlet| outlet.dtype().tensor_shape())
                    .and_then(|shape| shape.last().copied())
                    .filter(|&d| d > 0)
                    .map(|d| d as usize)
            })
            .ok_or("could not determine state_size from metadata or input shape")?;

        // Seed init_state from custom metadata (erb_norm_init then spec_norm_init).
        let erb_sz: usize = meta
            .custom("erb_norm_state_size")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(481);
        let spec_sz: usize = meta
            .custom("spec_norm_state_size")
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(96);
        let erb_init = meta
            .custom("erb_norm_init")
            .map(|s| parse_csv_f32(&s))
            .unwrap_or_default();
        let spec_init = meta
            .custom("spec_norm_init")
            .map(|s| parse_csv_f32(&s))
            .unwrap_or_default();
        // `ModelMetadata` borrows `session` and has a Drop impl; release it before moving `session`.
        drop(meta);

        let mut init_state = vec![0f32; state_size];
        if erb_init.len() == erb_sz && erb_sz <= state_size {
            init_state[0..erb_sz].copy_from_slice(&erb_init);
        }
        if spec_init.len() == spec_sz && erb_sz + spec_sz <= state_size {
            init_state[erb_sz..erb_sz + spec_sz].copy_from_slice(&spec_init);
        }

        Ok(Model {
            session,
            state_size,
            init_state,
        })
    }

    pub fn run(
        &mut self,
        spec: &[f32; SPEC_LEN],
        state_in: &[f32],
        spec_e: &mut [f32; SPEC_LEN],
        state_out: &mut Vec<f32>,
    ) -> Result<(), String> {
        let spec_t = TensorRef::from_array_view(([1usize, 1, FREQ_BINS, 2], spec.as_slice()))
            .map_err(|e| e.to_string())?;
        let state_t =
            TensorRef::from_array_view(([state_in.len()], state_in)).map_err(|e| e.to_string())?;
        let outputs = self
            .session
            .run(ort::inputs! { "spec" => spec_t, "state_in" => state_t })
            .map_err(|e| e.to_string())?;

        // `outputs[..]` (Index) PANICS on a missing name; use `get` so a model
        // with unexpected outputs degrades through the Err-to-silence path.
        let (_, e_slice) = outputs
            .get("spec_e")
            .ok_or("model has no output named 'spec_e'")?
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        if e_slice.len() != spec_e.len() {
            return Err(format!(
                "model output 'spec_e' has {} elements, expected {}",
                e_slice.len(),
                spec_e.len()
            ));
        }
        spec_e.copy_from_slice(e_slice);
        let (_, s_slice) = outputs
            .get("state_out")
            .ok_or("model has no output named 'state_out'")?
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        state_out.clear();
        state_out.extend_from_slice(s_slice);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stft::SPEC_LEN;
    use std::path::PathBuf;

    fn model_path() -> PathBuf {
        PathBuf::from(env!("HUSHMIC_DEFAULT_MODEL"))
    }

    #[test]
    fn loads_and_runs_one_frame() {
        ensure_runtime();
        let mut m = Model::load(&model_path()).expect("load model");
        // dpdfnet8 state size
        assert_eq!(m.state_size, 90228, "unexpected state size");
        // init_state has exactly 577 nonzero leading elements
        let nonzero = m.init_state.iter().filter(|&&x| x != 0.0).count();
        assert_eq!(
            nonzero, 577,
            "expected 577 metadata-seeded nonzero state elems"
        );

        let spec = [0f32; SPEC_LEN]; // zero (silence) frame is a valid input
        let mut spec_e = [0f32; SPEC_LEN];
        let mut state_out = vec![0f32; m.state_size];
        let state_in = m.init_state.clone();
        m.run(&spec, &state_in, &mut spec_e, &mut state_out)
            .expect("run");
        // running must mutate state (recurrent step happened)
        assert!(state_out != state_in, "state_out did not change after run");
        assert_eq!(state_out.len(), m.state_size);
    }
}
