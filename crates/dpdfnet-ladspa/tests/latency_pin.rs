//! Asset-gated end-to-end pin (issue #10): the whole plugin — real model,
//! real worker thread, real rings — delays audio by EXACTLY the declared
//! latency at the design quantum. Bypass mode is the measurement path
//! (latency-aligned raw), same as the engine crate's own latency tests.
//!
//! Timing note: cycles are paced at real time (10 ms per 480-sample
//! callback). A worker late by more than the 20 ms headroom gets silence
//! substituted and its late audio dropped (the latency holds, the sound
//! goes), which a fresh instance on a CPU-limited box does while it starts.
//! So the impulse goes in only once the plugin streams (a faint mark comes
//! through), and again every 30 cycles until one surfaces: each one that
//! does must sit at exactly the declared latency.
//!
//! Realtime suite: the plugin runs at real-time pace here (a worker that
//! falls behind substitutes silence, as in a call), so it needs the CPU a
//! live stream needs. The timing sweep proves it at a 25% CPU quota, not
//! the 6% the other suites get; a failure at 25% is a real finding.

mod common;

use dpdfnet_ladspa::{get_ladspa_descriptor, PLUGIN_LATENCY_SAMPLES};
use ladspa::{Plugin, PluginDescriptor, PortConnection, PortData};
use std::cell::RefCell;
use std::sync::Mutex;
use std::time::Duration;

const Q: usize = 480;

/// Serialize the tests (they share process-wide env + the ORT runtime).
static LOCK: Mutex<()> = Mutex::new(());

/// Point the plugin's own resolution at the dev assets. Returns false
/// (skip) when assets are not provisioned.
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

/// One host callback against a boxed plugin instance.
fn run_cycle(p: &mut Box<dyn Plugin + Send>, input: &[f32], db: f32, mode: f32) -> Vec<f32> {
    let mut out = vec![0f32; input.len()];
    {
        let d = descriptor();
        let conns = [
            PortConnection {
                port: d.ports[0],
                data: PortData::AudioInput(input),
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
        p.run(input.len(), &refs);
    }
    out
}

fn descriptor() -> PluginDescriptor {
    get_ladspa_descriptor(0).expect("descriptor 0")
}

/// A faint mark the plugin passes in bypass and passthrough: it says the
/// worker streams (silence substituted for late audio reads as 0).
const MARK: f32 = 1e-3;

fn try_impulse() -> Result<(), String> {
    let d = descriptor();
    let mut p = (d.new)(&d, 48_000);
    p.activate();
    let result = impulse_pin(&mut p);
    p.deactivate();
    result
}

fn impulse_pin(p: &mut Box<dyn Plugin + Send>) -> Result<(), String> {
    let mut emitted: Vec<f32> = Vec::new();
    let mut sent: Vec<usize> = Vec::new();
    let mut streaming = 0;
    let mut next: Option<usize> = None;
    let mut streams_at = None;
    // a minute of cycles: only a plugin that never streams runs out
    for c in 0..6_000 {
        let mut input = vec![MARK; Q];
        if streaming >= 5 && next.is_none_or(|n| c >= n) {
            input[7] = 1.0;
            sent.push(c * Q + 7);
            next = Some(c + 30);
        }
        let out = run_cycle(p, &input, 100.0, 1.0); // bypass
        streaming = if out.iter().all(|&v| v > MARK / 2.0) {
            streaming + 1
        } else {
            0
        };
        if streaming == 1 && streams_at.is_none() {
            streams_at = Some(c);
        }
        let from = emitted.len();
        emitted.extend(out);
        std::thread::sleep(Duration::from_millis(10)); // real-time pacing
        let Some(pos) = emitted[from..]
            .iter()
            .position(|&v| v > 0.5)
            .map(|i| from + i)
        else {
            continue;
        };
        if !sent.iter().any(|&at| at + PLUGIN_LATENCY_SAMPLES == pos) {
            return Err(format!(
                "impulse at {pos}, declared {} after one of {sent:?}",
                PLUGIN_LATENCY_SAMPLES
            ));
        }
        // nothing but the mark (or silence) around it: no stale audio, no
        // leakage
        if let Some(bad) = emitted
            .iter()
            .enumerate()
            .find(|&(i, v)| i != pos && v.abs() > 0.01)
        {
            return Err(format!("leakage {bad:?}"));
        }
        let k = sent
            .iter()
            .position(|&at| at + PLUGIN_LATENCY_SAMPLES == pos);
        eprintln!(
            "streaming from cycle {streams_at:?}; impulse {} of {} surfaced",
            k.map_or(0, |k| k + 1),
            sent.len()
        );
        return Ok(());
    }
    Err(format!(
        "no impulse surfaced (streaming for {streaming} cycles, {} sent)",
        sent.len()
    ))
}

#[test]
fn bypass_impulse_reappears_at_exactly_the_declared_latency() {
    let _g = LOCK.lock().unwrap();
    if !setup_env() {
        return;
    }
    if let Err(e) = try_impulse() {
        panic!("latency pin: {e}");
    }
}

/// The raw tier of the adaptive engine (issue #14) must land on the same
/// declared latency as the models: `HUSHMIC_DSP_TIER=passthrough` pins it
/// and the impulse must surface at exactly PLUGIN_LATENCY_SAMPLES.
#[test]
fn passthrough_tier_keeps_the_declared_latency() {
    let _g = LOCK.lock().unwrap();
    if !setup_env() {
        return;
    }
    std::env::set_var("HUSHMIC_DSP_TIER", "passthrough");
    let result = try_impulse();
    std::env::remove_var("HUSHMIC_DSP_TIER");
    if let Err(e) = result {
        panic!("passthrough latency pin: {e}");
    }
}

#[test]
fn processed_mode_streams_finite_nonsilent_audio() {
    let _g = LOCK.lock().unwrap();
    if !setup_env() {
        return;
    }
    let d = descriptor();
    let mut p = (d.new)(&d, 48_000);
    p.activate();
    let mut emitted = Vec::new();
    for c in 0..24 {
        // A steady tone is exactly what the suppressor removes, so pin
        // the attenuation limit at 0 dB: the limiter blends the aligned
        // input back fully while the model still runs every hop — the
        // whole inference path is exercised, and the output is audible.
        let input: Vec<f32> = (0..Q)
            .map(|i| {
                let t = (c * Q + i) as f32 / 48_000.0;
                0.5 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
            })
            .collect();
        emitted.extend(run_cycle(&mut p, &input, 0.0, 0.0)); // process
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        emitted.iter().all(|v| v.is_finite()),
        "processed audio must stay finite"
    );
    assert!(
        emitted.iter().any(|&v| v.abs() > 0.01),
        "processed audio must not be silence"
    );
}
