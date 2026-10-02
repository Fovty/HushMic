//! Repro harness for the ladder's emergency gap (evaluation, 2026-10).
//!
//! Drives the real worker, aligner and adaptive engine with the real models
//! exactly the way the plugin's `run()` does, at real 10 ms pacing, while
//! busy loops load the worker's CPU on the CI stress steps' schedules. Every
//! hop the ladder observes is recorded, and every run of exact zeros in the
//! output (what `ci/analyze-gaps.py` counts as a gap) is traced back to the
//! ladder state that let the lag build.
//!
//! ```text
//! cargo run --release --example ladder_gap -- [key=value ...]
//!   engine=onnx|native   scenario=continuity|flap|long|none   rt=0|1
//!   worker_cpu=2 driver_cpu=4 control_cpu=5 hogs=2 dwell=500
//!   pre=6 (seconds on the top tier before the load) trace=1 csv=hops.csv
//! ```
//!
//! `rt=1` puts the worker on SCHED_RR 10 (needs CAP_SYS_NICE, e.g. a
//! container with `--cap-add SYS_NICE`). The last line is a `SUMMARY`.

use dpdfnet_ladspa::{
    AdaptiveEngine, Aligner, Event, Ladder, Policy, ShadowKind, Step, Tier, WorkerHandle,
};
use hushmic_denoiser::{Inference, HOP};
use std::collections::BTreeMap;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
struct Rec {
    /// Microseconds since the start, when the hop was observed.
    t: u64,
    step: Step,
    live: f32,
    shadow: Option<f32>,
    lag: u32,
    event: Option<Event>,
}

struct Recording<P: Policy> {
    inner: P,
    log: Arc<Mutex<Vec<Rec>>>,
    t0: Instant,
}

impl<P: Policy> Policy for Recording<P> {
    fn step(&self) -> Step {
        self.inner.step()
    }
    fn duck_raw(&self) -> bool {
        self.inner.duck_raw()
    }
    fn observe(&mut self, live: f32, shadow: Option<f32>, lag: u32) -> Option<Event> {
        let step = self.inner.step();
        let event = self.inner.observe(live, shadow, lag);
        self.log.lock().unwrap().push(Rec {
            t: self.t0.elapsed().as_micros() as u64,
            step,
            live,
            shadow,
            lag,
            event,
        });
        event
    }
    fn note_pause(&mut self, hops: f32) {
        self.inner.note_pause(hops)
    }
    fn reset(&mut self) {
        self.inner.reset()
    }
    fn live(&self) -> Tier {
        self.inner.live()
    }
}

fn arg<'a>(args: &'a BTreeMap<String, String>, k: &str, d: &'a str) -> &'a str {
    args.get(k).map(String::as_str).unwrap_or(d)
}

fn pin(tid: libc::pid_t, cpu: usize) {
    // SAFETY: a zeroed cpu_set_t is valid; CPU_SET writes inside it.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        assert_eq!(
            libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set),
            0,
            "pin {tid} to {cpu}"
        );
    }
}

fn worker_tid() -> libc::pid_t {
    for _ in 0..200 {
        for e in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
            let comm = std::fs::read_to_string(e.path().join("comm")).unwrap_or_default();
            if comm.trim() == "hushmic-dsp" {
                return e.file_name().to_str().unwrap().parse().unwrap();
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no hushmic-dsp thread");
}

fn gettid() -> libc::pid_t {
    // SAFETY: no preconditions.
    unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t }
}

/// The CI stress signal: noise plus a 220 Hz tone, never an exact zero.
fn signal(n: usize) -> Vec<f32> {
    let mut x: u64 = 14;
    let mut rnd = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x >> 11) as f32 / (1u64 << 53) as f32
    };
    (0..n)
        .map(|i| {
            // Sum of uniforms, roughly gaussian.
            let g = (0..4).map(|_| rnd()).sum::<f32>() - 2.0;
            0.06 * g * 1.7 + 0.05 * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / 48_000.0).sin()
        })
        .collect()
}

fn step_word(s: &Step) -> String {
    match *s {
        Step::Steady { live } => format!("steady {}", live.word()),
        Step::Shadow { live, shadow, kind } => {
            let k = match kind {
                ShadowKind::Demote => "demote",
                ShadowKind::Trial => "trial",
                ShadowKind::Rejoin => "rejoin",
            };
            format!("{k} {}+{}", live.word(), shadow.word())
        }
        Step::Crossfade { from, to, hop, of } => {
            format!("xfade {}->{} {}/{}", from.word(), to.word(), hop, of)
        }
    }
}

/// The class of a gap: what was running when the lag started to build.
fn class_of(s: &Step) -> String {
    match *s {
        Step::Steady { live } => format!("steady-{}", live.word()),
        Step::Shadow { shadow, kind, .. } => match kind {
            ShadowKind::Demote => "demote-warmup".into(),
            ShadowKind::Trial => format!("trial-{}", shadow.word()),
            ShadowKind::Rejoin => format!("rejoin-{}", shadow.word()),
        },
        Step::Crossfade { to, .. } => format!("xfade-to-{}", to.word()),
    }
}

fn main() {
    let args: BTreeMap<String, String> = std::env::args()
        .skip(1)
        .filter_map(|a| {
            a.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect();
    let engine = arg(&args, "engine", "onnx").to_string();
    let scenario = arg(&args, "scenario", "continuity").to_string();
    let rt = arg(&args, "rt", "0") == "1";
    let worker_cpu: usize = arg(&args, "worker_cpu", "2").parse().unwrap();
    let driver_cpu: usize = arg(&args, "driver_cpu", "4").parse().unwrap();
    let control_cpu: usize = arg(&args, "control_cpu", "5").parse().unwrap();
    let hogs: usize = arg(&args, "hogs", "2").parse().unwrap();
    let dwell: u32 = arg(&args, "dwell", "500").parse().unwrap();
    let pre: f64 = arg(&args, "pre", "6").parse().unwrap();
    let trace = arg(&args, "trace", "1") == "1";
    let csv = args.get("csv").cloned();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let models = root.join("assets/models");

    hushmic_denoiser::init_runtime(root.join("assets/lib/libonnxruntime.so")).unwrap();
    let inference = match engine.as_str() {
        "onnx" => Inference::Onnx,
        _ => Inference::Auto,
    };
    let q =
        dpdfnet_ladspa::load_denoiser_with(&models.join("dpdfnet8_48khz_hr.onnx"), Ok(inference))
            .unwrap();
    let l =
        dpdfnet_ladspa::load_denoiser_with(&models.join("dpdfnet2_48khz_hr.onnx"), Ok(inference))
            .unwrap();

    // The load schedule, in seconds after `pre`: (start, end) of each burst.
    let bursts: Vec<(f64, f64)> = match scenario.as_str() {
        "continuity" => vec![(0.0, 20.0)],
        "flap" => vec![(0.0, 4.0), (8.0, 12.0), (16.0, 20.0)],
        "long" => vec![(0.0, 120.0)],
        _ => vec![],
    };
    let total = pre + bursts.iter().map(|b| b.1).fold(20.0, f64::max) + 15.0;

    let t0 = Instant::now();
    let log = Arc::new(Mutex::new(Vec::<Rec>::with_capacity(20_000)));
    let lines = Arc::new(Mutex::new(Vec::<(u64, String)>::new()));
    let sink = Arc::clone(&lines);
    let policy = Recording {
        inner: Ladder::new(&[Tier::Quality, Tier::Light, Tier::Raw]).with_dwell_base(dwell),
        log: Arc::clone(&log),
        t0,
    };
    let ae = AdaptiveEngine::with_clock_and_sink(
        Some(q),
        Some(l),
        policy,
        Box::new(Instant::now),
        Box::new(move |s: &str| {
            sink.lock()
                .unwrap()
                .push((t0.elapsed().as_micros() as u64, s.to_string()))
        }),
    );
    let mut w = WorkerHandle::spawn_timeshare(ae).expect("spawn");
    let wt = worker_tid();
    pin(wt, worker_cpu);
    if rt {
        let p = libc::sched_param { sched_priority: 10 };
        // SAFETY: valid tid and parameter.
        let r = unsafe { libc::sched_setscheduler(wt, libc::SCHED_RR, &p) };
        assert_eq!(r, 0, "SCHED_RR needs CAP_SYS_NICE");
    }

    // The control sleeper (as in CI): stalls of 20 ms or more on an idle CPU.
    let stop = Arc::new(AtomicBool::new(false));
    let stalls = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
    let control = {
        let (stop, stalls) = (Arc::clone(&stop), Arc::clone(&stalls));
        std::thread::spawn(move || {
            pin(gettid(), control_cpu);
            while !stop.load(Ordering::Relaxed) {
                let t = Instant::now();
                std::thread::sleep(Duration::from_millis(5));
                let d = t.elapsed();
                if d >= Duration::from_millis(25) {
                    stalls.lock().unwrap().push((
                        t.duration_since(t0).as_micros() as u64,
                        d.as_micros() as u64,
                    ));
                }
            }
        })
    };
    // The worker's scheduler wait (schedstat run_delay), sampled every 100 ms.
    let delays = Arc::new(Mutex::new(Vec::<(u64, u64, u64)>::new()));
    let sampler = {
        let (stop, delays) = (Arc::clone(&stop), Arc::clone(&delays));
        std::thread::spawn(move || {
            pin(gettid(), control_cpu);
            let path = format!("/proc/self/task/{wt}/schedstat");
            while !stop.load(Ordering::Relaxed) {
                if let Ok(s) = std::fs::read_to_string(&path) {
                    let v: Vec<u64> = s
                        .split_whitespace()
                        .filter_map(|x| x.parse().ok())
                        .collect();
                    if v.len() >= 2 {
                        delays
                            .lock()
                            .unwrap()
                            .push((t0.elapsed().as_micros() as u64, v[0], v[1]));
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };
    // The load: busy-loop processes on the worker's CPU.
    let loader = {
        let stop = Arc::clone(&stop);
        let bursts = bursts.clone();
        std::thread::spawn(move || {
            for (a, b) in bursts {
                let at = t0 + Duration::from_secs_f64(pre + a);
                while Instant::now() < at && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let mut kids: Vec<Child> = (0..hogs)
                    .map(|_| {
                        Command::new("chrt")
                            .args([
                                "-o",
                                "0",
                                "taskset",
                                "-c",
                                &worker_cpu.to_string(),
                                "sh",
                                "-c",
                                "while :; do :; done",
                            ])
                            .spawn()
                            .unwrap()
                    })
                    .collect();
                let until = t0 + Duration::from_secs_f64(pre + b);
                while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                for k in kids.iter_mut() {
                    let _ = k.kill();
                    let _ = k.wait();
                }
            }
        })
    };

    // Only now (helper threads and the busy loops must stay timeshare).
    pin(gettid(), driver_cpu);
    if rt {
        // Production's data loop is realtime above the worker.
        let p = libc::sched_param { sched_priority: 20 };
        // SAFETY: the calling thread, valid parameter.
        unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO, &p) };
    }
    // The RT side, exactly the plugin's run() at a 480-sample quantum.
    let sig = signal(48_000 * 10);
    let mut aligner = Aligner::new();
    assert!(w.request_reset_and_wait(Duration::from_secs(2)));
    w.drain_output();
    let cycles = (total * 100.0) as usize;
    let mut out = Vec::<f32>::with_capacity(cycles * HOP);
    let mut late_max = Duration::ZERO;
    let start = Instant::now();
    let start_us = start.duration_since(t0).as_micros() as u64;
    for c in 0..cycles {
        let due = Duration::from_millis(c as u64 * 10);
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
        late_max = late_max.max(start.elapsed().saturating_sub(due));
        let off = (c * HOP) % sig.len();
        let input = &sig[off..off + HOP];
        if w.push_input(input) != HOP {
            println!("OVERFLOW at cycle {c}");
            break;
        }
        w.wake();
        let plan = aligner.plan(HOP, w.output_available());
        for _ in 0..plan.discard {
            w.pop_output();
        }
        out.extend(std::iter::repeat_n(0.0, plan.lead_zeros));
        for _ in 0..plan.real {
            out.push(w.pop_output().unwrap_or(0.0));
        }
        out.extend(std::iter::repeat_n(0.0, plan.tail_zeros));
    }
    stop.store(true, Ordering::Relaxed);
    loader.join().unwrap();
    control.join().unwrap();
    sampler.join().unwrap();
    drop(w);

    // Zero runs, as analyze-gaps.py finds them (after a 2 s settle).
    let skip = 2 * 48_000;
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut run = 0usize;
    for (i, &s) in out.iter().enumerate().skip(skip) {
        if s == 0.0 {
            run += 1;
        } else {
            if run > 0 {
                runs.push((i - run, run));
            }
            run = 0;
        }
    }
    if run > 0 {
        runs.push((out.len() - run, run));
    }
    let ms = |n: usize| n as f64 / 48.0;
    let log = log.lock().unwrap();
    let stalls = stalls.lock().unwrap();
    // A sample emitted at output index i left in callback i / HOP.
    let t_of = |i: usize| start_us + (i / HOP) as u64 * 10_000;
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    let mut gaps50 = 0;
    let mut gaps100 = 0;
    let mut excused = 0;
    let mut longest = 0usize;
    for &(s, n) in &runs {
        longest = longest.max(n);
        if ms(n) < 50.0 {
            continue;
        }
        let (g0, g1) = (t_of(s), t_of(s + n));
        let stalled = stalls
            .iter()
            .any(|&(a, d)| g0 <= a + d + 50_000 && a <= g1 + 50_000);
        if stalled {
            excused += 1;
        }
        // Walk back from the gap to the last hop at lag 0: the hop after it
        // is where the backlog began.
        let first = log.partition_point(|r| r.t < g0.saturating_sub(10_000));
        let mut k = first.min(log.len().saturating_sub(1));
        while k > 0 && log[k].lag > 0 {
            k -= 1;
        }
        let origin = log.get(k + 1).unwrap_or(&log[k]);
        let class = class_of(&origin.step);
        let ev: Vec<String> = log[k..]
            .iter()
            .take_while(|r| r.t <= g1 + 20_000)
            .filter_map(|r| r.event.map(|e| format!("{:.2}s {e:?}", r.t as f64 / 1e6)))
            .collect();
        if ms(n) >= 100.0 {
            gaps100 += 1;
            *classes.entry(class.clone()).or_default() += 1;
        }
        gaps50 += 1;
        println!(
            "GAP at {:.3}s {:.0} ms, origin {} at {:.3}s{}; events: {}",
            g0 as f64 / 1e6,
            ms(n),
            class,
            origin.t as f64 / 1e6,
            if stalled { " (control stall)" } else { "" },
            ev.join(" | ")
        );
        if trace {
            for r in log[k..].iter().take_while(|r| r.t <= g1 + 30_000) {
                println!(
                    "  {:8.3} {:<26} cost {:5.2} shadow {:>5} lag {}{}",
                    r.t as f64 / 1e3,
                    step_word(&r.step),
                    r.live,
                    r.shadow.map(|s| format!("{s:.2}")).unwrap_or_default(),
                    r.lag,
                    r.event.map(|e| format!("  <- {e:?}")).unwrap_or_default()
                );
            }
        }
    }
    if let Some(path) = csv {
        use std::fmt::Write as _;
        let mut f = String::from("t_ms,step,live,shadow,lag,event\n");
        for r in log.iter() {
            let _ = writeln!(
                f,
                "{:.3},{},{:.3},{},{},{}",
                r.t as f64 / 1e3,
                step_word(&r.step),
                r.live,
                r.shadow.map(|s| format!("{s:.3}")).unwrap_or_default(),
                r.lag,
                r.event
                    .map(|e| format!("{e:?}").replace(',', ";"))
                    .unwrap_or_default()
            );
        }
        std::fs::write(path, f).unwrap();
    }
    {
        // Run delay per second of wall time, per window of the schedule.
        let d = delays.lock().unwrap();
        let loaded = |t: f64| {
            bursts
                .iter()
                .any(|&(a, b)| t >= pre + a + 0.5 && t < pre + b)
        };
        let (mut on, mut off) = ((0u64, 0u64, 0u64), (0u64, 0u64, 0u64));
        for w in d.windows(2) {
            let (t, dt) = (w[0].0 as f64 / 1e6, w[1].0 - w[0].0);
            let acc = if loaded(t) {
                &mut on
            } else if t > 3.0 {
                &mut off
            } else {
                continue;
            };
            acc.0 += w[1].2 - w[0].2;
            acc.1 += w[1].1 - w[0].1;
            acc.2 += dt;
        }
        let pct = |a: (u64, u64, u64)| {
            if a.2 == 0 {
                (0.0, 0.0)
            } else {
                (
                    a.0 as f64 / 10.0 / a.2 as f64,
                    a.1 as f64 / 10.0 / a.2 as f64,
                )
            }
        };
        let (a, b) = (pct(on), pct(off));
        println!(
            "SCHED loaded: wait {:.1}% run {:.1}% of wall; unloaded: wait {:.1}% run {:.1}%",
            a.0, a.1, b.0, b.1
        );
    }
    for (t, l) in lines.lock().unwrap().iter() {
        println!("LINE {:.3}s {l}", *t as f64 / 1e6);
    }
    let zero_ms: f64 = runs.iter().map(|&(_, n)| ms(n)).sum();
    let cls: Vec<String> = classes.iter().map(|(k, v)| format!("{k}:{v}")).collect();
    println!(
        "SUMMARY engine={engine} scenario={scenario} rt={} gaps50={gaps50} gaps100={gaps100} \
         excused={excused} longest_ms={:.0} zero_ms={zero_ms:.0} stalls={} late_max_ms={:.1} classes={}",
        rt as u8,
        ms(longest),
        stalls.len(),
        late_max.as_secs_f64() * 1e3,
        if cls.is_empty() { "-".into() } else { cls.join(",") }
    );
}
