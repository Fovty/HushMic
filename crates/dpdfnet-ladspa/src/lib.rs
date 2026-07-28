//! hushmic DPDFNet LADSPA plugin: a thin PipeWire-facing wrapper around the
//! `hushmic-denoiser` engine crate. Everything here is host plumbing —
//! control-port mapping, buffering to the host's quantum, and the bundled
//! ONNX Runtime's baked default paths.
use hushmic_denoiser::{Denoiser, Mode, RuntimeInit, HOP};
use ladspa::{DefaultValue, Plugin, PluginDescriptor, Port, PortConnection, PortDescriptor};
use std::path::PathBuf;

const LABEL: &str = "dpdfnet_mono";
const UNIQUE_ID: u64 = 0x68736D31; // "hsm1"
const DEFAULT_MODEL: &str = env!("HUSHMIC_DEFAULT_MODEL");
const DEFAULT_DYLIB: &str = env!("HUSHMIC_DEFAULT_DYLIB");

fn model_path() -> PathBuf {
    std::env::var("HUSHMIC_MODEL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_MODEL))
}

/// LADSPA control value -> mode: round to nearest, clamp to 0..=2.
/// Non-finite values fall back to Process (the safe default).
fn mode_from_control(v: f32) -> Mode {
    if !v.is_finite() {
        return Mode::Process;
    }
    match v.round().clamp(0.0, 2.0) as u8 {
        1 => Mode::Bypass,
        2 => Mode::Mute,
        _ => Mode::Process,
    }
}

/// Bring up the pinned bundled runtime and load the model. On any failure the
/// plugin runs engine-less (working-but-silent node). The runtime commit is
/// gated on OUR resolved path — a failed init must never fall through to a
/// random system libonnxruntime replacing the pinned bundled one, so no
/// `Denoiser` constructor (whose implicit resolution would try exactly that)
/// is ever reached on the error path.
fn init_engine() -> Option<Denoiser> {
    let dylib = std::env::var("ORT_DYLIB_PATH")
        .ok()
        .filter(|s| !s.is_empty()) // empty counts as unset, as in ort itself
        .unwrap_or_else(|| DEFAULT_DYLIB.to_string());
    match hushmic_denoiser::init_runtime(&dylib) {
        Err(e) => {
            eprintln!("[dpdfnet-ladspa] {e}");
            return None;
        }
        Ok(RuntimeInit::AlreadyInitialized) => eprintln!(
            "[dpdfnet-ladspa] note: an ONNX Runtime environment was already committed in \
             this process; the bundled runtime at {dylib} may not be the one in use"
        ),
        // RuntimeInit is non_exhaustive: any future variant still means "a
        // runtime is committed", so proceeding is the right default.
        Ok(_) => {}
    }
    match Denoiser::from_file(model_path()) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("[dpdfnet-ladspa] engine init failed: {e}");
            None
        }
    }
}

/// Largest PipeWire quantum we pre-reserve buffer space for, so `run()` never
/// reallocates on the audio thread once `activate()` has been called.
const MAX_EXPECTED_QUANTUM: usize = 8192;

struct DpdfnetPlugin {
    engine: Option<Denoiser>,
    in_buf: Vec<f32>,
    out_buf: Vec<f32>, // committed enhanced samples waiting to be emitted
    last_db: f32,
    last_mode: f32,
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
            init_engine()
        };
        DpdfnetPlugin {
            engine,
            in_buf: Vec::with_capacity(MAX_EXPECTED_QUANTUM + HOP),
            out_buf: Vec::with_capacity(MAX_EXPECTED_QUANTUM + HOP),
            last_db: f32::NAN,
            last_mode: f32::NAN,
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
        self.last_mode = f32::NAN; // NAN forces a set_mode on the first run()
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
            engine.set_attenuation_limit_db(db);
            self.last_db = db;
        }
        let mode_ctl = *ports[3].unwrap_control();
        if mode_ctl != self.last_mode {
            engine.set_mode(mode_from_control(mode_ctl));
            self.last_mode = mode_ctl;
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
            Port {
                // 0 = process, 1 = bypass (latency-aligned raw), 2 = mute.
                // Appended after the attn port so confs addressing controls
                // by name stay valid.
                name: "Mode",
                desc: PortDescriptor::ControlInput,
                hint: Some(ladspa::HINT_INTEGER),
                default: Some(DefaultValue::Minimum),
                lower_bound: Some(0.0),
                upper_bound: Some(2.0),
            },
        ],
        new: new_instance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_port_is_declared_fourth() {
        // Appended after the attn port so existing confs stay valid
        // (filter-chain addresses controls by name).
        let d = get_ladspa_descriptor(0).expect("descriptor 0 exists");
        assert_eq!(d.ports.len(), 4, "Input, Output, Attn, Mode");
        let p = &d.ports[3];
        assert_eq!(p.name, "Mode");
        assert!(matches!(p.desc, PortDescriptor::ControlInput));
        assert_eq!(p.lower_bound, Some(0.0));
        assert_eq!(p.upper_bound, Some(2.0));
        assert!(matches!(p.default, Some(DefaultValue::Minimum)));
        assert_eq!(p.hint, Some(ladspa::HINT_INTEGER), "integer-valued mode");
    }

    #[test]
    fn from_control_rounds_and_clamps() {
        assert_eq!(mode_from_control(0.0), Mode::Process);
        assert_eq!(mode_from_control(0.4), Mode::Process);
        assert_eq!(mode_from_control(0.6), Mode::Bypass);
        assert_eq!(mode_from_control(1.0), Mode::Bypass);
        assert_eq!(mode_from_control(2.0), Mode::Mute);
        assert_eq!(mode_from_control(2.7), Mode::Mute);
        assert_eq!(mode_from_control(-3.0), Mode::Process);
        assert_eq!(mode_from_control(7.0), Mode::Mute);
        assert_eq!(mode_from_control(f32::NAN), Mode::Process);
    }

    #[test]
    fn mismatched_sample_rate_disables_engine() {
        // 44.1 kHz hosts must get the silent-node degradation, never the 48 kHz
        // model running on wrongly-spaced spectra.
        let p = DpdfnetPlugin::new(44_100);
        assert!(p.engine.is_none());
    }
}
