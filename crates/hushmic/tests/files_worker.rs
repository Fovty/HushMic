//! The file worker's protocol, in-process and through `hushmic clean
//! --worker`: it opens, lights, finishes and stops on command or at the end
//! of its input; a file it cannot clean ends in one `error` line. Self-skips
//! without the repo's models (scripts/setup-assets.sh), unless
//! HUSHMIC_ASSERT_ASSETS=1.

use hushmic::files::decode::Decoder;
use hushmic::files::save::Pick;
use hushmic::files::worker::{prepare, serve, Args, Command, Msg, Prepare, Problem};
use hushmic_denoiser::Inference;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn assets() -> Option<(PathBuf, PathBuf)> {
    let models = root().join("assets/models");
    let rt = root().join("assets/lib/libonnxruntime.so");
    if models.join("dpdfnet2_48khz_hr.onnx").exists() && rt.exists() {
        return Some((models, rt));
    }
    assert!(
        std::env::var("HUSHMIC_ASSERT_ASSETS").as_deref() != Ok("1"),
        "assets missing"
    );
    eprintln!("SKIP: models not provisioned");
    None
}

fn args(file: &str, cache: &Path) -> Args {
    Args {
        file: root().join("tests/fixtures/files").join(file),
        cache: cache.to_path_buf(),
        model: "dpdfnet2_48khz_hr".into(),
        inference: Inference::Auto,
        ffmpeg: None,
        track: Default::default(),
        fake: false,
        probe: false,
        prepare: None,
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hushmic-worker-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// stdout as lines, as they come.
struct Lines(Vec<u8>, Sender<String>);

impl Write for Lines {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(b);
        while let Some(i) = self.0.iter().position(|&c| c == b'\n') {
            let line: Vec<u8> = self.0.drain(..=i).collect();
            let _ = self
                .1
                .send(String::from_utf8_lossy(&line).trim().to_string());
        }
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn next(rx: &Receiver<String>) -> Msg {
    let line = rx.recv_timeout(Duration::from_secs(120)).expect("a line");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line}: {e}"))
}

/// Messages up to and including the first `done` or `error`.
fn until_end(rx: &Receiver<String>) -> Vec<Msg> {
    let mut seen = Vec::new();
    loop {
        let m = next(rx);
        let end = matches!(m, Msg::Done | Msg::Error { .. });
        seen.push(m);
        if end {
            return seen;
        }
    }
}

#[test]
fn a_file_opens_lights_and_finishes_then_stops_on_command() {
    let Some((models, rt)) = assets() else { return };
    let dir = scratch("serve");
    let a = args("flac_44k_same.flac", &dir);
    let (cmd_r, mut cmd_w) = std::io::pipe().unwrap();
    let (tx, rx) = channel();
    let t = std::thread::spawn(move || {
        serve(
            &a,
            &models,
            &rt,
            BufReader::new(cmd_r),
            &mut Lines(Vec::new(), tx),
        )
    });
    match next(&rx) {
        Msg::Opened {
            rate,
            channels,
            stored,
            frames,
            ..
        } => assert_eq!(
            (rate, channels, stored, frames),
            (44_100, 2, 1, Some(48_000))
        ),
        m => panic!("first: {m:?}"),
    }
    // What Save writes: without ffmpeg FLAC stays FLAC.
    assert_eq!(
        next(&rx),
        Msg::Output {
            ext: "flac".into(),
            same_format: true,
            sound_only: false
        }
    );
    let seen = until_end(&rx);
    assert_eq!(seen.last(), Some(&Msg::Done));
    assert!(seen.contains(&Msg::Length { frames: 48_000 }));
    // The last cleaned parts before `done` cover the file.
    let lit = seen.iter().rev().find_map(|m| match m {
        Msg::Lit { ranges } => Some(ranges.clone()),
        _ => None,
    });
    assert_eq!(lit, Some(vec![(0, 48_000)]));
    // Still there after `done`, for what the daemon asks next.
    cmd_w
        .write_all(Command::Front(100).line().as_bytes())
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(!t.is_finished());
    cmd_w.write_all(Command::Stop.line().as_bytes()).unwrap();
    assert_eq!(t.join().unwrap(), 0);
    // The output for Save, from the cache: the original's rate and
    // channels, its length.
    let p = Prepare {
        db: 12.0,
        frames: 48_000,
        stored: 1,
        more: Vec::new(),
    };
    let (tx, rx) = channel();
    let a = args("flac_44k_same.flac", &dir);
    let (_keep, input) = {
        let (r, w) = std::io::pipe().unwrap();
        (w, BufReader::new(r))
    };
    assert_eq!(prepare(&a, p.clone(), input, &mut Lines(Vec::new(), tx)), 0);
    let file = dir.join("prepared.flac");
    // Past any word on how far it is.
    let last = std::iter::repeat_with(|| next(&rx))
        .find(|m| !matches!(m, Msg::Preparing { .. }))
        .unwrap();
    assert_eq!(
        last,
        Msg::Prepared {
            db: 12.0,
            file: file.display().to_string()
        }
    );
    let mut d = Decoder::open(&file, None).unwrap();
    assert_eq!((d.info().rate, d.info().channels), (44_100, 2));
    let mut n = 0;
    while let Some(b) = d.next_block().unwrap() {
        n += b[0].len();
    }
    assert_eq!(n, 44_100);
    // The original gone: Save cannot be prepared, and says why.
    let (tx, rx) = channel();
    let gone = Args {
        file: dir.join("gone.flac"),
        ..a
    };
    let (_keep, input) = {
        let (r, w) = std::io::pipe().unwrap();
        (w, BufReader::new(r))
    };
    assert_eq!(prepare(&gone, p, input, &mut Lines(Vec::new(), tx)), 1);
    assert!(matches!(
        next(&rx),
        Msg::SaveFailed {
            kind: Problem::Moved,
            ..
        }
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_silent_video_ends_in_one_error() {
    let Some((models, rt)) = assets() else { return };
    let dir = scratch("silent");
    let (tx, rx) = channel();
    let code = serve(
        &args("video_only.mp4", &dir),
        &models,
        &rt,
        std::io::empty(),
        &mut Lines(Vec::new(), tx),
    );
    assert_eq!(code, 1);
    match next(&rx) {
        Msg::Error { kind, .. } => assert_eq!(kind, Problem::NoSound),
        m => panic!("{m:?}"),
    }
    assert!(rx.try_recv().is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_binary_runs_a_worker_and_stops_when_its_input_ends() {
    let Some((models, rt)) = assets() else { return };
    let dir = scratch("bin");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_hushmic"))
        .args(["clean", "--worker"])
        .args(args("flac_44k_stereo.flac", &dir).to_args())
        .env("HUSHMIC_MODEL_DIR", &models)
        .env("ORT_DYLIB_PATH", &rt)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for l in out.lines().map_while(Result::ok) {
            let _ = tx.send(l);
        }
    });
    assert!(matches!(next(&rx), Msg::Opened { stored: 2, .. }));
    assert_eq!(until_end(&rx).last(), Some(&Msg::Done));
    assert!(dir.join("clean.f32").exists());
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    let _ = std::fs::remove_dir_all(dir);
}

/// B9: `--probe` looks at a file as it is added: one that opens says
/// nothing is wrong; a phone's M4A cut before its index is incomplete, a
/// text or noise named .mp3 isn't audio or video, a silent video has no
/// sound; without ffmpeg a WMA is a format HushMic doesn't open.
#[test]
fn a_first_look_names_what_is_wrong() {
    let dir = scratch("probe");
    std::fs::create_dir_all(&dir).unwrap();
    let fixtures = root().join("tests/fixtures/files");
    // an M4A whose index (written last) is cut away
    let whole = std::fs::read(fixtures.join("aac_44k_stereo.m4a")).unwrap();
    let moov = whole.windows(4).position(|w| w == b"moov").unwrap();
    let cut = dir.join("phone.m4a");
    std::fs::write(&cut, &whole[..moov.saturating_sub(4)]).unwrap();
    let text = dir.join("notes.mp3");
    std::fs::write(&text, "Interview notes\nAsk about the budget.\n").unwrap();
    let noise = dir.join("noise.mp3");
    let bytes: Vec<u8> = (0..50_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    std::fs::write(&noise, &bytes).unwrap();
    let junk = dir.join("noise.dat");
    std::fs::write(&junk, &bytes).unwrap();
    let probe = |file: &Path| -> Msg {
        let mut a = args("flac_44k_stereo.flac", &dir);
        (a.file, a.probe) = (file.to_path_buf(), true);
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_hushmic"))
            .args(["clean", "--worker"])
            .args(a.to_args())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "{file:?}");
        let line = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(line.trim()).unwrap_or_else(|e| panic!("{line}: {e}"))
    };
    let problem = |m: Msg| match m {
        Msg::Probed { problem, .. } => problem,
        m => panic!("{m:?}"),
    };
    assert_eq!(problem(probe(&fixtures.join("flac_44k_stereo.flac"))), None);
    assert_eq!(problem(probe(&fixtures.join("video_h264_aac.mp4"))), None);
    match probe(&cut) {
        Msg::Probed {
            problem: Some(Problem::Incomplete),
            detail,
        } => assert!(detail.contains("probably cut off"), "{detail}"),
        m => panic!("{m:?}"),
    }
    assert_eq!(problem(probe(&text)), Some(Problem::NotMedia));
    // no ffmpeg here: a recording's name is believed (a format HushMic
    // can't open alone), another name is not
    assert_eq!(problem(probe(&noise)), Some(Problem::Unsupported));
    assert_eq!(problem(probe(&junk)), Some(Problem::NotMedia));
    assert_eq!(
        problem(probe(&fixtures.join("video_only.mp4"))),
        Some(Problem::NoSound)
    );
    assert_eq!(
        problem(probe(&fixtures.join("wma_48k_mono.wma"))),
        Some(Problem::Unsupported)
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// An OBS-style recording (a picture, a "Desktop" and a "Mic" sound track):
/// the Mic's is cleaned (a desktop track is no voice) and Save keeps every
/// stream in its place with its own name, the Desktop's copied packet for
/// packet; with `--track all` every sound track is cleaned, the main one
/// first.
#[test]
fn the_voice_track_is_cleaned_and_every_track_kept() {
    let Some((models, rt)) = assets() else { return };
    let Some(ff) = hushmic::files::ffmpeg::Ffmpeg::find().filter(|f| f.ffprobe.is_some()) else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let dir = scratch("tracks");
    std::fs::create_dir_all(&dir).unwrap();
    let fx = root().join("tests/fixtures/files");
    let obs = dir.join("obs.mkv");
    let made = std::process::Command::new(&ff.ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(fx.join("video_h264_aac.mp4"))
        .arg("-i")
        .arg(fx.join("mp3_44k_stereo.mp3"))
        .args(["-map", "0:v", "-map", "0:a", "-map", "1:a", "-t", "1"])
        .args(["-c:v", "copy", "-c:a:0", "aac", "-c:a:1", "libopus"])
        .args([
            "-metadata:s:a:0",
            "title=Desktop",
            "-metadata:s:a:1",
            "title=Mic",
        ])
        .arg(&obs)
        .status()
        .unwrap()
        .success();
    assert!(made);
    let probe = |p: &Path| {
        let out = std::process::Command::new(ff.ffprobe.as_ref().unwrap())
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,codec_name:stream_tags=title",
            ])
            .args(["-of", "csv=p=0"])
            .arg(p)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let level = |p: &Path, k: usize| {
        let mut d = Decoder::open_track(p, Some(&ff.ffmpeg), k).unwrap();
        let mut e = 0f64;
        while let Some(b) = d.next_block().unwrap() {
            e += b.iter().flatten().map(|v| (*v as f64).powi(2)).sum::<f64>();
        }
        e
    };
    let packets = |p: &Path, k: usize| {
        let out = std::process::Command::new(&ff.ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(p)
            .args(["-map", &format!("0:a:{k}"), "-c", "copy", "-f", "md5", "-"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    for all in [false, true] {
        let cache = dir.join(if all { "cache-all" } else { "cache" });
        let a = Args {
            file: obs.clone(),
            ffmpeg: Some(ff.ffmpeg.clone()),
            track: if all { Pick::All } else { Pick::Auto },
            ..args("x", &cache)
        };
        let (tx, rx) = channel();
        let (cmd_r, mut cmd_w) = std::io::pipe().unwrap();
        let a2 = a.clone();
        let (m2, rt2) = (models.clone(), rt.clone());
        let t = std::thread::spawn(move || {
            serve(
                &a2,
                &m2,
                &rt2,
                BufReader::new(cmd_r),
                &mut Lines(Vec::new(), tx),
            )
        });
        let main = match next(&rx) {
            Msg::Opened {
                tracks,
                cleaning,
                dropped,
                video,
                track,
                titles,
                ..
            } => {
                assert_eq!(titles, ["Desktop", "Mic"]);
                let want = if all {
                    (2, 2, 0, true, 0)
                } else {
                    (2, 1, 0, true, 1)
                };
                assert_eq!((tracks, cleaning, dropped, video, track), want);
                track
            }
            m => panic!("{m:?}"),
        };
        let seen = until_end(&rx);
        let more = seen.iter().position(|m| matches!(m, Msg::MoreDone { .. }));
        assert_eq!(seen.last(), Some(&Msg::Done));
        let cleaned = match more {
            // the other track after the main one: its own word, then done
            Some(at) if all => {
                let before = &seen[..at];
                assert!(
                    before.iter().any(|m| matches!(m, Msg::Length { .. })),
                    "{seen:?}"
                );
                assert!(cache.join("track-1").join("clean.f32").exists());
                match &seen[at] {
                    Msg::MoreDone { track: 1, cleaned } => Some(cleaned.expect("cleaned")),
                    m => panic!("{m:?}"),
                }
            }
            None if !all => None,
            m => panic!("{m:?} in {seen:?}"),
        };
        cmd_w.write_all(Command::Stop.line().as_bytes()).unwrap();
        assert_eq!(t.join().unwrap(), 0);
        // Save: both tracks, in their places and names
        let first = seen
            .iter()
            .rev()
            .find_map(|m| match m {
                Msg::Length { frames } => Some(*frames),
                _ => None,
            })
            .unwrap();
        let p = Prepare {
            db: 100.0,
            frames: first,
            stored: 2,
            more: vec![cleaned],
        };
        let (tx, rx) = channel();
        let (_keep, input) = {
            let (r, w) = std::io::pipe().unwrap();
            (w, BufReader::new(r))
        };
        // as the daemon asks: for the track its pass cleaned
        let a = Args {
            track: if all { Pick::All } else { Pick::One(main) },
            ..a
        };
        assert_eq!(prepare(&a, p, input, &mut Lines(Vec::new(), tx)), 0);
        let last = std::iter::repeat_with(|| next(&rx))
            .find(|m| !matches!(m, Msg::Preparing { .. }))
            .unwrap();
        let Msg::Prepared { file, .. } = last else {
            panic!("{last:?}")
        };
        let file = Path::new(&file);
        assert_eq!(probe(file), probe(&obs), "every stream, in place");
        for k in 0..2 {
            let (was, now) = (level(&obs, k), level(file, k));
            if all || k == 1 {
                assert!(now < was * 0.9, "track {k}: {now} of {was}");
            } else {
                assert_eq!(
                    packets(file, k),
                    packets(&obs, k),
                    "the Desktop track as it was"
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}
