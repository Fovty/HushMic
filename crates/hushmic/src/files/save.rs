//! Save: the cleaned file, in the original's format at its rate and
//! channels, from the cache and a fresh decode of the original, mixed at
//! the strength on screen (see [`super::mix`]). The cleaned sound is
//! brought to the original's rate and mixed with the original as it is, so
//! the original's share stays untouched.
//!
//! With ffmpeg the original's codec, bit rate, tags and cover are kept and
//! a video keeps its picture (copied, not encoded again). Without it, WAV
//! stays WAV, everything else becomes FLAC, and a video its sound alone.
//!
//! The output is prepared in the cache at the strength on screen once the
//! pass is done ([`prepare`]), so Save ([`deliver`]) only puts it in place:
//! next to the original as `name (cleaned).ext`, then `(cleaned 2)` and
//! so on, never over a file that is there.

use super::cache::Cache;
use super::decode::{Container, DecodeError, Decoder, Info};
use super::ffmpeg::{encoder_for, Counts, Ffmpeg, Sound, Starts};
use super::mix::Blend;
use super::resample::Converter;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// Frames of the cache read at a time.
const CHUNK: usize = 8_192;

#[derive(Debug)]
pub enum SaveError {
    /// The disk is full.
    NoSpace,
    /// The original is not where it was.
    OriginalGone,
    /// The folder asked for is not there (any more): never made again.
    FolderGone,
    /// The folder it goes into can't be written (permission denied, a
    /// read-only file system): the folder.
    NoWrite(PathBuf),
    /// The original cannot be decoded again.
    Decode(String),
    /// ffmpeg failed: its last words (for the log, not the page).
    Ffmpeg(String),
    Io(io::Error),
    Cancelled,
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::NoSpace => write!(f, "no space left"),
            SaveError::OriginalGone => write!(f, "the original is gone"),
            SaveError::FolderGone => write!(f, "the folder is gone"),
            SaveError::NoWrite(d) => write!(f, "can't write to {}", d.display()),
            SaveError::Decode(e) => write!(f, "decode: {e}"),
            SaveError::Ffmpeg(e) => write!(f, "ffmpeg: {e}"),
            SaveError::Io(e) => e.fmt(f),
            SaveError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for SaveError {}

impl From<io::Error> for SaveError {
    fn from(e: io::Error) -> SaveError {
        match e.raw_os_error() {
            Some(libc::ENOSPC) | Some(libc::EDQUOT) => SaveError::NoSpace,
            _ => SaveError::Io(e),
        }
    }
}

impl From<DecodeError> for SaveError {
    fn from(e: DecodeError) -> SaveError {
        match e {
            DecodeError::Io(e) if e.kind() == io::ErrorKind::NotFound => SaveError::OriginalGone,
            e => SaveError::Decode(e.to_string()),
        }
    }
}

/// A write into `dir` that was refused: the folder can't be written (its
/// permissions, or a read-only file system). Anything else as it is.
fn into_folder(e: io::Error, dir: &Path) -> SaveError {
    match e.raw_os_error() {
        Some(libc::EACCES) | Some(libc::EPERM) | Some(libc::EROFS) => {
            SaveError::NoWrite(dir.to_path_buf())
        }
        _ => e.into(),
    }
}

/// How the output is written.
#[derive(Clone, Debug, PartialEq)]
pub enum Writer {
    Wav {
        bits: u16,
        float: bool,
    },
    Flac {
        bits: u32,
    },
    Ffmpeg {
        encoder: String,
        /// The original's, for a lossy encoder (else ffmpeg's own choice).
        bit_rate: Option<u64>,
        /// Lossless encoders: 24 keeps a 24-bit original's depth.
        bits: Option<u32>,
        layout: String,
        /// Copy the picture (and subtitles) of a video.
        video: bool,
        /// The original's other sound tracks (all but the main one), in
        /// its order: each cleaned and written as itself, or copied as it
        /// is.
        more: Vec<MoreTrack>,
        /// Copy its data streams and attachments too (dropped only when
        /// the output would not take them).
        extras: bool,
        /// The original's other streams, by kind (each keeps its tags).
        counts: Counts,
        /// When the original's sound starts: the copy keeps its times.
        starts: Option<Starts>,
    },
}

/// Which of an original's sound tracks a Save cleans: `main`, the one
/// played (cleaned into the cache folder itself), and with `all` every
/// other one too (each into a folder of its own, `cache::track_dir`). The
/// rest are copied as they are, byte for byte: an OBS recording's music
/// track is no voice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tracks {
    /// 0: the first.
    pub main: usize,
    pub all: bool,
}

/// Which sound track a file is cleaned for, as asked: the voice's (by
/// its title, [`voice_track`]), one by its place, or every one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pick {
    #[default]
    Auto,
    /// 0: the first.
    One(usize),
    All,
}

impl Pick {
    /// `N` (1 the first) or `all`; None for anything else.
    pub fn parse(v: &str) -> Option<Pick> {
        match v {
            "all" => Some(Pick::All),
            "auto" => Some(Pick::Auto),
            n => n
                .parse::<usize>()
                .ok()
                .filter(|&n| n >= 1)
                .map(|n| Pick::One(n - 1)),
        }
    }

    /// The word [`Pick::parse`] reads back.
    pub fn word(self) -> String {
        match self {
            Pick::Auto => "auto".into(),
            Pick::All => "all".into(),
            Pick::One(n) => (n + 1).to_string(),
        }
    }

    /// The tracks a Save of `path` cleans, as this asks, from its tracks'
    /// `titles` (every sound track's, in order; none without ffmpeg, which
    /// reads only the first). Err: the track asked for is not in the file
    /// (how many it has).
    pub fn resolve(self, titles: &[String]) -> Result<Tracks, usize> {
        let count = titles.len().max(1);
        match self {
            Pick::Auto => Ok(Tracks {
                main: voice_track(titles),
                all: false,
            }),
            Pick::All => Ok(Tracks { main: 0, all: true }),
            Pick::One(n) if n < count => Ok(Tracks {
                main: n,
                all: false,
            }),
            Pick::One(_) => Err(count),
        }
    }
}

/// The track a recording's voice is on: the first whose title names a
/// microphone or a voice ("Mic/Aux", OBS's own name; "Voice", "VO",
/// "Vocals"), else the first.
pub fn voice_track(titles: &[String]) -> usize {
    const WORDS: [&str; 7] = [
        "mic",
        "mics",
        "microphone",
        "voice",
        "vo",
        "vocal",
        "vocals",
    ];
    titles
        .iter()
        .position(|t| {
            t.split(|c: char| !c.is_alphanumeric())
                .any(|w| WORDS.contains(&w.to_lowercase().as_str()))
        })
        .unwrap_or(0)
}

/// The original's sound tracks' titles (empty ones without), as ffprobe
/// reads them; none without ffmpeg.
pub fn track_titles(path: &Path, ff: Option<&Ffmpeg>) -> Vec<String> {
    ff.and_then(|f| f.probe(path))
        .map(|p| p.titles())
        .unwrap_or_default()
}

/// A sound track other than the main one, as Save writes it.
#[derive(Clone, Debug, PartialEq)]
pub struct MoreTrack {
    /// Its place among the original's sound tracks (0: the first).
    pub at: usize,
    /// Its encoder; None: copied as it is (not cleaned: only the main
    /// track is, or no encoder for its codec).
    pub encoder: Option<String>,
    /// The original's, for a lossy encoder.
    pub bit_rate: Option<u64>,
    /// Lossless: the depth kept.
    pub bits: Option<u32>,
    pub layout: Option<String>,
}

impl MoreTrack {
    /// What Save writes for the original's `at`th sound stream, cleaned
    /// (`clean`, where ffmpeg can write its codec) or copied.
    fn of(s: &Sound, at: usize, clean: bool, ff: &Ffmpeg) -> MoreTrack {
        let lossless = s.codec.starts_with("pcm_") || matches!(s.codec.as_str(), "flac" | "alac");
        let encoder = encoder_for(&s.codec);
        MoreTrack {
            at,
            encoder: (clean && ff.has_encoder(encoder)).then(|| encoder.to_string()),
            bit_rate: lossy_rate(
                encoder,
                s.bit_rate.filter(|_| !lossless),
                s.channels.unwrap_or(2),
            ),
            bits: (lossless && !s.codec.starts_with("pcm_f")).then(|| {
                if s.bits.unwrap_or(16) >= 24 {
                    24
                } else {
                    16
                }
            }),
            layout: s.layout.clone(),
        }
    }

    /// It is cleaned (else copied as it is).
    pub fn cleaned(&self) -> bool {
        self.encoder.is_some()
    }
}

/// What Save writes for an original: the Output card words it.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub writer: Writer,
    /// The extension of the output (the original's, unless the format
    /// cannot be written).
    pub ext: String,
    /// The original's own format (else FLAC instead).
    pub same_format: bool,
    /// A video saved as its sound alone (no ffmpeg).
    pub sound_only: bool,
    /// The sound tracks the output keeps (an OBS recording has one per
    /// source), and the original's it lacks: without ffmpeg, or in a format
    /// that holds one (FLAC instead), only the main one is kept.
    pub tracks: usize,
    pub dropped: usize,
    /// The sound track cleaned and played (0: the first); without ffmpeg
    /// always the first.
    pub main: usize,
}

impl Plan {
    /// The other sound tracks that are cleaned too (the rest are copied):
    /// their places, as ffmpeg counts them (0 the first).
    pub fn cleaned_tracks(&self) -> Vec<usize> {
        self.more().filter(|t| t.cleaned()).map(|t| t.at).collect()
    }

    fn more(&self) -> impl Iterator<Item = &MoreTrack> {
        match &self.writer {
            Writer::Ffmpeg { more, .. } => more.iter(),
            _ => [].iter(),
        }
    }

    /// The slot of the original's `at`th sound track among the other
    /// tracks (what a list of them, such as [`Prepare::more`], is in the
    /// order of).
    ///
    /// [`Prepare::more`]: super::worker::Prepare::more
    pub fn slot(&self, at: usize) -> Option<usize> {
        self.more().position(|t| t.at == at)
    }
}

/// Bits of a PCM codec name ("pcm_s24le" → 24, float for "pcm_f32le").
fn pcm_bits(codec: &str) -> Option<(u16, bool)> {
    let rest = codec.strip_prefix("pcm_")?;
    let float = rest.starts_with('f');
    let digits: String = rest
        .chars()
        .skip(1)
        .take_while(char::is_ascii_digit)
        .collect();
    let bits: u16 = digits.parse().ok()?;
    match (bits, float) {
        (32, true) => Some((32, true)),
        (16 | 24 | 32, false) => Some((bits, false)),
        (8, false) => Some((16, false)),
        _ => None,
    }
}

/// The depth for a lossless output: the original's when it says 24 or
/// more, else 16 (a lossy original states none).
fn lossless_bits(info: &Info) -> u32 {
    match info.bits {
        Some(b) if b >= 24 => 24,
        _ => 16,
    }
}

/// AAC's least bit rate, for each channel.
const AAC_FLOOR: u64 = 64_000;

/// The bit rate a lossy encoder gets: the original's, but AAC never under
/// [`AAC_FLOOR`] a channel. ffmpeg's encoder writes AAC-LC only; at the
/// rate of an HE-AAC original (a phone clip's 42 kbit/s in stereo) it
/// keeps nothing above 8 kHz and the voice sounds tinny.
fn lossy_rate(encoder: &str, rate: Option<u64>, channels: usize) -> Option<u64> {
    match (encoder, rate) {
        ("aac", Some(r)) => Some(r.max(AAC_FLOOR * channels.clamp(1, 8) as u64)),
        (_, r) => r,
    }
}

fn layout_of(channels: usize) -> String {
    match channels {
        1 => "mono".into(),
        2 => "stereo".into(),
        n => format!("{n}c"),
    }
}

/// What Save writes for `path`, whose track `tracks.main` decodes as
/// `info` (without ffmpeg only the first can be: `main` is then 0).
pub fn plan(path: &Path, info: &Info, ff: Option<&Ffmpeg>, tracks: Tracks) -> Plan {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    // one sound track only: the others are not kept
    let dropped = info.tracks.saturating_sub(1);
    let flac = |sound_only| Plan {
        writer: Writer::Flac {
            bits: lossless_bits(info),
        },
        ext: "flac".into(),
        same_format: info.container == Container::Flac,
        sound_only,
        tracks: 1,
        dropped,
        main: 0,
    };
    let Some(ff) = ff else {
        if info.container == Container::Wav && !info.video {
            let (bits, float) = pcm_bits(&info.codec).unwrap_or((24, false));
            return Plan {
                writer: Writer::Wav { bits, float },
                ext,
                same_format: true,
                sound_only: false,
                tracks: 1,
                dropped,
                main: 0,
            };
        }
        return flac(info.video);
    };
    let probe = ff.probe(path).unwrap_or_default();
    let main = tracks.main.min(probe.sounds.len().saturating_sub(1));
    let sound = probe.sound(main);
    let codec = if sound.codec.is_empty() {
        info.codec.clone()
    } else {
        sound.codec.clone()
    };
    let video = probe.video || info.video;
    let lossless = codec.starts_with("pcm_") || matches!(codec.as_str(), "flac" | "alac");
    let layout = sound
        .layout
        .clone()
        .unwrap_or_else(|| layout_of(info.channels));
    // Float PCM keeps its own sample format: the encoder takes nothing else
    // (a depth forced on it fails the Save with no packet written).
    let float = codec.starts_with("pcm_f");
    // Every other sound track, in the original's own container: cleaned
    // with `all`, else copied as it is.
    let more: Vec<MoreTrack> = probe
        .sounds
        .iter()
        .enumerate()
        .filter(|&(at, _)| at != main)
        .map(|(at, s)| MoreTrack::of(s, at, tracks.all, ff))
        .collect();
    // what is copied (a picture, other tracks, chapters) keeps its time
    let starts = ff.starts(path);
    let writer = |encoder: &str, same| Plan {
        writer: Writer::Ffmpeg {
            encoder: encoder.to_string(),
            bit_rate: lossy_rate(
                encoder,
                sound.bit_rate.filter(|_| !lossless && same),
                sound.channels.unwrap_or(info.channels),
            ),
            bits: (lossless && same && !float).then(|| lossless_bits(info)),
            layout: layout.clone(),
            video,
            more: more.clone(),
            extras: probe.data || probe.attachments,
            counts: probe.counts,
            starts: starts.clone(),
        },
        ext: ext.clone(),
        same_format: same,
        sound_only: false,
        tracks: 1 + more.len(),
        dropped: 0,
        main,
    };
    let encoder = encoder_for(&codec);
    if ff.has_encoder(encoder) {
        return writer(encoder, true);
    }
    if video {
        // A sound codec ffmpeg cannot write: the container's usual one.
        let usual = match info.container {
            Container::Matroska if ff.has_encoder("libopus") => "libopus",
            _ => "aac",
        };
        return writer(usual, false);
    }
    // FLAC instead: one sound track
    Plan {
        writer: Writer::Ffmpeg {
            encoder: "flac".into(),
            bit_rate: None,
            bits: Some(lossless_bits(info)),
            layout,
            video: false,
            more: Vec::new(),
            extras: false,
            counts: Counts::default(),
            starts: None,
        },
        ext: "flac".into(),
        same_format: false,
        sound_only: false,
        tracks: 1,
        dropped: more.len(),
        main,
    }
}

/// The mix at the original's rate, block by block (an HE-AAC file read
/// without ffmpeg: at the rate it plays at, not its core's).
pub struct Mixdown {
    dec: Decoder,
    /// The original from its decoded rate to the one written, where they
    /// differ ([`Info::full_rate`]); and whether it gave its last.
    up: Option<Converter>,
    up_done: bool,
    out_info: Info,
    cache: Cache,
    conv: Converter,
    clean: Vec<Vec<f32>>,
    read48: u64,
    total48: u64,
    flushed: bool,
    blend: Blend,
    /// How far, 0 to 1 (f32 bits), for whoever watches; this mix is the
    /// part `span` of the whole (several tracks are mixed one after the
    /// other).
    done: Option<Arc<AtomicU32>>,
    span: (f32, f32),
}

impl Mixdown {
    /// The original at `path`, the cache in `cache_dir` (`stored`
    /// channels, `total48` frames at 48 kHz), mixed at `alpha`.
    pub fn open(
        path: &Path,
        ffmpeg: Option<&Path>,
        cache_dir: &Path,
        stored: usize,
        total48: u64,
        alpha: f32,
    ) -> Result<Mixdown, SaveError> {
        Mixdown::open_track(path, ffmpeg, cache_dir, stored, total48, alpha, 0)
    }

    /// [`Mixdown::open`] for the original's `track`th sound track (its
    /// cache in `cache_dir`, see [`super::cache::track_dir`]).
    pub fn open_track(
        path: &Path,
        ffmpeg: Option<&Path>,
        cache_dir: &Path,
        stored: usize,
        total48: u64,
        alpha: f32,
        track: usize,
    ) -> Result<Mixdown, SaveError> {
        if !path.exists() {
            return Err(SaveError::OriginalGone);
        }
        let dec = Decoder::open_track(path, ffmpeg, track)?;
        let decoded = dec.info().rate;
        let rate = dec.info().full_rate.unwrap_or(decoded);
        let conv = Converter::new(48_000, rate, stored)
            .ok_or_else(|| SaveError::Decode(format!("{rate} Hz")))?;
        let up = match rate != decoded {
            true => Some(
                Converter::new(decoded, rate, dec.info().channels)
                    .ok_or_else(|| SaveError::Decode(format!("{decoded} Hz")))?,
            ),
            false => None,
        };
        let out_info = Info {
            rate,
            ..dec.info().clone()
        };
        Ok(Mixdown {
            up,
            up_done: false,
            out_info,
            cache: Cache::open(cache_dir, stored)?,
            dec,
            conv,
            clean: vec![Vec::new(); stored],
            read48: 0,
            total48,
            flushed: false,
            blend: Blend::new(alpha),
            done: None,
            span: (0.0, 1.0),
        })
    }

    pub fn info(&self) -> &Info {
        &self.out_info
    }

    /// The original's next block at the rate written; None at its end.
    fn original(&mut self) -> Result<Option<Vec<Vec<f32>>>, SaveError> {
        loop {
            let block = match (self.dec.next_block()?, &mut self.up) {
                (Some(b), None) => return Ok(Some(b)),
                (Some(b), Some(up)) => up.push(&b),
                (None, Some(up)) if !self.up_done => {
                    self.up_done = true;
                    up.finish()
                }
                (None, _) => return Ok(None),
            };
            if block.first().is_some_and(|c| !c.is_empty()) {
                return Ok(Some(block));
            }
        }
    }

    /// The next block, planar; None at the end of the original.
    pub fn next_block(&mut self) -> Result<Option<Vec<Vec<f32>>>, SaveError> {
        let Some(orig) = self.original()? else {
            return Ok(None);
        };
        let n = orig[0].len();
        while self.clean[0].len() < n && !self.flushed {
            let k = (CHUNK as u64).min(self.total48 - self.read48) as usize;
            let out = if k > 0 {
                let c = self.cache.read(self.read48, k)?;
                self.read48 += k as u64;
                self.conv.push(&c)
            } else {
                self.flushed = true;
                self.conv.finish()
            };
            for (dst, src) in self.clean.iter_mut().zip(out) {
                dst.extend_from_slice(&src);
            }
        }
        if let Some(d) = &self.done {
            let f = (self.read48 as f32 / self.total48.max(1) as f32).min(1.0);
            let (a, b) = self.span;
            d.store((a + f * (b - a)).to_bits(), Ordering::Relaxed);
        }
        // The two lengths can differ by a frame of rounding: silence.
        let clean: Vec<Vec<f32>> = self
            .clean
            .iter_mut()
            .map(|c| {
                let k = n.min(c.len());
                let mut v: Vec<f32> = c.drain(..k).collect();
                v.resize(n, 0.0);
                v
            })
            .collect();
        Ok(Some(self.blend.mix(&clean, &orig)))
    }
}

/// A float sample as a `bits`-bit integer, the way decoders read it back
/// (÷ 2^(bits−1)).
fn quantize(v: f32, bits: u32) -> i32 {
    let s = (1i64 << (bits - 1)) as f32;
    (v * s).round().clamp(-s, s - 1.0) as i32
}

/// A sound track after the first, mixed at the strength into a raw file
/// (f32, interleaved) for the encoder: where, at what rate and how many
/// channels.
#[derive(Clone, Debug, PartialEq)]
pub struct MixedTrack {
    pub file: PathBuf,
    pub rate: u32,
    pub channels: usize,
}

/// Write the whole mix to `out` as `plan` says. The frames written. The
/// original's other sound tracks are copied as they are.
pub fn write(
    plan: &Plan,
    mix: &mut Mixdown,
    original: &Path,
    ff: Option<&Ffmpeg>,
    out: &Path,
    cancel: &AtomicBool,
) -> Result<u64, SaveError> {
    write_tracks(plan, mix, &[], original, ff, out, cancel)
}

/// [`write`], with the sound tracks after the first that were cleaned
/// (`mixed[k]`: the second track's, and so on; None: copied as it is).
pub fn write_tracks(
    plan: &Plan,
    mix: &mut Mixdown,
    mixed: &[Option<MixedTrack>],
    original: &Path,
    ff: Option<&Ffmpeg>,
    out: &Path,
    cancel: &AtomicBool,
) -> Result<u64, SaveError> {
    let (rate, channels) = (mix.info().rate, mix.info().channels);
    match &plan.writer {
        Writer::Wav { bits, float } => {
            let spec = hound::WavSpec {
                channels: channels as u16,
                sample_rate: rate,
                bits_per_sample: *bits,
                sample_format: if *float {
                    hound::SampleFormat::Float
                } else {
                    hound::SampleFormat::Int
                },
            };
            let file = BufWriter::new(std::fs::File::create(out)?);
            let mut w = hound::WavWriter::new(file, spec).map_err(hound_err)?;
            let mut n = 0;
            while let Some(b) = next(mix, cancel)? {
                for i in 0..b[0].len() {
                    for ch in &b {
                        if *float {
                            w.write_sample(ch[i]).map_err(hound_err)?;
                        } else {
                            w.write_sample(quantize(ch[i], *bits as u32))
                                .map_err(hound_err)?;
                        }
                    }
                }
                n += b[0].len() as u64;
            }
            w.finalize().map_err(hound_err)?;
            Ok(n)
        }
        Writer::Flac { bits } => {
            let file = std::fs::File::create(out)?;
            write_flac(file, rate, channels, *bits, mix, cancel)
        }
        Writer::Ffmpeg { .. } => {
            let ff = ff.ok_or_else(|| SaveError::Ffmpeg("no ffmpeg".into()))?;
            let mut cmd = Command::new(&ff.ffmpeg);
            cmd.args(ffmpeg_args(plan, rate, ff.layout_option, mixed, original));
            // It dies with this thread: a cancelled worker is killed, and
            // its encoder must not write on.
            // SAFETY: one plain syscall between fork and exec.
            unsafe {
                use std::os::unix::process::CommandExt;
                cmd.pre_exec(|| {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = cmd
                .arg(out)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| SaveError::Ffmpeg(e.to_string()))?;
            let mut err = child.stderr.take();
            let said = std::thread::spawn(move || {
                let mut s = String::new();
                if let Some(e) = err.as_mut() {
                    let _ = e.read_to_string(&mut s);
                }
                s
            });
            let mut stdin = child.stdin.take();
            let fed = (|| -> Result<u64, SaveError> {
                let pipe = stdin.as_mut().ok_or(SaveError::Ffmpeg("no input".into()))?;
                let mut bytes = Vec::new();
                let mut n = 0;
                while let Some(b) = next(mix, cancel)? {
                    bytes.clear();
                    for i in 0..b[0].len() {
                        for ch in &b {
                            bytes.extend_from_slice(&ch[i].to_le_bytes());
                        }
                    }
                    pipe.write_all(&bytes)?;
                    n += b[0].len() as u64;
                }
                Ok(n)
            })();
            drop(stdin);
            // A closed pipe is ffmpeg ending early: its own words say why
            // (an option it doesn't take, a full disk), not "Broken pipe".
            let ended =
                matches!(&fed, Err(SaveError::Io(e)) if e.kind() == io::ErrorKind::BrokenPipe);
            if fed.is_err() && !ended {
                let _ = child.kill();
            }
            let status = child.wait()?;
            let said = said.join().unwrap_or_default();
            if ended || (fed.is_ok() && !status.success()) {
                if said.contains("No space left") {
                    return Err(SaveError::NoSpace);
                }
                return Err(SaveError::Ffmpeg(ffmpeg_reason(&said)));
            }
            fed
        }
    }
}

/// ffmpeg's arguments for a Save (before the output's path): the main
/// sound track's mix on stdin at `rate`, each other cleaned track from its
/// raw file, then the original, whose picture, subtitles, data streams and
/// attachments are copied, its tags, chapters and each track's own tags (a
/// title such as "Mic", a language) kept; a sound track that was not
/// cleaned is copied as it is. Every sound track stays where it was, in
/// the original's order.
fn ffmpeg_args(
    plan: &Plan,
    rate: u32,
    layout_option: &str,
    mixed: &[Option<MixedTrack>],
    original: &Path,
) -> Vec<std::ffi::OsString> {
    let Writer::Ffmpeg {
        encoder,
        bit_rate,
        bits,
        layout,
        video,
        more,
        extras,
        counts,
        starts,
    } = &plan.writer
    else {
        return Vec::new();
    };
    // F10: ffmpeg moves each input to start at zero; the cleaned sound
    // goes where its track's first sample was and the original stays
    // where it was, so a picture, the tracks copied and the chapters keep
    // their times to the sample
    let at = |track: usize| -> Option<String> {
        let st = starts.as_ref()?;
        let t = st.sounds.get(track).copied().flatten().unwrap_or(st.file);
        (t != 0.0).then(|| format!("{t:.6}"))
    };
    let file_at = starts
        .as_ref()
        .filter(|st| st.file != 0.0)
        .map(|st| format!("{:.6}", st.file));
    use std::ffi::OsString;
    fn add(a: &mut Vec<OsString>, v: &[&str]) {
        a.extend(v.iter().map(OsString::from));
    }
    let mut a: Vec<OsString> = Vec::new();
    add(&mut a, &["-nostdin", "-v", "error", "-y"]);
    add(
        &mut a,
        &[
            "-f",
            "f32le",
            "-ar",
            &rate.to_string(),
            layout_option,
            layout,
        ],
    );
    if let Some(t) = at(plan.main) {
        add(&mut a, &["-itsoffset", &t]);
    }
    add(&mut a, &["-i", "pipe:0"]);
    // the other cleaned tracks, each an input of its own (by slot)
    let mut input = Vec::with_capacity(more.len());
    let mut next = 1;
    for (k, t) in more.iter().enumerate() {
        match mixed.get(k).cloned().flatten().filter(|_| t.cleaned()) {
            Some(m) => {
                let l = t.layout.clone().unwrap_or_else(|| layout_of(m.channels));
                if let Some(s) = at(t.at) {
                    add(&mut a, &["-itsoffset", &s]);
                }
                add(&mut a, &["-f", "f32le", "-ar", &m.rate.to_string()]);
                add(&mut a, &[layout_option, &l, "-i"]);
                a.push(m.file.into_os_string());
                input.push(Some(next));
                next += 1;
            }
            None => input.push(None),
        }
    }
    let o = next.to_string();
    if let Some(t) = &file_at {
        add(&mut a, &["-itsoffset", t]);
    }
    a.push("-i".into());
    a.push(original.as_os_str().to_owned());
    // the sound tracks in their order: the main one's mix, each other one
    // cleaned, or the original's own
    let total = 1 + more.len();
    let mut sound = Vec::with_capacity(2 * total);
    for at in 0..total {
        sound.push("-map".to_string());
        sound.push(match more.iter().position(|t| t.at == at) {
            None => "0:a".to_string(),
            Some(k) => match input[k] {
                Some(i) => format!("{i}:a"),
                None => format!("{o}:a:{at}"),
            },
        });
    }
    let sound: Vec<&str> = sound.iter().map(String::as_str).collect();
    if *video {
        add(&mut a, &["-map", &format!("{o}:v")]);
        add(&mut a, &sound);
        add(
            &mut a,
            &["-map", &format!("{o}:s?"), "-c:v", "copy", "-c:s", "copy"],
        );
    } else {
        add(&mut a, &sound);
        if keeps_cover(&plan.ext) {
            // A cover picture, where there is one.
            add(&mut a, &["-map", &format!("{o}:v?"), "-c:v", "copy"]);
        }
        // Ogg, WebM, WAV, AIFF: ffmpeg can't write a picture stream there
        // (an Opus file's cover would fail the Save), so the sound alone.
    }
    if *extras {
        add(
            &mut a,
            &["-map", &format!("{o}:d?"), "-map", &format!("{o}:t?")],
        );
        add(&mut a, &["-c:d", "copy", "-c:t", "copy"]);
    }
    add(&mut a, &["-map_metadata", &o, "-map_chapters", &o]);
    // Each stream's own tags (a sound track's title, a picture's rotation
    // on ffmpeg 4, an attachment's file name), by its place: once one
    // stream's tags are mapped by hand, ffmpeg copies no other's.
    let mut tags = |kind: &str, n: usize| {
        for i in 0..n {
            let (to, from) = (
                format!("-map_metadata:s:{kind}:{i}"),
                format!("{o}:s:{kind}:{i}"),
            );
            add(&mut a, &[&to, &from]);
        }
    };
    tags("a", total);
    let pictures = *video || keeps_cover(&plan.ext);
    tags("v", if pictures { counts.video } else { 0 });
    tags("s", if *video { counts.subtitle } else { 0 });
    tags("d", if *extras { counts.data } else { 0 });
    tags("t", if *extras { counts.attachment } else { 0 });
    // AAC outside MP4 (Matroska, MPEG-TS, ADTS): its decoders keep the
    // encoder's 1024 samples of delay in front, as the original's did (an
    // MP4's edit list skips them); the copy leaves out as many of its own
    // first ones, so each sample stays at its place (F10)
    let edit_list = matches!(
        plan.ext.as_str(),
        "mp4" | "m4a" | "m4v" | "m4b" | "mov" | "3gp"
    );
    let primed = |e: &str| e == "aac" && !edit_list;
    let m = plan.main;
    if primed(encoder) {
        add(
            &mut a,
            &[&format!("-filter:a:{m}"), "atrim=start_sample=1024"],
        );
    }
    add(&mut a, &[&format!("-c:a:{m}"), encoder]);
    if let Some(b) = bit_rate {
        add(&mut a, &[&format!("-b:a:{m}"), &b.to_string()]);
    }
    let d = depth_args(encoder, *bits, m);
    add(&mut a, &d.iter().map(String::as_str).collect::<Vec<_>>());
    for (k, t) in more.iter().enumerate() {
        let n = t.at;
        match (&t.encoder, input[k]) {
            (Some(e), Some(_)) => {
                if primed(e) {
                    add(
                        &mut a,
                        &[&format!("-filter:a:{n}"), "atrim=start_sample=1024"],
                    );
                }
                add(&mut a, &[&format!("-c:a:{n}"), e]);
                if let Some(b) = t.bit_rate {
                    add(&mut a, &[&format!("-b:a:{n}"), &b.to_string()]);
                }
                let d = depth_args(e, t.bits, n);
                add(&mut a, &d.iter().map(String::as_str).collect::<Vec<_>>());
            }
            _ => add(&mut a, &[&format!("-c:a:{n}"), "copy"]),
        }
    }
    a
}

/// The sample format a lossless encoder writes at the depth Save keeps:
/// 16 bits as 16 (from float input ffmpeg would pick 32 and FLAC would come
/// out 24-bit), 24 bits in 32-bit words. PCM encoders fix their own
/// format; a 24-bit one takes 32-bit words too.
/// `track`: the output's sound stream it is for.
fn depth_args(encoder: &str, bits: Option<u32>, track: usize) -> Vec<String> {
    let planar = encoder == "alac";
    let (fmt, raw) = (
        format!("-sample_fmt:a:{track}"),
        format!("-bits_per_raw_sample:a:{track}"),
    );
    let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect();
    match bits {
        Some(24) => {
            let f = if planar { "s32p" } else { "s32" };
            v(&[&fmt, f, &raw, "24"])
        }
        Some(16) if encoder == "flac" => v(&[&fmt, "s16"]),
        Some(16) if planar => v(&[&fmt, "s16p"]),
        _ => Vec::new(),
    }
}

/// What ffmpeg said went wrong, in one line: its first error (the cause;
/// what follows is what failed because of it, such as "Nothing was written
/// into output file"), without the "[muxer @ 0x…]" in front.
pub fn ffmpeg_reason(said: &str) -> String {
    let line = said
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("failed");
    let line = match (line.starts_with('['), line.find("] ")) {
        (true, Some(at)) => &line[at + 2..],
        _ => line,
    };
    line.trim().to_string()
}

/// Containers whose cover picture ffmpeg writes as a picture stream:
/// MP3 (ID3), MP4 and its kin, FLAC and Matroska audio.
fn keeps_cover(ext: &str) -> bool {
    matches!(
        ext,
        "mp3" | "m4a" | "m4b" | "m4r" | "mp4" | "mov" | "3gp" | "flac" | "mka"
    )
}

fn next(mix: &mut Mixdown, cancel: &AtomicBool) -> Result<Option<Vec<Vec<f32>>>, SaveError> {
    if cancel.load(Ordering::Acquire) {
        return Err(SaveError::Cancelled);
    }
    mix.next_block()
}

fn hound_err(e: hound::Error) -> SaveError {
    match e {
        hound::Error::IoError(e) => e.into(),
        e => SaveError::Io(io::Error::other(e)),
    }
}

/// FLAC as it is mixed, frame by frame: the header first, with its
/// totals and checksum filled in at the end.
fn write_flac<W: Write + Seek>(
    mut w: W,
    rate: u32,
    channels: usize,
    bits: u32,
    mix: &mut Mixdown,
    cancel: &AtomicBool,
) -> Result<u64, SaveError> {
    use flacenc::bitsink::ByteSink;
    use flacenc::component::{BitRepr, Stream, StreamInfo};
    use flacenc::error::Verify;
    use flacenc::source::{Context, Fill, FrameBuf};
    let bad = |e: &dyn std::fmt::Debug| SaveError::Io(io::Error::other(format!("flac: {e:?}")));
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|e| bad(&e))?;
    let block = config.block_size;
    let mut info = StreamInfo::new(rate as usize, channels, bits as usize).map_err(|e| bad(&e))?;
    info.set_block_sizes(block, block).map_err(|e| bad(&e))?;
    let header = |info: &StreamInfo| -> Result<Vec<u8>, SaveError> {
        let mut sink = ByteSink::new();
        Stream::with_stream_info(info.clone())
            .write(&mut sink)
            .map_err(|e| bad(&e))?;
        Ok(sink.as_slice().to_vec())
    };
    let head = header(&info)?;
    w.write_all(&head)?;
    let mut fb = (
        FrameBuf::with_size(channels, block).map_err(|e| bad(&e))?,
        Context::new(bits as usize, channels),
    );
    let mut pending: Vec<i32> = Vec::new();
    let mut sink = ByteSink::new();
    let mut frames = 0u64;
    let mut ended = false;
    while !ended || !pending.is_empty() {
        if !ended && pending.len() < block * channels {
            match next(mix, cancel)? {
                Some(b) => {
                    for i in 0..b[0].len() {
                        for ch in &b {
                            pending.push(quantize(ch[i], bits));
                        }
                    }
                    frames += b[0].len() as u64;
                }
                None => ended = true,
            }
            continue;
        }
        let take = pending.len().min(block * channels);
        let chunk: Vec<i32> = pending.drain(..take).collect();
        fb.fill_interleaved(&chunk).map_err(|e| bad(&e))?;
        let number = fb.1.current_frame_number().unwrap_or(0);
        let frame =
            flacenc::encode_fixed_size_frame(&config, &fb.0, number, &info).map_err(|e| bad(&e))?;
        info.update_frame_info(&frame);
        sink.clear();
        frame.write(&mut sink).map_err(|e| bad(&e))?;
        w.write_all(sink.as_slice())?;
    }
    info.set_md5_digest(&fb.1.md5_digest());
    info.set_total_samples(frames as usize);
    // A fixed block size, as the format counts it (the last block does
    // not count): readers tell fixed from variable by min == max.
    let b = if frames > 0 && (frames as usize) < block {
        frames as usize
    } else {
        block
    };
    info.set_block_sizes(b, b).map_err(|e| bad(&e))?;
    let done = header(&info)?;
    debug_assert_eq!(done.len(), head.len());
    w.seek(SeekFrom::Start(0))?;
    w.write_all(&done)?;
    w.flush()?;
    Ok(frames)
}

/// A prepared output in the cache, at the strength `alpha`.
#[derive(Clone, Debug, PartialEq)]
pub struct Prepared {
    pub file: PathBuf,
    pub alpha: f32,
    pub plan: Plan,
}

/// Write the output for the original at `path` into the cache folder,
/// at `alpha`. A cancelled or failed one leaves nothing behind.
pub fn prepare(
    path: &Path,
    cache_dir: &Path,
    stored: usize,
    total48: u64,
    alpha: f32,
    ff: Option<&Ffmpeg>,
    cancel: &AtomicBool,
) -> Result<Prepared, SaveError> {
    let done = Arc::new(AtomicU32::new(0));
    prepare_watched(path, cache_dir, stored, total48, alpha, ff, cancel, done)
}

/// [`prepare`], saying how far it is in `done` (0 to 1, f32 bits).
#[allow(clippy::too_many_arguments)]
pub fn prepare_watched(
    path: &Path,
    cache_dir: &Path,
    stored: usize,
    total48: u64,
    alpha: f32,
    ff: Option<&Ffmpeg>,
    cancel: &AtomicBool,
    done: Arc<AtomicU32>,
) -> Result<Prepared, SaveError> {
    prepare_tracks(
        path,
        cache_dir,
        stored,
        total48,
        Tracks::default(),
        &[],
        alpha,
        ff,
        cancel,
        done,
    )
}

/// The cleaned sound of a sound track other than the main one, in its
/// cache ([`super::cache::track_dir`]): its length at 48 kHz and channels
/// kept.
pub type CleanedTrack = (u64, usize);

/// [`prepare_watched`] for every sound track: the cache holds track
/// `tracks.main`'s; `more` the others' in their order ([`Plan::slot`]),
/// None where one was not cleaned (it is copied as it is). A Save whose
/// data streams or attachments the output would not take goes again
/// without them.
#[allow(clippy::too_many_arguments)]
pub fn prepare_tracks(
    path: &Path,
    cache_dir: &Path,
    stored: usize,
    total48: u64,
    tracks: Tracks,
    more: &[Option<CleanedTrack>],
    alpha: f32,
    ff: Option<&Ffmpeg>,
    cancel: &AtomicBool,
    done: Arc<AtomicU32>,
) -> Result<Prepared, SaveError> {
    let bin = ff.map(|f| f.ffmpeg.as_path());
    let main = if ff.is_some() { tracks.main } else { 0 };
    let open = || Mixdown::open_track(path, bin, cache_dir, stored, total48, alpha, main);
    let mut mix = open()?;
    let mut plan = plan(path, mix.info(), ff, Tracks { main, ..tracks });
    // each other cleaned track, mixed into a raw file first; the work
    // split evenly between the tracks for the progress
    let todo: Vec<(usize, usize, CleanedTrack)> = plan
        .cleaned_tracks()
        .into_iter()
        .filter_map(|k| {
            let slot = plan.slot(k)?;
            Some((k, slot, more.get(slot).copied().flatten()?))
        })
        .collect();
    let share = 1.0 / (todo.len() + 1) as f32;
    let mut mixed: Vec<Option<MixedTrack>> = vec![None; plan.tracks.saturating_sub(1)];
    for (n, &(k, slot, (frames, ch))) in todo.iter().enumerate() {
        let dir = super::cache::track_dir(cache_dir, k);
        let mut m = Mixdown::open_track(path, bin, &dir, ch, frames, alpha, k)?;
        m.done = Some(done.clone());
        m.span = (n as f32 * share, (n + 1) as f32 * share);
        let file = dir.join("mixed.f32");
        if let Err(e) = mix_raw(&mut m, &file, cancel) {
            let _ = std::fs::remove_file(&file);
            return Err(e);
        }
        mixed[slot] = Some(MixedTrack {
            file,
            rate: m.info().rate,
            channels: m.info().channels,
        });
    }
    let dot = if plan.ext.is_empty() { "" } else { "." };
    let file = cache_dir.join(format!("prepared{dot}{}", plan.ext));
    let partial = cache_dir.join(format!("prepared.partial{dot}{}", plan.ext));
    let span = (todo.len() as f32 * share, 1.0);
    mix.done = Some(done.clone());
    mix.span = span;
    let mut res = write_tracks(&plan, &mut mix, &mixed, path, ff, &partial, cancel);
    // the output wouldn't take a data stream or an attachment (a camera's
    // own telemetry, say): once more without them
    if let (Err(SaveError::Ffmpeg(why)), Writer::Ffmpeg { extras, counts, .. }) =
        (&res, &mut plan.writer)
    {
        if *extras {
            let why = why.clone();
            let had = counts.data + counts.attachment;
            *extras = false;
            let _ = std::fs::remove_file(&partial);
            mix = open()?;
            mix.done = Some(done);
            mix.span = span;
            res = write_tracks(&plan, &mut mix, &mixed, path, ff, &partial, cancel);
            // what really went: an MP4's chapters are a text stream that
            // can't be copied as one, but ffmpeg writes them again from the
            // chapters it keeps, so often nothing is lost at all
            if res.is_ok() {
                let kept = ff
                    .and_then(|f| f.probe(&partial))
                    .map_or(0, |p| p.counts.data + p.counts.attachment);
                if kept < had {
                    eprintln!(
                        "[hushmic] {}: saved without {} of its data streams or attachments, \
                         which this format can't hold ({why})",
                        path.display(),
                        had - kept
                    );
                }
            }
        }
    }
    for m in mixed.iter().flatten() {
        let _ = std::fs::remove_file(&m.file);
    }
    if let Err(e) = res {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    std::fs::rename(&partial, &file)?;
    Ok(Prepared { file, alpha, plan })
}

/// A whole mix as raw f32, interleaved, into `file`.
fn mix_raw(mix: &mut Mixdown, file: &Path, cancel: &AtomicBool) -> Result<(), SaveError> {
    let mut w = BufWriter::new(std::fs::File::create(file)?);
    let mut bytes = Vec::new();
    while let Some(b) = next(mix, cancel)? {
        bytes.clear();
        for i in 0..b[0].len() {
            for ch in &b {
                bytes.extend_from_slice(&ch[i].to_le_bytes());
            }
        }
        w.write_all(&bytes)?;
    }
    w.flush()?;
    Ok(())
}

/// The first free name for the cleaned copy of `original` in `dir`:
/// `name (cleaned).ext`, then `name (cleaned 2).ext` and so on.
pub fn output_path(original: &Path, ext: &str, dir: &Path) -> PathBuf {
    output_path_marked(original, ext, dir, None)
}

/// [`output_path`] with a re-save's mark: `name (cleaned, Gentle).ext`,
/// then `name (cleaned, Gentle 2).ext` (F5: the name says which is
/// which).
pub fn output_path_marked(original: &Path, ext: &str, dir: &Path, mark: Option<&str>) -> PathBuf {
    let stem = original
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "recording".into());
    (1u32..)
        .map(|k| dir.join(cleaned_name(&stem, ext, mark, k)))
        .find(|p| !p.exists() && p.symlink_metadata().is_err())
        .expect("a free name")
}

/// The `k`th name for a cleaned copy of `stem`: "talk (cleaned).wav",
/// "talk (cleaned 2).wav", "talk (cleaned, Gentle).wav".
pub fn cleaned_name(stem: &str, ext: &str, mark: Option<&str>, k: u32) -> String {
    let ext = ext.trim_start_matches('.');
    let dot = if ext.is_empty() { "" } else { "." };
    let tag = match (mark, k) {
        (None, 1) => "cleaned".to_string(),
        (None, k) => format!("cleaned {k}"),
        (Some(m), 1) => format!("cleaned, {m}"),
        (Some(m), k) => format!("cleaned, {m} {k}"),
    };
    format!("{stem} ({tag}){dot}{ext}")
}

/// A strength in a cleaned copy's name: "Gentle", or "18 dB" for one of
/// the config file's own.
pub fn strength_mark(db: f32) -> String {
    use crate::appstate::Strength;
    match Strength::from_attn(db) {
        Strength::Gentle => "Gentle".into(),
        Strength::Medium => "Medium".into(),
        Strength::Strong => "Strong".into(),
        Strength::Maximum => "Maximum".into(),
        Strength::Custom => format!("{} dB", db.round()),
    }
}

/// The mark a Save at `db` goes under for a file last saved at `saved`:
/// none for its first Save or one at the same strength (that is "(cleaned
/// 2)"), its strength for one at another.
pub fn resave_mark(saved: Option<f32>, db: f32) -> Option<String> {
    use crate::appstate::Strength;
    let same = |a: f32| match (Strength::from_attn(a), Strength::from_attn(db)) {
        (Strength::Custom, _) | (_, Strength::Custom) => (a - db).abs() < 0.5,
        (x, y) => x == y,
    };
    saved.filter(|&a| !same(a)).map(|_| strength_mark(db))
}

/// Put a prepared output (its file) in place: in `dir`, else next to the
/// original, under the first free name, never over a file. Where it lands.
/// `cancel` stops a copy to another disk midway.
pub fn deliver(
    file: &Path,
    original: &Path,
    dir: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<PathBuf, SaveError> {
    deliver_noted(file, original, dir, None, cancel, None)
}

/// The partial copies under way are noted here (in the cache root), one
/// file each holding the copy's path, so a process that died midway has
/// its copy removed at the next start ([`sweep_partials`]).
pub const PARTIALS: &str = "partials";

/// [`deliver`], under a re-save's `mark` (see [`resave_mark`]), noting a
/// copy to another disk in `journal` while it is under way.
pub fn deliver_noted(
    file: &Path,
    original: &Path,
    dir: Option<&Path>,
    mark: Option<&str>,
    cancel: &AtomicBool,
    journal: Option<&Path>,
) -> Result<PathBuf, SaveError> {
    let ext = file
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let asked = dir.is_some();
    let dir = match dir {
        Some(d) => d.to_path_buf(),
        None => original
            .parent()
            .map(Path::to_path_buf)
            .ok_or(SaveError::OriginalGone)?,
    };
    // A bare file name's folder is the working one.
    let here = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir.as_path()
    };
    // a folder that went (after the daemon's look said it was there) is
    // never made again: the daemon falls back as for a gone one
    let gone = || {
        if asked {
            SaveError::FolderGone
        } else {
            SaveError::OriginalGone
        }
    };
    if !here.is_dir() {
        return Err(gone());
    }
    // The same disk: one more name for the prepared file. Else a copy
    // under a hidden name there first, which then gets the real one.
    for _ in 0..100 {
        let target = output_path_marked(original, &ext, &dir, mark);
        match std::fs::hard_link(file, &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => break,
        }
    }
    let name = output_path_marked(original, &ext, &dir, mark);
    let tmp = dir.join(format!(
        ".{}.{}.partial",
        name.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let note = journal.and_then(|j| note_partial(j, &tmp));
    let copied = (|| -> Result<(), SaveError> {
        let mut from = std::fs::File::open(file)?;
        let mut to = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| into_folder(e, here))?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(SaveError::Cancelled);
            }
            match from.read(&mut buf)? {
                0 => break,
                n => to.write_all(&buf[..n])?,
            }
        }
        to.sync_all()?;
        Ok(())
    })();
    let res = copied.and_then(|()| {
        for _ in 0..100 {
            let target = output_path_marked(original, &ext, &dir, mark);
            match rename_new(&tmp, &target) {
                Ok(()) => return Ok(target),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(into_folder(e, here)),
            }
        }
        Err(SaveError::Io(io::Error::other("no free name")))
    });
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    // it went while the copy ran
    let res = match res {
        Err(SaveError::Io(_)) if !here.is_dir() => Err(gone()),
        r => r,
    };
    if let Some((n, held)) = note {
        let _ = std::fs::remove_file(n);
        drop(held);
    }
    res
}

/// Note `tmp` in `journal`; the note's path.
/// The note is held while the copy runs (`held`); its name is unique
/// (`PID-…`, the pid naming the copy's hidden name).
fn note_partial(journal: &Path, tmp: &Path) -> Option<(PathBuf, super::held::Held)> {
    use std::os::unix::ffi::OsStrExt;
    std::fs::create_dir_all(journal).ok()?;
    let note = journal.join(super::held::unique(""));
    let held = super::held::hold(&note).ok()?;
    let mut f = held.file();
    f.write_all(tmp.as_os_str().as_bytes()).ok()?;
    Some((note, held))
}

/// Remove the partial copies of processes that died midway (a crash, a
/// kill, a power cut), as `journal` notes them. A note no process holds
/// any more is followed, but only to a hidden `.NAME.PID.partial` of the
/// pid it names; a held note (a copy under way) stays.
pub fn sweep_partials(journal: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(dir) = std::fs::read_dir(journal) else {
        return;
    };
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.split('-').next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        // held while it is read and removed: the copy it names is no
        // run's (see held::take)
        let Some(_taken) = super::held::take(&e.path()) else {
            continue;
        };
        if let Ok(bytes) = std::fs::read(e.path()) {
            let tmp = PathBuf::from(std::ffi::OsStr::from_bytes(&bytes));
            let ours = tmp.file_name().is_some_and(|f| {
                let f = f.to_string_lossy();
                f.starts_with('.') && f.ends_with(&format!(".{pid}.partial"))
            });
            if ours {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        let _ = std::fs::remove_file(e.path());
    }
}

/// Rename, but never over an existing file (RENAME_NOREPLACE; a file
/// system without it gets a check first).
fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| {
        std::ffi::CString::new(p.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    };
    let (f, t) = (c(from)?, c(to)?);
    // SAFETY: two valid C strings; AT_FDCWD for relative paths.
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if r == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EINVAL) | Some(libc::ENOSYS) => {
            if to.symlink_metadata().is_ok() {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            std::fs::rename(from, to)
        }
        _ => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_voice_track_is_found_by_its_title() {
        let t = |v: &[&str]| voice_track(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(t(&["Desktop Audio", "Mic/Aux"]), 1, "OBS's own names");
        assert_eq!(t(&["Music", "VO"]), 1);
        assert_eq!(t(&["Game", "Discord", "Vocals"]), 2);
        assert_eq!(t(&["", "Microphone"]), 1);
        assert_eq!(t(&["Track 1", "Track 2"]), 0, "none named: the first");
        assert_eq!(t(&["Comic", "Voiceover"]), 0, "whole words only");
        assert_eq!(t(&[]), 0);
    }

    #[test]
    fn a_track_is_asked_for_by_number_or_all() {
        assert_eq!(Pick::parse("2"), Some(Pick::One(1)));
        assert_eq!(Pick::parse("all"), Some(Pick::All));
        assert_eq!(Pick::parse("0"), None);
        assert_eq!(Pick::parse("mic"), None);
        for p in [Pick::Auto, Pick::All, Pick::One(0), Pick::One(4)] {
            assert_eq!(Pick::parse(&p.word()), Some(p));
        }
        let titles = ["Desktop Audio".to_string(), "Mic/Aux".into()];
        let one = |main| Tracks { main, all: false };
        assert_eq!(Pick::Auto.resolve(&titles), Ok(one(1)));
        assert_eq!(Pick::One(0).resolve(&titles), Ok(one(0)));
        assert_eq!(Pick::One(2).resolve(&titles), Err(2));
        assert_eq!(
            Pick::All.resolve(&titles),
            Ok(Tracks { main: 0, all: true })
        );
        // without ffmpeg (no titles): the first only
        assert_eq!(Pick::Auto.resolve(&[]), Ok(one(0)));
        assert_eq!(Pick::One(1).resolve(&[]), Err(1));
    }

    /// An HE-AAC original's rate is too low for the AAC-LC that ffmpeg
    /// writes; other encoders keep the original's rate.
    #[test]
    fn aac_never_gets_less_than_its_floor() {
        assert_eq!(lossy_rate("aac", Some(42_222), 2), Some(128_000));
        assert_eq!(lossy_rate("aac", Some(42_222), 1), Some(64_000));
        assert_eq!(lossy_rate("aac", Some(256_000), 2), Some(256_000));
        assert_eq!(lossy_rate("aac", Some(96_000), 6), Some(384_000));
        // no rate stated: the encoder's own default
        assert_eq!(lossy_rate("aac", None, 2), None);
        assert_eq!(lossy_rate("libmp3lame", Some(32_000), 2), Some(32_000));
        assert_eq!(lossy_rate("libopus", Some(24_000), 1), Some(24_000));
    }

    /// The main track (the second here) comes from the pipe, the others
    /// from the original as they are, each in its place with its tags.
    #[test]
    fn the_cleaned_track_keeps_its_place_and_the_others_are_copied() {
        let copy = |at| MoreTrack {
            at,
            encoder: None,
            bit_rate: None,
            bits: None,
            layout: None,
        };
        let plan = Plan {
            writer: Writer::Ffmpeg {
                encoder: "aac".into(),
                bit_rate: Some(96_000),
                bits: None,
                layout: "mono".into(),
                video: true,
                more: vec![copy(0), copy(2)],
                extras: false,
                counts: Counts {
                    video: 1,
                    ..Counts::default()
                },
                starts: None,
            },
            ext: "mkv".into(),
            same_format: true,
            sound_only: false,
            tracks: 3,
            dropped: 0,
            main: 1,
        };
        assert_eq!(
            (plan.slot(0), plan.slot(2), plan.slot(1)),
            (Some(0), Some(1), None)
        );
        assert!(plan.cleaned_tracks().is_empty());
        let a: Vec<String> = ffmpeg_args(&plan, 48_000, "-ch_layout", &[], Path::new("/o.mkv"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = |w: &str| a.iter().position(|x| x == w).unwrap();
        let maps: Vec<&str> = a
            .windows(2)
            .filter(|w| w[0] == "-map")
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(maps, ["1:v", "1:a:0", "0:a", "1:a:2", "1:s?"]);
        assert_eq!(a[at("-c:a:1") + 1], "aac");
        assert_eq!(a[at("-b:a:1") + 1], "96000");
        assert_eq!(a[at("-c:a:0") + 1], "copy");
        assert_eq!(a[at("-c:a:2") + 1], "copy");
        assert_eq!(a[at("-map_metadata:s:a:2") + 1], "1:s:a:2");
        // without its times: no offset; AAC in Matroska leaves out its delay
        assert!(!a.iter().any(|x| x == "-itsoffset"));
        assert_eq!(a[at("-filter:a:1") + 1], "atrim=start_sample=1024");
        // F10: its sound began before the file's zero (an encoder's
        // delay): the cleaned track there, the original as it was
        let mut plan = plan;
        if let Writer::Ffmpeg { starts, .. } = &mut plan.writer {
            *starts = Some(Starts {
                file: -0.021,
                sounds: vec![Some(-0.021), Some(0.5), None],
            });
        }
        let a: Vec<String> = ffmpeg_args(&plan, 48_000, "-ch_layout", &[], Path::new("/o.mkv"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let offsets: Vec<(&str, &str)> = a
            .windows(4)
            .filter(|w| w[0] == "-itsoffset")
            .map(|w| {
                (
                    w[1].as_str(),
                    w.iter().skip(2).find(|x| *x != "-i").unwrap().as_str(),
                )
            })
            .collect();
        assert_eq!(offsets, [("0.500000", "pipe:0"), ("-0.021000", "/o.mkv")]);
        // in an MP4 the edit list skips the delay: all of it is kept
        plan.ext = "m4a".into();
        let a = ffmpeg_args(&plan, 48_000, "-ch_layout", &[], Path::new("/o.m4a"));
        assert!(!a.iter().any(|x| x == "-filter:a:1"));
    }

    #[test]
    fn pcm_depths_by_codec_name() {
        assert_eq!(pcm_bits("pcm_s16le"), Some((16, false)));
        assert_eq!(pcm_bits("pcm_s24be"), Some((24, false)));
        assert_eq!(pcm_bits("pcm_f32le"), Some((32, true)));
        assert_eq!(pcm_bits("pcm_u8"), Some((16, false)));
        assert_eq!(pcm_bits("aac"), None);
    }

    #[test]
    fn lossless_encoders_keep_the_depth_and_float_keeps_its_format() {
        assert_eq!(depth_args("flac", Some(16), 0), ["-sample_fmt:a:0", "s16"]);
        assert_eq!(depth_args("alac", Some(16), 1), ["-sample_fmt:a:1", "s16p"]);
        assert_eq!(
            depth_args("flac", Some(24), 0),
            ["-sample_fmt:a:0", "s32", "-bits_per_raw_sample:a:0", "24"]
        );
        assert_eq!(
            depth_args("alac", Some(24), 2),
            ["-sample_fmt:a:2", "s32p", "-bits_per_raw_sample:a:2", "24"]
        );
        // a PCM encoder fixes its own format at 16 bits
        assert!(depth_args("pcm_s16le", Some(16), 0).is_empty());
        assert!(depth_args("pcm_f32le", None, 0).is_empty());
        assert!(depth_args("aac", None, 0).is_empty());
    }

    #[test]
    fn ffmpegs_reason_is_its_first_error() {
        let said = "[aost#0:0/pcm_f32le @ 0x5581] Specified sample format s32 is invalid or not supported\n\
                    Error while opening encoder - maybe incorrect parameters such as bit_rate, rate, width or height.\n\
                    [out#0/wav @ 0x5590] Nothing was written into output file, because at least one of its streams received no packets.\n";
        assert_eq!(
            ffmpeg_reason(said),
            "Specified sample format s32 is invalid or not supported"
        );
        assert_eq!(
            ffmpeg_reason("\n  Unrecognized option 'x'.\nError splitting"),
            "Unrecognized option 'x'."
        );
        assert_eq!(ffmpeg_reason(""), "failed");
    }

    #[test]
    fn quantizing_matches_how_decoders_read_back() {
        assert_eq!(quantize(0.5, 16), 16_384);
        assert_eq!(quantize(1.0, 16), 32_767);
        assert_eq!(quantize(-1.0, 16), -32_768);
        assert_eq!(quantize(-2.0, 24), -8_388_608);
        assert_eq!(quantize(3.0 / 32_768.0, 16), 3);
    }

    #[test]
    fn names_never_take_a_file_that_is_there() {
        let d = std::env::temp_dir().join(format!("hushmic-save-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let o = Path::new("/music/My talk.m4a");
        assert_eq!(output_path(o, "m4a", &d), d.join("My talk (cleaned).m4a"));
        std::fs::write(d.join("My talk (cleaned).m4a"), b"").unwrap();
        assert_eq!(output_path(o, "m4a", &d), d.join("My talk (cleaned 2).m4a"));
        // A dangling link counts as taken too.
        std::os::unix::fs::symlink("/nonexistent", d.join("My talk (cleaned 2).m4a")).unwrap();
        assert_eq!(output_path(o, "m4a", &d), d.join("My talk (cleaned 3).m4a"));
        assert_eq!(output_path(o, "flac", &d), d.join("My talk (cleaned).flac"));
        // No overwrite, even when told the name.
        std::fs::write(d.join("a"), b"a").unwrap();
        std::fs::write(d.join("b"), b"b").unwrap();
        assert_eq!(
            rename_new(&d.join("a"), &d.join("b")).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(d.join("b")).unwrap(), b"b");
        // F5: a re-save at another strength is named by it
        let marked = output_path_marked(o, "m4a", &d, Some("Gentle"));
        assert_eq!(marked, d.join("My talk (cleaned, Gentle).m4a"));
        std::fs::write(&marked, b"").unwrap();
        assert_eq!(
            output_path_marked(o, "m4a", &d, Some("Gentle")),
            d.join("My talk (cleaned, Gentle 2).m4a")
        );
        assert_eq!(resave_mark(None, 6.0), None, "the first Save");
        assert_eq!(resave_mark(Some(6.0), 6.0), None, "the same strength");
        assert_eq!(resave_mark(Some(24.0), 6.0).as_deref(), Some("Gentle"));
        assert_eq!(resave_mark(Some(24.0), 18.0).as_deref(), Some("18 dB"));
        for (db, m) in [(12.0, "Medium"), (24.0, "Strong"), (100.0, "Maximum")] {
            let name = cleaned_name("a", "wav", Some(&strength_mark(db)), 1);
            assert_eq!(name, format!("a (cleaned, {m}).wav"));
            assert!(crate::files::gather::looks_cleaned(Path::new(&name)));
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_folder_that_went_after_the_look_is_gone_and_never_made_again() {
        let d = std::env::temp_dir().join(format!("hushmic-save-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (file, original) = (d.join("prepared.flac"), d.join("talk.flac"));
        std::fs::write(&file, b"cleaned").unwrap();
        std::fs::write(&original, b"original").unwrap();
        let chosen = d.join("chosen");
        std::fs::create_dir_all(&chosen).unwrap();
        // the daemon's look: there; then it goes before the write
        assert!(chosen.is_dir());
        std::fs::remove_dir(&chosen).unwrap();
        let no = AtomicBool::new(false);
        let r = deliver(&file, &original, Some(&chosen), &no);
        assert!(matches!(r, Err(SaveError::FolderGone)), "{r:?}");
        assert!(!chosen.exists(), "never made again");
        assert_eq!(
            save_problem_of(&r),
            Some(super::super::worker::Problem::FolderGone)
        );
        // next to the original, as the daemon then falls back
        let r = deliver(&file, &original, None, &no).unwrap();
        assert_eq!(r, d.join("talk (cleaned).flac"));
        let _ = std::fs::remove_dir_all(d);
    }

    fn save_problem_of(r: &Result<PathBuf, SaveError>) -> Option<super::super::worker::Problem> {
        r.as_ref().err().map(super::super::worker::save_problem)
    }

    #[test]
    fn a_folder_that_cant_be_written_is_named_with_the_folder() {
        let d = std::env::temp_dir().join(format!("hushmic-save-ro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // a refused write into the folder, whatever refused it
        for errno in [libc::EACCES, libc::EPERM, libc::EROFS] {
            let e = into_folder(io::Error::from_raw_os_error(errno), &d);
            assert!(matches!(&e, SaveError::NoWrite(f) if *f == d), "{e:?}");
        }
        assert!(matches!(
            into_folder(io::Error::from_raw_os_error(libc::ENOSPC), &d),
            SaveError::NoSpace
        ));
        assert!(matches!(
            into_folder(io::Error::from_raw_os_error(libc::EIO), &d),
            SaveError::Io(_)
        ));
        // a real read-only folder (root writes into it anyway)
        use std::os::unix::fs::PermissionsExt;
        let (file, original) = (d.join("prepared.flac"), d.join("talk.flac"));
        std::fs::write(&file, b"cleaned").unwrap();
        std::fs::write(&original, b"original").unwrap();
        let ro = d.join("read-only");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        let no = AtomicBool::new(false);
        let r = deliver(&file, &original, Some(&ro), &no);
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("SKIP the real read-only folder: root writes into it");
        } else {
            assert!(
                matches!(&r, Err(SaveError::NoWrite(f)) if *f == ro),
                "{r:?}"
            );
            assert_eq!(
                r.as_ref().unwrap_err().to_string(),
                format!("can't write to {}", ro.display())
            );
            assert_eq!(
                save_problem_of(&r),
                Some(super::super::worker::Problem::NoWrite)
            );
            assert_eq!(std::fs::read_dir(&ro).unwrap().count(), 0, "nothing left");
        }
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_full_disk_is_named_as_such() {
        let e: SaveError = io::Error::from_raw_os_error(libc::ENOSPC).into();
        assert!(matches!(e, SaveError::NoSpace));
        let e: SaveError = io::Error::from_raw_os_error(libc::EACCES).into();
        assert!(matches!(e, SaveError::Io(_)));
    }
}
