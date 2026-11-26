//! A file's first look, as it is added: does it open, and does its first
//! second decode? A file that won't is marked at once, with what is really
//! wrong, instead of failing when its turn comes (in a queue, many minutes
//! later): a phone's recording cut off before its index was written is
//! incomplete, bytes no recording starts with are not audio or video.
//!
//! The daemon runs it in a worker process (`clean --worker --probe`), as
//! it runs every decode: a file that crashes a decoder ends that process,
//! not the daemon.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::decode::{DecodeError, Decoder};
use super::kinds::{self, Sniffed};
use super::worker::Problem;

/// How much of the file is decoded to see that it really is sound.
const FIRST_SECONDS: u64 = 1;

/// What is wrong with `path` before any cleaning (None: it opens and its
/// first second decodes), and the detail for the log.
pub fn check(path: &Path, ffmpeg: Option<&Path>) -> Option<(Problem, String)> {
    if empty(path) {
        return Some((Problem::Empty, "0 bytes".into()));
    }
    let sniffed = kinds::sniff(path);
    if matches!(sniffed, Sniffed::Text | Sniffed::NotMedia) {
        return Some((
            Problem::NotMedia,
            "not audio or video (its first bytes)".into(),
        ));
    }
    let not_media = surely_not_media(path, sniffed, ffmpeg.is_some());
    let mut d = match Decoder::open(path, ffmpeg) {
        Ok(d) => d,
        Err(e) => return Some(open_problem(path, &e, ffmpeg.is_some())),
    };
    let (rate, channels) = (d.info().rate, d.info().channels);
    // a rate nothing converts from: noise read as sound (a real file at an
    // odd rate is a format of its own)
    if super::resample::Converter::new(rate, 48_000, channels).is_none() {
        let kind = if not_media {
            Problem::NotMedia
        } else {
            Problem::Unsupported
        };
        return Some((kind, format!("no converter from {rate} Hz")));
    }
    let want = u64::from(rate) * FIRST_SECONDS;
    let mut got = 0u64;
    while got < want {
        match d.next_block() {
            Ok(Some(block)) => got += block.first().map_or(0, |c| c.len() as u64),
            Ok(None) => break,
            Err(e) => {
                let kind = if not_media {
                    Problem::NotMedia
                } else {
                    Problem::Unreadable
                };
                return Some((kind, e.to_string()));
            }
        }
    }
    None
}

/// Nothing in it at all: an empty file, not a cut-off recording.
fn empty(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == 0)
}

/// Bytes that are surely no recording: text or a document's signature,
/// or none we know with nothing else to vouch for them. A recording's
/// name without ffmpeg vouches: HushMic knows no signature of some formats
/// only ffmpeg opens (TTA, DTS, Musepack with an odd header), and such a
/// file is a format this install can't open, not junk.
fn surely_not_media(path: &Path, sniffed: Sniffed, ffmpeg: bool) -> bool {
    match sniffed {
        Sniffed::Text | Sniffed::NotMedia => true,
        Sniffed::Media => false,
        Sniffed::Unknown => ffmpeg || kinds::named(path) == kinds::Named::Other,
    }
}

/// Why `path` did not open, in the page's words: an MP4 or MOV without
/// its index (a recording cut off before it was written) is incomplete;
/// bytes that no recording starts with are not audio or video (unless
/// its name says it is one and there was no ffmpeg to try: then it is a
/// format not supported here); else as the decoder says (not supported,
/// no sound, unreadable). `ffmpeg`: it was there to try.
pub fn open_problem(path: &Path, e: &DecodeError, ffmpeg: bool) -> (Problem, String) {
    let detail = e.to_string();
    if empty(path) {
        return (Problem::Empty, "0 bytes".into());
    }
    match e {
        DecodeError::NoSound => return (Problem::NoSound, detail),
        DecodeError::Io(_) => return (Problem::Unreadable, detail),
        _ => {}
    }
    if mp4_without_index(path) {
        return (Problem::Incomplete, detail);
    }
    let sniffed = kinds::sniff(path);
    if surely_not_media(path, sniffed, ffmpeg) {
        return (Problem::NotMedia, detail);
    }
    match sniffed {
        // a recording's name, no ffmpeg to open it
        Sniffed::Unknown => (Problem::Unsupported, detail),
        _ => match e {
            DecodeError::Unsupported(_) => (Problem::Unsupported, detail),
            _ => (Problem::Unreadable, detail),
        },
    }
}

/// An MP4 (or MOV, M4A) whose top-level boxes run out before its `moov`
/// (the index a phone writes last): it was cut off.
pub fn mp4_without_index(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let Ok(len) = f.metadata().map(|m| m.len()) else {
        return false;
    };
    let mut at = 0u64;
    let mut first = true;
    while at + 8 <= len {
        let mut h = [0u8; 16];
        if f.seek(SeekFrom::Start(at)).is_err() || f.read_exact(&mut h[..8]).is_err() {
            return false;
        }
        let kind = &h[4..8];
        if first
            && !matches!(
                kind,
                b"ftyp" | b"wide" | b"free" | b"mdat" | b"moov" | b"skip"
            )
        {
            return false; // not an MP4
        }
        first = false;
        if kind == b"moov" {
            return false;
        }
        let size = match u32::from_be_bytes([h[0], h[1], h[2], h[3]]) {
            // to the end of the file
            0 => len - at,
            1 => {
                if f.read_exact(&mut h[8..16]).is_err() {
                    return true;
                }
                u64::from_be_bytes(h[8..16].try_into().unwrap_or_default())
            }
            s => u64::from(s),
        };
        if size < 8 {
            return false; // not boxes after all
        }
        at += size;
    }
    // the boxes ran out (or past the end) with no index
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxes(parts: &[(&[u8; 4], usize)]) -> Vec<u8> {
        let mut v = Vec::new();
        for (kind, len) in parts {
            v.extend_from_slice(&((*len as u32) + 8).to_be_bytes());
            v.extend_from_slice(*kind);
            v.extend(std::iter::repeat_n(0u8, *len));
        }
        v
    }

    #[test]
    fn an_mp4_cut_before_its_index_is_incomplete() {
        let dir = std::env::temp_dir().join(format!("hushmic-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("cut.m4a");
        // ftyp, then an mdat that says it is longer than what is there
        let mut cut = boxes(&[(b"ftyp", 16)]);
        cut.extend_from_slice(&100_000u32.to_be_bytes());
        cut.extend_from_slice(b"mdat");
        cut.extend(std::iter::repeat_n(7u8, 4000));
        std::fs::write(&p, &cut).unwrap();
        assert!(mp4_without_index(&p));
        // whole: ftyp, mdat, moov
        std::fs::write(&p, boxes(&[(b"ftyp", 16), (b"mdat", 64), (b"moov", 32)])).unwrap();
        assert!(!mp4_without_index(&p));
        // the index first (a "fast start" file) and the rest cut: not this
        std::fs::write(&p, boxes(&[(b"ftyp", 16), (b"moov", 32)])).unwrap();
        assert!(!mp4_without_index(&p));
        // not an MP4 at all
        std::fs::write(&p, b"RIFF\0\0\0\0WAVEfmt ").unwrap();
        assert!(!mp4_without_index(&p));
        let e = DecodeError::Unsupported("moov atom not found".into());
        std::fs::write(&p, &cut).unwrap();
        assert_eq!(open_problem(&p, &e, true).0, Problem::Incomplete);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn text_and_noise_are_not_audio_or_video() {
        let dir = std::env::temp_dir().join(format!("hushmic-probe2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let txt = dir.join("notes.mp3");
        std::fs::write(&txt, "Interview notes\nAsk about the budget.\n").unwrap();
        assert_eq!(check(&txt, None).map(|p| p.0), Some(Problem::NotMedia));
        // bytes nothing opens
        let junk = dir.join("junk.mp3");
        let bytes: Vec<u8> = (0..20_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        std::fs::write(&junk, &bytes).unwrap();
        // with ffmpeg there to try, it is not one
        if let Some(ff) = crate::files::decode::which("ffmpeg") {
            let got = check(&junk, Some(&ff)).map(|p| p.0);
            assert_eq!(
                got,
                Some(Problem::NotMedia),
                "{:?}",
                check(&junk, Some(&ff))
            );
        }
        // a name that is no recording's: not one, ffmpeg or not
        let bin = dir.join("junk.bin");
        std::fs::write(&bin, &bytes).unwrap();
        assert_eq!(check(&bin, None).map(|p| p.0), Some(Problem::NotMedia));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// SF-B9a: a recording's name and bytes HushMic has no signature for,
    /// with no ffmpeg to open them (a TTA or DTS file on an install
    /// without ffmpeg): a format not supported here, not "isn't an audio
    /// or video file".
    #[test]
    fn without_ffmpeg_a_recordings_name_is_believed() {
        let dir = std::env::temp_dir().join(format!("hushmic-probe3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bytes: Vec<u8> = (0..20_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        for name in ["take.mp3", "take.ac3"] {
            let p = dir.join(name);
            std::fs::write(&p, &bytes).unwrap();
            let got = check(&p, None).map(|p| p.0);
            assert!(
                matches!(got, Some(Problem::Unsupported | Problem::Unreadable)),
                "{name}: {got:?}"
            );
        }
        let e = DecodeError::Unsupported("no demuxer".into());
        let p = dir.join("take.ac3");
        assert_eq!(open_problem(&p, &e, false).0, Problem::Unsupported);
        assert_eq!(open_problem(&p, &e, true).0, Problem::NotMedia);
        assert_eq!(
            open_problem(&dir.join("take.bin"), &e, false).0,
            Problem::NotMedia
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
