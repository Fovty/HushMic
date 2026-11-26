//! Playing a file: the mix of the cache and the original at the strength,
//! waiting at the end of the cleaned part until the pass is far enough
//! ahead, the hold playing the original past it, seeks landing on the
//! frame, and the feeder putting exactly that through `pw-cat` (a script
//! here that keeps what it is fed).

use hushmic::appstate::{FileJob, JobState};
use hushmic::files::cache::Cache;
use hushmic::files::decode::Decoder;
use hushmic::files::player::{FilePlayer, FileSource};
use hushmic::files::resample::Converter;
use hushmic::miccheck::player::PlayerState;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/files")
        .join(name)
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hushmic-play-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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

/// A job for `file` whose cache holds `clean`, cleaned in `lit`.
fn job(file: &str, dir: &Path, clean: &[Vec<f32>], lit: Vec<(u64, u64)>, done: bool) -> FileJob {
    let cache = Cache::create(dir, clean.len()).unwrap();
    cache.write(0, clean).unwrap();
    let frames = clean[0].len() as u64;
    FileJob {
        id: 1,
        path: fixture(file).display().to_string(),
        state: if done {
            JobState::Done
        } else {
            JobState::Cleaning
        },
        cache: dir.display().to_string(),
        stored: clean.len(),
        channels: clean.len(),
        frames: Some(frames),
        exact: true,
        lit,
        ..FileJob::default()
    }
}

/// Render until the source has nothing more right now.
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

#[test]
fn the_cleaned_part_plays_as_the_mix_at_the_strength() {
    let dir = scratch("mix");
    let x = original48(&fixture("master_48k_mono.wav"));
    let clean: Vec<Vec<f32>> = vec![x[0].iter().map(|v| v * 0.5).collect()];
    let j = job("master_48k_mono.wav", &dir, &clean, vec![(0, 48_000)], true);
    let mut s = FileSource::open(&j, None, 0, 0.25).unwrap();
    let y = drain(&mut s);
    assert!(s.ended());
    assert_eq!(y.len(), 48_000);
    // (1 − 0.25)·0.5x + 0.25·x, past the 5 ms fade in and before the end's.
    let worst = (240..47_760)
        .map(|i| (y[i] - 0.625 * x[0][i]).abs())
        .fold(0f32, f32::max);
    assert!(worst < 1e-6, "{worst}");
    assert!(
        y[0].abs() < 0.01 && y[47_999].abs() < 0.01,
        "fades in and out"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn caught_up_playback_waits_until_the_pass_is_far_enough_ahead() {
    let dir = scratch("wait");
    let x = original48(&fixture("master_48k_mono.wav"));
    let j = job("master_48k_mono.wav", &dir, &x, vec![(0, 24_000)], false);
    let mut s = FileSource::open(&j, None, 0, 0.0).unwrap();
    let y = drain(&mut s);
    // Up to the end of the cleaned part, the last 5 ms fading out.
    assert_eq!((y.len(), s.position()), (24_000, 24_000));
    assert!(s.waiting() && !s.ended());
    assert!(y[23_999].abs() < 0.01 && (y[23_000] - x[0][23_000]).abs() < 1e-6);
    // The pass at a quarter of real time: 22.5 s ahead are needed, 0.25 s
    // are not.
    // (A timeline of its own, past the speed window from the word at open,
    // which a loaded runner could have given any moment before.)
    let t0 = Instant::now() + Duration::from_secs(60);
    s.set_lit(&[(0, 24_000)], false, None, t0);
    s.set_lit(&[(0, 36_000)], false, None, t0 + Duration::from_secs(1));
    assert!((s.speed() - 0.25).abs() < 1e-9, "{}", s.speed());
    assert!(drain(&mut s).is_empty() && s.waiting());
    // Done: it plays on, fading in.
    s.set_lit(&[(0, 48_000)], true, None, t0 + Duration::from_secs(2));
    let y = drain(&mut s);
    assert_eq!(y.len(), 24_000);
    assert!(y[0].abs() < 0.01 && (y[1_000] - x[0][25_000]).abs() < 1e-6);
    assert!(s.ended());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_hold_plays_the_original_past_the_cleaned_part() {
    let dir = scratch("hold");
    let x = original48(&fixture("master_48k_mono.wav"));
    let zero = vec![vec![0f32; 48_000]];
    let j = job("master_48k_mono.wav", &dir, &zero, vec![(0, 12_000)], false);
    let mut s = FileSource::open(&j, None, 0, 0.0).unwrap();
    assert_eq!(drain(&mut s).len(), 12_000);
    assert!(s.waiting());
    s.hold(true);
    let y = drain(&mut s);
    assert_eq!(y.len(), 36_000, "the original to the end");
    assert!((y[1_000] - x[0][13_000]).abs() < 1e-6);
    // Let go past the cleaned part: a 5 ms fade out, then it waits.
    let mut s = FileSource::open(&j, None, 0, 0.0).unwrap();
    s.hold(true);
    let y = s.render(480).unwrap();
    assert!(!y.is_empty());
    let mut y = Vec::new();
    for _ in 0..40 {
        y.extend(s.render(480).unwrap());
    }
    s.hold(false);
    let tail = drain(&mut s);
    assert!(s.waiting());
    assert!(
        tail.len() <= 240 && tail.last().is_none_or(|v| v.abs() < 0.01),
        "{} {:?} at {}",
        tail.len(),
        tail.last(),
        s.position()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_seek_lands_on_the_frame_in_a_resampled_file() {
    let dir = scratch("seek");
    let x = original48(&fixture("flac_44k_stereo.flac"));
    let zero = vec![vec![0f32; 48_000]; 2];
    let j = job("flac_44k_stereo.flac", &dir, &zero, vec![(0, 48_000)], true);
    // Back from 40,000: the decoder seeks, the converter starts over.
    let mut s = FileSource::open(&j, None, 40_000, 0.5).unwrap();
    assert_eq!(s.channels(), 2);
    s.render(480).unwrap();
    s.seek(30_000);
    // The fade out, then the jump.
    let fade = s.render(480).unwrap();
    assert_eq!(fade.len(), 2 * 240);
    let y = s.render(480).unwrap();
    assert_eq!(s.position(), 30_480);
    // Interleaved: 0.5 of the original from frame 30,000 on, fading in.
    for i in 240..480 {
        for c in 0..2 {
            let want = 0.5 * x[c][30_000 + i];
            assert!((y[2 * i + c] - want).abs() < 1e-3, "frame {i} ch {c}");
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// `pw-cat` that keeps what it is fed (first on PATH, set once).
fn fake_pw_cat() -> PathBuf {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let dir = std::env::temp_dir().join(format!("hushmic-play-{}-bin", std::process::id()));
    ONCE.call_once(|| {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("pw-cat");
        std::fs::write(
            &p,
            "#!/bin/sh\n[ \"$1\" = --help ] && exit 0\nexec cat > \"$(dirname \"$0\")/fed.f32\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        // the playback's latency watch never reaches the box's PipeWire:
        // its tools fail, as without a graph
        for tool in ["pw-dump", "pw-top"] {
            let p = dir.join(tool);
            std::fs::write(&p, "#!/bin/sh\nexit 1\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![dir.clone()];
        paths.extend(std::env::split_paths(&path));
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    });
    dir
}

#[test]
fn the_feeder_puts_the_mix_through_pw_cat_and_ends() {
    let bin = fake_pw_cat();
    let dir = scratch("feed");
    let x = original48(&fixture("flac_44k_stereo.flac"));
    let clean: Vec<Vec<f32>> = x
        .iter()
        .map(|c| c.iter().map(|v| v * 0.5).collect())
        .collect();
    let j = job(
        "flac_44k_stereo.flac",
        &dir,
        &clean,
        vec![(0, 48_000)],
        true,
    );
    let want = drain(&mut FileSource::open(&j, None, 0, 0.25).unwrap());
    let p = FilePlayer::start(FileSource::open(&j, None, 0, 0.25).unwrap());
    let deadline = Instant::now() + Duration::from_secs(20);
    while p.poll() == PlayerState::Playing && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(p.poll(), PlayerState::Ended);
    assert_eq!(p.position(), 48_000);
    let bytes = std::fs::read(bin.join("fed.f32")).unwrap();
    let fed: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    assert_eq!(fed, want);
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(bin);
}
