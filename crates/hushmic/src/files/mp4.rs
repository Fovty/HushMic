//! The edit list of an MP4/MOV sound track: where its sound really starts
//! and how long it plays. AAC encoders put about 1,024 samples of priming
//! in front and pad the last frame; the edit list says what to drop.
//! symphonia's isomp4 reader parses the list but never applies it, so the
//! decoded sound would start 21 ms late (measured) and run long.

use std::io::{Read, Seek, SeekFrom};

/// What the first sound track's edit list asks for, in sample frames at
/// the track's own rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edit {
    /// Silence to play before the sound (an empty edit: the sound starts
    /// later than the picture).
    pub lead: u64,
    /// Decoded frames to drop at the start (the encoder's priming).
    pub skip: u64,
    /// Frames to play after the skip; None = to the end.
    pub play: Option<u64>,
}

/// The edit of the first sound track (`hdlr` = `soun`), converted to frames
/// at `rate`. None when the file has no edit list for it (play as decoded)
/// or is not an MP4 at all.
pub fn sound_edit<R: Read + Seek>(r: &mut R, rate: u32) -> Option<Edit> {
    sound_track(r)?.edit(rate)
}

/// The length the file states for its first sound track, in frames at
/// `rate`: where its edit ends (no further than the track reaches), else
/// the track's own length. None when it states none (a fragmented file).
pub fn stated_frames<R: Read + Seek>(r: &mut R, rate: u32) -> Option<u64> {
    stated_frames_of(r, rate, 0)
}

/// [`stated_frames`] for the file's `nth` sound track (0: the first).
pub fn stated_frames_of<R: Read + Seek>(r: &mut R, rate: u32, nth: usize) -> Option<u64> {
    let t = sound_track_nth(r, nth)?;
    match t.edit(rate) {
        Some(e) => e.play.map(|p| e.lead + p),
        None => (t.length > 0).then(|| scale_to(t.length, t.scale, rate)),
    }
}

/// The first sound track's own rate (its timescale): for AAC, the rate it
/// plays at, SBR's included.
pub fn sound_scale<R: Read + Seek>(r: &mut R) -> Option<u32> {
    sound_track(r).map(|t| t.scale).filter(|&s| s > 0)
}

/// The AAC audio object type of the first sound track (its `esds`): 2 is
/// AAC-LC, 5 HE-AAC (SBR), 29 HE-AACv2 (SBR and PS).
pub fn aac_object_type<R: Read + Seek>(r: &mut R) -> Option<u8> {
    sound_track(r)?.aac_object
}

/// What the first sound track says about itself.
struct SoundTrack {
    /// Its timescale (`mdhd`) and its length in it: what its samples
    /// hold (`stts`), else what `mdhd` states (0: neither, as in a
    /// fragmented file).
    scale: u32,
    length: u64,
    movie_scale: u32,
    /// Its edit list, if it has one.
    entries: Option<Vec<Entry>>,
    aac_object: Option<u8>,
}

impl SoundTrack {
    fn edit(&self, rate: u32) -> Option<Edit> {
        let entries = self.entries.as_ref()?;
        let edit = to_frames(entries, self.movie_scale, self.scale, rate);
        Some(within(edit, self.length, self.scale, rate))
    }
}

fn scale_to(v: u64, from: u32, rate: u32) -> u64 {
    if from == 0 {
        return 0;
    }
    ((v as u128 * rate as u128 + from as u128 / 2) / from as u128) as u64
}

fn sound_track<R: Read + Seek>(r: &mut R) -> Option<SoundTrack> {
    sound_track_nth(r, 0)
}

/// The file's `nth` sound track (`hdlr` = `soun`, in the order of its
/// `trak` boxes, as ffmpeg numbers its sound streams).
fn sound_track_nth<R: Read + Seek>(r: &mut R, nth: usize) -> Option<SoundTrack> {
    let mut seen = 0;
    let len = r.seek(SeekFrom::End(0)).ok()?;
    let moov = find_child(r, 0, len, b"moov")?;
    let movie_scale = read_mvhd_scale(r, moov)?;
    let mut at = moov.0;
    while let Some((kind, body, end)) = next_box(r, at, moov.1) {
        at = end;
        if &kind != b"trak" {
            continue;
        }
        let Some(mdia) = find_child(r, body, end, b"mdia") else {
            continue;
        };
        if !is_sound(r, mdia) {
            continue;
        }
        seen += 1;
        if seen <= nth {
            continue;
        }
        let (scale, stated) = read_mdhd(r, mdia)?;
        // ffmpeg 4's mdhd leaves out the priming (one AAC frame short of
        // the samples), later ones count it: the samples are the reach.
        let length = match samples_length(r, mdia) {
            Some(n) if n > 0 => n,
            _ => stated,
        };
        let entries = find_child(r, body, end, b"edts")
            .and_then(|edts| find_child(r, edts.0, edts.1, b"elst"))
            .and_then(|elst| read_elst(r, elst));
        let aac_object = esds_object_type(r, mdia);
        return Some(SoundTrack {
            scale,
            length,
            movie_scale,
            entries,
            aac_object,
        });
    }
    None
}

/// The audio object type in the sound entry's `esds` (in `mdia`: minf,
/// stbl, stsd, mp4a, esds, then its ES, DecoderConfig and
/// DecoderSpecificInfo descriptors): the AudioSpecificConfig's first 5
/// bits, or 6 more past 31.
fn esds_object_type<R: Read + Seek>(r: &mut R, mdia: (u64, u64)) -> Option<u8> {
    let minf = find_child(r, mdia.0, mdia.1, b"minf")?;
    let stbl = find_child(r, minf.0, minf.1, b"stbl")?;
    let stsd = find_child(r, stbl.0, stbl.1, b"stsd")?;
    // version/flags and the entry count, then the first entry
    let (kind, body, end) = next_box(r, stsd.0 + 8, stsd.1)?;
    if &kind != b"mp4a" {
        return None;
    }
    // the sound sample entry: 28 bytes, 16 or 36 more in QuickTime's
    // versions 1 and 2
    r.seek(SeekFrom::Start(body + 8)).ok()?;
    let mut v = [0u8; 2];
    r.read_exact(&mut v).ok()?;
    let extra = match u16::from_be_bytes(v) {
        1 => 16,
        2 => 36,
        _ => 0,
    };
    let esds = find_child(r, body + 28 + extra, end, b"esds")?;
    let n = (esds.1 - esds.0).min(4096) as usize;
    r.seek(SeekFrom::Start(esds.0)).ok()?;
    let mut b = vec![0u8; n];
    r.read_exact(&mut b).ok()?;
    asc_object_type(b.get(4..)?)
}

/// The object type in an `esds` box's descriptors (after its version and
/// flags).
fn asc_object_type(mut d: &[u8]) -> Option<u8> {
    // one descriptor: its tag, its size (7 bits a byte, up to 4 bytes),
    // its body; the rest after it
    fn take(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        let (&tag, mut rest) = d.split_first()?;
        let mut size = 0usize;
        for _ in 0..4 {
            let (&b, r) = rest.split_first()?;
            rest = r;
            size = (size << 7) | (b & 0x7F) as usize;
            if b & 0x80 == 0 {
                break;
            }
        }
        (size <= rest.len()).then(|| (tag, &rest[..size], &rest[size..]))
    }
    let (tag, es, _) = take(d)?;
    if tag != 0x03 || es.len() < 3 {
        return None;
    }
    let flags = es[2];
    let mut skip = 3;
    if flags & 0x80 != 0 {
        skip += 2;
    }
    if flags & 0x40 != 0 {
        skip += 1 + *es.get(skip)? as usize;
    }
    if flags & 0x20 != 0 {
        skip += 2;
    }
    d = es.get(skip..)?;
    while let Some((tag, body, rest)) = take(d) {
        d = rest;
        if tag != 0x04 {
            continue;
        }
        let mut inner = body.get(13..)?;
        while let Some((tag, asc, rest)) = take(inner) {
            inner = rest;
            if tag != 0x05 {
                continue;
            }
            let first = asc.first()? >> 3;
            return Some(if first == 31 {
                let b0 = asc[0] & 0x07;
                let b1 = asc.get(1)? >> 5;
                32 + ((b0 << 3) | b1)
            } else {
                first
            });
        }
    }
    None
}

/// One `elst` entry: duration in the movie's timescale, media time in the
/// track's (-1 = an empty edit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub duration: u64,
    pub media_time: i64,
}

/// Leading empty edits become lead silence; the first real edit gives the
/// skip and the length. Further edits (a cut inside the track) are rare in
/// recordings and are ignored: the sound plays on to the end.
pub fn to_frames(entries: &[Entry], movie_scale: u32, media_scale: u32, rate: u32) -> Edit {
    let scale = |v: u64, from: u32| scale_to(v, from, rate);
    let mut lead = 0;
    for e in entries {
        if e.media_time < 0 {
            lead += scale(e.duration, movie_scale);
            continue;
        }
        let skip = scale(e.media_time as u64, media_scale);
        // A zero duration means "the rest of the track" (fragmented files).
        let play = (e.duration > 0).then(|| scale(e.duration, movie_scale));
        return Edit { lead, skip, play };
    }
    Edit {
        lead,
        skip: 0,
        play: None,
    }
}

/// An edit plays no further than the track's sound reaches: some muxers
/// state the whole track's length as the edit's and start it after the
/// priming, so the edit runs past the last sample by the priming (0.11 s
/// for HE-AAC's 5,058). That end is not sound the file is missing.
/// `media_length` is the track's own length in `media_scale` units (0:
/// not stated, as in fragmented files).
pub fn within(mut edit: Edit, media_length: u64, media_scale: u32, rate: u32) -> Edit {
    if media_length == 0 || media_scale == 0 {
        return edit;
    }
    let reach = scale_to(media_length, media_scale, rate);
    let room = reach.saturating_sub(edit.skip);
    edit.play = edit.play.map(|p| p.min(room));
    edit
}

/// The next box between `at` and `end`: (type, body start, box end).
fn next_box<R: Read + Seek>(r: &mut R, at: u64, end: u64) -> Option<([u8; 4], u64, u64)> {
    if at + 8 > end {
        return None;
    }
    r.seek(SeekFrom::Start(at)).ok()?;
    let mut h = [0u8; 8];
    r.read_exact(&mut h).ok()?;
    let size32 = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
    let kind = [h[4], h[5], h[6], h[7]];
    let (size, header) = match size32 {
        0 => (end - at, 8),
        1 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b).ok()?;
            (u64::from_be_bytes(b), 16)
        }
        n => (n, 8),
    };
    if size < header || at.checked_add(size)? > end {
        return None;
    }
    Some((kind, at + header, at + size))
}

/// The body range of the first box of `kind` directly inside `[start, end)`.
fn find_child<R: Read + Seek>(
    r: &mut R,
    start: u64,
    end: u64,
    kind: &[u8; 4],
) -> Option<(u64, u64)> {
    let mut at = start;
    while let Some((k, body, box_end)) = next_box(r, at, end) {
        if &k == kind {
            return Some((body, box_end));
        }
        at = box_end;
    }
    None
}

/// `hdlr` inside `mdia` says `soun` for a sound track.
fn is_sound<R: Read + Seek>(r: &mut R, mdia: (u64, u64)) -> bool {
    let Some((body, end)) = find_child(r, mdia.0, mdia.1, b"hdlr") else {
        return false;
    };
    if body + 12 > end || r.seek(SeekFrom::Start(body + 8)).is_err() {
        return false;
    }
    let mut t = [0u8; 4];
    r.read_exact(&mut t).is_ok() && &t == b"soun"
}

/// Whether the file has a picture track (`hdlr` = `vide`).
pub fn has_video<R: Read + Seek>(r: &mut R) -> bool {
    has_handler(r, b"vide")
}

/// Whether the file has a sound track (`hdlr` = `soun`).
pub fn has_sound<R: Read + Seek>(r: &mut R) -> bool {
    has_handler(r, b"soun")
}

fn has_handler<R: Read + Seek>(r: &mut R, handler: &[u8; 4]) -> bool {
    let Ok(len) = r.seek(SeekFrom::End(0)) else {
        return false;
    };
    let Some(moov) = find_child(r, 0, len, b"moov") else {
        return false;
    };
    let mut at = moov.0;
    while let Some((kind, body, end)) = next_box(r, at, moov.1) {
        at = end;
        if &kind != b"trak" {
            continue;
        }
        let Some(mdia) = find_child(r, body, end, b"mdia") else {
            continue;
        };
        let Some((hb, he)) = find_child(r, mdia.0, mdia.1, b"hdlr") else {
            continue;
        };
        if hb + 12 <= he && r.seek(SeekFrom::Start(hb + 8)).is_ok() {
            let mut t = [0u8; 4];
            if r.read_exact(&mut t).is_ok() && &t == handler {
                return true;
            }
        }
    }
    false
}

/// Timescale of a full box whose version-dependent layout puts it after
/// creation and modification times (`mvhd`, `mdhd`).
fn read_scale<R: Read + Seek>(r: &mut R, body: u64) -> Option<u32> {
    r.seek(SeekFrom::Start(body)).ok()?;
    let mut vf = [0u8; 4];
    r.read_exact(&mut vf).ok()?;
    let skip = if vf[0] == 1 { 16 } else { 8 };
    r.seek(SeekFrom::Current(skip)).ok()?;
    let mut s = [0u8; 4];
    r.read_exact(&mut s).ok()?;
    Some(u32::from_be_bytes(s))
}

fn read_mvhd_scale<R: Read + Seek>(r: &mut R, moov: (u64, u64)) -> Option<u32> {
    let (body, _) = find_child(r, moov.0, moov.1, b"mvhd")?;
    read_scale(r, body)
}

/// The track's timescale and its length in it (`mdhd`).
fn read_mdhd<R: Read + Seek>(r: &mut R, mdia: (u64, u64)) -> Option<(u32, u64)> {
    let (body, _) = find_child(r, mdia.0, mdia.1, b"mdhd")?;
    let scale = read_scale(r, body)?;
    // the duration follows the timescale: 8 bytes in version 1, else 4
    r.seek(SeekFrom::Start(body)).ok()?;
    let mut vf = [0u8; 4];
    r.read_exact(&mut vf).ok()?;
    let length = if vf[0] == 1 {
        r.seek(SeekFrom::Current(20)).ok()?;
        let mut b = [0u8; 8];
        r.read_exact(&mut b).ok()?;
        u64::from_be_bytes(b)
    } else {
        r.seek(SeekFrom::Current(12)).ok()?;
        let mut b = [0u8; 4];
        r.read_exact(&mut b).ok()?;
        u64::from(u32::from_be_bytes(b))
    };
    Some((scale, length))
}

/// The sum of the sound's sample durations (`stts`, in the track's
/// timescale): how far its samples reach.
fn samples_length<R: Read + Seek>(r: &mut R, mdia: (u64, u64)) -> Option<u64> {
    let minf = find_child(r, mdia.0, mdia.1, b"minf")?;
    let stbl = find_child(r, minf.0, minf.1, b"stbl")?;
    let (body, end) = find_child(r, stbl.0, stbl.1, b"stts")?;
    r.seek(SeekFrom::Start(body)).ok()?;
    let mut head = [0u8; 8];
    r.read_exact(&mut head).ok()?;
    let count = u64::from(u32::from_be_bytes(head[4..8].try_into().ok()?));
    // the entries must fit in the box
    if body + 8 + count * 8 > end {
        return None;
    }
    // read in pieces: a long table (one entry a sample) stays small
    let mut sum = 0u64;
    let mut left = count as usize;
    let mut piece = vec![0u8; 8 * 4096];
    while left > 0 {
        let n = left.min(4096);
        r.read_exact(&mut piece[..n * 8]).ok()?;
        for at in (0..n * 8).step_by(8) {
            let e = &piece[at..at + 8];
            let k = u32::from_be_bytes([e[0], e[1], e[2], e[3]]);
            let d = u32::from_be_bytes([e[4], e[5], e[6], e[7]]);
            sum = sum.saturating_add(u64::from(k) * u64::from(d));
        }
        left -= n;
    }
    Some(sum)
}

/// Edit lists are short; more entries than this is a broken file.
const MAX_ENTRIES: u32 = 1024;

fn read_elst<R: Read + Seek>(r: &mut R, elst: (u64, u64)) -> Option<Vec<Entry>> {
    r.seek(SeekFrom::Start(elst.0)).ok()?;
    let mut vf = [0u8; 4];
    r.read_exact(&mut vf).ok()?;
    let mut n = [0u8; 4];
    r.read_exact(&mut n).ok()?;
    let count = u32::from_be_bytes(n);
    if count > MAX_ENTRIES {
        return None;
    }
    let v1 = vf[0] == 1;
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (duration, media_time) = if v1 {
            let mut b = [0u8; 16];
            r.read_exact(&mut b).ok()?;
            (
                u64::from_be_bytes(b[0..8].try_into().ok()?),
                i64::from_be_bytes(b[8..16].try_into().ok()?),
            )
        } else {
            let mut b = [0u8; 8];
            r.read_exact(&mut b).ok()?;
            (
                u32::from_be_bytes(b[0..4].try_into().ok()?) as u64,
                i32::from_be_bytes(b[4..8].try_into().ok()?) as i64,
            )
        };
        // media rate (16.16): skipped, recordings play at 1x.
        r.seek(SeekFrom::Current(4)).ok()?;
        if r.stream_position().ok()? > elst.1 {
            return None;
        }
        out.push(Entry {
            duration,
            media_time,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    fn full(version: u8, rest: &[u8]) -> Vec<u8> {
        let mut v = vec![version, 0, 0, 0];
        v.extend_from_slice(rest);
        v
    }

    fn scale_box(kind: &[u8; 4], scale: u32) -> Vec<u8> {
        // version 0: creation, modification, timescale, duration.
        let mut rest = vec![0u8; 8];
        rest.extend_from_slice(&scale.to_be_bytes());
        rest.extend_from_slice(&[0u8; 4]);
        bx(kind, &full(0, &rest))
    }

    fn hdlr(kind: &[u8; 4]) -> Vec<u8> {
        let mut rest = vec![0u8; 4];
        rest.extend_from_slice(kind);
        rest.extend_from_slice(&[0u8; 13]);
        bx(b"hdlr", &full(0, &rest))
    }

    fn elst(entries: &[(u32, i32)]) -> Vec<u8> {
        let mut rest = (entries.len() as u32).to_be_bytes().to_vec();
        for (d, m) in entries {
            rest.extend_from_slice(&d.to_be_bytes());
            rest.extend_from_slice(&m.to_be_bytes());
            rest.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        }
        bx(b"elst", &full(0, &rest))
    }

    fn trak(handler: &[u8; 4], media_scale: u32, edits: Option<&[(u32, i32)]>) -> Vec<u8> {
        let mut mdia = scale_box(b"mdhd", media_scale);
        mdia.extend(hdlr(handler));
        let mut t = Vec::new();
        if let Some(e) = edits {
            t.extend(bx(b"edts", &elst(e)));
        }
        t.extend(bx(b"mdia", &mdia));
        bx(b"trak", &t)
    }

    fn file(traks: &[Vec<u8>]) -> Vec<u8> {
        let mut moov = scale_box(b"mvhd", 1000);
        for t in traks {
            moov.extend_from_slice(t);
        }
        let mut f = bx(b"ftyp", b"isom\0\0\0\0");
        f.extend(bx(b"moov", &moov));
        f
    }

    #[test]
    fn aac_priming_and_length_come_from_the_sound_track() {
        // A picture track first, then the sound: 1 s at 1 kHz movie scale,
        // 1,024 frames of priming at 44.1 kHz.
        let f = file(&[
            trak(b"vide", 90_000, Some(&[(1000, 0)])),
            trak(b"soun", 44_100, Some(&[(1000, 1024)])),
        ]);
        let e = sound_edit(&mut Cursor::new(&f), 44_100).unwrap();
        assert_eq!(
            e,
            Edit {
                lead: 0,
                skip: 1024,
                play: Some(44_100)
            }
        );
        assert!(has_video(&mut Cursor::new(&f)));
        assert!(has_sound(&mut Cursor::new(&f)));
        let silent = file(&[trak(b"vide", 90_000, None)]);
        assert!(!has_sound(&mut Cursor::new(&silent)));
    }

    /// The muxer's mistake behind "Damaged after 0:59" on a 59 s video:
    /// the edit states the whole track's length and starts after HE-AAC's
    /// 5,058 frames of priming, so it ends 0.11 s past the last sample.
    #[test]
    fn an_edit_plays_no_further_than_the_track_reaches() {
        let mut mdhd = vec![0u8; 8];
        mdhd.extend_from_slice(&44_100u32.to_be_bytes());
        mdhd.extend_from_slice(&2_639_810u32.to_be_bytes());
        let mut mdia = bx(b"mdhd", &full(0, &mdhd));
        mdia.extend(hdlr(b"soun"));
        let mut t = bx(b"edts", &elst(&[(59_860, 5058)]));
        t.extend(bx(b"mdia", &mdia));
        let f = file(&[bx(b"trak", &t)]);
        let e = sound_edit(&mut Cursor::new(&f), 44_100).unwrap();
        assert_eq!(e.skip, 5058);
        assert_eq!(e.play, Some(2_639_810 - 5058), "to the track's end");
        // an edit inside the track keeps its length
        let inside = within(
            Edit {
                lead: 0,
                skip: 1024,
                play: Some(44_100),
            },
            46_080,
            44_100,
            44_100,
        );
        assert_eq!(inside.play, Some(44_100));
        // no stated track length (fragmented): as the edit says
        let open = within(inside, 0, 44_100, 44_100);
        assert_eq!(open.play, Some(44_100));
    }

    /// An AAC sound entry's `esds`, with the AudioSpecificConfig `asc`.
    fn mp4a(asc: &[u8]) -> Vec<u8> {
        let mut dsi = vec![0x05, asc.len() as u8];
        dsi.extend_from_slice(asc);
        // DecoderConfig: object 0x40 (audio), stream type, buffer, rates
        let mut dc = vec![0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        dc.extend(dsi);
        let mut es = vec![0, 1, 0];
        es.push(0x04);
        // a size in the 4-byte form, as many muxers write it
        es.extend_from_slice(&[0x80, 0x80, 0x80, dc.len() as u8]);
        es.extend(dc);
        let mut body = vec![0x03, es.len() as u8];
        body.extend(es);
        let esds = bx(b"esds", &full(0, &body));
        let mut entry = vec![0u8; 28];
        entry.extend(esds);
        let mp4a = bx(b"mp4a", &entry);
        let mut stsd = 1u32.to_be_bytes().to_vec();
        stsd.extend(mp4a);
        let stbl = bx(b"stbl", &bx(b"stsd", &full(0, &stsd)));
        bx(b"minf", &stbl)
    }

    /// A sound track with its timescale and length (`mdhd`), and what
    /// `extra` adds to its `mdia`.
    fn sound(scale: u32, length: u32, extra: Vec<u8>, edits: Option<&[(u32, i32)]>) -> Vec<u8> {
        let mut mdhd = vec![0u8; 8];
        mdhd.extend_from_slice(&scale.to_be_bytes());
        mdhd.extend_from_slice(&length.to_be_bytes());
        let mut mdia = bx(b"mdhd", &full(0, &mdhd));
        mdia.extend(hdlr(b"soun"));
        mdia.extend(extra);
        let mut t = Vec::new();
        if let Some(e) = edits {
            t.extend(bx(b"edts", &elst(e)));
        }
        t.extend(bx(b"mdia", &mdia));
        bx(b"trak", &t)
    }

    #[test]
    fn the_aac_object_type_comes_from_the_esds() {
        // AudioSpecificConfig: 5 bits of object type first
        for (asc, want) in [
            (vec![0x12, 0x10], 2),       // AAC-LC, 44.1 kHz
            (vec![0x2B, 0x92, 0x08], 5), // SBR
            (vec![0xEB, 0x09, 0x88], 29),
            (vec![0xF8, 0x00], 31 + 1), // escaped: 32 + the next 6 bits
        ] {
            let f = file(&[sound(44_100, 44_100, mp4a(&asc), None)]);
            assert_eq!(
                aac_object_type(&mut Cursor::new(&f)),
                Some(want),
                "{asc:x?}"
            );
            assert_eq!(sound_scale(&mut Cursor::new(&f)), Some(44_100));
        }
        let none = file(&[sound(44_100, 44_100, Vec::new(), None)]);
        assert_eq!(aac_object_type(&mut Cursor::new(&none)), None);
    }

    /// A track with no edit list states its own length; with one, where
    /// the edit ends (no further than the track reaches).
    #[test]
    fn the_stated_length_is_the_edit_or_the_track() {
        let plain = file(&[sound(44_100, 132_300, Vec::new(), None)]);
        assert_eq!(
            stated_frames(&mut Cursor::new(&plain), 44_100),
            Some(132_300)
        );
        assert_eq!(
            stated_frames(&mut Cursor::new(&plain), 22_050),
            Some(66_150)
        );
        let edited = file(&[sound(44_100, 132_300, Vec::new(), Some(&[(3000, 2112)]))]);
        assert_eq!(
            stated_frames(&mut Cursor::new(&edited), 44_100),
            Some(132_300 - 2112)
        );
        let fragmented = file(&[sound(44_100, 0, Vec::new(), None)]);
        assert_eq!(stated_frames(&mut Cursor::new(&fragmented), 44_100), None);
    }

    /// ffmpeg 4.4 writes `mdhd` without the priming: 44,100 for a second
    /// of AAC whose samples (`stts`) hold 45,124 with 1,024 of priming.
    /// The edit plays its whole second, not one frame less.
    #[test]
    fn the_samples_reach_further_than_an_mdhd_without_the_priming() {
        let mut stts = 2u32.to_be_bytes().to_vec();
        for (n, d) in [(44u32, 1024u32), (1, 68)] {
            stts.extend_from_slice(&n.to_be_bytes());
            stts.extend_from_slice(&d.to_be_bytes());
        }
        let minf = bx(b"minf", &bx(b"stbl", &bx(b"stts", &full(0, &stts))));
        let f = file(&[sound(44_100, 44_100, minf, Some(&[(1000, 1024)]))]);
        let e = sound_edit(&mut Cursor::new(&f), 44_100).unwrap();
        assert_eq!((e.skip, e.play), (1024, Some(44_100)));
        assert_eq!(stated_frames(&mut Cursor::new(&f), 44_100), Some(44_100));
        // with no edit, the samples' length is the stated one
        let mut stts = 1u32.to_be_bytes().to_vec();
        stts.extend_from_slice(&45u32.to_be_bytes());
        stts.extend_from_slice(&1024u32.to_be_bytes());
        let minf = bx(b"minf", &bx(b"stbl", &bx(b"stts", &full(0, &stts))));
        let f = file(&[sound(44_100, 44_100, minf, None)]);
        assert_eq!(stated_frames(&mut Cursor::new(&f), 44_100), Some(46_080));
        // a table that runs past its box is not read: mdhd stands
        let mut stts = 1000u32.to_be_bytes().to_vec();
        stts.extend_from_slice(&[0u8; 8]);
        let minf = bx(b"minf", &bx(b"stbl", &bx(b"stts", &full(0, &stts))));
        let f = file(&[sound(44_100, 44_100, minf, None)]);
        assert_eq!(stated_frames(&mut Cursor::new(&f), 44_100), Some(44_100));
    }

    #[test]
    fn an_empty_edit_becomes_lead_silence() {
        let f = file(&[trak(b"soun", 48_000, Some(&[(250, -1), (2000, 2112)]))]);
        let e = sound_edit(&mut Cursor::new(&f), 48_000).unwrap();
        assert_eq!(
            e,
            Edit {
                lead: 12_000,
                skip: 2112,
                play: Some(96_000)
            }
        );
        assert!(!has_video(&mut Cursor::new(&f)));
    }

    #[test]
    fn media_time_converts_from_the_track_scale() {
        // Track scale 90 kHz, decoded at 48 kHz: 1,920 units = 1,024 frames.
        let e = to_frames(
            &[Entry {
                duration: 0,
                media_time: 1920,
            }],
            1000,
            90_000,
            48_000,
        );
        assert_eq!(
            e,
            Edit {
                lead: 0,
                skip: 1024,
                play: None
            }
        );
    }

    #[test]
    fn no_edit_list_and_broken_files_give_none() {
        assert_eq!(
            sound_edit(
                &mut Cursor::new(file(&[trak(b"soun", 48_000, None)])),
                48_000
            ),
            None
        );
        assert_eq!(
            sound_edit(&mut Cursor::new(b"RIFF\0\0\0\0WAVE".to_vec()), 48_000),
            None
        );
        let mut f = file(&[trak(b"soun", 48_000, Some(&[(1000, 1024)]))]);
        f.truncate(f.len() - 6);
        assert_eq!(sound_edit(&mut Cursor::new(f), 48_000), None);
    }
}
