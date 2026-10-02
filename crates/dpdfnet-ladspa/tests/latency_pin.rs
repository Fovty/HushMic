//! Asset-gated end-to-end pin (issue #10): the whole plugin — real model,
//! real worker thread, real rings — delays audio by EXACTLY the declared
//! latency at the design quantum. Bypass mode is the measurement path
//! (latency-aligned raw), same as the engine crate's own latency tests.
//!
//! Timing note: cycles are paced at real time (10 ms per 480-sample
//! callback); a CI stall longer than the 40 ms headroom would substitute
//! zeros and swallow the impulse, so the impulse check retries a few
//! times before failing.

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

fn try_impulse() -> Result<(), String> {
    let d = descriptor();
    let mut p = (d.new)(&d, 48_000);
    p.activate();
    let impulse_at = 5 * Q + 7;
    let cycles = 2 + (impulse_at + PLUGIN_LATENCY_SAMPLES) / Q;
    let mut emitted = Vec::new();
    for c in 0..cycles {
        let mut input = vec![0f32; Q];
        if (c * Q..(c + 1) * Q).contains(&impulse_at) {
            input[impulse_at - c * Q] = 1.0;
        }
        emitted.extend(run_cycle(&mut p, &input, 100.0, 1.0)); // bypass
        std::thread::sleep(Duration::from_millis(10)); // real-time pacing
    }
    p.deactivate();
    let pos = emitted
        .iter()
        .position(|&v| v.abs() > 0.5)
        .ok_or("impulse never surfaced")?;
    if pos != impulse_at + PLUGIN_LATENCY_SAMPLES {
        return Err(format!(
            "impulse at {pos}, declared {} (input at {impulse_at})",
            impulse_at + PLUGIN_LATENCY_SAMPLES
        ));
    }
    // Nothing but near-silence before it (no stale audio, no leakage).
    if let Some(bad) = emitted[..pos].iter().find(|v| v.abs() > 0.01) {
        return Err(format!("pre-impulse leakage {bad}"));
    }
    Ok(())
}

#[test]
fn bypass_impulse_reappears_at_exactly_the_declared_latency() {
    let _g = LOCK.lock().unwrap();
    if !setup_env() {
        return;
    }
    let mut last = String::new();
    for attempt in 0..3 {
        match try_impulse() {
            Ok(()) => return,
            Err(e) => {
                eprintln!("attempt {attempt}: {e} (CI stall?); retrying");
                last = e;
            }
        }
    }
    panic!("latency pin failed 3 attempts; last: {last}");
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
    let mut last = String::new();
    let mut ok = false;
    for attempt in 0..3 {
        match try_impulse() {
            Ok(()) => {
                ok = true;
                break;
            }
            Err(e) => {
                eprintln!("attempt {attempt}: {e} (CI stall?); retrying");
                last = e;
            }
        }
    }
    std::env::remove_var("HUSHMIC_DSP_TIER");
    assert!(
        ok,
        "passthrough latency pin failed 3 attempts; last: {last}"
    );
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
