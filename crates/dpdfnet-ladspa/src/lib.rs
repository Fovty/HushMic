//! hushmic DPDFNet LADSPA plugin: a thin PipeWire-facing wrapper around the
//! `hushmic-denoiser` engine crate. Everything here is host plumbing —
//! control-port mapping, the inference worker + alignment ledger that
//! decouple the DSP from the host's cycle (issue #10), and the bundled
//! ONNX Runtime's baked default paths.
pub mod adaptive;
pub mod align;
pub mod ladder;
pub mod log;
pub mod rt;
pub mod worker;

pub use adaptive::{AdaptiveEngine, RawDelay};
pub use align::{
    required_lead, Aligner, PopPlan, DESIGN_QUANTUM, OUTPUT_LEAD, PLUGIN_LATENCY_SAMPLES,
    STALL_HEADROOM,
};
pub use ladder::{Event, Ladder, Pinned, Policy, ShadowKind, Step, Tier};
pub use rt::{RtOutcome, RT_PRIORITY};
pub use worker::{HopEngine, WorkerHandle, RING_CAPACITY, RUN_GUARD_AFTER, RUN_GUARD_MIN_PARK};

use hushmic_denoiser::{Denoiser, Mode, RuntimeInit};
use ladspa::{DefaultValue, Plugin, PluginDescriptor, Port, PortConnection, PortDescriptor};
use std::path::PathBuf;
use std::time::Duration;

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

/// Which tiers a main model file and an optional light engine yield, and
/// which slot the main model occupies. The file name decides: a main model
/// named `dpdfnet2*` IS the light tier, so the ladder is `[Light, Raw]`.
fn plan_tiers(main: &std::path::Path, light_loaded: bool) -> (Vec<Tier>, Tier) {
    let main_is_light = main
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|f| f.starts_with("dpdfnet2"));
    if main_is_light {
        (vec![Tier::Light, Tier::Raw], Tier::Light)
    } else if light_loaded {
        (vec![Tier::Quality, Tier::Light, Tier::Raw], Tier::Quality)
    } else {
        (vec![Tier::Quality, Tier::Raw], Tier::Quality)
    }
}

/// `HUSHMIC_DSP_TIER=quality|light|passthrough` pins the policy; a word
/// that names a tier this ladder does not have is ignored with a log line.
fn pinned_tier(value: Option<&str>, tiers: &[Tier]) -> Option<Tier> {
    let v = value?;
    let tier = match v {
        "quality" => Some(Tier::Quality),
        "light" => Some(Tier::Light),
        "passthrough" => Some(Tier::Raw),
        _ => None,
    };
    match tier {
        Some(t) if tiers.contains(&t) => Some(t),
        _ => {
            eprintln!(
                "[dpdfnet-ladspa] HUSHMIC_DSP_TIER={v} ignored (not one of this chain's tiers)"
            );
            None
        }
    }
}

fn build_policy(tiers: &[Tier]) -> Box<dyn Policy> {
    let pin = std::env::var("HUSHMIC_DSP_TIER").ok();
    if let Some(t) = pinned_tier(pin.as_deref(), tiers) {
        return Box::new(Pinned(t));
    }
    let mut ladder = Ladder::new(tiers);
    if let Some(hops) = std::env::var("HUSHMIC_DSP_DWELL_HOPS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        ladder = ladder.with_dwell_base(hops);
    }
    Box::new(ladder)
}

/// Bring up the pinned bundled runtime and load the model(s). On any failure
/// of the main model the plugin runs engine-less (working-but-silent node).
/// The runtime commit is gated on OUR resolved path — a failed init must
/// never fall through to a random system libonnxruntime replacing the pinned
/// bundled one, so no `Denoiser` constructor (whose implicit resolution would
/// try exactly that) is ever reached on the error path.
///
/// `HUSHMIC_FALLBACK_MODEL_PATH` names the light model; when it loads, the
/// adaptive engine can fall back to it under CPU pressure (issue #14). Both
/// loads happen here, on the instantiate thread, before the worker exists.
fn init_engine() -> Option<AdaptiveEngine<Denoiser, Box<dyn Policy>>> {
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
    let main_path = model_path();
    let main = match Denoiser::from_file(&main_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[dpdfnet-ladspa] engine init failed: {e}");
            return None;
        }
    };
    // A main model that is already the light one has no fallback to load;
    // the comparison is by canonical path so a symlinked or differently
    // spelled duplicate is not loaded twice as the "light" tier.
    let (_, main_tier) = plan_tiers(&main_path, false);
    let same_file = |p: &PathBuf| {
        let canon = |q: &std::path::Path| q.canonicalize().unwrap_or_else(|_| q.to_path_buf());
        canon(p) == canon(&main_path)
    };
    let light = std::env::var_os("HUSHMIC_FALLBACK_MODEL_PATH")
        .map(PathBuf::from)
        .filter(|p| main_tier != Tier::Light && !same_file(p))
        .and_then(|p| match Denoiser::from_file(&p) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("[dpdfnet-ladspa] light model unavailable: {e}");
                None
            }
        });
    let (tiers, main_tier) = plan_tiers(&main_path, light.is_some());
    let (quality, light) = match main_tier {
        Tier::Light => (None, Some(main)),
        _ => (Some(main), light),
    };
    let policy = build_policy(&tiers);
    Some(AdaptiveEngine::new(quality, light, policy))
}

struct DpdfnetPlugin {
    /// The RT-side handle to the inference worker that owns the engine
    /// (issue #10: inference must never run on the PipeWire data thread).
    /// `None` = failed init / wrong rate / unspawnable thread: the
    /// established working-but-silent degradation.
    worker: Option<WorkerHandle>,
    aligner: Aligner,
    /// The activation handshake succeeded; false degrades to silence.
    active: bool,
    /// Mid-stream recovery after an input-ring overflow: silence until
    /// the worker acks the reset, then resume fresh (see worker.rs).
    restarting: bool,
    // Control-port caches as bit patterns (NaN-proof compares).
    last_db_bits: u32,
    last_mode_bits: u32,
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
            worker: engine.and_then(WorkerHandle::spawn),
            aligner: Aligner::new(),
            active: false,
            restarting: false,
            last_db_bits: f32::NAN.to_bits(),
            last_mode_bits: f32::NAN.to_bits(),
        }
    }
}

/// Zero the whole output range (the silent-node degradation).
fn emit_silence(output: &mut [f32], sample_count: usize) {
    for o in output[..sample_count].iter_mut() {
        *o = 0.0;
    }
}

impl Plugin for DpdfnetPlugin {
    fn activate(&mut self) {
        // Fresh session: reset the engine (via the worker handshake — the
        // host calls activate before streaming, so a bounded wait is fine
        // here; a stuck worker degrades to silence and the next activate
        // retries), drop stale audio, restart the alignment ledger.
        self.aligner.reset();
        self.restarting = false;
        self.last_db_bits = f32::NAN.to_bits();
        self.last_mode_bits = f32::NAN.to_bits(); // force a set on first run()
        self.active = match self.worker.as_mut() {
            Some(w) => {
                let ok = w.request_reset_and_wait(Duration::from_secs(2));
                if ok {
                    w.drain_output();
                } else if !w.engine_dead() {
                    eprintln!("[dpdfnet-ladspa] worker did not ack activation; emitting silence");
                }
                ok
            }
            None => false,
        };
    }

    fn run<'a>(&mut self, sample_count: usize, ports: &[&'a PortConnection<'a>]) {
        let input = ports[0].unwrap_audio();
        let mut output = ports[1].unwrap_audio_mut();
        let db = *ports[2].unwrap_control();
        let mode_ctl = *ports[3].unwrap_control();

        // RT contract: nothing below allocates, locks, logs or panics:
        // ring pushes/pops, atomics, and one futex wake at most. The
        // degradation paths (engine death, input overflow) are reported
        // by the worker thread from flags set here.
        let w = match self.worker.as_mut() {
            Some(w) if self.active => w,
            _ => {
                emit_silence(&mut output, sample_count);
                return;
            } // silent node: failed init, wrong rate, or failed activation
        };
        if w.engine_dead() {
            emit_silence(&mut output, sample_count);
            return;
        }
        if db.to_bits() != self.last_db_bits {
            w.set_attn_db(db);
            self.last_db_bits = db.to_bits();
        }
        if mode_ctl.to_bits() != self.last_mode_bits {
            w.set_mode_control(mode_ctl);
            self.last_mode_bits = mode_ctl.to_bits();
        }
        if self.restarting {
            // Overflow recovery: silence until the worker acks the reset.
            // ORDER MATTERS: observe the ack BEFORE the final drain —
            // every worker push happens-before the ack store, so a drain
            // after an observed ack removes any stale partial hop, and
            // the worker stays parked (empty input ring) until fresh
            // input is pushed below. Draining first would let a push
            // landing in the drain->ack window survive into the fresh
            // stream: a stale blip plus permanent latency drift.
            // activate() follows the same ack-then-drain discipline.
            if w.reset_acked() {
                w.drain_output();
                self.restarting = false; // fall through: stream fresh audio
            } else {
                w.drain_output(); // housekeeping: bound ring occupancy meanwhile
                emit_silence(&mut output, sample_count);
                return;
            }
        }
        let accepted = w.push_input(&input[..sample_count]);
        if accepted < sample_count {
            // Input ring full: the worker is >680 ms behind. Restart the
            // stream — resync the mic to now instead of replaying stale
            // audio into a live call (see worker.rs / the design doc).
            w.request_reset();
            self.restarting = true;
            self.aligner.reset();
            w.drain_output();
            emit_silence(&mut output, sample_count);
            return;
        }
        w.note_quantum(sample_count);
        w.wake();
        let plan = self.aligner.plan(sample_count, w.output_available());
        for _ in 0..plan.discard {
            let _ = w.pop_output();
        }
        let mut i = 0;
        for _ in 0..plan.lead_zeros {
            output[i] = 0.0;
            i += 1;
        }
        for _ in 0..plan.real {
            output[i] = w.pop_output().unwrap_or(0.0);
            i += 1;
        }
        for _ in 0..plan.tail_zeros {
            output[i] = 0.0;
            i += 1;
        }
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
    fn tiers_follow_the_main_model_name_and_light_load() {
        let q = std::path::Path::new("/x/dpdfnet8_48khz_hr.onnx");
        let l = std::path::Path::new("/x/dpdfnet2_48khz_hr.onnx");
        assert_eq!(
            plan_tiers(q, true),
            (vec![Tier::Quality, Tier::Light, Tier::Raw], Tier::Quality)
        );
        assert_eq!(
            plan_tiers(q, false),
            (vec![Tier::Quality, Tier::Raw], Tier::Quality)
        );
        assert_eq!(
            plan_tiers(l, false),
            (vec![Tier::Light, Tier::Raw], Tier::Light)
        );
        assert_eq!(
            plan_tiers(l, true),
            (vec![Tier::Light, Tier::Raw], Tier::Light)
        );
    }

    #[test]
    fn pinned_tier_must_exist_in_the_ladder() {
        let qlr = [Tier::Quality, Tier::Light, Tier::Raw];
        let lr = [Tier::Light, Tier::Raw];
        assert_eq!(pinned_tier(Some("light"), &qlr), Some(Tier::Light));
        assert_eq!(pinned_tier(Some("passthrough"), &lr), Some(Tier::Raw));
        assert_eq!(pinned_tier(Some("quality"), &lr), None);
        assert_eq!(pinned_tier(Some("bogus"), &qlr), None);
        assert_eq!(pinned_tier(None, &qlr), None);
    }

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
        assert!(p.worker.is_none());
    }
}
