//! hushmic DPDFNet LADSPA plugin (v0.1).
pub mod attn;
pub mod engine;
pub mod model;
pub mod stft;

use engine::Engine;
use ladspa::{DefaultValue, Plugin, PluginDescriptor, Port, PortConnection, PortDescriptor};
use std::path::PathBuf;
use stft::HOP;

const LABEL: &str = "dpdfnet_mono";
const UNIQUE_ID: u64 = 0x68736D31; // "hsm1"
const DEFAULT_MODEL: &str = env!("HUSHMIC_DEFAULT_MODEL");

fn model_path() -> PathBuf {
    std::env::var("HUSHMIC_MODEL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_MODEL))
}

/// Largest PipeWire quantum we pre-reserve buffer space for, so `run()` never
/// reallocates on the audio thread once `activate()` has been called.
const MAX_EXPECTED_QUANTUM: usize = 8192;

struct DpdfnetPlugin {
    engine: Option<Engine>,
    in_buf: Vec<f32>,
    out_buf: Vec<f32>, // committed enhanced samples waiting to be emitted
    last_db: f32,
    run_err_logged: bool,
}

impl DpdfnetPlugin {
    fn new(sample_rate: u64) -> Self {
        // The DSP constants (N_FFT/HOP) and the DPDFNet models are 48 kHz-only.
        // LADSPA instantiate cannot cleanly reject, so a mismatched host gets the
        // same degradation as a failed engine init: a working-but-silent node
        // (audibly wrong beats subtly wrong enhancement).
        let engine = if sample_rate != 48_000 {
            eprintln!(
                "[dpdfnet-ladspa] unsupported sample rate {sample_rate} (need 48000); \
                 emitting silence"
            );
            None
        } else {
            match Engine::new(&model_path()) {
                Ok(e) => Some(e),
                Err(e) => {
                    eprintln!("[dpdfnet-ladspa] engine init failed: {e}");
                    None
                }
            }
        };
        DpdfnetPlugin {
            engine,
            in_buf: Vec::with_capacity(MAX_EXPECTED_QUANTUM + HOP),
            out_buf: Vec::with_capacity(MAX_EXPECTED_QUANTUM + HOP),
            last_db: f32::NAN,
            run_err_logged: false,
        }
    }
}

impl Plugin for DpdfnetPlugin {
    fn activate(&mut self) {
        // Reset recurrent state + buffers so no stale state bleeds across sessions.
        if let Some(e) = self.engine.as_mut() {
            e.reset();
        }
        self.in_buf.clear();
        self.out_buf.clear();
        // pre-fill one hop of silence => one-hop output latency, absorbs the first frame.
        self.out_buf.resize(HOP, 0.0);
        self.last_db = f32::NAN;
        self.run_err_logged = false;
    }

    fn run<'a>(&mut self, sample_count: usize, ports: &[&'a PortConnection<'a>]) {
        let input = ports[0].unwrap_audio();
        let mut output = ports[1].unwrap_audio_mut();
        let db = *ports[2].unwrap_control();

        let engine = match self.engine.as_mut() {
            Some(e) => e,
            None => {
                for o in output.iter_mut() {
                    *o = 0.0;
                }
                return;
            } // passthrough-silence on failure
        };
        if db != self.last_db {
            engine.set_attn_db(db);
            self.last_db = db;
        }

        // 1. enqueue input
        self.in_buf.extend_from_slice(&input[..sample_count]);
        // 2. drain whole hops through the engine. On a transient inference
        //    failure process_hop still fills out_hop (it feeds a zero frame
        //    through the synthesis/attenuation rings so the OLA alignment
        //    survives for the next good frame) — always emit what it produced.
        let mut hop_in = [0f32; HOP];
        let mut hop_out = [0f32; HOP];
        while self.in_buf.len() >= HOP {
            hop_in.copy_from_slice(&self.in_buf[..HOP]);
            if let Err(e) = engine.process_hop(&hop_in, &mut hop_out) {
                if !self.run_err_logged {
                    eprintln!("[dpdfnet-ladspa] inference failed (recovering per-hop): {e}");
                    self.run_err_logged = true;
                }
            }
            self.out_buf.extend_from_slice(&hop_out);
            self.in_buf.drain(..HOP);
        }
        // 3. emit sample_count from the output queue (zero-fill if underfilled)
        let avail = self.out_buf.len().min(sample_count);
        output[..avail].copy_from_slice(&self.out_buf[..avail]);
        for o in output[avail..sample_count].iter_mut() {
            *o = 0.0;
        }
        self.out_buf.drain(..avail);
    }
}

fn new_instance(_d: &PluginDescriptor, sample_rate: u64) -> Box<dyn Plugin + Send> {
    Box::new(DpdfnetPlugin::new(sample_rate))
}

// extern "C": the ladspa crate declares this symbol in an `extern {}` block and
// calls it from its C `ladspa_descriptor` entry point, so the definition must
// use the C ABI to match (a plain Rust-ABI fn is formally UB at that call).
// The Option<PluginDescriptor> signature is the ladspa crate's own contract —
// both sides are this exact Rust type, so the improper_ctypes lint is moot.
#[allow(improper_ctypes_definitions)]
#[no_mangle]
pub extern "C" fn get_ladspa_descriptor(index: u64) -> Option<PluginDescriptor> {
    if index != 0 {
        return None;
    }
    Some(PluginDescriptor {
        unique_id: UNIQUE_ID,
        label: LABEL,
        properties: ladspa::PROP_NONE,
        name: "hushmic DPDFNet Noise Suppressor (Mono)",
        maker: "hushmic",
        copyright: "MIT OR Apache-2.0",
        ports: vec![
            Port {
                name: "Input",
                desc: PortDescriptor::AudioInput,
                hint: None,
                default: None,
                lower_bound: None,
                upper_bound: None,
            },
            Port {
                name: "Output",
                desc: PortDescriptor::AudioOutput,
                hint: None,
                default: None,
                lower_bound: None,
                upper_bound: None,
            },
            Port {
                name: "Attenuation Limit (dB)",
                desc: PortDescriptor::ControlInput,
                hint: None,
                default: Some(DefaultValue::Maximum),
                lower_bound: Some(0.0),
                upper_bound: Some(100.0),
            },
        ],
        new: new_instance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatched_sample_rate_disables_engine() {
        // 44.1 kHz hosts must get the silent-node degradation, never the 48 kHz
        // model running on wrongly-spaced spectra.
        let p = DpdfnetPlugin::new(44_100);
        assert!(p.engine.is_none());
    }
}
