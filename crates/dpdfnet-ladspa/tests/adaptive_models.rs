//! Asset-gated pins for the adaptive engine with the real models: a pinned
//! model tier must be the bare `Denoiser`, bit for bit (the wrapper adds
//! nothing to the audio), and the raw tier must be the exact delayed input.

mod common;

use dpdfnet_ladspa::{AdaptiveEngine, Event, HopEngine, Ladder, Pinned, Policy, Step, Tier};
use hushmic_denoiser::{Denoiser, HOP, LATENCY_SAMPLES};
use std::sync::Mutex;
use std::time::Instant;

/// Serialize: the tests share the ORT runtime commit.
static LOCK: Mutex<()> = Mutex::new(());

fn read_flac_mono_f32(p: &std::path::Path) -> Vec<f32> {
    let mut r = claxon::FlacReader::open(p).expect("open flac");
    let info = r.streaminfo();
    assert_eq!(info.sample_rate, 48_000);
    assert_eq!(info.channels, 1);
    r.samples()
        .map(|s| s.expect("flac sample") as f32 / 32768.0)
        .collect()
}

fn fixture() -> Vec<f32> {
    read_flac_mono_f32(&common::repo_root().join("tests/fixtures/noisy_public_48k.flac"))
}

fn dev_denoiser(model: &str) -> Option<Denoiser> {
    let mp = common::model_path(model)?;
    let rt = common::runtime_path()?;
    hushmic_denoiser::init_runtime(rt).expect("bundled runtime must load");
    Some(Denoiser::from_file(mp).expect("denoiser"))
}

fn stream(engine: &mut dyn HopEngine, input: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(input.len());
    let mut hop_in = [0f32; HOP];
    let mut hop_out = [0f32; HOP];
    for h in 0..input.len() / HOP {
        hop_in.copy_from_slice(&input[h * HOP..(h + 1) * HOP]);
        engine.process_hop(&hop_in, &mut hop_out).expect("process");
        out.extend_from_slice(&hop_out);
    }
    out
}

fn pinned_tier_matches_bare(main: &str, other: &str, tier: Tier) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (Some(mut bare), Some(a), Some(b)) =
        (dev_denoiser(main), dev_denoiser(main), dev_denoiser(other))
    else {
        eprintln!("skipping: dev assets not provisioned");
        return;
    };
    let noisy = fixture();
    let expected = stream(&mut bare, &noisy);
    let (quality, light) = match tier {
        Tier::Quality => (Some(a), Some(b)),
        _ => (Some(b), Some(a)),
    };
    let mut adaptive = AdaptiveEngine::new(quality, light, Pinned(tier));
    let got = stream(&mut adaptive, &noisy);
    assert_eq!(got.len(), expected.len());
    let diff = got
        .iter()
        .zip(&expected)
        .filter(|(g, e)| g.to_bits() != e.to_bits())
        .count();
    assert_eq!(diff, 0, "{diff} samples differ from the bare engine");
}

#[test]
fn pinned_quality_is_the_bare_quality_denoiser() {
    pinned_tier_matches_bare(
        "dpdfnet8_48khz_hr.onnx",
        "dpdfnet2_48khz_hr.onnx",
        Tier::Quality,
    );
}

#[test]
fn pinned_light_is_the_bare_light_denoiser() {
    pinned_tier_matches_bare(
        "dpdfnet2_48khz_hr.onnx",
        "dpdfnet8_48khz_hr.onnx",
        Tier::Light,
    );
}

// ---------------------------------------------------------------------------
// Load scenarios on real recordings through the real ladder.
//
// The worker's clock is the engine's only view of CPU load, so a scripted
// clock plays the load: every inference advances it by the tier's nominal
// cost times the script's load factor, and the script sets the lag the
// worker would report. Three public clips (fan, café chatter, keyboard)
// each run the same story: a sustained overload that must demote to the
// light model without a single raw hop, the recovery to quality, then a
// burst of stalls (the flapping load of issue #14) whose raw phases must
// be short and whose end must bring a model tier back within the light
// tier's retry cap.
// ---------------------------------------------------------------------------

use std::sync::Arc;
use std::time::Duration;

const HOP_BUDGET: Duration = Duration::from_millis(10);

/// The scripted load, shared by the metered denoisers and the driver.
#[derive(Default)]
struct Load {
    factor: f32,
}

/// A denoiser whose inference time is scripted.
struct Metered {
    inner: Denoiser,
    nominal: f32,
    load: Arc<Mutex<Load>>,
    clock: Arc<Mutex<Duration>>,
}

impl HopEngine for Metered {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        let r = HopEngine::process_hop(&mut self.inner, input, output);
        let f = self.load.lock().unwrap().factor;
        *self.clock.lock().unwrap() += HOP_BUDGET.mul_f32(self.nominal * f);
        r
    }
    fn reset(&mut self) {
        HopEngine::reset(&mut self.inner);
    }
    fn set_mode(&mut self, mode: hushmic_denoiser::Mode) {
        HopEngine::set_mode(&mut self.inner, mode);
    }
    fn set_attenuation_limit_db(&mut self, db: f32) {
        HopEngine::set_attenuation_limit_db(&mut self.inner, db);
    }
}

/// The ladder, with the step it played for every hop and every event it
/// raised, recorded for the assertions.
struct Recorded {
    inner: Ladder,
    hop: u32,
    steps: Arc<Mutex<Vec<Step>>>,
    ducked: Arc<Mutex<Vec<bool>>>,
    events: Arc<Mutex<Vec<(u32, Event)>>>,
}

impl Policy for Recorded {
    fn duck_raw(&self) -> bool {
        self.inner.duck_raw()
    }
    fn step(&self) -> Step {
        self.inner.step()
    }
    fn observe(&mut self, c: f32, s: Option<f32>, lag: u32) -> Option<Event> {
        self.steps.lock().unwrap().push(self.inner.step());
        self.ducked.lock().unwrap().push(self.inner.duck_raw());
        let e = self.inner.observe(c, s, lag);
        if let Some(e) = e {
            self.events.lock().unwrap().push((self.hop, e));
        }
        self.hop += 1;
        e
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
    fn live(&self) -> Tier {
        self.inner.live()
    }
    fn describe(&self) -> String {
        self.inner.describe()
    }
}

fn involves_raw(s: &Step) -> bool {
    match *s {
        Step::Steady { live } => live == Tier::Raw,
        Step::Shadow { live, shadow, .. } => live == Tier::Raw || shadow == Tier::Raw,
        Step::Crossfade { from, to, .. } => from == Tier::Raw || to == Tier::Raw,
    }
}

/// Model output gets four settling hops after a raw interval.
const RAW_SETTLE_HOPS: usize = 4;
/// Sustained passthrough must regain full raw gain within one hop (10 ms).
const RAW_RELEASE_HOPS: usize = 1;

fn with_settling_tail(active: &[bool]) -> Vec<bool> {
    let mut until = 0;
    active
        .iter()
        .enumerate()
        .map(|(h, &active)| {
            if active {
                until = h + 1 + RAW_SETTLE_HOPS;
            }
            h < until
        })
        .collect()
}

fn sustained_raw_hops(steps: &[Step], ducked: &[bool]) -> Vec<bool> {
    assert_eq!(steps.len(), ducked.len());
    steps
        .iter()
        .zip(ducked)
        .map(|(step, &duck)| involves_raw(step) && !duck)
        .collect()
}

#[test]
fn leak_allowance_covers_sustained_raw_and_its_tail_but_not_ducked_raw() {
    let mut steps = vec![Step::Steady { live: Tier::Light }; 25];
    let mut ducked = vec![false; steps.len()];
    steps[2] = Step::Crossfade {
        from: Tier::Quality,
        to: Tier::Raw,
        hop: 0,
        of: 1,
    };
    steps[3] = Step::Shadow {
        live: Tier::Raw,
        shadow: Tier::Light,
        kind: dpdfnet_ladspa::ShadowKind::Rejoin,
    };
    steps[4] = Step::Crossfade {
        from: Tier::Raw,
        to: Tier::Light,
        hop: 0,
        of: 1,
    };
    ducked[2..5].fill(true);
    steps[15] = Step::Crossfade {
        from: Tier::Light,
        to: Tier::Raw,
        hop: 0,
        of: 1,
    };
    steps[16] = Step::Steady { live: Tier::Raw };
    steps[17] = Step::Crossfade {
        from: Tier::Raw,
        to: Tier::Light,
        hop: 0,
        of: 1,
    };
    let allowed = with_settling_tail(&sustained_raw_hops(&steps, &ducked));
    let protected = with_settling_tail(&ducked);
    for h in 0..steps.len() {
        assert_eq!(allowed[h], (15..22).contains(&h), "allowed hop {h}");
        assert_eq!(protected[h], (2..9).contains(&h), "protected hop {h}");
    }
}

fn hop_rms_db(out: &[f32]) -> Vec<f32> {
    out.chunks(HOP)
        .map(|h| {
            let e = h.iter().map(|v| v * v).sum::<f32>() / h.len() as f32;
            10.0 * (e + 1e-12).log10()
        })
        .collect()
}

/// Compare raw and model energy for the same audio samples.
fn delayed_input_db(input: &[f32]) -> Vec<f32> {
    let delayed: Vec<f32> = std::iter::repeat_n(0.0, LATENCY_SAMPLES)
        .chain(input.iter().copied())
        .take(input.len())
        .collect();
    hop_rms_db(&delayed)
}

#[test]
fn leak_reference_has_the_engine_delay() {
    let mut input = vec![0.0; LATENCY_SAMPLES + 2 * HOP];
    input[..HOP].fill(1.0);
    let db = delayed_input_db(&input);
    assert!(db[..LATENCY_SAMPLES / HOP].iter().all(|&v| v < -100.0));
    assert!(db[LATENCY_SAMPLES / HOP].abs() < 1e-6);
    assert!(db[LATENCY_SAMPLES / HOP + 1] < -100.0);
}

/// Nominal inference costs, in hops: a quality hop takes half the budget,
/// a light one a fraction of that (their real ratio on a laptop core).
const Q_COST: f32 = 0.5;
const L_COST: f32 = 0.15;

/// Sustained overload: quality would take 85 % of the budget, the pair
/// fits, so the demotion is preventive and silent.
const OVERLOAD: f32 = 1.7;
/// A burst: quality is over budget outright; light still fits.
const BURST: f32 = 2.4;

const WARM: u32 = 300;
const OVERLOAD_END: u32 = 700;
/// The quality trial after a demotion waits `DWELL_BASE` hops, then runs
/// for `TRIAL_COLD` plus `TRIAL` judged hops before the crossfade.
const QUALITY_BACK_BY: u32 = OVERLOAD_END + 2000 + 100;
const BURST_LEN: u32 = 150;
const BURST_GAP: u32 = 250;
const BURSTS: u32 = 3;
/// The tier above raw retries at most `RAW_RETRY_MAX` hops after it failed.
const MODEL_BACK_WITHIN: u32 = 2000;
const TAIL: u32 = 200;

fn scenario(name: &str, clip: &[f32]) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (Some(q), Some(l), Some(mut light_ref)) = (
        dev_denoiser("dpdfnet8_48khz_hr.onnx"),
        dev_denoiser("dpdfnet2_48khz_hr.onnx"),
        dev_denoiser("dpdfnet2_48khz_hr.onnx"),
    ) else {
        eprintln!("skipping: dev assets not provisioned");
        return;
    };
    let clip_hops = clip.len() / HOP;
    assert!(
        clip_hops >= 500,
        "{name}: fixture too short: {clip_hops} hops"
    );
    // Place the first raw phase where unfiltered noise would be audible,
    // so the duck must suppress a measurable burst.
    let flap_earliest = QUALITY_BACK_BY + 100;
    let flap_end_at = |f: u32| f + BURSTS * (BURST_LEN + BURST_GAP) - BURST_GAP;
    let raw_phase = 1 + 5 + 1;
    // One full clip covers every input position after quality recovers.
    let search_end = flap_earliest as usize + clip_hops;
    let search_hops = search_end + 1 + raw_phase;
    let max_hops = (flap_end_at(search_end as u32) + MODEL_BACK_WITHIN + TAIL) as usize;
    let mut input: Vec<f32> = clip.iter().copied().cycle().take(max_hops * HOP).collect();
    let mut reference = stream(&mut light_ref, &input[..search_hops * HOP]);
    let in_db = delayed_input_db(&input);
    let search_ref_db = hop_rms_db(&reference);
    let leakable = |h: usize| in_db[h] - search_ref_db[h] >= 6.0;
    // The stall is observed after its hop; raw starts on the next hop.
    let flap_at = (flap_earliest as usize..search_end)
        .max_by_key(|&h| {
            (
                (h + 1..h + 1 + raw_phase).filter(|&x| leakable(x)).count(),
                std::cmp::Reverse(h),
            )
        })
        .unwrap() as u32;
    let flap_end = flap_end_at(flap_at);
    let leakable_in_phase = (flap_at as usize + 1..flap_at as usize + 1 + raw_phase)
        .filter(|&x| leakable(x))
        .count();
    assert!(
        leakable_in_phase >= 3,
        "{name}: no stretch where a raw phase would leak ({leakable_in_phase} leakable hops)"
    );

    let total_hops = (flap_end + MODEL_BACK_WITHIN + TAIL) as usize;
    input.truncate(total_hops * HOP);
    if reference.len() < input.len() {
        reference.extend(stream(&mut light_ref, &input[reference.len()..]));
    } else {
        reference.truncate(input.len());
    }
    let ref_db = hop_rms_db(&reference);

    let load = Arc::new(Mutex::new(Load { factor: 1.0 }));
    let clock = Arc::new(Mutex::new(Duration::ZERO));
    let metered = |inner: Denoiser, nominal: f32| Metered {
        inner,
        nominal,
        load: Arc::clone(&load),
        clock: Arc::clone(&clock),
    };
    let steps = Arc::new(Mutex::new(Vec::with_capacity(total_hops)));
    let ducked = Arc::new(Mutex::new(Vec::with_capacity(total_hops)));
    let events = Arc::new(Mutex::new(Vec::new()));
    let policy = Recorded {
        inner: Ladder::new(&[Tier::Quality, Tier::Light, Tier::Raw]),
        hop: 0,
        steps: Arc::clone(&steps),
        ducked: Arc::clone(&ducked),
        events: Arc::clone(&events),
    };
    let base = Instant::now();
    let c = Arc::clone(&clock);
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink_lines = Arc::clone(&lines);
    let mut engine = AdaptiveEngine::with_clock_and_sink(
        Some(metered(q, Q_COST)),
        Some(metered(l, L_COST)),
        policy,
        Box::new(move || base + *c.lock().unwrap()),
        Box::new(move |s: &str| sink_lines.lock().unwrap().push(s.to_string())),
    );

    let mut out = Vec::with_capacity(input.len());
    let mut hop_in = [0f32; HOP];
    let mut hop_out = [0f32; HOP];
    for h in 0..total_hops as u32 {
        let in_flap = h >= flap_at && h < flap_end;
        let burst_pos = (h.saturating_sub(flap_at)) % (BURST_LEN + BURST_GAP);
        let (factor, lag) = if (WARM..OVERLOAD_END).contains(&h) {
            (OVERLOAD, 0)
        } else if in_flap && burst_pos < BURST_LEN {
            (BURST, if burst_pos == 0 { 4 } else { 0 })
        } else {
            (1.0, 0)
        };
        load.lock().unwrap().factor = factor;
        engine.set_lag_hops(lag);
        hop_in.copy_from_slice(&input[h as usize * HOP..(h as usize + 1) * HOP]);
        engine.process_hop(&hop_in, &mut hop_out).expect("process");
        out.extend_from_slice(&hop_out);
    }
    let steps = steps.lock().unwrap();
    let ducked = ducked.lock().unwrap();
    assert_eq!(ducked.len(), total_hops);
    let events = events.lock().unwrap();
    let out_db = hop_rms_db(&out);
    let story = || {
        format!(
            "{name}: flap at {flap_at}..{flap_end}, events {:?}, log {:?}",
            *events,
            *lines.lock().unwrap()
        )
    };

    // Never a hole once the models' delay lines are full: the emergency
    // path fades, it does not mute.
    let holes: Vec<usize> = (LATENCY_SAMPLES / HOP + 1..total_hops)
        .filter(|&h| out[h * HOP..(h + 1) * HOP].iter().all(|&v| v == 0.0))
        .collect();
    assert!(holes.is_empty(), "exact-zero hops {holes:?}; {}", story());

    // Sustained overload: a preventive demotion to light, and no raw hop
    // anywhere near it.
    let demoted_to_light = events
        .iter()
        .find(|(_, e)| {
            matches!(
                e,
                Event::Demoted {
                    to: Tier::Light,
                    lag: 0,
                    ..
                }
            )
        })
        .map(|(h, _)| *h);
    assert!(
        matches!(demoted_to_light, Some(h) if h > WARM && h < WARM + 300),
        "no preventive demotion to light in the overload; {}",
        story()
    );
    let raw_before_flap: Vec<usize> = (0..flap_at as usize)
        .filter(|&h| involves_raw(&steps[h]))
        .collect();
    assert!(
        raw_before_flap.is_empty(),
        "raw played outside the flap at hops {raw_before_flap:?}; {}",
        story()
    );
    // ... and quality is back once the load is gone.
    let quality_back = events
        .iter()
        .find(|(h, e)| *h > OVERLOAD_END && matches!(e, Event::Promoted { to: Tier::Quality }))
        .map(|(h, _)| *h);
    assert!(
        matches!(quality_back, Some(h) if h < QUALITY_BACK_BY),
        "quality not back by hop {QUALITY_BACK_BY}; {}",
        story()
    );
    assert_eq!(
        steps[flap_at as usize - 1],
        Step::Steady {
            live: Tier::Quality
        },
        "quality not live when the flap starts; {}",
        story()
    );

    // The first stall enters raw in one hop and warms light before rejoining.
    // The held raw output must be quiet without becoming digital silence.
    let first_raw = (flap_at as usize..)
        .find(|&h| involves_raw(&steps[h]))
        .unwrap();
    assert!(
        first_raw <= flap_at as usize + 1,
        "stall at {flap_at} took until {first_raw} to reach raw; {}",
        story()
    );
    let raw_run = (first_raw..)
        .take_while(|&h| involves_raw(&steps[h]))
        .count();
    assert_eq!(raw_run, raw_phase, "first raw phase length; {}", story());
    assert!(
        ducked[first_raw..first_raw + raw_run]
            .iter()
            .all(|&duck| duck),
        "first raw phase was not fully ducked; {}",
        story()
    );
    let held: Vec<usize> = (0..total_hops)
        .filter(|&h| {
            ducked[h]
                && matches!(
                    steps[h],
                    Step::Shadow {
                        live: Tier::Raw,
                        kind: dpdfnet_ladspa::ShadowKind::Rejoin,
                        ..
                    }
                )
        })
        .collect();
    assert_eq!(
        held.iter()
            .filter(|&&h| (first_raw..first_raw + raw_run).contains(&h))
            .count(),
        5,
        "first held Rejoin hops; {}",
        story()
    );
    let mut peak_ratio_db = f32::NEG_INFINITY;
    let mut peak_dbfs = f32::NEG_INFINITY;
    for h in held {
        assert!(
            out_db[h] <= in_db[h] - 30.0,
            "held raw hop {h}: output {} dB, delayed input {} dB; {}",
            out_db[h],
            in_db[h],
            story()
        );
        let peak = |samples: &[f32]| samples.iter().copied().map(f32::abs).fold(0.0, f32::max);
        let output_peak = peak(&out[h * HOP..(h + 1) * HOP]);
        let input_peak = peak(&input[h * HOP - LATENCY_SAMPLES..(h + 1) * HOP - LATENCY_SAMPLES]);
        peak_ratio_db = peak_ratio_db.max(20.0 * (output_peak / input_peak).log10());
        peak_dbfs = peak_dbfs.max(20.0 * output_peak.log10());
    }
    eprintln!(
        "{name}: held Rejoin peak relative to delayed input {peak_ratio_db:.2} dB, output peak {peak_dbfs:.2} dBFS"
    );

    let leaks = |h: usize| out_db[h] > ref_db[h] + 6.0 && out_db[h] > in_db[h] - 6.0;
    let sustained = sustained_raw_hops(&steps, &ducked);
    let allowed = with_settling_tail(&sustained);
    let protected = with_settling_tail(&ducked);
    let first_leaks = (first_raw..first_raw + raw_run)
        .filter(|&h| leaks(h))
        .count();
    assert_eq!(first_leaks, 0, "first ducked raw phase leaked; {}", story());
    let stray: Vec<usize> = (flap_at as usize - 20..total_hops)
        .filter(|&h| (protected[h] || !allowed[h]) && leaks(h))
        .collect();
    assert!(
        stray.is_empty(),
        "noise leaked outside sustained raw and its settling tail at hops {stray:?}; {}",
        story()
    );

    let mut sustained_start = None;
    let mut releases = 0;
    for h in first_raw..total_hops {
        if !sustained[h] {
            sustained_start = None;
            continue;
        }
        let start = *sustained_start.get_or_insert(h);
        if h + 1 == start + RAW_RELEASE_HOPS {
            let last = (h + 1) * HOP - 1;
            assert!(
                (out[last] - input[last - LATENCY_SAMPLES]).abs() < 1e-7,
                "sustained raw did not reach unity within {RAW_RELEASE_HOPS} hop at {h}; {}",
                story()
            );
            releases += 1;
        }
        if h >= start + RAW_RELEASE_HOPS
            && matches!(
                steps[h],
                Step::Steady { live: Tier::Raw }
                    | Step::Shadow {
                        live: Tier::Raw,
                        ..
                    }
            )
        {
            assert!(
                out[h * HOP..(h + 1) * HOP]
                    .iter()
                    .zip(&input[h * HOP - LATENCY_SAMPLES..(h + 1) * HOP - LATENCY_SAMPLES])
                    .all(|(output, input)| (output - input).abs() < 1e-7),
                "sustained raw stayed attenuated at {h}; {}",
                story()
            );
        }
    }
    assert!(
        releases > 0,
        "scenario never checked sustained raw release; {}",
        story()
    );

    // After the last burst a model tier is back within the light tier's
    // retry cap and raw is not played again.
    let model_back = (flap_end as usize..total_hops).find(|&h| !involves_raw(&steps[h]));
    assert!(
        matches!(model_back, Some(h) if h < (flap_end + MODEL_BACK_WITHIN) as usize),
        "still on passthrough {MODEL_BACK_WITHIN} hops after the last burst; {}",
        story()
    );
    let raw_after: Vec<usize> = (model_back.unwrap()..total_hops)
        .filter(|&h| involves_raw(&steps[h]))
        .collect();
    assert!(
        raw_after.is_empty(),
        "raw came back after the flap at {raw_after:?}; {}",
        story()
    );
}

#[test]
fn fan_noise_survives_overload_and_a_flapping_load() {
    scenario("fan", &fixture());
}

#[test]
fn cafe_chatter_survives_overload_and_a_flapping_load() {
    scenario(
        "cafe",
        &read_flac_mono_f32(&common::repo_root().join("tests/fixtures/noisy_cafe_48k.flac")),
    );
}

#[test]
fn keyboard_noise_survives_overload_and_a_flapping_load() {
    scenario(
        "keyboard",
        &read_flac_mono_f32(&common::repo_root().join("tests/fixtures/noisy_keyboard_48k.flac")),
    );
}

#[test]
fn pinned_raw_is_the_input_delayed_by_the_engine_latency() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(q) = dev_denoiser("dpdfnet8_48khz_hr.onnx") else {
        eprintln!("skipping: dev assets not provisioned");
        return;
    };
    let noisy = fixture();
    let mut adaptive = AdaptiveEngine::new(Some(q), None, Pinned(Tier::Raw));
    let got = stream(&mut adaptive, &noisy);
    let n = got.len();
    assert!(got[..LATENCY_SAMPLES].iter().all(|&v| v == 0.0));
    assert_eq!(got[LATENCY_SAMPLES..], noisy[..n - LATENCY_SAMPLES]);
}
