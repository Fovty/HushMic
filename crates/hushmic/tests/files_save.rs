//! Save: WAV and FLAC decode back to the mix bit for bit; without ffmpeg
//! a lossy file or a video becomes FLAC; the output lands under a free
//! name and never over a file, also on another disk; a full disk, a
//! cancel and a vanished original are reported, and leave nothing. With
//! ffmpeg (skipped where there is none), every format is written as
//! itself: the same codec, the picture copied, the sound on time and as
//! long, tags and rotation kept.

use hushmic::files::cache::Cache;
use hushmic::files::decode::{Decoder, Via};
use hushmic::files::ffmpeg::Ffmpeg;
use hushmic::files::resample::Converter;
use hushmic::files::save::{
    deliver, deliver_noted, plan, prepare, sweep_partials, write, Mixdown, Plan, Prepared,
    SaveError, Tracks, Writer,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/files")
        .join(name)
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hushmic-save-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A cache for `original` as a pass would leave it: here simply half the
/// original at 48 kHz (what matters is that it differs from it). Its
/// channels and length.
fn cache_for(original: &Path, dir: &Path, ff: Option<&Path>) -> (usize, u64) {
    let mut d = Decoder::open(original, ff).unwrap();
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
    let half: Vec<Vec<f32>> = x
        .iter()
        .map(|c| c.iter().map(|v| v * 0.5).collect())
        .collect();
    Cache::create(dir, ch).unwrap().write(0, &half).unwrap();
    (ch, x[0].len() as u64)
}

/// HE-AAC read without ffmpeg (here: the AAC fixture with its track's
/// timescale doubled, as implicit SBR looks): Save writes at the rate the
/// file plays at, not its core's, and as long.
#[test]
fn he_aac_without_ffmpeg_saves_at_the_rate_it_plays_at() {
    let mut b = std::fs::read(fixture("aac_44k_stereo.m4a")).unwrap();
    let at = b.windows(4).position(|w| w == b"mdhd").unwrap() + 4 + 12;
    let scale = u32::from_be_bytes(b[at..at + 4].try_into().unwrap());
    b[at..at + 4].copy_from_slice(&(scale * 2).to_be_bytes());
    let dir = scratch("he-aac");
    let path = dir.join("core.m4a");
    std::fs::write(&path, &b).unwrap();
    let decoded = decode(&path, None)[0].len();
    let (ch, total) = cache_for(&path, &dir.join("cache"), None);
    let m = Mixdown::open(&path, None, &dir.join("cache"), ch, total, 1.0).unwrap();
    assert_eq!(m.info().rate, 88_200);
    let mix = mix_of(&path, None, &dir.join("cache"), ch, total, 1.0);
    let _ = std::fs::remove_dir_all(&dir);
    let want = decoded * 2;
    assert!(
        mix[0].len().abs_diff(want) <= 2,
        "{} of {want}",
        mix[0].len()
    );
}

/// The mix, planar, as Save computes it.
fn mix_of(
    original: &Path,
    ff: Option<&Path>,
    cache: &Path,
    ch: usize,
    total: u64,
    alpha: f32,
) -> Vec<Vec<f32>> {
    let mut m = Mixdown::open(original, ff, cache, ch, total, alpha).unwrap();
    let mut out = vec![Vec::new(); ch];
    while let Some(b) = m.next_block().unwrap() {
        for (o, v) in out.iter_mut().zip(b) {
            o.extend_from_slice(&v);
        }
    }
    out
}

fn decode(path: &Path, ff: Option<&Path>) -> Vec<Vec<f32>> {
    let mut d = Decoder::open(path, ff).unwrap();
    let mut out = vec![Vec::new(); d.info().channels];
    while let Some(b) = d.next_block().unwrap() {
        for (o, v) in out.iter_mut().zip(b) {
            o.extend_from_slice(&v);
        }
    }
    out
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

#[test]
fn wav_and_flac_decode_back_to_the_mix_bit_for_bit() {
    for name in [
        "master_44k_stereo.wav",
        "flac_44k_stereo.flac",
        "master_48k_mono.wav",
    ] {
        let dir = scratch(&format!("exact-{name}"));
        let src = fixture(name);
        let (ch, total) = cache_for(&src, &dir, None);
        let p = prepare(&src, &dir, ch, total, 0.25, None, &no_cancel()).unwrap();
        assert!(p.plan.same_format, "{name}");
        let want = mix_of(&src, None, &dir, ch, total, 0.25);
        let got = decode(&p.file, None);
        assert_eq!(got[0].len(), want[0].len(), "{name}: length");
        for (g, w) in got.iter().zip(&want) {
            let off = g
                .iter()
                .zip(w)
                .filter(|(a, b)| {
                    **a != ((**b * 32_768.0).round().clamp(-32_768.0, 32_767.0) / 32_768.0)
                })
                .count();
            assert_eq!(off, 0, "{name}");
        }
        // Not the original: the cleaned share is in it.
        assert_ne!(got, decode(&src, None));
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn twenty_four_bits_stay_twenty_four_bits() {
    let dir = scratch("24");
    let src = dir.join("deep.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&src, spec).unwrap();
    for v in &decode(&fixture("master_48k_mono.wav"), None)[0] {
        // Below the 16-bit grid: lost at 16 bits.
        w.write_sample(((v * 8_388_608.0) as i32 + 37).clamp(-8_388_608, 8_388_607))
            .unwrap();
    }
    w.finalize().unwrap();
    let cache = dir.join("cache");
    let (ch, total) = cache_for(&src, &cache, None);
    let want = mix_of(&src, None, &cache, ch, total, 0.25);
    let q = |v: f32| (v * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) / 8_388_608.0;
    for (writer, ext) in [
        (
            Writer::Wav {
                bits: 24,
                float: false,
            },
            "wav",
        ),
        (Writer::Flac { bits: 24 }, "flac"),
    ] {
        let p = Plan {
            writer,
            ext: ext.into(),
            same_format: true,
            sound_only: false,
            tracks: 1,
            dropped: 0,
            main: 0,
        };
        let out = dir.join(format!("out.{ext}"));
        let mut m = Mixdown::open(&src, None, &cache, ch, total, 0.25).unwrap();
        write(&p, &mut m, &src, None, &out, &no_cancel()).unwrap();
        let got = decode(&out, None);
        assert_eq!(Decoder::open(&out, None).unwrap().info().bits, Some(24));
        let off = got[0]
            .iter()
            .zip(&want[0])
            .filter(|(a, b)| **a != q(**b))
            .count();
        assert_eq!(off, 0, "{ext}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn without_ffmpeg_lossy_files_and_videos_become_flac() {
    let info = |n: &str| Decoder::open(&fixture(n), None).unwrap().info().clone();
    let mp3 = plan(
        &fixture("mp3_44k_stereo.mp3"),
        &info("mp3_44k_stereo.mp3"),
        None,
        Tracks::default(),
    );
    assert_eq!(
        (mp3.ext.as_str(), mp3.same_format, mp3.sound_only),
        ("flac", false, false)
    );
    assert_eq!(mp3.writer, Writer::Flac { bits: 16 });
    let v = plan(
        &fixture("video_h264_aac.mp4"),
        &info("video_h264_aac.mp4"),
        None,
        Tracks::default(),
    );
    assert_eq!((v.ext.as_str(), v.sound_only), ("flac", true));
    // And it is written: the video's sound, as FLAC.
    let dir = scratch("sound-only");
    let src = fixture("video_h264_aac.mp4");
    let (ch, total) = cache_for(&src, &dir, None);
    let p = prepare(&src, &dir, ch, total, 0.25, None, &no_cancel()).unwrap();
    assert_eq!(p.file, dir.join("prepared.flac"));
    assert_eq!(decode(&p.file, None)[0].len(), 48_000);
    let _ = std::fs::remove_dir_all(dir);
    let w = plan(
        &fixture("master_44k_stereo.wav"),
        &info("master_44k_stereo.wav"),
        None,
        Tracks::default(),
    );
    assert_eq!(
        w.writer,
        Writer::Wav {
            bits: 16,
            float: false
        }
    );
}

#[test]
fn the_output_lands_under_a_free_name_never_over_a_file() {
    let dir = scratch("deliver");
    let cache = dir.join("cache");
    let src = dir.join("My talk.wav");
    std::fs::copy(fixture("master_48k_mono.wav"), &src).unwrap();
    let (ch, total) = cache_for(&src, &cache, None);
    let p = prepare(&src, &cache, ch, total, 0.25, None, &no_cancel()).unwrap();
    let a = deliver(&p.file, &src, None, &no_cancel()).unwrap();
    assert_eq!(a, dir.join("My talk (cleaned).wav"));
    std::fs::write(dir.join("My talk (cleaned 2).wav"), b"mine").unwrap();
    let b = deliver(&p.file, &src, None, &no_cancel()).unwrap();
    assert_eq!(b, dir.join("My talk (cleaned 3).wav"));
    assert_eq!(
        std::fs::read(dir.join("My talk (cleaned 2).wav")).unwrap(),
        b"mine"
    );
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&p.file).unwrap());
    // Another disk: a copy under a hidden name first, then the real one.
    let shm = Path::new("/dev/shm").join(format!("hushmic-save-{}", std::process::id()));
    if std::fs::create_dir_all(&shm).is_ok() {
        let c = deliver(&p.file, &src, Some(&shm), &no_cancel()).unwrap();
        assert_eq!(c, shm.join("My talk (cleaned).wav"));
        assert_eq!(std::fs::read(&c).unwrap(), std::fs::read(&p.file).unwrap());
        let left: Vec<_> = std::fs::read_dir(&shm).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "no partial file left");
        // Cancelled: nothing lands, nothing is left.
        let stop = AtomicBool::new(true);
        assert!(matches!(
            deliver(&p.file, &src, Some(&shm), &stop),
            Err(SaveError::Cancelled)
        ));
        assert_eq!(std::fs::read_dir(&shm).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&shm);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_full_disk_a_cancel_and_a_vanished_original_are_reported() {
    let dir = scratch("errors");
    let src = fixture("master_44k_stereo.wav");
    let (ch, total) = cache_for(&src, &dir, None);
    let mut m = Mixdown::open(&src, None, &dir, ch, total, 0.25).unwrap();
    let wav = Plan {
        writer: Writer::Wav {
            bits: 16,
            float: false,
        },
        ext: "wav".into(),
        same_format: true,
        sound_only: false,
        tracks: 1,
        dropped: 0,
        main: 0,
    };
    let full = write(
        &wav,
        &mut m,
        &src,
        None,
        Path::new("/dev/full"),
        &no_cancel(),
    );
    assert!(matches!(full, Err(SaveError::NoSpace)), "{full:?}");
    let cancelled = prepare(&src, &dir, ch, total, 0.25, None, &AtomicBool::new(true));
    assert!(matches!(cancelled, Err(SaveError::Cancelled)));
    let gone = prepare(
        &dir.join("moved.wav"),
        &dir,
        ch,
        total,
        0.25,
        None,
        &no_cancel(),
    );
    assert!(matches!(gone, Err(SaveError::OriginalGone)));
    let left: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("prepared"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    let _ = std::fs::remove_dir_all(dir);
}

fn probe(ff: &Ffmpeg, path: &Path, entries: &str) -> String {
    let out = Command::new(ff.ffprobe.as_ref().unwrap())
        .args(["-v", "error", "-count_packets", "-show_entries", entries])
        .args(["-of", "default=nw=1"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The shift (expected index − decoded index) that matches best.
fn lag(x: &[f32], m: &[f32]) -> (i64, f64) {
    let (from, len) = (3000usize, 16_000usize.min(x.len().saturating_sub(3000)));
    let a = &x[from..from + len];
    let mut best = (0i64, -1f64);
    for lag in -2500i64..=2500 {
        let s = from as i64 + lag;
        if s < 0 || s as usize + len > m.len() {
            continue;
        }
        let b = &m[s as usize..s as usize + len];
        let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
        for (p, q) in a.iter().zip(b) {
            ab += *p as f64 * *q as f64;
            aa += *p as f64 * *p as f64;
            bb += *q as f64 * *q as f64;
        }
        let c = ab / (aa * bb).sqrt().max(1e-30);
        if c > best.1 {
            best = (lag, c);
        }
    }
    best
}

#[test]
fn with_ffmpeg_every_format_is_written_as_itself() {
    let Some(ff) = Ffmpeg::find().filter(|f| f.ffprobe.is_some()) else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let cases = [
        "mp3_44k_stereo.mp3",
        "aac_44k_stereo.m4a",
        "vorbis_44k_stereo.ogg",
        "opus_48k_mono.opus",
        "alac_44k_stereo.m4a",
        "aiff_44k_stereo.aiff",
        "flac_44k_stereo.flac",
        "master_44k_stereo.wav",
        "video_h264_aac.mp4",
        "video_h264_aac.mov",
        "video_h264_opus.mkv",
        "video_vp8_opus.webm",
    ];
    for name in cases {
        let dir = scratch(&format!("ff-{name}"));
        let src = fixture(name);
        let bin = Some(ff.ffmpeg.as_path());
        let info = Decoder::open(&src, bin).unwrap().info().clone();
        let (ch, total) = cache_for(&src, &dir, bin);
        let p: Prepared = prepare(&src, &dir, ch, total, 0.25, Some(&ff), &no_cancel())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(p.plan.same_format, "{name}: {:?}", p.plan);
        let ext = src.extension().unwrap().to_string_lossy().to_lowercase();
        assert_eq!(p.plan.ext, ext);
        // The same sound codec, the same picture.
        let entries = "stream=codec_type,codec_name,nb_read_packets";
        let (was, now) = (probe(&ff, &src, entries), probe(&ff, &p.file, entries));
        let audio = |s: &str| {
            s.lines()
                .collect::<Vec<_>>()
                .windows(2)
                .find(|w| w[1] == "codec_type=audio")
                .map(|w| w[0].to_string())
        };
        assert_eq!(audio(&now), audio(&was), "{name}: codec");
        // A lossless file keeps its depth (16-bit FLAC was written 24).
        let depth = "stream=sample_fmt,bits_per_raw_sample";
        let fmt = |s: &str| {
            s.lines()
                .find(|l| l.starts_with("sample_fmt="))
                .map(str::to_string)
        };
        if matches!(ext.as_str(), "flac" | "wav" | "aiff") || name.starts_with("alac") {
            let (was, now) = (probe(&ff, &src, depth), probe(&ff, &p.file, depth));
            assert_eq!(fmt(&now), fmt(&was), "{name}: depth");
        }
        if info.video {
            let video = |s: &str| {
                let l: Vec<&str> = s.lines().collect();
                let i = l.iter().position(|x| *x == "codec_type=video").unwrap();
                (l[i - 1].to_string(), l[i + 1].to_string())
            };
            assert_eq!(video(&now), video(&was), "{name}: the picture");
        }
        // On time and as long as the original, against the mix.
        let want = mix_of(&src, bin, &dir, ch, total, 0.25);
        let got = decode(&p.file, bin);
        let native = Decoder::open(&p.file, None).map(|d| d.info().via == Via::Native);
        let extra = got[0].len() as i64 - want[0].len() as i64;
        // Matroska's end padding is not read natively (decode tests).
        let allowed = if ext == "mkv" || ext == "webm" {
            960
        } else {
            0
        };
        assert!(
            (0..=allowed).contains(&extra),
            "{name}: {extra} frames off ({native:?})"
        );
        let (l, c) = lag(&got[0], &want[0]);
        assert!(l.abs() <= 1 && c > 0.98, "{name}: lag {l}, corr {c:.4}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// An Audacity-style 32-bit float WAV saves as float WAV through ffmpeg
/// (a forced integer depth made ffmpeg write nothing), and as it was mixed.
#[test]
fn with_ffmpeg_a_float_wav_saves_as_float() {
    let Some(ff) = Ffmpeg::find().filter(|f| f.ffprobe.is_some()) else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let dir = scratch("float");
    let src = dir.join("audacity_export_f32.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(&src, spec).unwrap();
    for v in &decode(&fixture("master_48k_mono.wav"), None)[0] {
        w.write_sample(*v * 0.7).unwrap();
    }
    w.finalize().unwrap();
    let bin = Some(ff.ffmpeg.as_path());
    let cache = dir.join("cache");
    let (ch, total) = cache_for(&src, &cache, bin);
    let p = prepare(&src, &cache, ch, total, 0.25, Some(&ff), &no_cancel())
        .unwrap_or_else(|e| panic!("float WAV: {e}"));
    assert!(p.plan.same_format);
    let entries = "stream=codec_name,sample_fmt";
    assert_eq!(
        probe(&ff, &p.file, entries),
        "codec_name=pcm_f32le\nsample_fmt=flt\n"
    );
    let want = mix_of(&src, bin, &cache, ch, total, 0.25);
    let got = decode(&p.file, None);
    assert_eq!(got[0].len(), want[0].len());
    let off = got[0]
        .iter()
        .zip(&want[0])
        .filter(|(a, b)| (**a - **b).abs() > 1e-7)
        .count();
    assert_eq!(off, 0, "float in, float out");
    let _ = std::fs::remove_dir_all(dir);
}

/// Every stream is kept: an MP4's timecode track (a data stream), a
/// Matroska file's attachment, and a second sound track nobody cleaned
/// (copied as it is, in its place).
#[test]
fn with_ffmpeg_every_stream_is_kept() {
    let Some(ff) = Ffmpeg::find().filter(|f| f.ffprobe.is_some()) else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let dir = scratch("streams");
    let timecode = dir.join("timecode.mov");
    let attached = dir.join("attached.mkv");
    let font = dir.join("font.ttf");
    std::fs::write(&font, b"not really a font").unwrap();
    let ok = |c: &mut Command| assert!(c.status().unwrap().success());
    ok(Command::new(&ff.ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(fixture("video_h264_aac.mp4"))
        .args(["-c", "copy", "-timecode", "01:00:00:00"])
        .arg(&timecode));
    ok(Command::new(&ff.ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(fixture("video_h264_opus.mkv"))
        .arg("-i")
        .arg(fixture("mp3_44k_stereo.mp3"))
        .args(["-map", "0", "-map", "1:a", "-c", "copy", "-attach"])
        .arg(&font)
        .args(["-metadata:s:t", "mimetype=application/x-truetype-font"])
        .arg(&attached));
    let kinds = "stream=codec_type";
    for src in [&timecode, &attached] {
        let cache = dir.join(format!("c-{}", src.file_name().unwrap().to_string_lossy()));
        let bin = Some(ff.ffmpeg.as_path());
        let (ch, total) = cache_for(src, &cache, bin);
        let p = prepare(src, &cache, ch, total, 0.25, Some(&ff), &no_cancel())
            .unwrap_or_else(|e| panic!("{}: {e}", src.display()));
        let (was, now) = (probe(&ff, src, kinds), probe(&ff, &p.file, kinds));
        let sorted = |s: &str| {
            let mut v: Vec<String> = s.lines().map(str::to_string).collect();
            v.sort();
            v
        };
        assert_eq!(sorted(&now), sorted(&was), "{}", src.display());
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn with_ffmpeg_tags_and_rotation_are_kept() {
    let Some(ff) = Ffmpeg::find().filter(|f| f.ffprobe.is_some()) else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let dir = scratch("tags");
    let tagged = dir.join("tagged.mp3");
    let rotated = dir.join("rotated.mp4");
    let ok = |c: &mut Command| assert!(c.status().unwrap().success());
    ok(Command::new(&ff.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(fixture("mp3_44k_stereo.mp3"))
        .args(["-c", "copy", "-metadata", "title=A talk"])
        .arg(&tagged));
    // `-display_rotation` from ffmpeg 6 on; before it (Ubuntu 22.04's
    // 4.4), the stream's rotate tag, which it turns into the same matrix.
    let rotate = |before_input: &[&str], after_input: &[&str]| {
        Command::new(&ff.ffmpeg)
            .args(["-v", "error", "-y"])
            .args(before_input)
            .arg("-i")
            .arg(fixture("video_h264_aac.mp4"))
            .args(["-c", "copy"])
            .args(after_input)
            .arg(&rotated)
            .status()
            .unwrap()
            .success()
    };
    assert!(
        rotate(&["-display_rotation", "90"], &[]) || rotate(&[], &["-metadata:s:v:0", "rotate=90"])
    );
    // ffprobe 4.4 calls the section side_data.
    let rotation = if probe(&ff, &rotated, "stream_side_data=rotation").contains('=') {
        "stream_side_data=rotation"
    } else {
        "side_data=rotation"
    };
    for (src, entries) in [(&tagged, "format_tags=title"), (&rotated, rotation)] {
        let cache = dir.join(format!("c-{}", src.file_name().unwrap().to_string_lossy()));
        let (ch, total) = cache_for(src, &cache, Some(ff.ffmpeg.as_path()));
        let p = prepare(src, &cache, ch, total, 0.25, Some(&ff), &no_cancel()).unwrap();
        let was = probe(&ff, src, entries);
        assert!(was.contains('='), "{entries}: {was:?}");
        assert_eq!(probe(&ff, &p.file, entries), was, "{entries}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_ffmpeg_that_ends_at_once_says_why() {
    let Some(mut ff) = Ffmpeg::find() else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    // As Ubuntu 22.04's ffmpeg 4.4 did with -ch_layout: an option it
    // doesn't take, so it ends before reading a byte.
    ff.layout_option = "-no_such_option";
    let dir = scratch("ends");
    let src = fixture("flac_44k_stereo.flac");
    let cache = dir.join("c");
    let (ch, total) = cache_for(&src, &cache, Some(ff.ffmpeg.as_path()));
    match prepare(&src, &cache, ch, total, 0.25, Some(&ff), &no_cancel()) {
        Err(SaveError::Ffmpeg(said)) => {
            assert!(!said.contains("Broken pipe"), "{said}");
            assert!(said.to_lowercase().contains("option"), "{said}");
        }
        Err(e) => panic!("ffmpeg's own words, not {e}"),
        Ok(_) => panic!("saved without its option"),
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Base64, for a test-made Vorbis comment.
fn base64(b: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                out.push(A[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn with_ffmpeg_an_opus_file_with_a_cover_saves() {
    let Some(ff) = Ffmpeg::find().filter(|f| f.ffprobe.is_some()) else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let dir = scratch("cover");
    // A cover as downloaded audio carries it: a FLAC picture block in the
    // METADATA_BLOCK_PICTURE comment, which ffmpeg reads as a picture.
    let png = dir.join("cover.png");
    assert!(Command::new(&ff.ffmpeg)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=8x8"
        ])
        .args(["-frames:v", "1"])
        .arg(&png)
        .status()
        .unwrap()
        .success());
    let png = std::fs::read(png).unwrap();
    let mut block = Vec::new();
    for n in [3u32, 9] {
        block.extend(n.to_be_bytes());
    }
    block.extend(b"image/png");
    for n in [0u32, 8, 8, 24, 0, png.len() as u32] {
        block.extend(n.to_be_bytes());
    }
    block.extend(&png);
    let src = dir.join("talk.opus");
    assert!(Command::new(&ff.ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(fixture("opus_48k_mono.opus"))
        .args(["-c", "copy", "-metadata"])
        .arg(format!("METADATA_BLOCK_PICTURE={}", base64(&block)))
        .arg(&src)
        .status()
        .unwrap()
        .success());
    assert!(
        probe(&ff, &src, "stream=codec_type").contains("codec_type=video"),
        "ffmpeg sees the cover"
    );
    let cache = dir.join("c");
    let bin = Some(ff.ffmpeg.as_path());
    let (ch, total) = cache_for(&src, &cache, bin);
    let p = prepare(&src, &cache, ch, total, 0.25, Some(&ff), &no_cancel())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(p.plan.same_format && p.plan.ext == "opus", "{:?}", p.plan);
    let streams = probe(&ff, &p.file, "stream=codec_name");
    assert_eq!(streams.trim(), "codec_name=opus", "the sound alone");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_copy_under_way_is_noted_and_a_dead_processs_copy_is_swept() {
    let dir = scratch("partials");
    let journal = dir.join("partials");
    // A dead process (no pid is this high) left a copy and its note; a
    // live one (this test) has one under way; a note naming someone
    // else's file is never followed.
    let dead = 999_999_999u32;
    let theirs = dir.join(format!(".talk (cleaned).wav.{dead}.partial"));
    let mine = dir.join(format!(
        ".talk (cleaned).wav.{}.partial",
        std::process::id()
    ));
    let other = dir.join("notes.txt");
    for f in [&theirs, &mine, &other] {
        std::fs::write(f, b"x").unwrap();
    }
    std::fs::create_dir_all(&journal).unwrap();
    let note = |name: String, p: &Path| {
        std::fs::write(journal.join(name), p.display().to_string()).unwrap()
    };
    note(format!("{dead}-0"), &theirs);
    note(format!("{dead}-1"), &other);
    // a copy under way holds its note
    let live = journal.join(format!("{}-0", std::process::id()));
    let held = hushmic::files::held::hold(&live).unwrap();
    note(format!("{}-0", std::process::id()), &mine);
    sweep_partials(&journal);
    assert!(!theirs.exists(), "the dead process's copy goes");
    assert!(other.exists(), "not a partial copy of that process");
    assert!(mine.exists(), "a held note's copy stays");
    let notes: Vec<_> = std::fs::read_dir(&journal).unwrap().flatten().collect();
    assert_eq!(notes.len(), 1, "the unheld notes go");
    drop(held);
    // A copy to another disk is noted while under way, and not after.
    let shm = Path::new("/dev/shm").join(format!("hushmic-noted-{}", std::process::id()));
    let src = dir.join("talk.wav");
    std::fs::copy(fixture("master_48k_mono.wav"), &src).unwrap();
    let (ch, total) = cache_for(&src, &dir.join("c"), None);
    let p = prepare(&src, &dir.join("c"), ch, total, 0.25, None, &no_cancel()).unwrap();
    let j2 = dir.join("j2");
    if std::fs::create_dir_all(&shm).is_ok() {
        let out = deliver_noted(&p.file, &src, Some(&shm), None, &no_cancel(), Some(&j2)).unwrap();
        assert!(out.exists());
        assert_eq!(std::fs::read_dir(&j2).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&shm);
    }
    let _ = std::fs::remove_dir_all(dir);
}
