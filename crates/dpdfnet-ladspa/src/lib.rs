//! hushmic DPDFNet LADSPA plugin: a thin PipeWire-facing wrapper around the
//! `hushmic-denoiser` engine crate. Everything here is host plumbing —
//! control-port mapping (or, in HushMic's own chain, the control file that
//! stands in for the Mode and Attenuation Limit ports), the inference
//! worker + alignment ledger that decouple the DSP from the host's cycle
//! (issue #10), the inference engine choice (issue #18), and the bundled
//! ONNX Runtime's baked default paths.
pub mod adaptive;
pub mod align;
pub mod gate;
pub mod ladder;
pub mod log;
pub mod rt;
pub mod view;
pub mod worker;

pub use adaptive::{AdaptiveEngine, RawDelay};
pub use align::{
    required_lead, Aligner, PopPlan, DESIGN_QUANTUM, OUTPUT_LEAD, PLUGIN_LATENCY_SAMPLES,
    STALL_HEADROOM,
};
pub use ladder::{Event, Ladder, Pinned, Policy, ShadowKind, Step, Tier};
pub use rt::{RtOutcome, RT_PRIORITY};
pub use view::{EngineView, TapSlot};
pub use worker::{HopEngine, WorkerHandle, RING_CAPACITY, RUN_GUARD_AFTER, RUN_GUARD_MIN_PARK};

use hushmic_denoiser::{Denoiser, Mode, RuntimeInit};
use hushmic_shm::control::{ControlReader, LastValid, ATTN_MAX, ATTN_MIN};
use ladspa::{DefaultValue, Plugin, PluginDescriptor, Port, PortConnection, PortDescriptor};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

const LABEL: &str = "dpdfnet_mono";
const UNIQUE_ID: u64 = 0x68736D31; // "hsm1"
const DEFAULT_MODEL: &str = env!("HUSHMIC_DEFAULT_MODEL");
const DEFAULT_DYLIB: &str = env!("HUSHMIC_DEFAULT_DYLIB");

fn model_path() -> PathBuf {
    model_path_from(std::env::var_os("HUSHMIC_MODEL_PATH").as_deref())
}

/// `HUSHMIC_MODEL_PATH` overrides the compiled-in default; unset or empty
/// means the default, as for `ORT_DYLIB_PATH`.
fn model_path_from(named: Option<&std::ffi::OsStr>) -> PathBuf {
    named
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MODEL))
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

/// HushMic's own chain: the control file named in `HUSHMIC_CONTROL`, mapped
/// at the first instantiate in this process and shared by every instance
/// (see `hushmic_shm::control`). `None`: no file named (any other host),
/// or one this plugin cannot honour (one log line): the ports, as ever.
fn control() -> Option<&'static ControlReader> {
    static CONTROL: OnceLock<Option<ControlReader>> = OnceLock::new();
    CONTROL
        .get_or_init(|| {
            let path = std::env::var_os(hushmic_shm::CONTROL_ENV).filter(|p| !p.is_empty())?;
            let path = Path::new(&path);
            let reader = ControlReader::open(path)
                .map_err(|e| {
                    crate::log::contract_line(&format!(
                        "control: not available ({}: {e}); following the ports",
                        path.display()
                    ))
                })
                .ok()?;
            if !reader.locked() {
                // Still followed: a swapped-out page costs one fault on
                // the audio thread, the ports cost a silent WirePlumber.
                crate::log::contract_line(&format!(
                    "control: {} is not locked in RAM (memlock limit); a swapped-out page \
                     would stall the audio thread once",
                    path.display()
                ));
            }
            Some(reader)
        })
        .as_ref()
}

/// This cycle's (Attenuation Limit, Mode) control values: the control
/// file's newest valid words, else its last valid ones (`last`), else the
/// ports'. An invalid word reads as none, so it never turns a mute into
/// processing. RT-safe.
fn file_controls(
    last: &mut LastValid,
    file_db: Option<f32>,
    file_mode: Option<u32>,
    port_db: f32,
    port_mode: f32,
) -> (f32, f32) {
    (
        last.attn_db(file_db).unwrap_or(port_db),
        last.mode(file_mode).map_or(port_mode, |m| m as f32),
    )
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

/// File stem of the light model, the fallback tier under CPU pressure.
const LIGHT_MODEL: &str = "dpdfnet2_48khz_hr";

/// Where the light model comes from, before any file is touched.
#[derive(Debug, PartialEq)]
enum LightModel {
    /// The main model already is the light one: no fallback model.
    None,
    /// `HUSHMIC_DSP_TIER=quality` pins the engine to the main model: the
    /// light one would never run, so it is not loaded (issue #36).
    Unused,
    /// Named by `HUSHMIC_FALLBACK_MODEL_PATH` (the HushMic app sets it).
    Named(PathBuf),
    /// No host named one: the light model's file next to the main model,
    /// with the main model's extension. Hosts that set no variables
    /// (EasyEffects) get the light tier this way.
    Sibling(PathBuf),
}

/// `named` is `HUSHMIC_FALLBACK_MODEL_PATH`; unset or empty means "find
/// it", as for `ORT_DYLIB_PATH`. A named path always wins, so a host can
/// still point the plugin at another file. `pin` is `HUSHMIC_DSP_TIER`.
fn light_model(
    named: Option<&std::ffi::OsStr>,
    main: &std::path::Path,
    pin: Option<&str>,
) -> LightModel {
    if plan_tiers(main, false).1 == Tier::Light {
        return LightModel::None;
    }
    if pin == Some("quality") {
        return LightModel::Unused;
    }
    if let Some(p) = named.filter(|p| !p.is_empty()) {
        return LightModel::Named(PathBuf::from(p));
    }
    let file = match main.extension() {
        Some(ext) => {
            let mut f = std::ffi::OsString::from(format!("{LIGHT_MODEL}."));
            f.push(ext);
            f
        }
        None => LIGHT_MODEL.into(),
    };
    LightModel::Sibling(main.with_file_name(file))
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

/// The engine `HUSHMIC_INFERENCE` asks for: unset or empty is `auto`; an
/// unknown word is an `Err` carrying the reason ONNX runs instead.
#[cfg(feature = "native")]
pub fn inference_from(value: Option<&str>) -> Result<hushmic_denoiser::Inference, String> {
    use hushmic_denoiser::Inference;
    match value.unwrap_or("") {
        "" => Ok(Inference::Auto),
        w => Inference::parse(w)
            .ok_or_else(|| format!("HUSHMIC_INFERENCE={w} is not auto, onnx or native")),
    }
}

/// What a loaded denoiser runs on, as the contract line names it: `onnx`,
/// or `native w7a8` plus the kernel set the runtime picked (`avx-vnni` or
/// `avx2`). No parentheses: those hold the reason ONNX runs.
#[cfg(feature = "native")]
pub fn engine_label(d: &Denoiser) -> String {
    match d.native_kernels() {
        Some(kernels) => format!("{} {kernels}", d.engine()),
        None => d.engine().to_string(),
    }
}

/// The inference contract line for one model: `inference: native w7a8 avx2
/// for <model>` or `inference: onnx (<reason>) for <model>`. The app parses it
/// for `status` and `--doctor`; it must never start with `engine: `, which
/// is the tier contract.
pub fn inference_line(model: &str, engine: &str, reason: Option<&str>) -> String {
    match reason {
        Some(why) => format!("inference: {engine} ({why}) for {model}"),
        None => format!("inference: {engine} for {model}"),
    }
}

fn model_name(path: &std::path::Path) -> &str {
    path.file_stem().and_then(|f| f.to_str()).unwrap_or("?")
}

/// Load one model on the engine `HUSHMIC_INFERENCE` asks for (default
/// auto: native where the CPU and the installed weights allow it).
/// Every native failure falls back to ONNX; one contract line per model
/// says what runs and, for ONNX, why.
#[cfg(feature = "native")]
pub fn load_denoiser(path: &std::path::Path) -> Result<Denoiser, hushmic_denoiser::Error> {
    let requested = inference_from(std::env::var("HUSHMIC_INFERENCE").ok().as_deref());
    load_denoiser_with(path, requested)
}

/// `load_denoiser` with the request given (tests pick the engine here).
#[cfg(feature = "native")]
pub fn load_denoiser_with(
    path: &std::path::Path,
    requested: Result<hushmic_denoiser::Inference, String>,
) -> Result<Denoiser, hushmic_denoiser::Error> {
    use hushmic_denoiser::Inference;
    let (inference, note) = match requested {
        Ok(i) => (i, None),
        Err(why) => (Inference::Onnx, Some(why)),
    };
    let d = Denoiser::from_file_with(path, inference)?;
    let reason = match (&note, d.fallback_reason(), inference) {
        (Some(why), _, _) => Some(why.as_str()),
        (None, Some(why), _) => Some(why),
        (None, None, Inference::Onnx) => Some("requested"),
        (None, None, _) => None,
    };
    log::contract_line(&inference_line(model_name(path), &engine_label(&d), reason));
    Ok(d)
}

#[cfg(not(feature = "native"))]
pub fn load_denoiser(path: &std::path::Path) -> Result<Denoiser, hushmic_denoiser::Error> {
    let d = Denoiser::from_file(path)?;
    log::contract_line(&inference_line(
        model_name(path),
        "onnx",
        Some("built without the native engine"),
    ));
    Ok(d)
}

/// `HUSHMIC_VOICE_HOLD`: `1`, `true` or `on` turns the voice hold on in
/// every model of this instance (see `Denoiser::set_voice_hold`); anything
/// else, and no variable at all, leaves it off. The HushMic app sets it for
/// its setting; another host (EasyEffects) can set it in its environment.
fn voice_hold_from(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true" | "on"))
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

/// The runtime this plugin committed, when it was this plugin (the first
/// instance in the process; later ones find it already committed).
static OUR_RUNTIME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// What to say when an instance finds a runtime already committed: nothing
/// when it is the one this plugin loaded for the same path, otherwise which
/// runtime is in use instead of the requested one.
fn runtime_note(ours: Option<&str>, requested: &str) -> Option<String> {
    match ours {
        Some(p) if p == requested => None,
        Some(p) => Some(format!(
            "ONNX Runtime from {p} is already in use in this process; {requested} was not loaded"
        )),
        None => Some(format!(
            "another component of this process set up ONNX Runtime first; it is used \
             instead of {requested}"
        )),
    }
}

/// Bring up the pinned bundled runtime and load the model(s). On any failure
/// of the main model the plugin runs engine-less (working-but-silent node).
/// The runtime commit is gated on OUR resolved path — a failed init must
/// never fall through to a random system libonnxruntime replacing the pinned
/// bundled one, so no `Denoiser` constructor (whose implicit resolution would
/// try exactly that) is ever reached on the error path.
///
/// The light model (see `light_model`) is the adaptive engine's fallback
/// under CPU pressure (issue #14); without it overload goes straight to
/// unfiltered audio. Loading it costs memory and init time in every
/// instance, not CPU: it only runs while the ladder trials or uses it. A
/// chain pinned to the main model (`HUSHMIC_DSP_TIER=quality`) does not
/// load it. Both
/// loads happen here, on the instantiate thread, before the worker exists,
/// and both go through `load_denoiser`, so both tiers run on the selected
/// engine. The runtime is committed even when both models end up native:
/// any fallback to ONNX needs it.
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
        Ok(RuntimeInit::AlreadyInitialized) => {
            if let Some(note) = runtime_note(OUR_RUNTIME.get().map(String::as_str), &dylib) {
                eprintln!("[dpdfnet-ladspa] note: {note}");
            }
        }
        Ok(RuntimeInit::Committed) => {
            let _ = OUR_RUNTIME.set(dylib.clone());
        }
        // RuntimeInit is non_exhaustive: any future variant still means "a
        // runtime is committed", so proceeding is the right default.
        Ok(_) => {}
    }
    let main_path = model_path();
    let hold = voice_hold_from(std::env::var("HUSHMIC_VOICE_HOLD").ok().as_deref());
    if hold {
        eprintln!("[dpdfnet-ladspa] voice hold: on");
    }
    let main = match load_denoiser(&main_path) {
        Ok(mut d) => {
            d.set_voice_hold(hold);
            d
        }
        Err(e) => {
            eprintln!("[dpdfnet-ladspa] engine init failed: {e}");
            return None;
        }
    };
    // The comparison is by canonical path so a symlinked or differently
    // spelled duplicate of the main model is not loaded twice as the
    // "light" tier.
    let same_file = |p: &PathBuf| {
        let canon = |q: &std::path::Path| q.canonicalize().unwrap_or_else(|_| q.to_path_buf());
        canon(p) == canon(&main_path)
    };
    let light_path = match light_model(
        std::env::var_os("HUSHMIC_FALLBACK_MODEL_PATH").as_deref(),
        &main_path,
        std::env::var("HUSHMIC_DSP_TIER").ok().as_deref(),
    ) {
        LightModel::None => None,
        LightModel::Unused => {
            eprintln!("[dpdfnet-ladspa] HUSHMIC_DSP_TIER=quality: the light model is not loaded");
            None
        }
        LightModel::Named(p) => Some(p),
        LightModel::Sibling(p) if same_file(&p) => {
            eprintln!(
                "[dpdfnet-ladspa] {} is the main model itself; no light model to \
                 fall back to",
                p.display()
            );
            None
        }
        LightModel::Sibling(p) if p.exists() => {
            eprintln!(
                "[dpdfnet-ladspa] light model: {} (next to the main model)",
                p.display()
            );
            Some(p)
        }
        LightModel::Sibling(p) => {
            eprintln!(
                "[dpdfnet-ladspa] no light model at {}; under CPU overload the \
                 audio passes unfiltered",
                p.display()
            );
            None
        }
    };
    let light = light_path
        .filter(|p| !same_file(p))
        .and_then(|p| match load_denoiser(&p) {
            Ok(mut d) => {
                d.set_voice_hold(hold);
                Some(d)
            }
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
    /// The control file, read every cycle in place of the Mode and
    /// Attenuation Limit ports when HushMic's chain named one.
    control: Option<&'static ControlReader>,
    last: LastValid,
    /// Mute on this thread: cut within the cycle, reopen only on audio
    /// captured after an unmute (see gate.rs).
    gate: gate::MuteGate,
    /// Output samples of this activation so far (the gate's clock).
    frames: u64,
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
            // The engine view (meter + Mic check tap) only for HushMic's
            // own chain, which names a meter file in the environment.
            worker: engine.and_then(|e| WorkerHandle::spawn_with_view(e, EngineView::from_env())),
            aligner: Aligner::new(),
            active: false,
            restarting: false,
            last_db_bits: f32::NAN.to_bits(),
            last_mode_bits: f32::NAN.to_bits(),
            // Mapped here, at instantiate, never in run().
            control: control(),
            last: LastValid::default(),
            gate: gate::MuteGate::new(),
            frames: 0,
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
        self.gate = gate::MuteGate::new();
        self.frames = 0;
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
        let (db, mode_ctl) = {
            let (db, mode) = (*ports[2].unwrap_control(), *ports[3].unwrap_control());
            match self.control {
                // Two relaxed loads from a page faulted in at instantiate.
                Some(c) => file_controls(&mut self.last, c.attn_db(), c.mode(), db, mode),
                None => (db, mode),
            }
        };
        let at = self.frames;
        self.frames = self.frames.wrapping_add(sample_count as u64);
        self.gate
            .set_muted(mode_from_control(mode_ctl) == Mode::Mute, at);

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
        // The latency in force: plan() grows the lead for a longer cycle.
        self.gate.set_delay(
            (hushmic_denoiser::LATENCY_SAMPLES + self.aligner.lead_total() + gate::ENGINE_REACH)
                as u64,
        );
        self.gate.apply(&mut output[..sample_count], at);
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
                lower_bound: Some(ATTN_MIN),
                upper_bound: Some(ATTN_MAX),
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
    fn runtime_note_only_when_the_runtime_is_not_ours() {
        // Every instance after the first (EasyEffects: the right channel).
        assert_eq!(runtime_note(Some("/usr/lib/x.so"), "/usr/lib/x.so"), None);
        let other = runtime_note(Some("/a/x.so"), "/b/x.so").unwrap();
        assert!(
            other.contains("/a/x.so") && other.contains("/b/x.so"),
            "{other}"
        );
        let foreign = runtime_note(None, "/b/x.so").unwrap();
        assert!(foreign.contains("another component"), "{foreign}");
        assert!(!other.contains("bundled") && !foreign.contains("bundled"));
    }

    #[test]
    fn empty_model_path_means_the_default() {
        use std::ffi::OsStr;
        assert_eq!(model_path_from(None), PathBuf::from(DEFAULT_MODEL));
        assert_eq!(
            model_path_from(Some(OsStr::new(""))),
            PathBuf::from(DEFAULT_MODEL)
        );
        assert_eq!(
            model_path_from(Some(OsStr::new("/m/x.onnx"))),
            PathBuf::from("/m/x.onnx")
        );
    }

    #[test]
    fn light_model_is_found_next_to_the_main_one_unless_named() {
        use std::ffi::OsStr;
        use std::path::Path;
        let q = Path::new("/usr/share/hushmic/models/dpdfnet8_48khz_hr.onnx");
        let sibling =
            LightModel::Sibling("/usr/share/hushmic/models/dpdfnet2_48khz_hr.onnx".into());
        // Unset and empty both mean "find it" (EasyEffects sets nothing).
        assert_eq!(light_model(None, q, None), sibling);
        assert_eq!(light_model(Some(OsStr::new("")), q, None), sibling);
        // A named path wins, existing or not.
        assert_eq!(
            light_model(Some(OsStr::new("/elsewhere/l.onnx")), q, None),
            LightModel::Named("/elsewhere/l.onnx".into())
        );
        // A light main model has no fallback, named or not.
        let l = Path::new("/m/dpdfnet2_48khz_hr.onnx");
        assert_eq!(light_model(None, l, None), LightModel::None);
        assert_eq!(
            light_model(Some(OsStr::new("/m/dpdfnet2_48khz_hr.onnx")), l, None),
            LightModel::None
        );
        // The sibling keeps the main model's extension, or has none.
        assert_eq!(
            light_model(None, Path::new("/m/custom.model"), None),
            LightModel::Sibling("/m/dpdfnet2_48khz_hr.model".into())
        );
        assert_eq!(
            light_model(None, Path::new("/m/custom"), None),
            LightModel::Sibling("/m/dpdfnet2_48khz_hr".into())
        );
        // A relative main model gets a relative sibling in the same place.
        assert_eq!(
            light_model(None, Path::new("dpdfnet8_48khz_hr.onnx"), None),
            LightModel::Sibling("dpdfnet2_48khz_hr.onnx".into())
        );
        // Pinned to the main model, the light one is not loaded, named or
        // not; any other pin keeps it (the ladder decides what it is for).
        assert_eq!(light_model(None, q, Some("quality")), LightModel::Unused);
        assert_eq!(
            light_model(Some(OsStr::new("/elsewhere/l.onnx")), q, Some("quality")),
            LightModel::Unused
        );
        assert_eq!(light_model(None, q, Some("light")), sibling);
        assert_eq!(light_model(None, q, Some("passthrough")), sibling);
        assert_eq!(light_model(None, l, Some("quality")), LightModel::None);
    }

    #[test]
    fn voice_hold_is_off_unless_the_variable_says_on() {
        for on in ["1", "true", "on"] {
            assert!(voice_hold_from(Some(on)), "{on}");
        }
        for off in [
            None,
            Some(""),
            Some("0"),
            Some("false"),
            Some("off"),
            Some("yes please"),
        ] {
            assert!(!voice_hold_from(off), "{off:?}");
        }
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
    fn inference_lines_never_look_like_the_tier_contract() {
        assert_eq!(
            inference_line("dpdfnet8_48khz_hr", "native w7a8 avx-vnni", None),
            "inference: native w7a8 avx-vnni for dpdfnet8_48khz_hr"
        );
        assert_eq!(
            inference_line("dpdfnet2_48khz_hr", "onnx", Some("requested")),
            "inference: onnx (requested) for dpdfnet2_48khz_hr"
        );
        assert!(!inference_line("m", "onnx", None).starts_with("engine: "));
    }

    #[cfg(feature = "native")]
    #[test]
    #[allow(deprecated)]
    fn inference_env_words() {
        use hushmic_denoiser::Inference;
        assert_eq!(inference_from(None), Ok(Inference::Auto));
        assert_eq!(inference_from(Some("")), Ok(Inference::Auto));
        assert_eq!(inference_from(Some("onnx")), Ok(Inference::Onnx));
        assert_eq!(inference_from(Some("native")), Ok(Inference::NativeInt8));
        // Earlier words keep starting the chain: native-int8 and
        // native-w7a8 are the native engine, native-fp32 runs ONNX and says
        // why.
        assert_eq!(
            inference_from(Some("native-int8")),
            Ok(Inference::NativeInt8)
        );
        assert_eq!(
            inference_from(Some("native-w7a8")),
            Ok(Inference::NativeInt8)
        );
        assert_eq!(
            inference_from(Some("native-fp32")),
            Ok(Inference::NativeFp32)
        );
        let e = inference_from(Some("tensorrt")).unwrap_err();
        assert!(e.contains("HUSHMIC_INFERENCE=tensorrt"), "{e}");
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
    fn the_control_file_holds_its_last_valid_word() {
        use hushmic_shm::control::{MODE_BYPASS, MODE_MUTE};
        let mut last = LastValid::default();
        let mut c = |db, mode| file_controls(&mut last, db, mode, 40.0, 0.0);
        // Nothing valid yet: the ports.
        assert_eq!(c(None, None), (40.0, 0.0));
        // Valid words replace the ports.
        assert_eq!(c(Some(24.0), Some(MODE_MUTE)), (24.0, 2.0));
        // An invalid word (none) keeps the mute, never processing.
        assert_eq!(c(None, None), (24.0, 2.0));
        assert_eq!(c(Some(0.0), Some(MODE_BYPASS)), (0.0, 1.0));
        // The values map as the port's do.
        assert_eq!(mode_from_control(c(None, None).1), Mode::Bypass);
    }

    #[test]
    fn mismatched_sample_rate_disables_engine() {
        // 44.1 kHz hosts must get the silent-node degradation, never the 48 kHz
        // model running on wrongly-spaced spectra.
        let p = DpdfnetPlugin::new(44_100);
        assert!(p.worker.is_none());
    }
}
