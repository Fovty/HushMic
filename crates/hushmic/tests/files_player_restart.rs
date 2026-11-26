//! Playing a file where `pw-cat` cannot play from a pipe: the restart
//! player takes over from the playhead and plays WAVs of the mix through
//! `pw-play`, starting over on a seek or a hold, and waiting at the end of
//! the cleaned part. The scripts here stand in for both: `pw-cat` ends at
//! once, `pw-play` keeps a copy of each WAV it is given and "plays" for as
//! long as its mode file says, or until the test lets it go. The tests take
//! turns (they share the scripts and the process-wide restart switch).

use hushmic::appstate::{FileJob, JobState};
use hushmic::files::cache::Cache;
use hushmic::files::decode::Decoder;
use hushmic::files::player::{self, FilePlayer, FileSource};
use hushmic::files::resample::Converter;
use hushmic::miccheck::player::PlayerState;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

/// How long a wait for what the player does may take: only a hang comes
/// near it (a loaded runner renders slowly, never this slowly).
const WAIT: Duration = Duration::from_secs(120);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/files")
        .join(name)
}

/// The scripts' directory (first on PATH) and the runtime dir, set once
/// before any thread of this binary spawns a process.
fn setup() -> (PathBuf, PathBuf) {
    static ONCE: Once = Once::new();
    let name = format!("hushmic-fpr-{}", std::process::id());
    let bin = std::env::temp_dir().join(&name);
    let run = hushmic_shm::test_dir_base().join(&name);
    ONCE.call_once(|| {
        use std::os::unix::fs::PermissionsExt;
        for d in [&bin, &run] {
            let _ = std::fs::remove_dir_all(d);
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &run);
        let script = |name: &str, body: &str| {
            let p = bin.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        // the playback's latency watch never reaches the box's PipeWire:
        // its tools fail, as without a graph
        script("pw-dump", "exit 1");
        script("pw-top", "exit 1");
        // pw-cat: --help lists nothing; any playback ends at once, counted.
        script(
            "pw-cat",
            "d=\"$(dirname \"$0\")\"\n\
             [ \"$1\" = --help ] && exit 0\n\
             echo x >> \"$d/cat-calls\"\n\
             exit 1",
        );
        // pw-play: keeps the WAV as played-NNN.wav; mode "fail" fails at
        // once; a play the test holds (hold-NNN) plays until the test lets
        // it go; else it plays for as many seconds as the mode says.
        script(
            "pw-play",
            "d=\"$(dirname \"$0\")\"\n\
             for a; do f=\"$a\"; done\n\
             n=$(ls \"$d\" | grep -c '^played-')\n\
             cp \"$f\" \"$d/.copy-$$\"\n\
             mv \"$d/.copy-$$\" \"$d/played-$(printf %03d \"$n\").wav\"\n\
             m=$(cat \"$d/play-mode\")\n\
             [ \"$m\" = fail ] && exit 1\n\
             h=\"$d/hold-$(printf %03d \"$n\")\"\n\
             if [ -e \"$h\" ]; then\n\
               while [ -e \"$h\" ]; do sleep 0.05; done\n\
               exit 0\n\
             fi\n\
             exec sleep \"$m\"",
        );
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![bin.clone()];
        paths.extend(std::env::split_paths(&path));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    });
    (bin, run)
}

/// One test at a time: pw-play's mode, the restart switch, no WAVs yet.
fn serial(play_mode: &str, restart_only: bool) -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (bin, _) = setup();
    for e in std::fs::read_dir(&bin).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("played-") || n.starts_with("hold-") || n == "cat-calls" {
            std::fs::remove_file(e.path()).unwrap();
        }
    }
    // A test that failed may have left its player's WAVs.
    for f in play_files() {
        let _ = std::fs::remove_file(f);
    }
    std::fs::write(bin.join("play-mode"), play_mode).unwrap();
    player::set_restart_only(restart_only);
    g
}

/// The WAVs pw-play was given so far, in order: (channels, samples).
fn played() -> Vec<(u16, Vec<f32>)> {
    let (bin, _) = setup();
    let mut names: Vec<PathBuf> = std::fs::read_dir(&bin)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("played-")
        })
        .collect();
    names.sort();
    names
        .iter()
        .map(|p| {
            let mut r = hound::WavReader::open(p).unwrap();
            let ch = r.spec().channels;
            (ch, r.samples::<f32>().map(Result::unwrap).collect())
        })
        .collect()
}

fn cat_calls() -> usize {
    std::fs::read_to_string(setup().0.join("cat-calls"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// The restart players' WAVs left in the runtime dir.
fn play_files() -> Vec<PathBuf> {
    let dir = setup().1.join("hushmic");
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("files-play-")
                })
                .collect()
        })
        .unwrap_or_default()
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < WAIT, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The original as the player hears it: decoded and at 48 kHz.
fn original48(path: &Path) -> Vec<Vec<f32>> {
    let mut d = Decoder::open(path, None).unwrap();
    let ch = d.info().channels;
    let mut c = Converter::new(d.info().rate, 48_000, ch).unwrap();
    let mut x = vec![Vec::new(); ch];
    while let Some(b) = d.next_block().unwrap() {
        for (o, v) in x.iter_mut().zip(c.push(&b)) {
            o.extend_from_slice(&v);
        }
    }
    for (o, v) in x.iter_mut().zip(c.finish()) {
        o.extend_from_slice(&v);
    }
    x
}

/// A mono 48 kHz WAV of `secs` in the temp dir: an original longer than
/// the fixtures. A rising tone (220 Hz, 40 Hz more each second), so no
/// stretch of it equals another: a stretch played twice shows.
fn long_original(tag: &str, secs: usize) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hushmic-fpr-{}-{tag}.wav", std::process::id()));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(&p, spec).unwrap();
    for i in 0..secs * 48_000 {
        let t = i as f64 / 48_000.0;
        let phase = std::f64::consts::TAU * (220.0 * t + 20.0 * t * t);
        w.write_sample(0.3 * phase.sin() as f32).unwrap();
    }
    w.finalize().unwrap();
    p
}

/// A job for `file` whose cache (in a fresh `tag` folder) holds `clean`,
/// cleaned in `lit`.
fn job(file: &str, tag: &str, clean: &[Vec<f32>], lit: Vec<(u64, u64)>, done: bool) -> FileJob {
    let dir = std::env::temp_dir().join(format!("hushmic-fpr-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cache = Cache::create(&dir, clean.len()).unwrap();
    cache.write(0, clean).unwrap();
    FileJob {
        id: 1,
        path: if Path::new(file).is_absolute() {
            file.to_string()
        } else {
            fixture(file).display().to_string()
        },
        state: if done {
            JobState::Done
        } else {
            JobState::Cleaning
        },
        cache: dir.display().to_string(),
        stored: clean.len(),
        channels: clean.len(),
        frames: Some(clean[0].len() as u64),
        exact: true,
        lit,
        ..FileJob::default()
    }
}

fn drain(s: &mut FileSource) -> Vec<f32> {
    let mut out = Vec::new();
    loop {
        let b = s.render(480).unwrap();
        if b.is_empty() {
            return out;
        }
        out.extend(b);
    }
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0f32, f32::max)
}

/// Hold play `n` (from 0) of this test: it plays until [`let_go`].
fn hold_play(n: usize) {
    std::fs::write(setup().0.join(format!("hold-{n:03}")), "").unwrap();
}

fn let_go(n: usize) {
    std::fs::remove_file(setup().0.join(format!("hold-{n:03}"))).unwrap();
}

/// Until the player ends: it fails only when nothing moves (the playhead,
/// the stretches played) for [`WAIT`], so a long file on a slow runner is
/// no failure.
fn wait_end(p: &FilePlayer) -> PlayerState {
    let mut last = (p.position(), played().len());
    let mut moved = Instant::now();
    while p.poll() == PlayerState::Playing && moved.elapsed() < WAIT {
        std::thread::sleep(Duration::from_millis(10));
        let now = (p.position(), played().len());
        if now != last {
            (last, moved) = (now, Instant::now());
        }
    }
    p.poll()
}

#[test]
fn a_pw_cat_that_cannot_play_a_pipe_plays_the_mix_through_pw_play() {
    let _s = serial("0.2", false);
    let x = original48(&fixture("flac_44k_stereo.flac"));
    let clean: Vec<Vec<f32>> = x
        .iter()
        .map(|c| c.iter().map(|v| v * 0.5).collect())
        .collect();
    let j = job(
        "flac_44k_stereo.flac",
        "mix",
        &clean,
        vec![(0, 48_000)],
        true,
    );
    let want = drain(&mut FileSource::open(&j, None, 0, 0.25).unwrap());
    let p = FilePlayer::start(FileSource::open(&j, None, 0, 0.25).unwrap());
    assert_eq!(wait_end(&p), PlayerState::Ended);
    assert_eq!(p.position(), 48_000);
    assert_eq!(cat_calls(), 1, "pw-cat was tried first");
    let wavs = played();
    assert_eq!(wavs.len(), 1, "one stretch: the whole file");
    assert_eq!(wavs[0].0, 2);
    assert_eq!(wavs[0].1.len(), want.len());
    // The same mix, but for the decoder starting over at 0.
    assert!(
        worst(&wavs[0].1, &want) < 1e-4,
        "{}",
        worst(&wavs[0].1, &want)
    );
    assert!(
        player::restart_only(),
        "it played: the next file starts there"
    );
    wait_until("the WAV to go", || play_files().is_empty());
}

#[test]
fn the_restart_player_sticks_and_starts_over_on_a_seek() {
    let _s = serial("30", true);
    let x = original48(&fixture("master_48k_mono.wav"));
    let j = job("master_48k_mono.wav", "seek", &x, vec![(0, 48_000)], true);
    let want = drain(&mut FileSource::open(&j, None, 0, 0.0).unwrap());
    let mut p = FilePlayer::start(FileSource::open(&j, None, 0, 0.0).unwrap());
    wait_until("the first stretch", || played().len() == 1);
    assert_eq!(cat_calls(), 0, "no pw-cat any more");
    assert_eq!(play_files().len(), 1);
    p.seek(24_000);
    wait_until("the stretch after the seek", || played().len() == 2);
    let wavs = played();
    assert_eq!(wavs[0].1.len(), 48_000);
    let after = &wavs[1].1;
    assert_eq!(after.len(), 24_000, "from the seek's target to the end");
    assert!(
        after[0].abs() <= x[0][24_000].abs() / 200.0 + 1e-6,
        "fades in"
    );
    assert!(worst(&after[240..23_760], &want[24_240..47_760]) < 1e-4);
    assert_eq!(p.poll(), PlayerState::Playing);
    let pos = p.position();
    assert!((24_000..=48_000).contains(&pos), "{pos}");
    p.stop();
    assert!(play_files().is_empty(), "the WAV goes with the player");
}

#[test]
fn a_hold_starts_over_with_the_original_past_the_cleaned_part() {
    let _s = serial("30", true);
    let x = original48(&fixture("master_48k_mono.wav"));
    let zero = vec![vec![0f32; 48_000]];
    let j = job(
        "master_48k_mono.wav",
        "hold",
        &zero,
        vec![(0, 12_000)],
        false,
    );
    let mut p = FilePlayer::start(FileSource::open(&j, None, 0, 0.0).unwrap());
    wait_until("the first stretch", || played().len() == 1);
    // The cleaned part only, and it is silent (the cache holds zeros).
    assert_eq!(played()[0].1.len(), 12_000);
    assert!(played()[0].1.iter().all(|v| *v == 0.0));
    std::thread::sleep(Duration::from_millis(300));
    p.hold(true);
    wait_until("the stretch of the original", || played().len() == 2);
    let held = played()[1].1.clone();
    // From the playhead (somewhere in the first stretch) to the end of the
    // file: the original alone, past the cleaned part.
    let from = 48_000 - held.len();
    assert!(from > 0 && from <= 12_000, "{from}");
    assert!(worst(&held[240..held.len() - 240], &x[0][from + 240..47_760]) < 1e-4);
    p.stop();
}

#[test]
fn caught_up_it_waits_and_goes_on_once_cleaned() {
    let _s = serial("0.05", true);
    let x = original48(&fixture("master_48k_mono.wav"));
    let mut j = job("master_48k_mono.wav", "wait", &x, vec![(0, 24_000)], false);
    let whole = job(
        "master_48k_mono.wav",
        "wait-all",
        &x,
        vec![(0, 48_000)],
        true,
    );
    let want = drain(&mut FileSource::open(&whole, None, 0, 0.0).unwrap());
    let p = FilePlayer::start(FileSource::open(&j, None, 0, 0.0).unwrap());
    wait_until("the wait at the end of the cleaned part", || p.waiting());
    assert_eq!(p.position(), 24_000);
    assert_eq!(played().len(), 1);
    assert_eq!(played()[0].1.len(), 24_000);
    j.lit = vec![(0, 48_000)];
    j.state = JobState::Done;
    p.set_lit(&j);
    assert_eq!(wait_end(&p), PlayerState::Ended);
    assert_eq!(p.position(), 48_000);
    let wavs = played();
    assert_eq!(wavs.len(), 2);
    assert_eq!(wavs[1].1.len(), 24_000);
    assert!(worst(&wavs[1].1[240..23_760], &want[24_240..47_760]) < 1e-4);
}

#[test]
fn a_pw_play_that_fails_too_is_a_failure_that_does_not_stick() {
    let _s = serial("fail", false);
    let x = original48(&fixture("master_48k_mono.wav"));
    let j = job("master_48k_mono.wav", "fail", &x, vec![(0, 48_000)], true);
    let p = FilePlayer::start(FileSource::open(&j, None, 0, 0.0).unwrap());
    assert!(matches!(wait_end(&p), PlayerState::Failed { .. }));
    assert!(
        !player::restart_only(),
        "nothing played: pw-cat is tried again"
    );
    wait_until("the WAV to go", || play_files().is_empty());
}

#[test]
fn the_same_hold_or_strength_again_does_not_start_over() {
    let _s = serial("30", true);
    let orig = long_original("same", 10);
    let x = original48(&orig);
    let j = job(
        &orig.display().to_string(),
        "same",
        &x,
        vec![(0, 480_000)],
        true,
    );
    let mut p = FilePlayer::start(FileSource::open(&j, None, 0, 0.0).unwrap());
    wait_until("the first stretch", || played().len() == 1);
    // What the page sends as a player arrives: what is in force already.
    p.hold(false);
    p.set_strength(0.0);
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(played().len(), 1, "nothing changed: it plays on");
    // A strength under a hold is not heard either; letting go is.
    p.hold(true);
    wait_until("the hold", || played().len() == 2);
    std::thread::sleep(Duration::from_millis(250));
    p.set_strength(0.5);
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(played().len(), 2, "a strength under the hold");
    p.hold(false);
    wait_until("letting go", || played().len() == 3);
    p.stop();
    let _ = std::fs::remove_file(orig);
}

#[test]
fn a_seek_shows_its_place_at_once() {
    let _s = serial("30", true);
    let x = original48(&fixture("master_48k_mono.wav"));
    let j = job("master_48k_mono.wav", "place", &x, vec![(0, 48_000)], true);
    let mut p = FilePlayer::start(FileSource::open(&j, None, 0, 0.0).unwrap());
    wait_until("the first stretch", || played().len() == 1);
    // Within MIN_RESTART of the stretch's start the start-over waits, the
    // playhead does not: once the player has looked, it is at the target,
    // on from there by no more than the time since the seek (the first
    // stretch's own playhead, counted from before, cannot pass for it
    // past the target for long: it plays at the same pace from earlier).
    let before = p.position();
    p.seek(30_000);
    let t = Instant::now();
    let shows = || {
        let pos = p.position();
        let most = 30_000 + (t.elapsed().as_secs_f64() * 48_000.0) as u64;
        (pos >= 30_000 && pos <= most, pos)
    };
    loop {
        let (at, pos) = shows();
        if at && (before < 30_000 || pos < before) {
            break;
        }
        assert!(t.elapsed() < WAIT, "the seek never showed ({pos})");
        std::thread::sleep(Duration::from_millis(2));
    }
    p.stop();
}

#[test]
fn a_long_file_starts_short_and_plays_on_in_stretches_that_join() {
    let _s = serial("0.05", true);
    let orig = long_original("long", 30);
    let x = original48(&orig);
    let name = orig.display().to_string();
    let j = job(&name, "long", &x, vec![(0, 1_440_000)], true);
    let want = drain(&mut FileSource::open(&j, None, 0, 0.0).unwrap());
    let p = FilePlayer::start(FileSource::open(&j, None, 0, 0.0).unwrap());
    assert_eq!(wait_end(&p), PlayerState::Ended);
    assert_eq!(p.position(), 1_440_000);
    let wavs = played();
    let lens: Vec<usize> = wavs.iter().map(|w| w.1.len()).collect();
    assert_eq!(lens, [96_000, 960_000, 384_000], "2 s first, then 20 s");
    let all: Vec<f32> = wavs.into_iter().flat_map(|w| w.1).collect();
    assert_eq!(all.len(), want.len());
    // The same mix, but for 5 ms out and in at each cut.
    for (from, to) in [(240, 95_760), (96_240, 1_055_760), (1_056_240, 1_439_760)] {
        assert!(
            worst(&all[from..to], &want[from..to]) < 1e-4,
            "{from}..{to}"
        );
    }
    let _ = std::fs::remove_file(orig);
}

#[test]
fn the_next_stretch_is_ready_before_this_one_ends() {
    let _s = serial("30", true);
    let orig = long_original("ready", 30);
    let x = original48(&orig);
    let name = orig.display().to_string();
    let j = job(&name, "ready", &x, vec![(0, 1_440_000)], true);
    // the first stretch plays until the next one is written
    hold_play(0);
    let mut p = FilePlayer::start(FileSource::open(&j, None, 0, 0.0).unwrap());
    wait_until("the first stretch", || played().len() == 1);
    wait_until("the next one written while the first plays", || {
        play_files().len() == 2
    });
    assert_eq!(played().len(), 1);
    // After a start-over, a short one again.
    std::thread::sleep(Duration::from_millis(250));
    p.seek(480_000);
    wait_until("the stretch after the seek", || played().len() == 2);
    assert_eq!(played()[1].1.len(), 96_000);
    p.stop();
    assert!(play_files().is_empty());
    let _ = std::fs::remove_file(orig);
}

#[test]
fn the_stretch_written_while_one_played_is_the_one_played_next() {
    // The first stretch plays until the next one is written into the
    // other file; then it ends and the next one plays.
    let _s = serial("30", true);
    let orig = long_original("next", 30);
    let x = original48(&orig);
    let name = orig.display().to_string();
    let j = job(&name, "next", &x, vec![(0, 1_440_000)], true);
    let want = drain(&mut FileSource::open(&j, None, 0, 0.0).unwrap());
    hold_play(0);
    let mut p = FilePlayer::start(FileSource::open(&j, None, 0, 0.0).unwrap());
    wait_until("the first stretch", || played().len() == 1);
    wait_until("the next one written while the first plays", || {
        play_files().len() == 2
    });
    assert_eq!(played().len(), 1);
    let_go(0);
    wait_until("the second stretch", || played().len() == 2);
    p.stop();
    let w = played();
    let lens: Vec<usize> = w.iter().map(|w| w.1.len()).collect();
    assert_eq!(lens, [96_000, 960_000]);
    assert!(worst(&w[0].1[240..95_760], &want[240..95_760]) < 1e-4);
    // 2 s to 22 s, not the first stretch again.
    assert!(
        worst(&w[1].1[240..959_760], &want[96_240..1_055_760]) < 1e-4,
        "the second stretch is not 2-22 s"
    );
    let _ = std::fs::remove_file(orig);
}
