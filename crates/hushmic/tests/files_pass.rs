//! The cleaning pass against an offline run of the same engine: one front
//! is bit-exact; a seek front, a replaced front and the gap filling join
//! with warmed-up sound; mono stored as stereo is cleaned once, and a
//! wrong guess about it starts over with both channels. Every way leaves
//! the file's picture whole. Self-skips
//! without the repo's models (scripts/setup-assets.sh), unless
//! HUSHMIC_ASSERT_ASSETS=1.

use hushmic::files::cache::Cache;
use hushmic::files::decode::Decoder;
use hushmic::files::engine::{init_runtime, EngineSpec};
use hushmic::files::pass::{Event, Pass, JOIN, RATE, XFADE};
use hushmic::files::picture::{self, Sheet};
use hushmic::files::resample::Converter;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The Light model (fast in debug builds), or None to skip.
fn spec() -> Option<EngineSpec> {
    spec_of("dpdfnet2_48khz_hr")
}

fn spec_of(model: &str) -> Option<EngineSpec> {
    let models = root().join("assets/models");
    let rt = root().join("assets/lib/libonnxruntime.so");
    let ok = models.join("dpdfnet2_48khz_hr.onnx").exists() && rt.exists();
    if !ok {
        assert!(
            std::env::var("HUSHMIC_ASSERT_ASSETS").as_deref() != Ok("1"),
            "assets missing"
        );
        eprintln!("SKIP: models not provisioned");
        return None;
    }
    init_runtime(&rt).unwrap();
    Some(EngineSpec::new(
        &models,
        model,
        hushmic_denoiser::Inference::Auto,
    ))
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hushmic-pass-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// The engine over the whole file in one go: what the pass must equal.
fn offline(path: &Path, spec: &EngineSpec, channels: usize) -> Vec<Vec<f32>> {
    let mut d = Decoder::open(path, None).unwrap();
    let rate = d.info().rate;
    let mut c = Converter::new(rate, RATE, channels).unwrap();
    let mut x = vec![Vec::new(); channels];
    while let Some(b) = d.next_block().unwrap() {
        let b = if channels == 1 { vec![b[0].clone()] } else { b };
        for (o, v) in x.iter_mut().zip(c.push(&b)) {
            o.extend_from_slice(&v);
        }
    }
    for (o, v) in x.iter_mut().zip(c.finish()) {
        o.extend_from_slice(&v);
    }
    x.iter()
        .map(|ch| {
            let mut e = spec.build().unwrap();
            let lat = e.denoiser().latency_samples();
            let mut y = e.process(ch).to_vec();
            let z = vec![0f32; e.pending()];
            y.extend_from_slice(e.process(&z));
            y.drain(..lat);
            y.truncate(ch.len());
            y
        })
        .collect()
}

/// How long the pass may go without progress (more cleaned, its length,
/// or its end) and without its threads using any CPU before a test calls
/// it stuck. Never a limit on the whole pass: on a loaded runner (a 6% CPU
/// quota shared by four tests) a pass is slow, not stuck, and its model
/// can take minutes to load; only a pass that stopped moving waits this
/// long.
const STALL: Duration = Duration::from_secs(240);

/// The frames an event says are cleaned (None: not a Lit).
fn lit(e: &Event) -> Option<u64> {
    match e {
        Event::Lit(r) => Some(r.iter().map(|(a, b)| b - a).sum()),
        _ => None,
    }
}

/// The CPU the passes' threads have used so far (`hushmic-file-*`, each
/// by its tid; user + system ticks). It changes while any pass works.
fn pass_cpu() -> Vec<(u32, u64)> {
    let mut v = Vec::new();
    for e in std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .flatten()
    {
        let comm = std::fs::read_to_string(e.path().join("comm")).unwrap_or_default();
        if !comm.starts_with("hushmic-file") {
            continue;
        }
        let stat = std::fs::read_to_string(e.path().join("stat")).unwrap_or_default();
        // after the ")" that ends the name: state is field 3, utime 14
        let f: Vec<&str> = stat
            .rsplit(')')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        let ticks = |i: usize| {
            f.get(i - 3)
                .and_then(|t| t.parse::<u64>().ok())
                .unwrap_or(0)
        };
        let tid = e.file_name().to_string_lossy().parse().unwrap_or(0);
        v.push((tid, ticks(14) + ticks(15)));
    }
    v.sort_unstable();
    v
}

/// Waits for the pass's next event: a stuck pass is one with no event
/// and no CPU used by its threads for `STALL`.
struct Watch {
    moved: Instant,
    cpu: Vec<(u32, u64)>,
}

impl Watch {
    fn new() -> Watch {
        Watch {
            moved: Instant::now(),
            cpu: pass_cpu(),
        }
    }

    fn moved(&mut self) {
        self.moved = Instant::now();
    }

    /// The next event; None once the pass is stuck.
    fn next(&mut self, rx: &Receiver<Event>) -> Option<Event> {
        loop {
            if let Ok(e) = rx.recv_timeout(Duration::from_secs(1)) {
                return Some(e);
            }
            let cpu = pass_cpu();
            if cpu != self.cpu {
                (self.cpu, self.moved) = (cpu, Instant::now());
            }
            if self.moved.elapsed() >= STALL {
                return None;
            }
        }
    }
}

/// The next event, or a panic once the pass is stuck.
fn next(rx: &Receiver<Event>) -> Event {
    Watch::new()
        .next(rx)
        .unwrap_or_else(|| panic!("the pass made no progress for {STALL:?}"))
}

/// Waits for the cleaned parts to be `n` ranges at once.
fn until_ranges(rx: &Receiver<Event>, n: usize) {
    loop {
        match next(rx) {
            Event::Lit(r) if r.len() == n => return,
            Event::Done | Event::Failed(_) => panic!("the pass ended before {n} ranges"),
            _ => {}
        }
    }
}

/// Events until Done (or a failure). Fails when the pass stops moving: for
/// `STALL`, no event but repeats of what was already cleaned, and no CPU.
fn wait_done(rx: &Receiver<Event>) -> Vec<Event> {
    let mut seen = Vec::new();
    let mut most = 0;
    let mut watch = Watch::new();
    while let Some(e) = watch.next(rx) {
        match lit(&e) {
            Some(n) if n <= most => {}
            Some(n) => {
                most = n;
                watch.moved();
            }
            None => watch.moved(),
        }
        let stop = matches!(e, Event::Done | Event::Failed(_));
        seen.push(e);
        if stop {
            break;
        }
    }
    assert!(
        matches!(seen.last(), Some(Event::Done)),
        "no Done (no progress for {STALL:?}): {:?}",
        seen.iter().rev().take(3).collect::<Vec<_>>()
    );
    seen
}

fn snr_db(a: &[f32], b: &[f32]) -> f64 {
    let (mut s, mut n) = (0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        s += (*y as f64).powi(2);
        n += (*x as f64 - *y as f64).powi(2);
    }
    10.0 * (s / n.max(1e-30)).log10()
}

/// A longer mono file for the multi-front cases: the three 48 kHz fixtures
/// back to back (about 36 s), stamped with `rate`.
fn long_file(dir: &Path, rate: u32) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(format!("long{rate}.wav"));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&p, spec).unwrap();
    for f in [
        "noisy_public_48k.flac",
        "noisy_cafe_48k.flac",
        "noisy_keyboard_48k.flac",
    ] {
        let mut d = Decoder::open(&root().join("tests/fixtures").join(f), None).unwrap();
        while let Some(b) = d.next_block().unwrap() {
            for v in &b[0] {
                w.write_sample((v * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
                    .unwrap();
            }
        }
    }
    w.finalize().unwrap();
    p
}

/// The largest step between neighbouring samples: a hand-over that clicks
/// steps far beyond the straight run's largest.
fn step(v: &[f32]) -> f32 {
    v.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0f32, f32::max)
}

/// The picture a finished pass left in `cache`: every column of the
/// file's `total` frames has the original and the cleaned sound.
fn whole_picture(cache: &Cache, total: u64) -> Sheet {
    let sheet = Sheet::read(cache.dir()).unwrap().expect("a picture");
    let span = picture::span_for(Some(total));
    assert!((sheet.column_secs() * RATE as f64 - span as f64).abs() < 1e-6);
    assert_eq!(sheet.cols() as u64, total.div_ceil(span));
    assert_eq!(sheet.lit(), sheet.cols(), "every column is cleaned");
    let mut levels = Vec::new();
    sheet.levels(0.0, &mut levels);
    let silent = levels
        .chunks(levels.len() / sheet.cols())
        .filter(|c| c[..c.len() / 2].iter().all(|l| *l == 0))
        .count();
    assert_eq!(silent, 0, "every column has the original");
    sheet
}

fn whole(cache: &Cache, total: u64) -> Vec<Vec<f32>> {
    cache.read(0, total as usize).unwrap()
}

#[test]
fn one_front_equals_the_engine_over_the_whole_file() {
    let Some(spec) = spec() else { return };
    let src = root().join("tests/fixtures/noisy_public_48k.flac");
    let dir = scratch("one");
    let (tx, rx) = channel();
    let pass = Pass::start(&src, None, spec.clone(), &dir, tx).unwrap();
    wait_done(&rx);
    let total = pass.total().unwrap();
    let want = offline(&src, &spec, 1);
    assert_eq!(total as usize, want[0].len());
    let got = whole(&pass.cache(), total);
    let worst = got[0]
        .iter()
        .zip(&want[0])
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    assert_eq!(
        worst, 0.0,
        "the pass differs from the offline run by {worst}"
    );
    assert_eq!(pass.segments().ranges(), &[(0, total)]);
    // The file's picture: the cleaned lane is the quieter one, and at
    // full strength the mix is the original again.
    let sheet = whole_picture(&pass.cache(), total);
    let (mut cleaned, mut original) = (Vec::new(), Vec::new());
    sheet.levels(0.0, &mut cleaned);
    sheet.levels(1.0, &mut original);
    let half = cleaned.len() / sheet.cols() / 2;
    let (mut less, mut more) = (0usize, 0usize);
    for (c, o) in cleaned.chunks(2 * half).zip(original.chunks(2 * half)) {
        assert_eq!(o[..half], o[half..], "all of the original left in");
        for (clean, raw) in c[half..].iter().zip(&c[..half]) {
            less += (clean < raw) as usize;
            more += (*clean as u16 > *raw as u16 + 8) as usize;
        }
    }
    assert!(
        less > 20 * more.max(1),
        "{less} bands quieter, {more} louder"
    );
    drop(pass);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_quality_model_and_onnx_run_the_same_way() {
    let Some(quality) = spec_of("dpdfnet8_48khz_hr") else {
        return;
    };
    let mut onnx = spec().unwrap();
    onnx.inference = hushmic_denoiser::Inference::Onnx;
    let src = root().join("tests/fixtures/files/master_48k_mono.wav");
    for (k, spec) in [quality, onnx].into_iter().enumerate() {
        let dir = scratch(&format!("models{k}"));
        let (tx, rx) = channel();
        let pass = Pass::start(&src, None, spec.clone(), &dir, tx).unwrap();
        wait_done(&rx);
        let want = offline(&src, &spec, 1);
        let got = whole(&pass.cache(), pass.total().unwrap());
        assert_eq!(got[0], want[0], "{}", spec.model.display());
        drop(pass);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn a_seek_front_takes_over_warmed_up() {
    let Some(spec) = spec() else { return };
    for rate in [48_000, 44_100] {
        seek_front_at(&spec, rate);
    }
}

fn seek_front_at(spec: &EngineSpec, rate: u32) {
    let dir = scratch(&format!("seek{rate}"));
    let src = long_file(&dir, rate);
    let (tx, rx) = channel();
    // The first front is held within its first second until the seek
    // front has cleaned something: far from the seek when it is asked
    // for (a request near a running front is left to that front), and
    // not there before it, whatever the threads' pace.
    let cache = dir.join("cache");
    let pass = Pass::start_held(&src, None, spec.clone(), &cache, tx, &[48_000]).unwrap();
    // Off the engine's hop grid (and the rates' common grid) on purpose:
    // the front starts on both anyway.
    let s = 20 * 48_000 + 123;
    pass.request(s);
    until_ranges(&rx, 2);
    pass.release();
    let events = wait_done(&rx);
    let total = pass.total().unwrap();
    let want = offline(&src, spec, 1);
    let got = whole(&pass.cache(), total);
    // Up to the hand-over the first front's sound is the straight run.
    let exact_to = (s + JOIN - XFADE) as usize;
    let worst = got[0][..exact_to]
        .iter()
        .zip(&want[0][..exact_to])
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    // (A failure says where, and what the events were.)
    let first = got[0][..exact_to]
        .iter()
        .zip(&want[0][..exact_to])
        .position(|(a, b)| a != b);
    let lit: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Lit(r) if r.len() > 1 => Some(r.clone()),
            _ => None,
        })
        .take(3)
        .collect();
    assert_eq!(
        worst, 0.0,
        "{rate}: before the hand-over: {worst} from frame {first:?}; s {s}; lit {lit:?}"
    );
    // After it, the seek front's sound. The engine's long memory keeps a
    // front that started 2.5 s earlier from ever equalling the straight run
    // (22 to 40 dB from it on these recordings, measured; the float engine
    // the same), but it is cleaned and on time: the raw sound is 2.3 dB from
    // the straight run here, the cleaned sound 1 ms late -2.3 dB.
    let snr = snr_db(&got[0][exact_to..], &want[0][exact_to..]);
    assert!(
        snr > 20.0,
        "{rate}: after the hand-over: {snr:.1} dB from the straight run"
    );
    assert!(
        step(&got[0]) < 1.5 * step(&want[0]) + 0.01,
        "{rate}: a click"
    );
    whole_picture(&pass.cache(), total);
    drop(pass);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_replaced_front_leaves_a_gap_the_pass_fills() {
    let Some(spec) = spec() else { return };
    let dir = scratch("gap");
    let src = long_file(&dir, 48_000);
    let (tx, rx) = channel();
    // Every front is held within a second of where it starts to write,
    // so the three stand where the case needs them whatever the threads'
    // pace: no front is near the next one's start when that is asked for
    // (such a request is left to the running front), and none gets there
    // before the next has cleaned something.
    let (second, third) = (12 * 48_000, 28 * 48_000);
    let holds = [48_000, second + 48_000, third + 48_000];
    let cache = dir.join("cache");
    let pass = Pass::start_held(&src, None, spec.clone(), &cache, tx, &holds).unwrap();
    pass.request(second);
    // Once the 12 s front has cleaned something, replace it (two fronts at
    // most): its range ends early and the pass has to come back for what
    // lies between.
    loop {
        match next(&rx) {
            Event::Lit(r) if r.iter().any(|&(a, _)| a == second) => break,
            _ => {}
        }
    }
    pass.request(third);
    // Three ranges at once: the first front's, what the replaced one
    // left, and the third's.
    until_ranges(&rx, 3);
    pass.release();
    wait_done(&rx);
    let total = pass.total().unwrap();
    assert_eq!(pass.segments().ranges(), &[(0, total)]);
    let want = offline(&src, &spec, 1);
    let got = whole(&pass.cache(), total);
    let snr = snr_db(&got[0], &want[0]);
    assert!(snr > 20.0, "{snr:.1} dB from the straight run");
    assert!(step(&got[0]) < 1.5 * step(&want[0]) + 0.01, "a click");
    whole_picture(&pass.cache(), total);
    drop(pass);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn mono_stored_as_stereo_is_cleaned_once() {
    let Some(spec) = spec() else { return };
    let fixtures = root().join("tests/fixtures/files");
    for (name, stored) in [("flac_44k_same.flac", 1), ("flac_44k_stereo.flac", 2)] {
        let dir = scratch(name);
        let (tx, rx) = channel();
        let pass = Pass::start(&fixtures.join(name), None, spec.clone(), &dir, tx).unwrap();
        wait_done(&rx);
        assert_eq!(
            (pass.channels(), pass.stored_channels()),
            (2, stored),
            "{name}"
        );
        // One second at 44.1 kHz is exactly 48,000 frames at 48 kHz.
        assert_eq!(pass.total(), Some(48_000), "{name}");
        let want = offline(&fixtures.join(name), &spec, stored);
        let got = whole(&pass.cache(), 48_000);
        for c in 0..stored {
            let worst = got[c]
                .iter()
                .zip(&want[c])
                .map(|(a, b)| (a - b).abs())
                .fold(0f32, f32::max);
            assert_eq!(worst, 0.0, "{name} channel {c}");
        }
        drop(pass);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn channels_that_differ_between_the_windows_restart_the_pass() {
    let Some(spec) = spec() else { return };
    let dir = scratch("restart");
    std::fs::create_dir_all(&dir).unwrap();
    // 15 s of stereo, the same in both channels but 1.25 to 1.75 s, which
    // lies between the eight 1 s windows of the quick check.
    let src = dir.join("almost.wav");
    let ws = hound::WavSpec {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&src, ws).unwrap();
    let mut d = Decoder::open(&root().join("tests/fixtures/noisy_public_48k.flac"), None).unwrap();
    let mut i = 0usize;
    while let Some(b) = d.next_block().unwrap() {
        for v in &b[0] {
            let q = |v: f32| (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            let r = if (60_000..84_000).contains(&i) {
                v * 0.5
            } else {
                *v
            };
            w.write_sample(q(*v)).unwrap();
            w.write_sample(q(r)).unwrap();
            i += 1;
        }
    }
    w.finalize().unwrap();
    let (tx, rx) = channel();
    let pass = Pass::start(&src, None, spec.clone(), &dir.join("cache"), tx).unwrap();
    let events = wait_done(&rx);
    assert_eq!(pass.stored_channels(), 2);
    // Only a restart clears the cleaned parts.
    assert!(
        events.contains(&Event::Lit(Vec::new())),
        "the windows found the difference"
    );
    let total = pass.total().unwrap();
    let want = offline(&src, &spec, 2);
    let got = whole(&pass.cache(), total);
    for c in 0..2 {
        let worst = got[c]
            .iter()
            .zip(&want[c])
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert_eq!(worst, 0.0, "channel {c}");
    }
    // The picture's original lane survives the new cache.
    whole_picture(&pass.cache(), total);
    drop(pass);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn stop_ends_every_thread_and_the_events() {
    let Some(spec) = spec() else { return };
    let dir = scratch("stop");
    let src = long_file(&dir, 48_000);
    let (tx, rx) = channel();
    let pass = Pass::start(&src, None, spec, &dir.join("cache"), tx).unwrap();
    pass.request(20 * 48_000);
    let first = next(&rx);
    assert!(matches!(first, Event::Lit(_) | Event::Length(_)));
    pass.stop();
    while rx.try_recv().is_ok() {}
    std::thread::sleep(Duration::from_millis(300));
    assert!(rx.try_recv().is_err(), "events after stop");
    drop(pass);
    let _ = std::fs::remove_dir_all(dir);
}
