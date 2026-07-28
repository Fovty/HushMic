use crate::attn::AttnLimiter;
use crate::error::Error;
use crate::mode::{GainRamp, Mode};
use crate::model::Model;
use crate::stft::{Analysis, Synthesis, HOP, SPEC_LEN};
use std::path::Path;

/// A streaming DPDFNet noise suppressor: 48 kHz mono, one 480-sample hop at a
/// time. `Send`, so the usual pattern is construct-at-init, move into the
/// audio thread. For arbitrary chunk sizes, wrap it in
/// [`StreamDenoiser`](crate::StreamDenoiser).
pub struct Denoiser {
    analysis: Analysis,
    synthesis: Synthesis,
    model: Model,
    state: Vec<f32>,
    spec: [f32; SPEC_LEN],
    spec_e: [f32; SPEC_LEN],
    state_out: Vec<f32>,
    attn: AttnLimiter,
    gain: GainRamp,
    mode: Mode,
    attn_limit_db: f32,
}

impl Denoiser {
    /// Load a DPDFNet model from a `.onnx` file.
    ///
    /// With the default `load-dynamic` feature, if no ONNX Runtime
    /// environment is committed yet one is resolved now: `ORT_DYLIB_PATH` if
    /// set and non-empty, else the platform soname (an executable-adjacent
    /// copy wins over the dlopen default search, mirroring ort). Apps that
    /// bundle their own runtime should call `init_runtime` first. Never
    /// panics on a missing/incompatible runtime — that is [`Error::Runtime`].
    /// (Static-linking builds skip all of this; ort brings its own runtime.)
    pub fn from_file(model_path: impl AsRef<Path>) -> Result<Denoiser, Error> {
        crate::runtime::ensure_runtime()?;
        let model = Model::load(model_path.as_ref()).map_err(Error::Model)?;
        Ok(Denoiser::with_model(model))
    }

    /// Load a DPDFNet model from in-memory bytes (e.g. `include_bytes!` or a
    /// download your app manages). Runtime handling as in [`Denoiser::from_file`].
    pub fn from_memory(model_bytes: &[u8]) -> Result<Denoiser, Error> {
        crate::runtime::ensure_runtime()?;
        let model = Model::from_memory(model_bytes).map_err(Error::Model)?;
        Ok(Denoiser::with_model(model))
    }

    fn with_model(model: Model) -> Denoiser {
        let state = model.init_state.clone();
        let state_out = vec![0f32; model.state_size];
        Denoiser {
            analysis: Analysis::new(),
            synthesis: Synthesis::new(),
            model,
            state,
            spec: [0f32; SPEC_LEN],
            spec_e: [0f32; SPEC_LEN],
            state_out,
            attn: AttnLimiter::new(),
            gain: GainRamp::new(),
            mode: Mode::Process,
            attn_limit_db: f32::INFINITY,
        }
    }

    /// Clear all stream state (STFT rings, recurrent model state, ramps) for
    /// a fresh session. Configuration survives: the current [`Mode`] and
    /// attenuation limit stay in force — in particular, a `Denoiser` reset
    /// while muted stays silent from the very first sample.
    pub fn reset(&mut self) {
        self.analysis.reset();
        self.synthesis.reset();
        self.state.clear();
        self.state.extend_from_slice(&self.model.init_state);
        self.attn.reset();
        self.gain.reset_to(self.mode == Mode::Mute);
    }

    /// Cap how deep suppression may go by blending the latency-aligned noisy
    /// signal back in: `0.0` disables suppression entirely (you get the
    /// aligned input back), values up to ~100 allow progressively deeper
    /// suppression, and `>= 200` or non-finite means unlimited (pure model
    /// output). A fresh `Denoiser` defaults to unlimited.
    pub fn set_attenuation_limit_db(&mut self, db: f32) {
        self.attn_limit_db = db;
        self.attn.set_db(db);
    }

    /// The last value passed to [`Denoiser::set_attenuation_limit_db`]
    /// (`f32::INFINITY` — unlimited — on a fresh instance).
    pub fn attenuation_limit_db(&self) -> f32 {
        self.attn_limit_db
    }

    /// The engine keeps running in every mode (state stays warm, transitions
    /// are instant); bypass and mute only change what leaves it.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.attn.set_bypass(mode == Mode::Bypass);
        self.gain.set_muted(mode == Mode::Mute);
    }

    /// The current [`Mode`].
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Algorithmic latency of the process path in samples: input at sample
    /// `n` shapes output at sample `n + latency_samples()`. Equals
    /// [`LATENCY_SAMPLES`](crate::LATENCY_SAMPLES) for the shipped DPDFNet
    /// models; an instance method so future models with different group
    /// delays stay API-compatible.
    pub fn latency_samples(&self) -> usize {
        crate::LATENCY_SAMPLES
    }

    /// Process exactly one hop. `output` is ALWAYS filled, even on `Err`: a
    /// transient model failure feeds a zero frame through the attenuation
    /// delay line and the OLA synthesis instead of skipping them, so every
    /// ring stays in lockstep with the analysis ring and the next good frame
    /// reconstructs correctly (skipping would desynchronize the overlap-add
    /// by one hop for good).
    pub fn process_hop(
        &mut self,
        input: &[f32; HOP],
        output: &mut [f32; HOP],
    ) -> Result<(), Error> {
        self.analysis.push_hop(input, &mut self.spec);
        let result = self.model.run(
            &self.spec,
            &self.state,
            &mut self.spec_e,
            &mut self.state_out,
        );
        match &result {
            Ok(()) => {
                std::mem::swap(&mut self.state, &mut self.state_out);
            }
            Err(_) => {
                // Keep the recurrent state as-is (last good frame) and emit a
                // zero spectrum; attn.apply below still blends in the delayed
                // noisy floor, so a capped limiter degrades to quiet passthrough
                // rather than a hard dropout.
                self.spec_e = [0f32; SPEC_LEN];
            }
        }
        self.attn.apply(&self.spec, &mut self.spec_e); // blend noisy floor / bypass mix
        self.synthesis.add_frame(&self.spec_e, output);
        self.gain.process(output); // mute ramp, after synthesis
        result.map_err(Error::Inference)
    }
}
