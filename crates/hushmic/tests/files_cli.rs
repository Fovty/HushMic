//! `hushmic clean FILE…` as a user runs it: the cleaned files land next to
//! the originals (or in `--out`), never over a file; one line per file, or
//! one JSON object with `--json`; exit 0 when all are cleaned, 1 when one
//! is not or only in part (damaged), 2 for a usage error. Self-skips without the repo's models
//! (scripts/setup-assets.sh), unless HUSHMIC_ASSERT_ASSETS=1.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn assets() -> bool {
    let ok = root().join("assets/models/dpdfnet2_48khz_hr.onnx").exists()
        && root().join("assets/lib/libonnxruntime.so").exists();
    assert!(
        ok || std::env::var("HUSHMIC_ASSERT_ASSETS").as_deref() != Ok("1"),
        "assets missing"
    );
    if !ok {
        eprintln!("SKIP: models not provisioned");
    }
    ok
}

/// `hushmic clean ARGS` in `dir`, with the repo's models and a config and
/// cache of its own there.
fn clean(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hushmic"))
        .arg("clean")
        .args(args)
        .current_dir(dir)
        .env("HUSHMIC_MODEL_DIR", root().join("assets/models"))
        .env(
            "ORT_DYLIB_PATH",
            root().join("assets/lib/libonnxruntime.so"),
        )
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .output()
        .unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hushmic-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for f in ["master_48k_mono.wav", "flac_44k_stereo.flac"] {
        std::fs::copy(root().join("tests/fixtures/files").join(f), d.join(f)).unwrap();
    }
    d
}

/// Frames and channels, decoded.
fn frames(p: &Path) -> (usize, usize) {
    let mut d = hushmic::files::decode::Decoder::open(p, None).unwrap();
    let ch = d.info().channels;
    let mut n = 0;
    while let Some(b) = d.next_block().unwrap() {
        n += b[0].len();
    }
    (n, ch)
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn usage_errors_exit_1_before_any_work() {
    let dir = std::env::temp_dir();
    for bad in [
        &[][..],
        &["--strength", "loud", "a.wav"][..],
        &["a.wav", "--out", "/etc/hostname"][..],
    ] {
        let out = clean(&dir, bad);
        assert_eq!(out.status.code(), Some(1), "{bad:?}");
        assert!(text(&out.stderr).contains("usage: hushmic clean"));
        assert!(out.stdout.is_empty());
    }
}

/// `--help` is the help (exit 0, stdout), and no cleaning starts.
#[test]
fn help_is_the_help_and_nothing_runs() {
    let dir = scratch("help");
    for h in ["--help", "-h"] {
        let out = clean(&dir, &[h, "master_48k_mono.wav"]);
        assert_eq!(out.status.code(), Some(0));
        let help = text(&out.stdout);
        assert!(
            help.starts_with("Usage: hushmic clean FILE|FOLDER…"),
            "{help}"
        );
        assert!(help.contains("--quiet") && help.contains("Exit codes"));
        assert!(out.stderr.is_empty());
    }
    assert!(!dir.join("master_48k_mono (cleaned).wav").exists());
    let out = Command::new(env!("CARGO_BIN_EXE_hushmic"))
        .args(["open-files", "--help"])
        .env("XDG_RUNTIME_DIR", &dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stdout).starts_with("Usage: hushmic open-files FILE|FOLDER…"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn files_are_cleaned_next_to_themselves_and_never_over_a_file() {
    if !assets() {
        return;
    }
    let dir = scratch("next");
    let out = clean(
        &dir,
        &[
            "master_48k_mono.wav",
            "flac_44k_stereo.flac",
            "--strength",
            "medium",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "master_48k_mono.wav → master_48k_mono (cleaned).wav\n\
         flac_44k_stereo.flac → flac_44k_stereo (cleaned).flac\n"
    );
    let first = std::fs::read(dir.join("master_48k_mono (cleaned).wav")).unwrap();
    assert_eq!(
        frames(&dir.join("master_48k_mono (cleaned).wav")),
        frames(&dir.join("master_48k_mono.wav")),
        "the original's length"
    );
    // Again: a second name, the first untouched.
    let out = clean(&dir, &["master_48k_mono.wav"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        text(&out.stdout),
        "master_48k_mono.wav → master_48k_mono (cleaned 2).wav\n"
    );
    assert_eq!(
        std::fs::read(dir.join("master_48k_mono (cleaned).wav")).unwrap(),
        first
    );
    // Nothing is left in the cache.
    let left = std::fs::read_dir(dir.join("cache/hushmic/files"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(left, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn json_lines_say_each_files_outcome_and_one_failure_exits_1() {
    if !assets() {
        return;
    }
    let dir = scratch("json");
    // --out makes its folder
    let out_dir = dir.join("out/new");
    let out = clean(
        &dir,
        &[
            "missing.wav",
            "master_48k_mono.wav",
            "--json",
            "--out",
            "out/new",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let lines: Vec<serde_json::Value> = text(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["file"], "missing.wav");
    assert_eq!(lines[0]["problem"], "unreadable");
    assert!(lines[0]["output"].is_null());
    assert_eq!(lines[1]["file"], "master_48k_mono.wav");
    assert_eq!(lines[1]["output"], "out/new/master_48k_mono (cleaned).wav");
    assert_eq!(lines[1]["same_format"], true);
    assert!(lines[1]["problem"].is_null());
    // what it was cleaned with: the Files page's Strong unless told, the
    // Settings' model
    assert_eq!(lines[1]["strength"], "strong");
    assert_eq!(lines[1]["model"], "quality");
    // the track fields always (a stable schema), defaults too
    assert_eq!(
        (&lines[1]["tracks"], &lines[1]["tracks_dropped"]),
        (&1.into(), &0.into())
    );
    assert_eq!(
        (&lines[0]["tracks"], &lines[0]["tracks_dropped"]),
        (&0.into(), &0.into())
    );
    assert_eq!(lines[1]["track_cleaned"], 1);
    let has = |l: &serde_json::Value, k: &str| l.as_object().unwrap().contains_key(k);
    assert!(has(&lines[0], "track_cleaned") && lines[0]["track_cleaned"].is_null());
    // an empty file says so (A104 N-E1), in words and in JSON
    std::fs::write(dir.join("zero.wav"), b"").unwrap();
    let out = clean(&dir, &["zero.wav"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("zero.wav: the file is empty (0 bytes)")
            || text(&out.stdout).contains("zero.wav: the file is empty (0 bytes)"),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    let out = clean(&dir, &["zero.wav", "--json"]);
    let line: serde_json::Value = serde_json::from_str(text(&out.stdout).trim()).unwrap();
    assert_eq!(line["problem"], "empty");
    // no terminal: no progress line
    assert!(!text(&out.stderr).contains('\r'));
    assert!(out_dir.join("master_48k_mono (cleaned).wav").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_damaged_file_is_cleaned_as_far_as_it_reads_and_exits_1() {
    if !assets() {
        return;
    }
    let dir = scratch("damaged");
    std::fs::copy(
        root().join("tests/fixtures/files/mp3_44k_cut.mp3"),
        dir.join("cut.mp3"),
    )
    .unwrap();
    let out = clean(&dir, &["cut.mp3", "--json"]);
    // Its copy is written, but a script must not take it for the whole.
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let line: serde_json::Value = serde_json::from_str(text(&out.stdout).trim()).unwrap();
    let output = line["output"].as_str().expect("a copy is written");
    assert!(dir.join(output).exists());
    let at = line["damaged_at"].as_f64().expect("says how far it read");
    assert!((1.0..1.3).contains(&at), "read {at} s");
    // seconds, to the millisecond (F12)
    assert_eq!((at * 1000.0).round() / 1000.0, at, "{at}");
    let _ = std::fs::remove_dir_all(dir);
}

/// HE-AAC without ffmpeg (the AAC fixture with its timescale doubled, as
/// implicit SBR looks; PATH holds no ffmpeg): cleaned, exit 0, and said to
/// be reduced, in JSON and as a note on stderr.
#[test]
fn he_aac_without_ffmpeg_is_said_to_be_reduced() {
    if !assets() {
        return;
    }
    let dir = scratch("reduced");
    let mut b = std::fs::read(root().join("tests/fixtures/files/aac_44k_stereo.m4a")).unwrap();
    let at = b.windows(4).position(|w| w == b"mdhd").unwrap() + 4 + 12;
    let scale = u32::from_be_bytes(b[at..at + 4].try_into().unwrap());
    b[at..at + 4].copy_from_slice(&(scale * 2).to_be_bytes());
    std::fs::write(dir.join("core.m4a"), &b).unwrap();
    let empty = dir.join("no-tools");
    std::fs::create_dir_all(&empty).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_hushmic"))
            .arg("clean")
            .args(args)
            .current_dir(&dir)
            .env("PATH", &empty)
            .env("HUSHMIC_MODEL_DIR", root().join("assets/models"))
            .env(
                "ORT_DYLIB_PATH",
                root().join("assets/lib/libonnxruntime.so"),
            )
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .output()
            .unwrap()
    };
    let out = run(&["core.m4a", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let line: serde_json::Value = serde_json::from_str(text(&out.stdout).trim()).unwrap();
    assert_eq!(line["reduced_quality"], true, "{line}");
    assert!(line["damaged_at"].is_null());
    let first = dir.join(line["output"].as_str().expect("a copy is written"));
    std::fs::remove_file(first).unwrap();
    let out = run(&["core.m4a"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stdout).starts_with("core.m4a → "));
    assert_eq!(
        text(&out.stderr).trim(),
        "core.m4a: cleaned at reduced quality. Install ffmpeg for full quality."
    );
    // a whole file at its own rate says nothing of the kind
    let out = run(&["flac_44k_stereo.flac", "--json"]);
    assert!(
        !text(&out.stdout).contains("reduced"),
        "{}",
        text(&out.stdout)
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn two_runs_at_once_each_clean_in_a_scratch_folder_of_their_own() {
    if !assets() {
        return;
    }
    let dir = scratch("together");
    // one cache for both, as two runs of one user
    let start = |file: &str| {
        Command::new(env!("CARGO_BIN_EXE_hushmic"))
            .args(["clean", file, "--json"])
            .current_dir(&dir)
            .env("HUSHMIC_MODEL_DIR", root().join("assets/models"))
            .env(
                "ORT_DYLIB_PATH",
                root().join("assets/lib/libonnxruntime.so"),
            )
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a = start("master_48k_mono.wav");
    let b = start("flac_44k_stereo.flac");
    // while both run, each holds a folder of its own
    let root_dir = dir.join("cache/hushmic/files");
    let mut most = 0;
    let t = std::time::Instant::now();
    while t.elapsed() < std::time::Duration::from_secs(5) {
        let n = std::fs::read_dir(&root_dir).map_or(0, |d| {
            d.flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.starts_with("clean-") && !n.ends_with(".lock")
                })
                .count()
        });
        most = most.max(n);
        if most >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (a, b) = (a.wait_with_output().unwrap(), b.wait_with_output().unwrap());
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    assert_eq!(b.status.code(), Some(0), "{}", text(&b.stderr));
    for name in ["master_48k_mono", "flac_44k_stereo"] {
        let ext = if name.starts_with("flac") {
            "flac"
        } else {
            "wav"
        };
        assert_eq!(
            frames(&dir.join(format!("{name} (cleaned).{ext}"))),
            frames(&dir.join(format!("{name}.{ext}"))),
            "{name}: whole"
        );
    }
    assert!(most >= 2, "both scratch folders at once (saw {most})");
    // and both are gone, locks too
    let left: Vec<String> = std::fs::read_dir(&root_dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("clean-"))
                .collect()
        })
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_interrupted_runs_scratch_folder_goes_at_the_next_run() {
    let dir = scratch("stale");
    // What a `hushmic clean` killed midway left (no pid is this high).
    let stale = dir.join("cache/hushmic/files/clean-999999999/0");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("clean.f32"), vec![0u8; 4096]).unwrap();
    let out = clean(&dir, &["missing.wav"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!dir.join("cache/hushmic/files/clean-999999999").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn it_cleans_at_idle_priority() {
    if !assets() {
        return;
    }
    let dir = scratch("idle");
    let mut child = Command::new(env!("CARGO_BIN_EXE_hushmic"))
        .args(["clean", "flac_44k_stereo.flac", "master_48k_mono.wav"])
        .current_dir(&dir)
        .env("HUSHMIC_MODEL_DIR", root().join("assets/models"))
        .env(
            "ORT_DYLIB_PATH",
            root().join("assets/lib/libonnxruntime.so"),
        )
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // The scheduling policy, field 41 of /proc/PID/stat (5: SCHED_IDLE),
    // while it runs.
    let stat = format!("/proc/{}/stat", child.id());
    let mut seen = None;
    while child.try_wait().unwrap().is_none() {
        if let Ok(s) = std::fs::read_to_string(&stat) {
            let after = &s[s.rfind(')').unwrap() + 2..];
            if let Some(p) = after.split(' ').nth(38) {
                seen = Some(p.to_string());
                if p == "5" {
                    break;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(seen.as_deref(), Some("5"), "SCHED_IDLE");
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(dir);
}

/// An OBS recording (a picture, a "Desktop Audio" track and a "Mic/Aux"
/// one): the mic's is cleaned, the other is copied packet for packet (a
/// music track is no voice), both kept; `--track` picks another, or all;
/// without ffmpeg only the first, and it says so.
#[test]
fn every_sound_track_of_a_recording_is_kept() {
    if !assets() {
        return;
    }
    let Some(ff) = hushmic::files::ffmpeg::Ffmpeg::find().filter(|f| f.ffprobe.is_some()) else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let dir = scratch("tracks");
    let fx = root().join("tests/fixtures/files");
    let made = Command::new(&ff.ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(fx.join("video_h264_aac.mp4"))
        .arg("-i")
        .arg(fx.join("master_48k_mono.wav"))
        .args(["-map", "0:v", "-map", "0:a", "-map", "1:a", "-t", "1"])
        .args(["-c:v", "copy", "-c:a", "aac"])
        .args(["-metadata:s:a:0", "title=Desktop Audio"])
        .args(["-metadata:s:a:1", "title=Mic/Aux"])
        .arg(dir.join("obs.mkv"))
        .status()
        .unwrap()
        .success();
    assert!(made);
    // the packets of a file's `n`th sound track (0 the first)
    let packets = |file: &Path, n: usize| {
        let o = Command::new(&ff.ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(file)
            .args(["-map", &format!("0:a:{n}"), "-c", "copy", "-f", "md5", "-"])
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", text(&o.stderr));
        text(&o.stdout)
    };
    let obs = dir.join("obs.mkv");
    let cleaned = |k: usize| match k {
        1 => dir.join("obs (cleaned).mkv"),
        k => dir.join(format!("obs (cleaned {k}).mkv")),
    };
    let json = |args: &[&str]| -> serde_json::Value {
        let out = clean(&dir, args);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        serde_json::from_str(text(&out.stdout).trim()).unwrap()
    };
    let line = json(&["obs.mkv", "--json", "--model", "light"]);
    assert_eq!(line["tracks"], 2);
    assert_eq!(line["tracks_dropped"], 0);
    assert_eq!(line["model"], "light");
    assert_eq!(line["track_cleaned"], 2, "the Mic/Aux track");
    assert_eq!(
        packets(&cleaned(1), 0),
        packets(&obs, 0),
        "Desktop Audio as it was"
    );
    assert_ne!(packets(&cleaned(1), 1), packets(&obs, 1), "Mic/Aux cleaned");
    let streams = Command::new(ff.ffprobe.as_ref().unwrap())
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type:stream_tags=title",
            "-of",
            "csv=p=0",
        ])
        .arg(cleaned(1))
        .output()
        .unwrap();
    assert_eq!(
        text(&streams.stdout),
        "video\naudio,Desktop Audio\naudio,Mic/Aux\n",
        "every track in its place, with its title"
    );
    // the first, by number: the other copied as it is
    let line = json(&["obs.mkv", "--json", "--track", "1", "--model", "light"]);
    assert_eq!(line["track_cleaned"], 1);
    assert_ne!(packets(&cleaned(2), 0), packets(&obs, 0));
    assert_eq!(packets(&cleaned(2), 1), packets(&obs, 1));
    let line = json(&["obs.mkv", "--json", "--track=all", "--model", "light"]);
    assert_eq!(line["track_cleaned"], "all");
    assert_ne!(packets(&cleaned(3), 0), packets(&obs, 0));
    assert_ne!(packets(&cleaned(3), 1), packets(&obs, 1));
    // a track the file hasn't: that file fails, saying why
    let out = clean(
        &dir,
        &["obs.mkv", "--json", "--track", "3", "--model", "light"],
    );
    assert_eq!(out.status.code(), Some(1));
    let line: serde_json::Value = serde_json::from_str(text(&out.stdout).trim()).unwrap();
    assert_eq!(
        line["detail"],
        "there is no sound track 3 (it has 2 sound tracks)"
    );
    for bad in [
        &["obs.mkv", "--track", "0"][..],
        &["obs.mkv", "--track", "mic"][..],
    ] {
        let out = clean(&dir, bad);
        assert_eq!(out.status.code(), Some(1), "{bad:?}");
        assert!(text(&out.stderr).contains("--track: a number from 1, or all"));
    }
    // without ffmpeg on PATH: the first track only, said on stderr
    let out = Command::new(env!("CARGO_BIN_EXE_hushmic"))
        .args(["clean", "obs.mkv", "--model", "light"])
        .current_dir(&dir)
        .env("PATH", "/nonexistent")
        .env("HUSHMIC_MODEL_DIR", root().join("assets/models"))
        .env(
            "ORT_DYLIB_PATH",
            root().join("assets/lib/libonnxruntime.so"),
        )
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("only the first of its 2 sound tracks is kept"),
        "{}",
        text(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// F6: `clean FOLDER` cleans the recordings in it (its top level; `-r`
/// goes down), leaves out HushMic's own copies, and an empty folder is a
/// failure said as one (exit 1).
#[test]
fn a_folder_is_cleaned_file_by_file() {
    if !assets() {
        return;
    }
    let dir = scratch("folder");
    let week = dir.join("Week");
    std::fs::create_dir_all(week.join("Day 2")).unwrap();
    std::fs::rename(dir.join("master_48k_mono.wav"), week.join("a.wav")).unwrap();
    std::fs::copy(week.join("a.wav"), week.join("a (cleaned).wav")).unwrap();
    std::fs::rename(dir.join("flac_44k_stereo.flac"), week.join("Day 2/b.flac")).unwrap();
    let out = clean(&dir, &["Week"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "Week/a.wav → Week/a (cleaned 2).wav\n");
    assert!(
        text(&out.stderr).contains("a (cleaned).wav: left out, HushMic's cleaned copy"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        !week.join("Day 2/b (cleaned).flac").exists(),
        "the top level only"
    );
    let out = clean(&dir, &["-r", "Week"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "Week/Day 2/b.flac → Week/Day 2/b (cleaned).flac\n\
         Week/a.wav → Week/a (cleaned 3).wav\n"
    );
    std::fs::create_dir_all(dir.join("Empty")).unwrap();
    let out = clean(&dir, &["Empty", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_str(text(&out.stdout).trim()).unwrap();
    assert_eq!(v["problem"], "not_media", "{v}");
    assert_eq!(
        (&v["strength"], &v["model"]),
        (&"strong".into(), &"quality".into())
    );
    // --json: a line for each file a folder left out, not a failure
    let out = clean(&dir, &["Week", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let lines: Vec<serde_json::Value> = text(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let skipped = lines
        .iter()
        .find(|l| l["file"] == "Week/a (cleaned).wav")
        .expect("the copy's line");
    assert_eq!(skipped["problem"], "skipped");
    assert_eq!(
        skipped["detail"],
        "HushMic's cleaned copy of a file beside it"
    );
    assert!(lines
        .iter()
        .any(|l| l["file"] == "Week/a.wav" && l["problem"].is_null()));
    let _ = std::fs::remove_dir_all(dir);
}

/// F10: a copy keeps every sample where the original has it, and its
/// chapters too: AAC in Matroska (whose decoders keep the encoder's delay
/// in front, the picture at zero), and an MP3 that starts late (its
/// chapter at 15 s stays at 15 s).
#[test]
fn every_sample_and_chapter_keeps_its_time() {
    if !assets() {
        return;
    }
    let Some(ff) = hushmic::files::ffmpeg::Ffmpeg::find().filter(|f| f.ffprobe.is_some()) else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let dir = scratch("timing");
    let run = |args: &[&str]| {
        let ok = Command::new(&ff.ffmpeg)
            .args(["-v", "error", "-y"])
            .args(args)
            .current_dir(&dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "{args:?}");
    };
    let tone = "aevalsrc=if(gte(t\\,0.5)\\,0.5*sin(2*PI*440*t)\\,0):d=20:s=48000";
    run(&[
        "-f",
        "lavfi",
        "-i",
        tone,
        "-f",
        "lavfi",
        "-i",
        "testsrc=s=64x48:r=25:d=20",
        "-c:a",
        "aac",
        "-c:v",
        "mpeg4",
        "talk.mkv",
    ]);
    let mut files = vec!["talk.mkv"];
    if ff.has_encoder("libmp3lame") {
        std::fs::write(
            dir.join("chapters.txt"),
            ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=15000\ntitle=One\n\
             [CHAPTER]\nTIMEBASE=1/1000\nSTART=15000\nEND=20000\ntitle=Two\n",
        )
        .unwrap();
        run(&[
            "-f",
            "lavfi",
            "-i",
            tone,
            "-i",
            "chapters.txt",
            "-map",
            "0:a",
            "-map_chapters",
            "1",
            "-c:a",
            "libmp3lame",
            "talk.mp3",
        ]);
        files.push("talk.mp3");
    }
    let mut args = files.clone();
    args.extend(["--strength", "gentle"]);
    let out = clean(&dir, &args);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    // how many samples it decodes to, and where the tone begins
    let samples = |f: &str| {
        let o = Command::new(&ff.ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(dir.join(f))
            .args(["-map", "0:a:0", "-ac", "1", "-f", "f32le", "-"])
            .output()
            .unwrap();
        let x: Vec<f32> = o
            .stdout
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        (x.len(), x.iter().position(|v| v.abs() > 0.05).unwrap())
    };
    let probe = |f: &str, what: &str| {
        let o = Command::new(ff.ffprobe.as_ref().unwrap())
            .args(["-v", "error", "-show_entries", what, "-of", "csv=p=0"])
            .arg(dir.join(f))
            .output()
            .unwrap();
        text(&o.stdout)
    };
    for f in files {
        let copy = f.replace("talk.", "talk (cleaned).");
        let (n, at) = samples(f);
        let (cn, cat) = samples(&copy);
        assert_eq!(cn, n, "{f}: as many samples");
        assert!(
            cat.abs_diff(at) < 16,
            "{f}: the tone at {at}, in the copy at {cat}"
        );
        assert_eq!(
            probe(&copy, "chapter=start_time"),
            probe(f, "chapter=start_time"),
            "{f}"
        );
        assert_eq!(
            probe(&copy, "stream=start_time"),
            probe(f, "stream=start_time"),
            "{f}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}
