//! Decoding a recording to planar f32 at its own rate, exactly as long as
//! it plays, so a cleaned track lines up with the original to the sample
//! and a kept picture stays in sync:
//!
//! - MP3 encoder and decoder delay: symphonia's gapless mode (1,105 frames
//!   for LAME); an MP3 without the encoder's header states neither, and is
//!   read whole, delay included;
//! - Ogg (Vorbis, Opus) pre-skip and end trim: the same;
//! - AAC priming and padding in MP4/MOV: the edit list ([`super::mp4`]),
//!   which symphonia parses but never applies (1,024 frames);
//! - Opus in Matroska: the start is exact (pre-skip); the end can run up to
//!   one Opus packet long (13.5 ms in the fixtures), since the padding
//!   element is not read.
//!
//! symphonia (pure Rust) opens WAV, AIFF, CAF, FLAC, ALAC, MP3, AAC and
//! Vorbis, and Opus with the `opus` feature (system libopus), in their
//! usual containers. Anything else goes through the `ffmpeg` command when
//! there is one (its own trimming applies there).

use super::mp4;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CodecRegistry, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::TimeBase;

/// Frames per block handed out.
pub const BLOCK: usize = 4096;
/// Peak columns per second of a [`prescan`] (50 ms).
pub const COLUMNS_PER_SECOND: u32 = 20;

/// The container, from the first bytes (the extension can lie).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Wav,
    Aiff,
    Caf,
    Flac,
    Mp3,
    Ogg,
    /// MP4, M4A, MOV and their kin (ISO base media).
    Mp4,
    /// Matroska and WebM.
    Matroska,
    Other,
}

impl Container {
    pub fn sniff(head: &[u8]) -> Container {
        let at = |i: usize, s: &[u8]| head.get(i..i + s.len()) == Some(s);
        if at(0, b"RIFF") || at(0, b"RF64") {
            Container::Wav
        } else if at(0, b"FORM") {
            Container::Aiff
        } else if at(0, b"caff") {
            Container::Caf
        } else if at(0, b"fLaC") {
            Container::Flac
        } else if at(0, b"OggS") {
            Container::Ogg
        } else if at(0, &[0x1A, 0x45, 0xDF, 0xA3]) {
            Container::Matroska
        } else if at(4, b"ftyp")
            || at(4, b"moov")
            || at(4, b"mdat")
            || at(4, b"wide")
            || at(4, b"free")
        {
            Container::Mp4
        } else if at(0, b"ID3") || (head.len() > 1 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0) {
            Container::Mp3
        } else {
            Container::Other
        }
    }
}

/// Who decodes the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Native,
    Ffmpeg,
}

#[derive(Clone, Debug)]
pub struct Info {
    pub rate: u32,
    pub channels: usize,
    /// The length in frames, when the container states it (a guide for the
    /// page before the pre-scan has counted; not exact for every format).
    pub frames: Option<u64>,
    pub codec: String,
    /// The stored sample size (lossless and PCM formats), when stated.
    pub bits: Option<u32>,
    pub container: Container,
    /// The file has a picture (a sound track inside a video).
    pub video: bool,
    pub via: Via,
    /// The rate the file plays at, where it is more than what this decoder
    /// delivers: HE-AAC read without ffmpeg (symphonia decodes the AAC-LC
    /// core only, at half the rate and without the highs SBR adds). Save
    /// writes at it, and the page says the quality is reduced.
    pub full_rate: Option<u32>,
    /// The sound tracks in the file (an OBS recording has one per
    /// source), at least 1.
    pub tracks: usize,
    /// The track decoded is the file's first sound track, as ffmpeg numbers
    /// them (symphonia skips one whose codec it lacks, and decodes the next).
    pub first_track: bool,
}

#[derive(Debug)]
pub enum DecodeError {
    Io(io::Error),
    /// The file holds no sound track (a silent video).
    NoSound,
    /// Neither symphonia nor ffmpeg opens it (or there is no ffmpeg).
    Unsupported(String),
    /// ffmpeg could not be started or stopped early on its own.
    Ffmpeg(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Io(e) => write!(f, "cannot read the file: {e}"),
            DecodeError::NoSound => write!(f, "the file has no sound track"),
            DecodeError::Unsupported(w) => write!(f, "the format is not supported ({w})"),
            DecodeError::Ffmpeg(w) => write!(f, "ffmpeg: {w}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// A file being decoded, block by block.
pub struct Decoder {
    info: Info,
    path: PathBuf,
    src: Source,
    /// The container's edit: lead silence and priming (MP4).
    edit: mp4::Edit,
    /// Track timestamp of output frame 0 (after the lead): priming and
    /// pre-skip the decoder trims itself still count in the timestamps.
    ts_zero: u64,
    /// Frames taken from the source so far (after the priming skip).
    pulled: u64,
    /// Where each packet's sound starts, for tracks whose timestamps are
    /// coarser than a frame (Matroska counts milliseconds, so a seek by
    /// time alone lands up to 48 frames off): (timestamp, frame), built by
    /// reading the file once.
    index: Option<std::sync::Arc<Vec<(u64, u64)>>>,
    building: Option<Vec<(u64, u64)>>,
    /// After a seek: the frame wanted next. The first buffer the codec
    /// gives back says where it really resumed (Vorbis, for one, returns
    /// nothing for its first packet), and the frames before the wanted
    /// one are dropped from there.
    resync: Option<u64>,
    /// Silence still to hand out before the sound (an MP4 empty edit).
    lead: u64,
    /// Decoded frames still to drop (priming).
    skip: u64,
    /// Stop after this many frames (lead included).
    limit: Option<u64>,
    emitted: u64,
    /// Decoded, not yet handed out (planar).
    queue: Vec<Vec<f32>>,
    source_done: bool,
    damaged: Option<u64>,
}

enum Source {
    Native(Box<Native>),
    Ffmpeg(Ffmpeg),
}

struct Native {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track: u32,
    buf: Option<SampleBuffer<f32>>,
    /// The rate of the last decoded buffer.
    rate: Option<u32>,
    /// The track's timestamp unit, when it is not one frame (Matroska: 1 ms).
    time_base: Option<TimeBase>,
    /// The timestamp of the packet behind the last decoded buffer.
    last_ts: u64,
    /// The stream stopped on an error other than its end.
    broken: bool,
}

struct Ffmpeg {
    exe: PathBuf,
    /// The sound track it decodes (0: the first).
    track: usize,
    child: Child,
    out: ChildStdout,
    bytes: Vec<u8>,
}

/// A seek lands this many frames early and decodes its way in, so the
/// codec's overlap (AAC 1,024, MP3's bit reservoir, Vorbis and Opus
/// windows) is primed when the wanted frame comes.
const SEEK_WARM: u64 = 4096;

/// Where an executable lives on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file() && is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn registry() -> CodecRegistry {
    let mut r = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut r);
    #[cfg(feature = "opus")]
    r.register_all::<symphonia_adapter_libopus::OpusDecoder>();
    r
}

impl Decoder {
    /// Open `path`. `ffmpeg` is the fallback decoder's command (None: none).
    pub fn open(path: &Path, ffmpeg: Option<&Path>) -> Result<Decoder, DecodeError> {
        Decoder::open_track(path, ffmpeg, 0)
    }

    /// Open the file's `track`th sound track (0: the first). The tracks
    /// after the first are ffmpeg's, numbered as it numbers them, so each
    /// cleaned track goes back in its own place when Save writes them all
    /// (only with ffmpeg are they kept); the first is read natively where
    /// it can be, unless that would be another track than ffmpeg's first.
    pub fn open_track(
        path: &Path,
        ffmpeg: Option<&Path>,
        track: usize,
    ) -> Result<Decoder, DecodeError> {
        let mut file = File::open(path).map_err(DecodeError::Io)?;
        let mut head = [0u8; 16];
        let n = file.read(&mut head).map_err(DecodeError::Io)?;
        let container = Container::sniff(&head[..n]);
        if track > 0 {
            let ff = ffmpeg.ok_or_else(|| {
                DecodeError::Unsupported("a second sound track needs ffmpeg".into())
            })?;
            return open_ffmpeg(path, ff, container, track);
        }
        let native = open_native(path, container);
        // symphonia skipped a first track it can't decode: ffmpeg's first,
        // so the tracks keep their order
        if let (Ok(d), Some(ff)) = (&native, ffmpeg) {
            if !d.info.first_track {
                return open_ffmpeg(path, ff, container, 0);
            }
        }
        match native {
            // HE-AAC: ffmpeg decodes what symphonia can't (SBR, PS), at the
            // file's own rate; without it, the core at half the rate
            Ok(mut d) => match he_aac_rate(path, &d) {
                Some(full) => match ffmpeg {
                    Some(ff) => open_ffmpeg(path, ff, container, 0),
                    None => {
                        d.info.full_rate = Some(full);
                        Ok(d)
                    }
                },
                None => Ok(d),
            },
            // A video with only a picture: nothing to clean, whatever decodes it.
            Err(DecodeError::Unsupported(_))
                if container == Container::Mp4
                    && File::open(path)
                        .map(|mut f| mp4::has_video(&mut f) && !mp4::has_sound(&mut f))
                        .unwrap_or(false) =>
            {
                Err(DecodeError::NoSound)
            }
            Err(DecodeError::Unsupported(why)) => match ffmpeg {
                Some(ff) => open_ffmpeg(path, ff, container, 0),
                None => Err(DecodeError::Unsupported(why)),
            },
            Err(e) => Err(e),
        }
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    /// The seek index this decoder built by reading to the end (only for
    /// tracks that need one), to share with other decoders of the file.
    pub fn seek_index(&self) -> Option<std::sync::Arc<Vec<(u64, u64)>>> {
        self.index.clone()
    }

    /// Use an index another decoder of the same file built.
    pub fn set_seek_index(&mut self, index: std::sync::Arc<Vec<(u64, u64)>>) {
        self.building = None;
        self.index = Some(index);
    }

    /// Continue at `frame` (counted as [`Decoder::next_block`] counts:
    /// lead silence included, priming excluded), exact to the frame.
    pub fn seek(&mut self, frame: u64) -> Result<(), DecodeError> {
        for q in &mut self.queue {
            q.clear();
        }
        self.source_done = false;
        self.damaged = None;
        self.emitted = frame;
        self.lead = self.edit.lead.saturating_sub(frame);
        let media = frame.saturating_sub(self.edit.lead);
        match &mut self.src {
            Source::Native(n) => {
                let rate = self.info.rate.max(1) as u64;
                let coarse = n
                    .time_base
                    .is_some_and(|tb| !(tb.numer == 1 && tb.denom as u64 == rate));
                // Land early enough to prime the codec, on a packet whose
                // place in our frames is known: by timestamp where it counts
                // frames, by the index where it counts milliseconds (none
                // yet: from the start).
                let from = media.saturating_sub(SEEK_WARM);
                let target_ts = if !coarse {
                    self.ts_zero + from
                } else {
                    match &self.index {
                        Some(ix) => {
                            let i = ix.partition_point(|&(_, f)| f <= from).saturating_sub(1);
                            ix.get(i).map_or(0, |&(ts, _)| ts)
                        }
                        None => 0,
                    }
                };
                let to = SeekTo::TimeStamp {
                    ts: target_ts,
                    track_id: n.track,
                };
                match n.format.seek(SeekMode::Accurate, to) {
                    Ok(_) => {}
                    // Past the end (or unseekable): nothing more to read.
                    Err(_) => {
                        self.source_done = true;
                        return Ok(());
                    }
                }
                n.decoder.reset();
                n.broken = false;
                self.skip = 0;
                self.resync = Some(media);
                // A seek before the index is complete ends its building.
                self.building = None;
            }
            Source::Ffmpeg(f) => {
                *f = spawn_ffmpeg(&f.exe, &self.path, Some(media), f.track)?;
                self.skip = 0;
            }
        }
        Ok(())
    }

    /// Frames handed out so far.
    pub fn position(&self) -> u64 {
        self.emitted
    }

    /// Set once the file ended early or broke: the frames that could be
    /// read. Only meaningful after `next_block` returned None.
    pub fn damaged(&self) -> Option<u64> {
        self.damaged
    }

    /// The next block of up to [`BLOCK`] frames, one Vec per channel; None
    /// at the end.
    pub fn next_block(&mut self) -> Result<Option<Vec<Vec<f32>>>, DecodeError> {
        let ch = self.info.channels;
        let room = self
            .limit
            .map_or(u64::MAX, |l| l.saturating_sub(self.emitted));
        if room == 0 {
            return Ok(self.finish());
        }
        if self.lead > 0 {
            let n = (self.lead.min(BLOCK as u64)).min(room) as usize;
            self.lead -= n as u64;
            self.emitted += n as u64;
            return Ok(Some(vec![vec![0.0; n]; ch]));
        }
        while self.queue[0].len() < BLOCK && !self.source_done {
            match self.pull()? {
                Some(mut block) => {
                    if let Some(want) = self.resync.take() {
                        let at = self.resume_frame();
                        self.skip = (want as i64 - at).max(0) as u64;
                        self.pulled = want;
                    }
                    if let (Some(b), Source::Native(n)) = (self.building.as_mut(), &self.src) {
                        if self.skip == 0 {
                            b.push((n.last_ts, self.pulled));
                        }
                    }
                    if self.skip > 0 {
                        let d = (self.skip as usize).min(block[0].len());
                        for c in &mut block {
                            c.drain(..d);
                        }
                        self.skip -= d as u64;
                    }
                    self.pulled += block[0].len() as u64;
                    for (q, b) in self.queue.iter_mut().zip(block) {
                        q.extend_from_slice(&b);
                    }
                }
                None => {
                    self.source_done = true;
                    if let Some(b) = self.building.take() {
                        self.index = Some(std::sync::Arc::new(b));
                    }
                }
            }
        }
        let n = (self.queue[0].len().min(BLOCK) as u64).min(room) as usize;
        if n == 0 {
            return Ok(self.finish());
        }
        let out: Vec<Vec<f32>> = self
            .queue
            .iter_mut()
            .map(|q| q.drain(..n).collect())
            .collect();
        self.emitted += n as u64;
        Ok(Some(out))
    }

    /// The end: decide whether the file was cut short.
    fn finish(&mut self) -> Option<Vec<Vec<f32>>> {
        let broken = match &mut self.src {
            Source::Native(n) => n.broken,
            Source::Ffmpeg(f) => !f.child.wait().map(|s| s.success()).unwrap_or(false),
        };
        self.damaged = cut_short(broken, self.emitted, self.info.frames, self.info.rate);
        None
    }

    /// Our frame at the start of the buffer just pulled after a seek. It
    /// can lie before frame 0: a codec reset at the very first packet
    /// returns the Opus pre-skip that the first open trimmed.
    fn resume_frame(&self) -> i64 {
        let Source::Native(n) = &self.src else {
            return 0;
        };
        let rate = self.info.rate.max(1) as u64;
        let zero = self.ts_zero as i64;
        let coarse = n
            .time_base
            .is_some_and(|tb| !(tb.numer == 1 && tb.denom as u64 == rate));
        if !coarse {
            return n.last_ts as i64 - zero;
        }
        if let Some(ix) = &self.index {
            match ix.binary_search_by_key(&n.last_ts, |&(t, _)| t) {
                Ok(0) => return -zero,
                Ok(i) => return ix[i].1 as i64,
                Err(_) => {}
            }
        }
        if n.last_ts == 0 {
            return -zero;
        }
        // A packet the index has not seen: by time, up to a millisecond off.
        let t = n
            .time_base
            .map(|tb| tb.calc_time(n.last_ts))
            .unwrap_or_default();
        (t.seconds * rate + (t.frac * rate as f64).round() as u64) as i64 - zero
    }

    /// The next decoded buffer, planar, with the file's channel count.
    fn pull(&mut self) -> Result<Option<Vec<Vec<f32>>>, DecodeError> {
        let ch = self.info.channels;
        let block = match &mut self.src {
            Source::Native(n) => n.pull(),
            Source::Ffmpeg(f) => f.pull(ch)?,
        };
        Ok(block.map(|b| fit(b, ch)))
    }
}

/// `block` with exactly `ch` channels: extra ones dropped, missing ones
/// silent (a layout change mid-stream).
fn fit(mut block: Vec<Vec<f32>>, ch: usize) -> Vec<Vec<f32>> {
    let n = block.first().map_or(0, Vec::len);
    block.truncate(ch);
    block.resize(ch, vec![0.0; n]);
    block
}

impl Native {
    /// The next decoded buffer, planar, in its own layout.
    fn pull(&mut self) -> Option<Vec<Vec<f32>>> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return None
                }
                Err(SymError::ResetRequired) => return None,
                Err(_) => {
                    self.broken = true;
                    return None;
                }
            };
            if packet.track_id() != self.track {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                // A damaged packet: skip it, as symphonia advises.
                Err(SymError::DecodeError(_)) => continue,
                Err(SymError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return None
                }
                Err(_) => {
                    self.broken = true;
                    return None;
                }
            };
            let spec = *decoded.spec();
            if decoded.frames() == 0 {
                continue;
            }
            self.last_ts = packet.ts();
            let have = spec.channels.count().max(1);
            if self
                .buf
                .as_ref()
                .is_none_or(|b| b.capacity() < decoded.capacity() * have)
            {
                self.buf = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
            }
            let buf = self.buf.as_mut()?;
            buf.copy_planar_ref(decoded);
            self.rate = Some(spec.rate);
            let s = buf.samples();
            let per = s.len() / have;
            return Some(
                (0..have)
                    .map(|c| s[c * per..(c + 1) * per].to_vec())
                    .collect(),
            );
        }
    }
}

impl Ffmpeg {
    fn pull(&mut self, ch: usize) -> Result<Option<Vec<Vec<f32>>>, DecodeError> {
        let frame = 4 * ch;
        let mut chunk = vec![0u8; BLOCK * frame];
        loop {
            let n = self.out.read(&mut chunk).map_err(DecodeError::Io)?;
            if n == 0 {
                // A partial frame at the end is dropped.
                return Ok(None);
            }
            self.bytes.extend_from_slice(&chunk[..n]);
            let whole = self.bytes.len() / frame;
            if whole == 0 {
                continue;
            }
            let mut out = vec![Vec::with_capacity(whole); ch];
            for f in self.bytes[..whole * frame].chunks_exact(frame) {
                for (c, o) in out.iter_mut().enumerate() {
                    let b = &f[c * 4..c * 4 + 4];
                    o.push(f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                }
            }
            self.bytes.drain(..whole * frame);
            return Ok(Some(out));
        }
    }
}

impl Drop for Ffmpeg {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn open_native(path: &Path, container: Container) -> Result<Decoder, DecodeError> {
    open_native_with(path, container, true)
}

fn open_native_with(
    path: &Path,
    container: Container,
    gapless: bool,
) -> Result<Decoder, DecodeError> {
    let file = File::open(path).map_err(DecodeError::Io)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let opts = FormatOptions {
        enable_gapless: gapless,
        ..Default::default()
    };
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &opts, &MetadataOptions::default())
        .map_err(|e| DecodeError::Unsupported(e.to_string()))?;
    let format = probed.format;
    let tracks = format.tracks().to_vec();
    // the sound tracks, as ffmpeg numbers them; symphonia decodes the first
    // whose codec it has
    let sound: Vec<_> = tracks
        .iter()
        .filter(|t| t.codec_params.sample_rate.is_some())
        .collect();
    let Some(track) = sound
        .iter()
        .copied()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
    else {
        // Tracks, but none with a sound codec symphonia knows: a silent
        // video, or a codec only ffmpeg has.
        return Err(DecodeError::Unsupported(
            "no sound track symphonia decodes".into(),
        ));
    };
    let params = track.codec_params.clone();
    // An MP3 without the encoder's gapless header has no delay to trim, and
    // its length is a guess from the first frame's bitrate (no Xing count):
    // gapless mode would cut every frame past that guess, the last seconds
    // of a VBR file that starts loud. Read it as it is.
    if gapless && container == Container::Mp3 && params.delay.is_none() {
        return open_native_with(path, container, false);
    }
    let codec = symphonia::default::get_codecs()
        .get_codec(params.codec)
        .map(|d| d.short_name.to_string())
        .unwrap_or_else(|| "opus".into());
    let decoder = registry()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| DecodeError::Unsupported(format!("{codec}: {e}")))?;
    let track_id = track.id;
    let time_base = params.time_base;
    let index_needed = time_base.is_some_and(|tb| {
        let r = params.sample_rate.unwrap_or(0) as u64;
        !(tb.numer == 1 && tb.denom as u64 == r)
    });
    let mut native = Native {
        format,
        decoder,
        track: track_id,
        buf: None,
        rate: None,
        time_base,
        last_ts: 0,
        broken: false,
    };
    // MP4 tracks often state no channel layout: the first decoded buffer
    // tells, and its rate is the one that counts.
    let first = native.pull();
    let first_ts = native.last_ts;
    let rate = native.rate.or(params.sample_rate).unwrap_or(48_000);
    let channels = first
        .as_ref()
        .map(Vec::len)
        .or(params.channels.map(|c| c.count()))
        .unwrap_or(1)
        .clamp(1, 8);
    let video = match container {
        Container::Mp4 => File::open(path)
            .map(|mut f| mp4::has_video(&mut f))
            .unwrap_or(false),
        _ => tracks.iter().any(|t| t.codec_params.sample_rate.is_none()),
    };
    let edit = if container == Container::Mp4 {
        File::open(path)
            .ok()
            .and_then(|mut f| mp4::sound_edit(&mut f, rate))
    } else {
        None
    };
    let stated = params.n_frames.and_then(|n| match container {
        // The Ogg reader counts the pre-skip in; MP3's count excludes it,
        // and is only a count (not a guess from the file size) when the
        // encoder wrote its gapless header.
        Container::Ogg => Some(n.saturating_sub(params.delay.unwrap_or(0) as u64)),
        Container::Mp3 => params.delay.map(|_| n),
        // A WAV written as a stream (ffmpeg or sox to a pipe, arecord to
        // stdout or killed) has a placeholder data size: no length.
        Container::Wav if wav_size_unstated(path) => None,
        _ => Some(n),
    });
    let frames = match container {
        // where the sound track's edit ends, else the track's own length
        Container::Mp4 => File::open(path)
            .ok()
            .and_then(|mut f| mp4::stated_frames(&mut f, rate)),
        Container::Matroska | Container::Other => None,
        _ => stated,
    };
    // Where output frame 0 sits in the track's timestamps (measured per
    // format): after the MP4 priming we drop ourselves, after the Opus
    // pre-skip the decoder drops; MP3 and Vorbis timestamps already start
    // after what symphonia's gapless mode trims.
    let ts_zero = match edit {
        Some(e) => e.skip,
        None if codec == "opus" => opus_pre_skip(params.extra_data.as_deref()).unwrap_or(0),
        None => 0,
    };
    let mut queue = vec![Vec::new(); channels];
    let mut skip = edit.map_or(0, |e| e.skip);
    if let Some(block) = first {
        let block = fit(block, channels);
        let d = (skip as usize).min(block[0].len());
        for (q, b) in queue.iter_mut().zip(block) {
            q.extend_from_slice(&b[d..]);
        }
        skip -= d as u64;
    }
    Ok(Decoder {
        info: Info {
            rate,
            channels,
            frames,
            codec,
            bits: params.bits_per_sample,
            container,
            video,
            via: Via::Native,
            full_rate: None,
            tracks: sound.len().max(1),
            first_track: sound.first().is_some_and(|t| t.id == track_id),
        },
        path: path.to_path_buf(),
        src: Source::Native(Box::new(native)),
        edit: edit.unwrap_or(mp4::Edit {
            lead: 0,
            skip: 0,
            play: None,
        }),
        ts_zero,
        pulled: queue[0].len() as u64,
        index: None,
        building: index_needed.then(|| vec![(first_ts, 0)]),
        resync: None,
        lead: edit.map_or(0, |e| e.lead),
        skip,
        limit: edit.and_then(|e| e.play.map(|p| e.lead + p)),
        emitted: 0,
        queue,
        source_done: false,
        damaged: None,
    })
}

/// The rate an MP4's AAC track plays at when it is HE-AAC (SBR, or SBR and
/// PS), which symphonia decodes as its AAC-LC core only: the `esds` says
/// object type 5 or 29, or (implicit SBR, signalled in the stream alone)
/// the track's timescale is twice the rate the decoder found. None for
/// anything else.
fn he_aac_rate(path: &Path, d: &Decoder) -> Option<u32> {
    let i = d.info();
    if i.container != Container::Mp4 || i.codec != "aac" || i.via != Via::Native {
        return None;
    }
    let mut f = File::open(path).ok()?;
    let object = mp4::aac_object_type(&mut f);
    let scale = mp4::sound_scale(&mut f)?;
    // SBR doubles the core's rate (PS rides on SBR)
    he_aac(object, scale, i.rate).then_some(i.rate * 2)
}

/// HE-AAC, from the object type the file states and its track's timescale
/// against the rate decoded.
fn he_aac(object: Option<u8>, scale: u32, decoded: u32) -> bool {
    matches!(object, Some(5 | 29)) || scale == decoded * 2
}

/// Whether a read that ended at `read` frames is damaged, and where: it
/// stopped more than a second short of the length the file states (with
/// no stated length, an error is all there is to go by). Within a second
/// of the stated end, error or not, it is whole: what is missing there is
/// a muxer's or encoder's rounding (priming counted into the length, a
/// trailing tag or chunk the reader rejects), and "Damaged after 0:59"
/// for a 59 s file helps no one.
fn cut_short(broken: bool, read: u64, stated: Option<u64>, rate: u32) -> Option<u64> {
    let tolerance = u64::from(rate.max(1));
    let damaged = match stated {
        Some(want) => read + tolerance < want,
        None => broken,
    };
    damaged.then_some(read)
}

/// The data chunk's size is a placeholder a WAV writer leaves when it
/// cannot seek back to fill it in: 0xFFFFFFFF (ffmpeg, sox), 0x80000000
/// (arecord), 0x7FFFF000 (what sox's reader also takes as unspecified).
/// The sound then runs to the end of the file. Only a cut WAV of 2 GiB or
/// more loses its damage report to this.
fn wav_size_unstated(path: &Path) -> bool {
    use std::io::{Seek, SeekFrom};
    const PLACEHOLDER: u32 = 0x7FFF_F000;
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut head = [0u8; 12];
    if f.read_exact(&mut head).is_err() || &head[..4] != b"RIFF" {
        return false;
    }
    let mut at = 12u64;
    for _ in 0..64 {
        let mut chunk = [0u8; 8];
        if f.seek(SeekFrom::Start(at)).is_err() || f.read_exact(&mut chunk).is_err() {
            return false;
        }
        let len = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        if &chunk[..4] == b"data" {
            return len >= PLACEHOLDER;
        }
        at += 8 + len as u64 + (len as u64 & 1);
    }
    false
}

fn open_ffmpeg(
    path: &Path,
    ffmpeg: &Path,
    container: Container,
    track: usize,
) -> Result<Decoder, DecodeError> {
    let probe = ffmpeg.with_file_name("ffprobe");
    let out = Command::new(&probe)
        .args(["-v", "error", "-show_entries", "stream=index,codec_type,codec_name,sample_rate,channels:stream_disposition=attached_pic", "-of", "default=nw=1"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| DecodeError::Ffmpeg(format!("{}: {e}", probe.display())))?;
    if !out.status.success() {
        return Err(DecodeError::Unsupported(last_line(&out.stderr)));
    }
    let streams = parse_streams(&String::from_utf8_lossy(&out.stdout));
    let video = streams.iter().any(|s| s.kind == "video" && !s.attached_pic);
    let sound: Vec<&Stream> = streams.iter().filter(|s| s.kind == "audio").collect();
    let Some(audio) = sound.get(track) else {
        return Err(DecodeError::NoSound);
    };
    let (rate, channels) = (
        audio.rate.unwrap_or(48_000),
        audio.channels.unwrap_or(2).clamp(1, 8),
    );
    let src = spawn_ffmpeg(ffmpeg, path, None, track)?;
    // ffmpeg exits as if whole on a cut file: an MP4's stated length is how
    // a cut one tells (ffmpeg applies the edit, as `stated_frames` reads it)
    let frames = match container {
        Container::Mp4 => File::open(path)
            .ok()
            .and_then(|mut f| mp4::stated_frames_of(&mut f, rate, track)),
        _ => None,
    };
    Ok(Decoder {
        info: Info {
            rate,
            channels,
            frames,
            codec: audio.codec.clone(),
            bits: None,
            container,
            video,
            via: Via::Ffmpeg,
            full_rate: None,
            tracks: sound.len(),
            first_track: true,
        },
        path: path.to_path_buf(),
        src: Source::Ffmpeg(src),
        edit: mp4::Edit {
            lead: 0,
            skip: 0,
            play: None,
        },
        ts_zero: 0,
        pulled: 0,
        index: None,
        building: None,
        resync: None,
        lead: 0,
        skip: 0,
        limit: None,
        emitted: 0,
        queue: vec![Vec::new(); channels],
        source_done: false,
        damaged: None,
    })
}

/// The pre-skip in an `OpusHead` (RFC 7845: bytes 10 and 11, little endian).
fn opus_pre_skip(head: Option<&[u8]>) -> Option<u64> {
    let h = head?;
    (h.len() >= 12 && &h[..8] == b"OpusHead").then(|| u16::from_le_bytes([h[10], h[11]]) as u64)
}

/// ffmpeg decoding the `track`th sound track to f32, from frame `at`.
fn spawn_ffmpeg(
    exe: &Path,
    path: &Path,
    at: Option<u64>,
    track: usize,
) -> Result<Ffmpeg, DecodeError> {
    let mut cmd = Command::new(exe);
    cmd.args(["-nostdin", "-v", "error"]);
    cmd.arg("-i")
        .arg(path)
        .args(["-map", &format!("0:a:{track}")]);
    // Exact to the sample: decode from the start and trim (an input seek
    // lands where the demuxer's timestamps say, up to a packet off).
    if let Some(n) = at {
        cmd.arg("-af").arg(format!("atrim=start_sample={n}"));
    }
    let mut child = cmd
        .args(["-c:a", "pcm_f32le", "-f", "f32le", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| DecodeError::Ffmpeg(format!("{}: {e}", exe.display())))?;
    let out = child
        .stdout
        .take()
        .ok_or_else(|| DecodeError::Ffmpeg("no output".into()))?;
    Ok(Ffmpeg {
        exe: exe.to_path_buf(),
        track,
        child,
        out,
        bytes: Vec::new(),
    })
}

struct Stream {
    kind: String,
    codec: String,
    rate: Option<u32>,
    channels: Option<usize>,
    attached_pic: bool,
}

/// ffprobe's `key=value` lines, one block per stream (each starts with
/// `index=`).
fn parse_streams(text: &str) -> Vec<Stream> {
    let mut out: Vec<Stream> = Vec::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k == "index" {
            out.push(Stream {
                kind: String::new(),
                codec: String::new(),
                rate: None,
                channels: None,
                attached_pic: false,
            });
            continue;
        }
        let Some(s) = out.last_mut() else { continue };
        match k {
            "codec_type" => s.kind = v.to_string(),
            "codec_name" => s.codec = v.to_string(),
            "sample_rate" => s.rate = v.parse().ok(),
            "channels" => s.channels = v.parse().ok(),
            "DISPOSITION:attached_pic" => s.attached_pic = v == "1",
            _ => {}
        }
    }
    out
}

fn last_line(stderr: &[u8]) -> String {
    BufReader::new(stderr)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .last()
        .unwrap_or_else(|| "ffprobe failed".into())
}

/// What a full read of the file found: its exact length, its peaks,
/// whether the channels are one recording twice, and whether it is cut short.
#[derive(Clone, Debug)]
pub struct Prescan {
    pub frames: u64,
    /// The loudest sample (any channel) per 50 ms column.
    pub peaks: Vec<f32>,
    /// Two or more channels carrying the same sound (mono recorded as
    /// stereo): cleaned once and written to all.
    pub same_channels: bool,
    pub loudest: f32,
    pub damaged: Option<u64>,
}

/// Channels whose difference is this far below their level are the same.
const SAME_BELOW: f64 = 1e-6;

/// Read the whole file once (decoding runs 200 to 4,300 times real time).
pub fn prescan(d: &mut Decoder) -> Result<Prescan, DecodeError> {
    let rate = d.info().rate.max(1) as u64;
    let mut peaks: Vec<f32> = Vec::new();
    let mut pos = 0u64;
    let (mut diff, mut level) = (0f64, 0f64);
    let mut loudest = 0f32;
    let cols = COLUMNS_PER_SECOND as u64;
    // The first frame of column k is ceil(k * rate / cols).
    let edge = |k: u64| (k * rate).div_ceil(cols);
    let mut col = 0u64;
    let mut next_edge = edge(1);
    while let Some(block) = d.next_block()? {
        let n = block[0].len();
        for i in 0..n {
            while pos + i as u64 >= next_edge {
                col += 1;
                next_edge = edge(col + 1);
            }
            if peaks.len() <= col as usize {
                peaks.resize(col as usize + 1, 0.0);
            }
            let mut m = 0f32;
            for c in &block {
                m = m.max(c[i].abs());
            }
            let p = &mut peaks[col as usize];
            *p = p.max(m);
            loudest = loudest.max(m);
            if block.len() > 1 {
                let a = block[0][i] as f64;
                for c in &block[1..] {
                    let b = c[i] as f64;
                    diff += (a - b) * (a - b);
                    level += a * a + b * b;
                }
            }
        }
        pos += n as u64;
    }
    let same_channels = d.info().channels > 1 && diff <= SAME_BELOW * level;
    Ok(Prescan {
        frames: pos,
        peaks,
        same_channels,
        loudest,
        damaged: d.damaged(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_is_only_sound_missing_from_what_the_file_states() {
        // Read to the stated length (within a second): whole, error or not.
        assert_eq!(cut_short(false, 48_000, Some(48_000), 48_000), None);
        assert_eq!(cut_short(true, 48_000, Some(48_000), 48_000), None);
        assert_eq!(cut_short(true, 44_000, Some(48_000), 48_000), None);
        assert_eq!(cut_short(true, 96_000, Some(143_000), 48_000), None);
        // Further short of it: damaged where the read stopped, error or not.
        assert_eq!(cut_short(false, 28_800, Some(96_000), 48_000), Some(28_800));
        assert_eq!(cut_short(true, 28_800, Some(96_000), 48_000), Some(28_800));
        // No stated length: only an error tells.
        assert_eq!(cut_short(false, 28_800, None, 48_000), None);
        assert_eq!(cut_short(true, 28_800, None, 48_000), Some(28_800));
    }

    /// HE-AAC is told by its object type (5: SBR, 29: SBR and PS), or by a
    /// track that plays at twice the rate its AAC-LC core decodes at.
    #[test]
    fn he_aac_is_told_by_its_object_type_or_its_rate() {
        assert!(he_aac(Some(5), 44_100, 22_050));
        assert!(he_aac(Some(29), 48_000, 24_000));
        assert!(he_aac(Some(2), 44_100, 22_050), "implicit SBR");
        assert!(he_aac(None, 48_000, 24_000), "implicit SBR, no esds read");
        assert!(!he_aac(Some(2), 44_100, 44_100), "AAC-LC");
        assert!(!he_aac(None, 1_000, 44_100), "a movie-style timescale");
    }

    #[test]
    fn containers_are_told_by_their_first_bytes() {
        assert_eq!(Container::sniff(b"RIFF\0\0\0\0WAVE"), Container::Wav);
        assert_eq!(Container::sniff(b"FORM\0\0\0\0AIFF"), Container::Aiff);
        assert_eq!(Container::sniff(b"fLaC"), Container::Flac);
        assert_eq!(Container::sniff(b"OggS"), Container::Ogg);
        assert_eq!(
            Container::sniff(&[0x1A, 0x45, 0xDF, 0xA3, 0]),
            Container::Matroska
        );
        assert_eq!(Container::sniff(b"\0\0\0\x20ftypisom"), Container::Mp4);
        assert_eq!(Container::sniff(b"ID3\x04"), Container::Mp3);
        assert_eq!(Container::sniff(&[0xFF, 0xFB, 0x90]), Container::Mp3);
        assert_eq!(Container::sniff(b"0&\xb2u"), Container::Other);
    }

    #[test]
    fn ffprobe_streams_parse_and_cover_art_is_not_a_picture() {
        let s = parse_streams(
            "index=0\ncodec_name=mp3\ncodec_type=audio\nsample_rate=44100\nchannels=2\nDISPOSITION:attached_pic=0\n\
             index=1\ncodec_name=mjpeg\ncodec_type=video\nDISPOSITION:attached_pic=1\n",
        );
        assert_eq!(s.len(), 2);
        assert_eq!(
            (s[0].kind.as_str(), s[0].rate, s[0].channels),
            ("audio", Some(44_100), Some(2))
        );
        assert!(s[1].attached_pic);
    }
}
