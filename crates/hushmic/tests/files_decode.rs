//! "Clean a file" decoding against the fixtures in tests/fixtures/files/
//! (scripts/make-file-fixtures.sh): every format starts on the master's
//! first sample and is exactly as long, lossless formats decode to the
//! master bit for bit, a cut file is reported as damaged, and a mono
//! recording stored as stereo is recognised.
//!
//! Opus decodes natively with the `opus` feature, else through ffmpeg;
//! the ffmpeg cases skip (and say so) where there is no ffmpeg.

use hushmic::files::decode::{prescan, Container, DecodeError, Decoder, Via};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/files")
        .join(name)
}

fn ffmpeg() -> Option<PathBuf> {
    hushmic::files::decode::which("ffmpeg").filter(|p| p.with_file_name("ffprobe").is_file())
}

/// The master WAV as f32 (i16 / 32768), planar.
fn master(name: &str) -> Vec<Vec<f32>> {
    let mut r = hound::WavReader::open(fixture(name)).unwrap();
    let ch = r.spec().channels as usize;
    let s: Vec<i16> = r.samples::<i16>().map(Result::unwrap).collect();
    (0..ch)
        .map(|c| {
            s.iter()
                .skip(c)
                .step_by(ch)
                .map(|&v| v as f32 / 32768.0)
                .collect()
        })
        .collect()
}

fn decode_all(name: &str) -> (Decoder, Vec<Vec<f32>>) {
    let mut d = Decoder::open(&fixture(name), ffmpeg().as_deref())
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut out = vec![Vec::new(); d.info().channels];
    while let Some(b) = d.next_block().unwrap() {
        for (o, c) in out.iter_mut().zip(b) {
            o.extend_from_slice(&c);
        }
    }
    (d, out)
}

/// The shift (master index − decoded index) that matches best, and how well.
fn best_lag(x: &[f32], m: &[f32]) -> (i64, f64) {
    let (from, len) = (3000usize, 16_000usize);
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
            ab += (*p as f64) * (*q as f64);
            aa += (*p as f64) * (*p as f64);
            bb += (*q as f64) * (*q as f64);
        }
        let c = ab / (aa * bb).sqrt().max(1e-30);
        if c > best.1 {
            best = (lag, c);
        }
    }
    best
}

#[test]
fn lossless_formats_decode_to_the_master_bit_for_bit() {
    let m44 = master("master_44k_stereo.wav");
    for name in [
        "aiff_44k_stereo.aiff",
        "flac_44k_stereo.flac",
        "alac_44k_stereo.m4a",
        "master_44k_stereo.wav",
    ] {
        let (d, x) = decode_all(name);
        assert_eq!(
            (d.info().rate, d.info().channels, d.info().via),
            (44_100, 2, Via::Native),
            "{name}"
        );
        assert_eq!(x[0].len(), 44_100, "{name}");
        for (c, (xc, mc)) in x.iter().zip(&m44).enumerate() {
            let off = xc
                .iter()
                .zip(mc)
                .filter(|(a, b)| ((**a * 32768.0).round() - **b * 32768.0).abs() > 0.5)
                .count();
            assert_eq!(off, 0, "{name}: channel {c} differs in {off} samples");
        }
        assert_eq!(d.damaged(), None, "{name}");
    }
}

#[test]
fn lossy_formats_start_on_the_first_sample_and_last_exactly() {
    let m44 = master("master_44k_stereo.wav");
    let m48 = master("master_48k_mono.wav");
    let cases: &[(&str, u32, usize, bool)] = &[
        ("mp3_44k_stereo.mp3", 44_100, 2, false),
        ("aac_44k_stereo.m4a", 44_100, 2, false),
        ("vorbis_44k_stereo.ogg", 44_100, 2, false),
        ("video_h264_aac.mp4", 48_000, 1, true),
        ("video_h264_aac.mov", 48_000, 1, true),
    ];
    for &(name, rate, ch, video) in cases {
        let (d, x) = decode_all(name);
        let i = d.info();
        assert_eq!(
            (i.rate, i.channels, i.video, i.via),
            (rate, ch, video, Via::Native),
            "{name}"
        );
        let m = if rate == 44_100 { &m44 } else { &m48 };
        assert_eq!(x[0].len(), m[0].len(), "{name}: length");
        let (lag, corr) = best_lag(&x[0], &m[0]);
        assert_eq!(lag, 0, "{name}: starts {lag} frames off (corr {corr:.4})");
        assert!(corr > 0.99, "{name}: corr {corr:.4}");
    }
}

#[test]
fn opus_starts_on_the_first_sample() {
    let m48 = master("master_48k_mono.wav");
    if !cfg!(feature = "opus") && ffmpeg().is_none() {
        eprintln!("SKIP: no opus feature and no ffmpeg");
        return;
    }
    for (name, video, container) in [
        ("opus_48k_mono.opus", false, Container::Ogg),
        ("video_h264_opus.mkv", true, Container::Matroska),
        ("video_vp8_opus.webm", true, Container::Matroska),
    ] {
        let (d, x) = decode_all(name);
        let i = d.info();
        let via = if cfg!(feature = "opus") {
            Via::Native
        } else {
            Via::Ffmpeg
        };
        assert_eq!(
            (i.rate, i.channels, i.video, i.via, i.container),
            (48_000, 1, video, via, container),
            "{name}"
        );
        let (lag, corr) = best_lag(&x[0], &m48[0]);
        assert_eq!(lag, 0, "{name}: starts {lag} frames off");
        assert!(corr > 0.99, "{name}: corr {corr:.4}");
        // Matroska's end padding is not read natively: at most one Opus
        // frame (20 ms) long. Ogg and ffmpeg are exact.
        let extra = x[0].len() as i64 - 48_000;
        let allowed = if container == Container::Matroska && via == Via::Native {
            960
        } else {
            0
        };
        assert!(
            (0..=allowed).contains(&extra),
            "{name}: {extra} frames too long"
        );
    }
}

#[test]
fn ffmpeg_opens_what_symphonia_cannot() {
    let Some(ff) = ffmpeg() else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let path = fixture("wma_48k_mono.wma");
    let (d, x) = decode_all("wma_48k_mono.wma");
    assert_eq!(
        (d.info().via, d.info().rate, d.info().channels),
        (Via::Ffmpeg, 48_000, 1)
    );
    assert!(x[0].len() > 40_000);
    // Without ffmpeg the same file is plainly unsupported.
    assert!(matches!(
        Decoder::open(&path, None),
        Err(DecodeError::Unsupported(_))
    ));
    let _ = ff;
}

#[test]
fn a_video_without_sound_says_so() {
    let path = fixture("video_only.mp4");
    for ff in [None, ffmpeg()] {
        match Decoder::open(&path, ff.as_deref()) {
            Err(DecodeError::NoSound) => {}
            Err(e) => panic!("expected NoSound (ffmpeg {ff:?}), got {e}"),
            Ok(_) => panic!("expected NoSound (ffmpeg {ff:?}), got a decoder"),
        }
    }
}

#[test]
fn a_cut_file_reports_how_much_could_be_read() {
    let mut d = Decoder::open(&fixture("mp3_44k_cut.mp3"), None).unwrap();
    let p = prescan(&mut d).unwrap();
    let at = p.damaged.expect("a cut MP3 is damaged");
    assert_eq!(at, p.frames);
    // 1.15 s of the 3 s it states: well before its end
    assert!((48_000..54_000).contains(&at), "read {at} frames");
}

/// A cut M4A (its index first, as phones and ffmpeg's +faststart write
/// it) is damaged where it stops: its index states 3 s. symphonia won't
/// open it (its sound data runs past the end of the file), so ffmpeg
/// reads it, and exits as if whole: the stated length tells. Without
/// ffmpeg it is a file that can't be read, never a whole one.
#[test]
fn a_cut_m4a_is_damaged_where_it_stops() {
    match Decoder::open(&fixture("aac_44k_cut.m4a"), None) {
        Err(DecodeError::Unsupported(_)) => {}
        Err(e) => panic!("{e}"),
        Ok(mut d) => {
            let p = prescan(&mut d).unwrap();
            assert!(p.damaged.is_some(), "read whole without ffmpeg");
        }
    }
    let Some(ff) = ffmpeg() else {
        eprintln!("SKIP a_cut_m4a_is_damaged_where_it_stops: no ffmpeg");
        return;
    };
    let mut d = Decoder::open(&fixture("aac_44k_cut.m4a"), Some(&ff)).unwrap();
    assert_eq!(d.info().via, Via::Ffmpeg);
    let stated = d.info().frames.expect("its index states its length");
    assert!((131_000..134_000).contains(&stated), "{stated}");
    let p = prescan(&mut d).unwrap();
    let at = p.damaged.expect("a cut M4A is damaged");
    assert_eq!(at, p.frames);
    assert!((44_100..66_150).contains(&at), "read {at} frames");
}

/// An AAC track whose timescale is twice the rate it decodes at is HE-AAC
/// with its SBR signalled in the stream alone (implicit): read without
/// ffmpeg, it says the rate it plays at, which Save writes at. Made from
/// the AAC fixture with its track's timescale doubled.
#[test]
fn implicit_sbr_without_ffmpeg_keeps_the_rate_it_plays_at() {
    let path = doubled_timescale("implicit");
    let opened = Decoder::open(&path, None);
    let _ = std::fs::remove_file(&path);
    let d = opened.unwrap();
    assert_eq!(d.info().rate, 44_100, "the core, as symphonia decodes it");
    assert_eq!(d.info().full_rate, Some(88_200));
}

/// The AAC fixture with its sound track's timescale doubled (in a temp
/// file, which the caller removes).
fn doubled_timescale(name: &str) -> PathBuf {
    let mut b = std::fs::read(fixture("aac_44k_stereo.m4a")).unwrap();
    let at = b.windows(4).position(|w| w == b"mdhd").unwrap() + 4 + 12;
    let scale = u32::from_be_bytes(b[at..at + 4].try_into().unwrap());
    b[at..at + 4].copy_from_slice(&(scale * 2).to_be_bytes());
    let path =
        std::env::temp_dir().join(format!("hushmic-decode-{}-{name}.m4a", std::process::id()));
    std::fs::write(&path, &b).unwrap();
    path
}

/// HE-AAC goes through ffmpeg, which decodes its SBR: the rate it plays
/// at, the whole length (no damage), and a cut one is damaged where it
/// stops. Needs an HE-AAC encoder (ffmpeg's libfdk_aac) to make one;
/// skipped without.
#[test]
fn he_aac_goes_through_ffmpeg_at_its_own_rate() {
    let Some(ff) = ffmpeg() else {
        eprintln!("SKIP he_aac_goes_through_ffmpeg_at_its_own_rate: no ffmpeg");
        return;
    };
    let dir = std::env::temp_dir().join(format!("hushmic-heaac-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let he = dir.join("he.m4a");
    let made = std::process::Command::new(&ff)
        .args(["-nostdin", "-v", "error", "-y", "-stream_loop", "4", "-i"])
        .arg(fixture("master_44k_stereo.wav"))
        .args(["-c:a", "libfdk_aac", "-profile:a", "aac_he", "-b:a", "48k"])
        .args(["-movflags", "+faststart"])
        .arg(&he)
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!(
            "SKIP he_aac_goes_through_ffmpeg_at_its_own_rate: this ffmpeg has no HE-AAC encoder (libfdk_aac)"
        );
        return;
    }
    let mut d = Decoder::open(&he, Some(&ff)).unwrap();
    assert_eq!((d.info().via, d.info().rate), (Via::Ffmpeg, 44_100));
    let p = prescan(&mut d).unwrap();
    assert_eq!(p.damaged, None);
    assert!((p.frames as i64 - 5 * 44_100).abs() < 4_410, "{}", p.frames);
    // without ffmpeg: its core, and the rate it plays at for Save
    let d = Decoder::open(&he, None).unwrap();
    assert_eq!((d.info().rate, d.info().full_rate), (22_050, Some(44_100)));
    // cut to its first 40 %: damaged, through ffmpeg
    let b = std::fs::read(&he).unwrap();
    let cut = dir.join("cut.m4a");
    std::fs::write(&cut, &b[..b.len() * 4 / 10]).unwrap();
    let mut d = Decoder::open(&cut, Some(&ff)).unwrap();
    let p = prescan(&mut d).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let at = p.damaged.expect("a cut HE-AAC file is damaged");
    assert!((44_100..132_300).contains(&at), "damaged at {at}");
}

/// An MP4 whose edit runs past its sound track's end (the whole track's
/// length, from after the priming: some muxers write it so, and HE-AAC's
/// 5,058 frames of priming put it 0.11 s past) is whole: it once read as
/// "Damaged after 0:59" for a 59 s video.
#[test]
fn an_edit_past_the_tracks_end_is_not_damage() {
    let mut d = Decoder::open(&fixture("aac_44k_edit_overrun.m4a"), None).unwrap();
    // the track holds 45,124 frames; the edit starts after 6,000
    assert_eq!(d.info().frames, Some(45_124 - 6_000));
    let p = prescan(&mut d).unwrap();
    assert_eq!((p.frames, p.damaged), (45_124 - 6_000, None));
}

/// A WAV written as a stream (ffmpeg or sox to a pipe, arecord to stdout or
/// killed) has a placeholder data size. That is "to the end of the file",
/// not a length the file falls short of: it was read as damaged at its own
/// end ("Damaged after 56 s" for a 56 s file).
#[test]
fn a_streamed_wav_is_whole() {
    let m48 = master("master_48k_mono.wav");
    let (d, x) = decode_all("wav_48k_streamed.wav");
    assert_eq!(d.info().frames, None, "no stated length");
    assert_eq!(d.damaged(), None);
    assert_eq!(x[0], m48[0]);
    let mut d = Decoder::open(&fixture("wav_48k_streamed.wav"), None).unwrap();
    let p = prescan(&mut d).unwrap();
    assert_eq!((p.frames, p.damaged), (48_000, None));

    // The other writers' placeholders: arecord's 0x80000000, 0x7FFFFFFF,
    // and the 0x7FFFF000 sox's reader takes as unspecified.
    let streamed = std::fs::read(fixture("wav_48k_streamed.wav")).unwrap();
    let at = streamed.windows(4).position(|w| w == b"data").unwrap() + 4;
    for size in [0x8000_0000u32, 0x7FFF_FFFF, 0x7FFF_F000] {
        let mut b = streamed.clone();
        b[at..at + 4].copy_from_slice(&size.to_le_bytes());
        let path = std::env::temp_dir().join(format!(
            "hushmic-decode-{}-{size:x}.wav",
            std::process::id()
        ));
        std::fs::write(&path, &b).unwrap();
        let opened = Decoder::open(&path, None);
        // Gone before any assert can fail (the open file stays readable).
        let _ = std::fs::remove_file(&path);
        let mut d = opened.unwrap();
        assert_eq!(d.info().frames, None, "{size:#x}: no stated length");
        let p = prescan(&mut d).unwrap();
        assert_eq!((p.frames, p.damaged), (48_000, None), "{size:#x}");
    }

    // A WAV whose header states its size still tells when it is cut.
    // (3 s stated, 1.2 s there: damage counts only more than a second
    // before the stated end)
    let mut d = Decoder::open(&fixture("wav_48k_cut.wav"), None).unwrap();
    assert_eq!(d.info().frames, Some(144_000));
    let p = prescan(&mut d).unwrap();
    assert_eq!(p.damaged, Some(p.frames));
    assert!((57_000..58_000).contains(&p.frames), "read {}", p.frames);
}

/// A VBR MP3 without the Xing header: symphonia guesses its length from the
/// first frames' bitrate, and its gapless mode cut every frame past the
/// guess (43,776 of 46,080 frames here). It reads to the end, as ffmpeg does.
#[test]
fn a_vbr_mp3_without_its_header_reads_to_the_end() {
    let m44 = master("master_44k_stereo.wav");
    let (d, x) = decode_all("mp3_44k_vbr_noxing.mp3");
    assert_eq!((d.info().frames, d.damaged()), (None, None));
    // No encoder header, so no delay to trim: the sound comes 1,105 frames
    // in (LAME's 576 + the decoder's 529) and the last frame is whole.
    assert_eq!(x[0].len(), 46_080);
    let (lag, corr) = best_lag(&x[0][1105..], &m44[0]);
    assert_eq!(lag, 0, "corr {corr:.4}");
    assert!(corr > 0.99, "corr {corr:.4}");
}

#[test]
fn prescan_finds_the_length_peaks_and_mono_as_stereo() {
    let mut d = Decoder::open(&fixture("flac_44k_same.flac"), None).unwrap();
    let same = prescan(&mut d).unwrap();
    assert!(same.same_channels);
    assert_eq!(same.frames, 44_100);
    assert_eq!(same.peaks.len(), 20, "one column per 50 ms");
    assert!(same.loudest > 0.01 && same.damaged.is_none());

    let mut d = Decoder::open(&fixture("flac_44k_stereo.flac"), None).unwrap();
    assert!(!prescan(&mut d).unwrap().same_channels);
    let mut d = Decoder::open(&fixture("master_48k_mono.wav"), None).unwrap();
    let mono = prescan(&mut d).unwrap();
    assert!(!mono.same_channels);
    assert_eq!((mono.frames, mono.peaks.len()), (48_000, 20));
}

#[test]
fn seeking_lands_on_the_exact_frame() {
    let mut names = vec![
        "master_44k_stereo.wav",
        "aiff_44k_stereo.aiff",
        "flac_44k_stereo.flac",
        "alac_44k_stereo.m4a",
        "mp3_44k_stereo.mp3",
        "mp3_44k_vbr_noxing.mp3",
        "aac_44k_stereo.m4a",
        "vorbis_44k_stereo.ogg",
        "video_h264_aac.mp4",
        "wav_48k_streamed.wav",
    ];
    if cfg!(feature = "opus") || ffmpeg().is_some() {
        names.extend(["opus_48k_mono.opus", "video_h264_opus.mkv"]);
    }
    if ffmpeg().is_some() {
        names.push("wma_48k_mono.wma");
    }
    for name in names {
        let (first, whole) = decode_all(name);
        let mut d = Decoder::open(&fixture(name), ffmpeg().as_deref()).unwrap();
        // Matroska counts milliseconds: the index from the first read
        // makes its seeks exact (without one, a seek reads from the start).
        if let Some(ix) = first.seek_index() {
            d.set_seek_index(ix);
        }
        for at in [2_000u64, 21_000, 30_517] {
            d.seek(at).unwrap();
            assert_eq!(d.position(), at);
            let mut got = vec![Vec::new(); d.info().channels];
            while got[0].len() < 6_000 {
                let Some(b) = d.next_block().unwrap() else {
                    break;
                };
                for (g, c) in got.iter_mut().zip(b) {
                    g.extend_from_slice(&c);
                }
            }
            let n = got[0].len().min(6_000);
            assert!(n == 6_000, "{name} @ {at}: only {n} frames");
            if name.contains("aac") {
                // AAC fills some bands with noise from a generator the
                // seek resets (PNS): the same sound, other noise. Check the
                // timing, not the samples.
                let w = &whole[0][at as usize..at as usize + n];
                let (lag, corr) = best_lag(
                    &[vec![0.0; 3000], got[0][..n].to_vec(), vec![0.0; 16_000]].concat(),
                    &[vec![0.0; 3000], w.to_vec(), vec![0.0; 16_000]].concat(),
                );
                assert!(
                    lag == 0 && corr > 0.99,
                    "{name} @ {at}: lag {lag}, corr {corr:.4}"
                );
                continue;
            }
            let worst = got
                .iter()
                .zip(&whole)
                .flat_map(|(g, w)| {
                    g[..n]
                        .iter()
                        .zip(&w[at as usize..at as usize + n])
                        .map(|(a, b)| (a - b).abs())
                })
                .fold(0f32, f32::max);
            assert!(
                worst < 1e-3,
                "{name} @ {at}: differs from the straight decode by {worst}"
            );
        }
    }
}

/// A recording with a sound track per source (OBS): the tracks are counted,
/// each one opens by its number (ffmpeg's), and without ffmpeg Save keeps
/// only the first and says how many it drops.
#[test]
fn each_sound_track_opens_by_its_number() {
    let Some(ff) = ffmpeg() else {
        eprintln!("SKIP: no ffmpeg");
        return;
    };
    let dir = std::env::temp_dir().join(format!("hushmic-decode-tracks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let two = dir.join("two.mka");
    let made = std::process::Command::new(&ff)
        .args(["-v", "error", "-y", "-i"])
        .arg(fixture("master_44k_stereo.wav"))
        .arg("-i")
        .arg(fixture("master_48k_mono.wav"))
        .args(["-map", "0:a", "-map", "1:a", "-c:a", "flac"])
        .arg(&two)
        .status()
        .unwrap()
        .success();
    assert!(made);
    let first = Decoder::open(&two, None).unwrap();
    assert_eq!(first.info().tracks, 2);
    assert!(first.info().first_track);
    assert_eq!((first.info().rate, first.info().channels), (44_100, 2));
    let mut second = Decoder::open_track(&two, Some(&ff), 1).unwrap();
    assert_eq!((second.info().rate, second.info().channels), (48_000, 1));
    let mut n = 0;
    while let Some(b) = second.next_block().unwrap() {
        n += b[0].len();
    }
    assert_eq!(n, master("master_48k_mono.wav")[0].len());
    // the second needs ffmpeg
    assert!(matches!(
        Decoder::open_track(&two, None, 1),
        Err(DecodeError::Unsupported(_))
    ));
    use hushmic::files::save::Tracks;
    let plan = hushmic::files::save::plan(&two, first.info(), None, Tracks::default());
    assert_eq!((plan.tracks, plan.dropped), (1, 1));
    let ffm = hushmic::files::ffmpeg::Ffmpeg::at(&ff).unwrap();
    let plan = hushmic::files::save::plan(&two, first.info(), Some(&ffm), Tracks::default());
    assert_eq!((plan.tracks, plan.dropped), (2, 0));
    // one track cleaned, the other copied as it is
    assert_eq!(plan.cleaned_tracks(), Vec::<usize>::new());
    let all = Tracks { main: 0, all: true };
    let plan = hushmic::files::save::plan(&two, first.info(), Some(&ffm), all);
    assert_eq!(plan.cleaned_tracks(), [1]);
    let _ = std::fs::remove_dir_all(dir);
}
