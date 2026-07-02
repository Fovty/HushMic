use crate::attn::AttnLimiter;
use crate::model::Model;
use crate::stft::{Analysis, Synthesis, HOP, SPEC_LEN};
use std::path::Path;

pub struct Engine {
    analysis: Analysis,
    synthesis: Synthesis,
    model: Model,
    state: Vec<f32>,
    spec: [f32; SPEC_LEN],
    spec_e: [f32; SPEC_LEN],
    state_out: Vec<f32>,
    attn: AttnLimiter,
}

impl Engine {
    pub fn new(model_path: &Path) -> Result<Engine, String> {
        let model = Model::load(model_path)?;
        let state = model.init_state.clone();
        let state_out = vec![0f32; model.state_size];
        Ok(Engine {
            analysis: Analysis::new(),
            synthesis: Synthesis::new(),
            model,
            state,
            spec: [0f32; SPEC_LEN],
            spec_e: [0f32; SPEC_LEN],
            state_out,
            attn: AttnLimiter::new(),
        })
    }

    pub fn reset(&mut self) {
        self.analysis.reset();
        self.synthesis.reset();
        self.state.clear();
        self.state.extend_from_slice(&self.model.init_state);
        self.attn.reset();
    }

    pub fn set_attn_db(&mut self, db: f32) {
        self.attn.set_db(db);
    }

    /// `out_hop` is ALWAYS filled, even on `Err`: a transient model failure
    /// feeds a zero frame through the attenuation delay line and the OLA
    /// synthesis instead of skipping them, so every ring stays in lockstep
    /// with the analysis ring and the next good frame reconstructs correctly
    /// (skipping would desynchronize the overlap-add by one hop for good).
    pub fn process_hop(
        &mut self,
        in_hop: &[f32; HOP],
        out_hop: &mut [f32; HOP],
    ) -> Result<(), String> {
        self.analysis.push_hop(in_hop, &mut self.spec);
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
        self.attn.apply(&self.spec, &mut self.spec_e); // blend noisy floor per dB cap
        self.synthesis.add_frame(&self.spec_e, out_hop);
        result
    }
}
