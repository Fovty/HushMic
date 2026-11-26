//! The mute gate through the plugin's LADSPA interface, real model and real
//! worker (asset-gated): a mute cuts what is queued within the cycle that
//! sees it, an unmute plays nothing captured while muted and opens with a
//! ramp, and run() allocates nothing doing it. The port drives the mode;
//! the control file goes through the same gate.
//!
//! Timing note: cycles are paced at real time (10 ms per 480 samples). A
//! runner stall only substitutes silence, which none of the checks below
//! mistakes for sound; the "speech is flowing" precondition retries.

mod common;

use dpdfnet_ladspa::gate::{ENGINE_REACH, RAMP};
use dpdfnet_ladspa::{get_ladspa_descriptor, required_lead, OUTPUT_LEAD, PLUGIN_LATENCY_SAMPLES};
use ladspa::{Plugin, PluginDescriptor, PortConnection, PortData};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// The design quantum; the gate also holds at a longer one (the ledger's
/// lead grows, and the gate's delay with it).
const Q: usize = 480;
/// Longer cycles grow the lead by 992, 2016 and 4064 samples.
const LONG_QS: [usize; 3] = [1024, 2048, 4096];
const PROCESS: f32 = 0.0;
const BYPASS: f32 = 1.0;
const MUTE: f32 = 2.0;

/// Serialize the tests (they share process-wide env + the ORT runtime).
static LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    static IN_RUN: Cell<bool> = const { Cell::new(false) };
}
static RUN_ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

fn count() {
    if IN_RUN.with(Cell::get) {
        RUN_ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: forwards every call to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count();
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        count();
        System.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        count();
        System.realloc(p, l, n)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
}

#[global_allocator]
static A: Counting = Counting;

fn setup_env() -> bool {
    let (Some(model), Some(rt)) = (
        common::model_path("dpdfnet2_48khz_hr.onnx"),
        common::runtime_path(),
    ) else {
        eprintln!("skipping: dev assets not provisioned");
        return false;
    };
    std::env::set_var("HUSHMIC_MODEL_PATH", model);
    std::env::set_var("ORT_DYLIB_PATH", rt);
    true
}

fn descriptor() -> PluginDescriptor {
    get_ladspa_descriptor(0).expect("descriptor 0")
}

/// The committed public speech clip, mono f32.
fn speech() -> Vec<f32> {
    let p = common::repo_root().join("tests/fixtures/noisy_public_48k.flac");
    let mut r = claxon::FlacReader::open(&p).expect("open flac");
    r.samples()
        .map(|s| s.expect("flac sample") as f32 / 32768.0)
        .collect()
}

struct Host {
    plugin: Box<dyn Plugin + Send>,
    /// Samples per cycle.
    q: usize,
    speech: Vec<f32>,
    n: usize,
}

impl Host {
    fn new(q: usize) -> Host {
        let d = descriptor();
        let mut plugin = (d.new)(&d, 48_000);
        plugin.activate();
        Host {
            plugin,
            speech: speech(),
            q,
            n: 0,
        }
    }

    /// One real-time cycle with `mode` on the port; speech, or silence.
    fn cycle(&mut self, mode: f32, talking: bool) -> (Vec<f32>, Vec<f32>) {
        let len = self.speech.len();
        let q = self.q;
        let input: Vec<f32> = (0..q)
            .map(|k| {
                if talking {
                    self.speech[(self.n * q + k) % len]
                } else {
                    0.0
                }
            })
            .collect();
        self.n += 1;
        let mut out = vec![0f32; q];
        {
            let d = descriptor();
            let db = 100.0f32;
            let conns = [
                PortConnection {
                    port: d.ports[0],
                    data: PortData::AudioInput(&input),
                },
                PortConnection {
                    port: d.ports[1],
                    data: PortData::AudioOutput(RefCell::new(&mut out[..])),
                },
                PortConnection {
                    port: d.ports[2],
                    data: PortData::ControlInput(&db),
                },
                PortConnection {
                    port: d.ports[3],
                    data: PortData::ControlInput(&mode),
                },
            ];
            let refs: Vec<&PortConnection> = conns.iter().collect();
            IN_RUN.with(|f| f.set(true));
            self.plugin.run(q, &refs);
            IN_RUN.with(|f| f.set(false));
        }
        std::thread::sleep(Duration::from_micros(q as u64 * 1_000_000 / 48_000));
        (input, out)
    }

    /// Talk in `mode` until speech comes out (the queue is full of it).
    fn flowing(&mut self, mode: f32) -> bool {
        let mut loud = 0;
        for _ in 0..200 {
            let (_, out) = self.cycle(mode, true);
            loud = if out.iter().any(|v| v.abs() > 0.02) {
                loud + 1
            } else {
                0
            };
            if loud >= 3 {
                return true;
            }
        }
        false
    }
}

#[test]
fn a_mute_cuts_queued_speech_within_the_cycle() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !setup_env() {
        return;
    }
    for q in [Q, 1024, 2048, 4096] {
        let mut h = Host::new(q);
        assert!(h.flowing(BYPASS), "speech never came through");
        // Mute while the pipeline is full of speech: the cycle that sees it
        // ramps down within 5 ms, and nothing after.
        let (_, out) = h.cycle(MUTE, true);
        assert!(
            out[RAMP..].iter().all(|&v| v.to_bits() == 0),
            "q {q}: sound after the ramp in the muting cycle"
        );
        for c in 0..30 {
            let (_, out) = h.cycle(MUTE, true);
            assert!(
                out.iter().all(|&v| v.to_bits() == 0),
                "q {q}: sound in cycle {c}"
            );
        }
    }
    assert_eq!(RUN_ALLOCS.load(Ordering::Relaxed), 0, "run() allocated");
}

#[test]
fn an_unmute_plays_nothing_captured_while_muted() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !setup_env() {
        return;
    }
    // Both shipped models (the gate's margin covers their reach back), and
    // a longer cycle (the ledger's lead grows; the gate waits for it).
    for (model, unmute_to, q) in [
        ("dpdfnet2_48khz_hr.onnx", BYPASS, Q),
        ("dpdfnet2_48khz_hr.onnx", PROCESS, Q),
        ("dpdfnet8_48khz_hr.onnx", PROCESS, Q),
        // The growth at 1024 stays within the reach margin, so only
        // processing (which reaches back) would show it there.
        ("dpdfnet2_48khz_hr.onnx", PROCESS, LONG_QS[0]),
        ("dpdfnet2_48khz_hr.onnx", BYPASS, LONG_QS[1]),
        ("dpdfnet8_48khz_hr.onnx", PROCESS, LONG_QS[1]),
        ("dpdfnet2_48khz_hr.onnx", BYPASS, LONG_QS[2]),
    ] {
        let path = common::model_path(model).expect("model");
        std::env::set_var("HUSHMIC_MODEL_PATH", path);
        let delay =
            hushmic_denoiser::LATENCY_SAMPLES + required_lead(q).max(OUTPUT_LEAD) + ENGINE_REACH;
        let mut h = Host::new(q);
        assert!(h.flowing(BYPASS), "speech never came through");
        // Talk while muted: the engine's delay and the rings fill with it.
        for _ in 0..40 {
            h.cycle(MUTE, true);
        }
        // Unmuted into silence: any sound now was captured while muted.
        // Until the unmuting cycle's own input comes out, past the model's
        // reach back, the gate gives exact zeros; after it only float
        // residue (about -150 dBFS) is allowed, never speech.
        for c in 0..40 {
            let (_, out) = h.cycle(unmute_to, false);
            let closed = (c + 1) * q <= delay;
            for &v in &out {
                assert!(
                    if closed {
                        v.to_bits() == 0
                    } else {
                        v.abs() < 1e-4
                    },
                    "{model} q {q} mode {unmute_to}: {v} in cycle {c} after the unmute"
                );
            }
        }
    }
    assert_eq!(RUN_ALLOCS.load(Ordering::Relaxed), 0, "run() allocated");
}

#[test]
fn an_unmute_opens_on_new_speech_with_a_ramp() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !setup_env() {
        return;
    }
    let mut h = Host::new(Q);
    assert!(h.flowing(BYPASS), "speech never came through");
    for _ in 0..40 {
        h.cycle(MUTE, true);
    }
    // Unmuted (bypass) while talking on: the output opens with a ramp
    // where the unmuting cycle's input comes out, past the model's reach.
    let mut input = Vec::new();
    let mut out = Vec::new();
    for _ in 0..(PLUGIN_LATENCY_SAMPLES + ENGINE_REACH + 20 * Q) / Q {
        let (i, o) = h.cycle(BYPASS, true);
        input.extend(i);
        out.extend(o);
    }
    let peak = input.iter().fold(0f32, |m, v| m.max(v.abs()));
    let l = PLUGIN_LATENCY_SAMPLES + ENGINE_REACH;
    assert!(
        out[..l].iter().all(|&v| v.to_bits() == 0),
        "audio from before the unmute"
    );
    for k in 0..RAMP {
        let bound = (k + 1) as f32 / RAMP as f32 * peak * 1.05 + 1e-4;
        assert!(
            out[l + k].abs() <= bound,
            "no ramp at {k}: {} > {bound}",
            out[l + k]
        );
    }
    assert!(
        out[l..].iter().any(|v| v.abs() > 0.02),
        "the gate never opened"
    );
    assert_eq!(RUN_ALLOCS.load(Ordering::Relaxed), 0, "run() allocated");
}
