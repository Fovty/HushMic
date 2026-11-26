//! The file worker: one recording's pass in its own process, which the
//! daemon starts as `hushmic clean --worker …` and can stop (SIGSTOP) and
//! continue at any moment. Commands come on stdin, one per line; what
//! happens goes to stdout as JSON lines. The end of stdin (the daemon is
//! gone) stops the pass like `stop`.

use super::decode::{DecodeError, Info, Via};
use super::engine::{init_runtime, EngineSpec};
use super::ffmpeg::Ffmpeg;
use super::mix::alpha;
use super::pass::{clean_more, Event, More, Pass, PassError, RATE};
use super::resample::converted_len;
use super::save::{self, Pick, SaveError, Tracks};
use hushmic_denoiser::Inference;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The cleaned parts go out at most this often, the rate the daemon
/// publishes progress at (the pass reports every 250 ms of sound, many
/// times a second); everything else at once.
const LIT_EVERY: Duration = Duration::from_millis(250);

/// `hushmic clean --worker --cache DIR --model ID [--inference onnx]
/// [--ffmpeg PATH] [--track N|all] [--prepare DB --cleaned
/// FRAMES:CHANNELS [--more FRAMES:CHANNELS|copy]…] [--fake] FILE`
#[derive(Clone, Debug, PartialEq)]
pub struct Args {
    pub file: PathBuf,
    pub cache: PathBuf,
    pub model: String,
    pub inference: Inference,
    pub ffmpeg: Option<PathBuf>,
    /// The sound track it cleans (the voice's unless asked).
    pub track: Pick,
    /// Tests of the daemon's jobs: no engine, a made-up 10 s file that
    /// lights in 1 s (see [`fake`]).
    pub fake: bool,
    /// No pass: the cache holds the whole file; write the output for Save
    /// at this strength and exit.
    pub prepare: Option<Prepare>,
    /// No pass: a first look at the file ([`super::probe`]), said as
    /// [`Msg::Probed`], then exit.
    pub probe: bool,
}

/// The output for Save, from a cache that holds the whole file.
#[derive(Clone, Debug, PartialEq)]
pub struct Prepare {
    /// The strength: the dB cap, as in the Settings.
    pub db: f32,
    /// The cleaned length, 48 kHz frames.
    pub frames: u64,
    /// Channels in the cache.
    pub stored: usize,
    /// The other sound tracks, in order (`save::Plan::slot`): cleaned
    /// (their length and channels, each in its folder of the cache), or
    /// copied as they are (None).
    pub more: Vec<More>,
}

impl Args {
    pub fn parse(args: &[String]) -> Result<Args, String> {
        let (mut cache, mut model, mut ffmpeg, mut file) = (None, None, None, None);
        let mut inference = Inference::Auto;
        let mut track = Pick::Auto;
        let (mut fake, mut probe) = (false, false);
        let (mut db, mut cleaned) = (None, None);
        let mut more = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
            match a.as_str() {
                "--cache" => cache = Some(PathBuf::from(value()?)),
                "--model" => model = Some(value()?),
                "--ffmpeg" => ffmpeg = Some(PathBuf::from(value()?)),
                "--track" => {
                    let v = value()?;
                    track = Pick::parse(&v).ok_or(format!("--track N or all, not {v}"))?;
                }
                // the hidden worker's own argv: only a daemon that fakes
                // its workers passes it (`fake_file_worker` in main.rs,
                // test-seams builds and the tests)
                "--fake" => fake = true,
                "--probe" => probe = true,
                "--prepare" => {
                    let v = value()?;
                    db = Some(
                        v.parse::<f32>()
                            .ok()
                            .filter(|d| !d.is_nan())
                            .ok_or(format!("--prepare DB, not {v}"))?,
                    );
                }
                "--cleaned" => {
                    let v = value()?;
                    let bad = || format!("--cleaned FRAMES:CHANNELS, not {v}");
                    let (f, c) = v.split_once(':').ok_or_else(bad)?;
                    let n = |x: &str| x.parse().map_err(|_| bad());
                    cleaned = Some((n(f)?, n(c)? as usize));
                }
                "--more" => {
                    let v = value()?;
                    if v == "copy" {
                        more.push(None);
                    } else {
                        let bad = || format!("--more FRAMES:CHANNELS or copy, not {v}");
                        let (f, c) = v.split_once(':').ok_or_else(bad)?;
                        more.push(Some((
                            f.parse().map_err(|_| bad())?,
                            c.parse().map_err(|_| bad())?,
                        )));
                    }
                }
                "--inference" => {
                    inference = match value()?.as_str() {
                        "auto" => Inference::Auto,
                        "onnx" => Inference::Onnx,
                        v => return Err(format!("--inference: auto or onnx, not {v}")),
                    }
                }
                f if !f.starts_with("--") && file.is_none() => file = Some(PathBuf::from(f)),
                other => return Err(format!("unexpected {other}")),
            }
        }
        Ok(Args {
            file: file.ok_or("no file")?,
            cache: cache.ok_or("no --cache")?,
            model: model.ok_or("no --model")?,
            inference,
            ffmpeg,
            track,
            fake,
            probe,
            prepare: match (db, cleaned) {
                (Some(db), Some((frames, stored))) => Some(Prepare {
                    db,
                    frames,
                    stored,
                    more,
                }),
                (None, None) if more.is_empty() => None,
                _ => return Err("--prepare and --cleaned go together".into()),
            },
        })
    }

    /// The command line that `parse` reads back.
    pub fn to_args(&self) -> Vec<String> {
        let mut v = vec![
            "--cache".to_string(),
            self.cache.display().to_string(),
            "--model".into(),
            self.model.clone(),
        ];
        if self.inference == Inference::Onnx {
            v.extend(["--inference".into(), "onnx".into()]);
        }
        if let Some(f) = &self.ffmpeg {
            v.extend(["--ffmpeg".into(), f.display().to_string()]);
        }
        if self.track != Pick::Auto {
            v.extend(["--track".into(), self.track.word()]);
        }
        if let Some(p) = &self.prepare {
            v.extend(["--prepare".into(), p.db.to_string()]);
            v.extend(["--cleaned".into(), format!("{}:{}", p.frames, p.stored)]);
            for m in &p.more {
                v.push("--more".into());
                v.push(match m {
                    Some((f, c)) => format!("{f}:{c}"),
                    None => "copy".into(),
                });
            }
        }
        if self.fake {
            v.push("--fake".into());
        }
        if self.probe {
            v.push("--probe".into());
        }
        v.push(self.file.display().to_string());
        v
    }
}

/// What a worker says, one JSON object per line. Frames are 48 kHz.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Msg {
    /// The file is open and the engine loaded. `frames` is the stated
    /// length (a guide until `length`); `stored` is 1 for mono recorded as
    /// stereo.
    Opened {
        rate: u32,
        channels: usize,
        stored: usize,
        frames: Option<u64>,
        video: bool,
        codec: String,
        ffmpeg: bool,
        /// Read below its quality: HE-AAC without ffmpeg (its core only).
        #[serde(default)]
        reduced: bool,
        /// The sound tracks Save keeps (an OBS recording has one per
        /// source), of those how many are cleaned (the rest are copied as
        /// they are: no encoder for them), and those Save can't keep
        /// (without ffmpeg only the first is).
        #[serde(default = "one")]
        tracks: usize,
        #[serde(default = "one")]
        cleaning: usize,
        #[serde(default)]
        dropped: usize,
        /// The sound track cleaned and played (0: the first), and every
        /// sound track's title (empty without; none without ffmpeg).
        #[serde(default)]
        track: usize,
        #[serde(default)]
        titles: Vec<String>,
    },
    /// Another sound track is being cleaned (`track`: its place, 0 the
    /// first), once the main one is done: how much of it, and its length
    /// once known.
    More {
        track: usize,
        cleaned: u64,
        frames: Option<u64>,
    },
    /// That track is through: its cleaned sound (length, channels), or
    /// None when it could not be cleaned (Save copies it as it is).
    MoreDone {
        track: usize,
        cleaned: Option<(u64, usize)>,
    },
    /// The cleaned parts so far. Empty after a restart with every channel.
    Lit { ranges: Vec<(u64, u64)> },
    /// The channels in the cache changed (the file's channels turned out
    /// to differ after all): the pass starts over with all of them.
    Stored { stored: usize },
    /// The exact length.
    Length { frames: u64 },
    /// The file stops early: this much can be cleaned.
    Damaged { frames: u64 },
    /// Every frame is cleaned.
    Done,
    /// The worker gives up; it exits after this.
    Error { kind: Problem, detail: String },
    /// What Save writes for this file: its extension, whether in the
    /// original's own format, and whether a video saves as its sound only.
    Output {
        ext: String,
        same_format: bool,
        sound_only: bool,
    },
    /// How far the output for Save is (`--prepare`), 0 to 1.
    Preparing { done: f32 },
    /// The output for Save is ready in the cache (`--prepare`), at this
    /// strength.
    Prepared { db: f32, file: String },
    /// The output could not be written (`--prepare`; it exits after this).
    SaveFailed { kind: Problem, detail: String },
    /// The first look (`--probe`): what is wrong with the file, if
    /// anything (it exits after this).
    Probed {
        problem: Option<Problem>,
        detail: String,
    },
}

fn one() -> usize {
    1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Problem {
    /// A video without a sound track.
    NoSound,
    /// Not a format HushMic (or ffmpeg) reads.
    Unsupported,
    /// The file cannot be read.
    Unreadable,
    /// The model or the runtime would not load.
    Engine,
    /// The cache folder cannot be written.
    Cache,
    /// The pass stopped on an error of its own.
    Failed,
    /// Not enough free disk for the cleaned sound (the daemon's check).
    NoSpace,
    /// The worker could not start, or ended without a word.
    Crashed,
    /// The original is not where it was (Save).
    Moved,
    /// The folder a Save was to go into is gone (the daemon falls back:
    /// next to the original, or in a sandbox the page asks where).
    FolderGone,
    /// The folder a Save goes into can't be written (permission denied, a
    /// read-only file system).
    NoWrite,
    /// The encoder (ffmpeg) failed to write the output's format (Save).
    Encode,
    /// An MP4 or MOV cut off before its index was written (a phone's
    /// recording that stopped short): nothing can read it.
    Incomplete,
    /// Not audio or video at all (text, a document, noise).
    NotMedia,
    /// The file has nothing in it (0 bytes).
    Empty,
}

/// What the daemon says, one per line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Playback wants this frame (48 kHz): start a front there if needed.
    Front(u64),
    Stop,
}

impl Command {
    pub fn parse(line: &str) -> Option<Command> {
        let mut w = line.split_whitespace();
        let c = match (w.next()?, w.next()) {
            ("front", Some(at)) => Command::Front(at.parse().ok()?),
            ("stop", None) => Command::Stop,
            _ => return None,
        };
        w.next().is_none().then_some(c)
    }

    pub fn line(self) -> String {
        match self {
            Command::Front(at) => format!("front {at}\n"),
            Command::Stop => "stop\n".into(),
        }
    }
}

/// Why a pass didn't start on `file`, and its detail: a decode error as the
/// first look ([`super::probe::open_problem`]) names it (an incomplete
/// recording, not audio or video), the rest as [`pass_problem`].
pub fn start_problem(file: &Path, e: &PassError, ffmpeg: bool) -> (Problem, String) {
    match e {
        PassError::Decode(d) => {
            let (kind, detail) = super::probe::open_problem(file, d, ffmpeg);
            (kind, problem_detail(kind, &detail))
        }
        _ => (pass_problem(e), e.to_string()),
    }
}

/// A problem's detail in plain words where the decoder's would mislead
/// ("format not supported" for a cut-off M4A).
pub fn problem_detail(kind: Problem, detail: &str) -> String {
    match kind {
        Problem::Incomplete => {
            format!("the file is incomplete (the recording was probably cut off): {detail}")
        }
        Problem::NotMedia => format!("this isn't an audio or video file: {detail}"),
        Problem::Empty => "the file is empty (0 bytes)".to_string(),
        _ => detail.to_string(),
    }
}

pub fn pass_problem(e: &PassError) -> Problem {
    match e {
        PassError::Decode(DecodeError::NoSound) => Problem::NoSound,
        PassError::Decode(DecodeError::Unsupported(_)) => Problem::Unsupported,
        PassError::Decode(_) => Problem::Unreadable,
        PassError::Engine(_) => Problem::Engine,
        PassError::Cache(_) => Problem::Cache,
    }
}

pub fn save_problem(e: &SaveError) -> Problem {
    match e {
        SaveError::NoSpace => Problem::NoSpace,
        SaveError::OriginalGone => Problem::Moved,
        SaveError::FolderGone => Problem::FolderGone,
        SaveError::NoWrite(_) => Problem::NoWrite,
        SaveError::Decode(_) => Problem::Unreadable,
        SaveError::Ffmpeg(_) => Problem::Encode,
        SaveError::Io(_) | SaveError::Cancelled => Problem::Failed,
    }
}

/// A failed Save's detail, short enough for the page: an I/O error
/// without its "(os error N)", at most [`SHORT_REASON`] characters.
pub fn short_reason(detail: &str) -> String {
    let d = detail.trim();
    let d = match d.rfind(" (os error ") {
        Some(at) if d.ends_with(')') => &d[..at],
        _ => d,
    };
    if d.chars().count() <= SHORT_REASON {
        return d.to_string();
    }
    let mut s: String = d.chars().take(SHORT_REASON - 1).collect();
    s.push('…');
    s
}

/// The longest reason a failed Save's title carries.
pub const SHORT_REASON: usize = 60;

/// What Save will write for the file, as the page shows it, and its plan.
fn output(path: &Path, info: &Info, ff: Option<&Ffmpeg>, tracks: Tracks) -> (Msg, save::Plan) {
    let p = save::plan(path, info, ff, tracks);
    let m = Msg::Output {
        ext: p.ext.clone(),
        same_format: p.same_format,
        sound_only: p.sound_only,
    };
    (m, p)
}

fn say(out: &mut impl Write, m: &Msg) -> bool {
    let Ok(mut line) = serde_json::to_string(m) else {
        return false;
    };
    line.push('\n');
    out.write_all(line.as_bytes())
        .and_then(|_| out.flush())
        .is_ok()
}

/// Run one file until `stop`, the end of `input`, or an error. The exit
/// code: 0 when stopped (cleaned or not), 1 after an `error` line.
pub fn serve(
    args: &Args,
    model_dir: &Path,
    runtime: &Path,
    input: impl BufRead + Send + 'static,
    out: &mut impl Write,
) -> i32 {
    // The native engine runs without ONNX Runtime; a missing runtime only
    // matters when the engine needs it, and then the load error says so.
    let runtime_note = init_runtime(runtime).err();
    let spec = EngineSpec::new(model_dir, &args.model, args.inference);
    let ff = args.ffmpeg.as_deref().and_then(Ffmpeg::at);
    let titles = save::track_titles(&args.file, ff.as_ref());
    let tracks = match args.track.resolve(&titles) {
        Ok(t) => t,
        Err(count) => {
            let detail = format!(
                "there is no sound track {} (it has {count})",
                args.track.word()
            );
            say(
                out,
                &Msg::Error {
                    kind: Problem::Failed,
                    detail,
                },
            );
            return 1;
        }
    };
    let (tx, rx) = channel();
    let started = Pass::start_track(
        &args.file,
        args.ffmpeg.as_deref(),
        spec,
        &args.cache,
        tracks.main,
        tx,
    );
    let pass = match started {
        Ok(p) => Arc::new(p),
        Err(e) => {
            let (kind, mut detail) = start_problem(&args.file, &e, args.ffmpeg.is_some());
            if let (PassError::Engine(_), Some(n)) = (&e, runtime_note) {
                detail.push_str(&format!(" (runtime: {n})"));
            }
            say(out, &Msg::Error { kind, detail });
            return 1;
        }
    };
    let i = pass.info();
    let (said_output, plan) = output(&args.file, i, ff.as_ref(), tracks);
    // the main track first, then any other one Save cleans
    let more = plan.cleaned_tracks();
    let opened = Msg::Opened {
        rate: i.rate,
        channels: i.channels,
        stored: pass.stored_channels(),
        frames: i.frames.map(|f| converted_len(f, i.rate, RATE)),
        video: i.video,
        codec: i.codec.clone(),
        ffmpeg: i.via == Via::Ffmpeg,
        reduced: i.full_rate.is_some(),
        tracks: plan.tracks,
        cleaning: 1 + more.len(),
        dropped: plan.dropped,
        track: plan.main,
        titles,
    };
    if !say(out, &opened) || !say(out, &said_output) {
        return 0;
    }
    let mut stored = pass.stored_channels();
    let quit = Arc::new(AtomicBool::new(false));
    {
        let (pass, quit) = (pass.clone(), quit.clone());
        let _ = std::thread::Builder::new()
            .name("hushmic-file-commands".into())
            .spawn(move || {
                for line in input.lines() {
                    match line.ok().as_deref().and_then(Command::parse) {
                        Some(Command::Front(at)) => pass.request(at),
                        Some(Command::Stop) => break,
                        None => {}
                    }
                }
                quit.store(true, Ordering::Release);
                pass.stop();
            });
    }
    let mut lit: Option<Vec<(u64, u64)>> = None;
    let mut last = Instant::now() - LIT_EVERY;
    loop {
        let msg = match rx.recv_timeout(LIT_EVERY) {
            Ok(Event::Lit(r)) => {
                lit = Some(r);
                None
            }
            Ok(Event::Length(frames)) => Some(Msg::Length { frames }),
            Ok(Event::Damaged(frames)) => Some(Msg::Damaged { frames }),
            // the main track is through: the others, then done
            Ok(Event::Done) if !more.is_empty() => {
                if let Some(r) = lit.take() {
                    say(out, &Msg::Lit { ranges: r });
                }
                let spec = EngineSpec::new(model_dir, &args.model, args.inference);
                let q = quit.clone();
                let mut said = Instant::now() - LIT_EVERY;
                let done = clean_more(
                    &args.file,
                    args.ffmpeg.as_deref(),
                    &spec,
                    &args.cache,
                    &more,
                    &move || q.load(Ordering::Acquire),
                    &mut |track, cleaned, frames| {
                        if said.elapsed() >= LIT_EVERY {
                            said = Instant::now();
                            say(
                                out,
                                &Msg::More {
                                    track,
                                    cleaned,
                                    frames,
                                },
                            );
                        }
                    },
                );
                let Some(done) = done else { break };
                for (&track, cleaned) in more.iter().zip(done) {
                    say(out, &Msg::MoreDone { track, cleaned });
                }
                Some(Msg::Done)
            }
            Ok(Event::Done) => Some(Msg::Done),
            Ok(Event::Failed(detail)) => Some(Msg::Error {
                kind: Problem::Failed,
                detail,
            }),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if quit.load(Ordering::Acquire) {
            break;
        }
        // A new cache before its cleaned parts.
        if lit.is_some() && pass.stored_channels() != stored {
            stored = pass.stored_channels();
            if !say(out, &Msg::Stored { stored }) {
                break;
            }
        }
        // The cleaned parts go before anything that follows them.
        if lit.is_some() && (msg.is_some() || last.elapsed() >= LIT_EVERY) {
            if !say(
                out,
                &Msg::Lit {
                    ranges: lit.take().unwrap_or_default(),
                },
            ) {
                break;
            }
            last = Instant::now();
        }
        if let Some(m) = msg {
            if !say(out, &m) {
                break;
            }
            if matches!(m, Msg::Error { .. }) {
                pass.stop();
                return 1;
            }
        }
    }
    pass.stop();
    0
}

/// `hushmic clean --worker …`: the paths the daemon's plugin uses, stdin
/// and stdout.
pub fn main(args: &[String]) -> i32 {
    let a = match Args::parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("hushmic clean --worker: {e}");
            return 2;
        }
    };
    let stdin = std::io::BufReader::new(std::io::stdin());
    if a.probe {
        step_aside();
        let found = if a.fake {
            None
        } else {
            super::probe::check(&a.file, a.ffmpeg.as_deref())
        };
        let (problem, detail) = match found {
            Some((kind, detail)) => (Some(kind), problem_detail(kind, &detail)),
            None => (None, String::new()),
        };
        say(
            &mut std::io::stdout().lock(),
            &Msg::Probed { problem, detail },
        );
        return 0;
    }
    if a.fake {
        return fake(&a, stdin, &mut std::io::stdout().lock());
    }
    step_aside();
    if let Some(p) = a.prepare.clone() {
        return prepare(&a, p, stdin, &mut std::io::stdout().lock());
    }
    let paths = crate::controller::Paths::resolve();
    serve(
        &a,
        &paths.model_dir,
        &paths.dylib,
        stdin,
        &mut std::io::stdout().lock(),
    )
}

/// `--prepare`: write the output for Save into the cache, then say where.
/// `stop` or the end of `input` cancels it. The exit code: 0 when written
/// or cancelled, 1 after `save_failed`.
pub fn prepare(
    args: &Args,
    p: Prepare,
    input: impl BufRead + Send + 'static,
    out: &mut impl Write,
) -> i32 {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = cancel.clone();
        let _ = std::thread::Builder::new()
            .name("hushmic-file-commands".into())
            .spawn(move || {
                for line in input.lines() {
                    if line.ok().as_deref().and_then(Command::parse) == Some(Command::Stop) {
                        break;
                    }
                }
                cancel.store(true, Ordering::Release);
            });
    }
    let done = Arc::new(AtomicU32::new(0));
    let (tx, rx) = channel();
    {
        let (a, cancel, done) = (args.clone(), cancel.clone(), done.clone());
        std::thread::spawn(move || {
            let ff = a.ffmpeg.as_deref().and_then(Ffmpeg::at);
            // the track its pass cleaned: the daemon names it
            let titles = save::track_titles(&a.file, ff.as_ref());
            let tracks = a.track.resolve(&titles).unwrap_or_default();
            let res = save::prepare_tracks(
                &a.file,
                &a.cache,
                p.stored,
                p.frames,
                tracks,
                &p.more,
                alpha(p.db),
                ff.as_ref(),
                &cancel,
                done,
            );
            let _ = tx.send(res);
        });
    }
    let mut said = 0.0;
    let res = loop {
        match rx.recv_timeout(LIT_EVERY) {
            Ok(r) => break r,
            Err(RecvTimeoutError::Timeout) => {
                let now = f32::from_bits(done.load(Ordering::Relaxed));
                if now > said {
                    said = now;
                    say(out, &Msg::Preparing { done: now });
                }
            }
            Err(RecvTimeoutError::Disconnected) => break Err(SaveError::Cancelled),
        }
    };
    match res {
        Ok(done) => {
            say(
                out,
                &Msg::Prepared {
                    db: p.db,
                    file: done.file.display().to_string(),
                },
            );
            0
        }
        Err(SaveError::Cancelled) => 0,
        Err(e) => {
            say(
                out,
                &Msg::SaveFailed {
                    kind: save_problem(&e),
                    detail: e.to_string(),
                },
            );
            1
        }
    }
}

/// Out of everything else's way: SCHED_IDLE, which every thread the pass
/// starts inherits, and on hybrid Intel the efficiency cores, the mirror of
/// the filter chain's pin to the performance cores (HUSHMIC_NO_CPU_PIN=1
/// turns both pins off). `hushmic clean` steps aside the same way.
pub(crate) fn step_aside() {
    let p = libc::sched_param { sched_priority: 0 };
    // SAFETY: a plain syscall on this (the main) thread, before any other.
    unsafe { libc::sched_setscheduler(0, libc::SCHED_IDLE, &p) };
    if std::env::var_os("HUSHMIC_NO_CPU_PIN").is_some() {
        return;
    }
    let Ok(list) = std::fs::read_to_string("/sys/devices/cpu_atom/cpus") else {
        return;
    };
    let ecores = crate::controller::parse_cpu_list(list.trim());
    let Some(allowed) = crate::controller::current_affinity() else {
        return;
    };
    let Some(cores) = crate::controller::pin_intersection(&ecores, &allowed) else {
        return;
    };
    // SAFETY: a zeroed cpu_set_t filled with ids below CPU_SETSIZE.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for c in cores {
            if c < libc::CPU_SETSIZE as usize {
                libc::CPU_SET(c, &mut set);
            }
        }
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

/// The worker without an engine, for tests of the daemon's jobs: the file
/// must exist; it says it is 10 s of mono, lights 0.5 s every 50 ms and is
/// done after 1 s, then waits for `stop` like the real one. A stopped
/// (SIGSTOP) fake stops lighting, so the tests can see it. With
/// `--prepare` it writes `prepared.wav` (its strength as text) after
/// 200 ms, unless stopped first; a file whose name has "nosave" in it
/// fails then instead.
pub fn fake(args: &Args, input: impl BufRead + Send + 'static, out: &mut impl Write) -> i32 {
    const FRAMES: u64 = 480_000;
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in input.lines() {
            if line.ok().as_deref().and_then(Command::parse) == Some(Command::Stop) {
                break;
            }
        }
        let _ = tx.send(());
    });
    if let Some(p) = &args.prepare {
        if rx.recv_timeout(Duration::from_millis(200)).is_ok() {
            return 0;
        }
        if args.file.to_string_lossy().contains("nosave") {
            say(
                out,
                &Msg::SaveFailed {
                    kind: Problem::Failed,
                    detail: "fake".into(),
                },
            );
            return 1;
        }
        let file = args.cache.join("prepared.wav");
        let _ = std::fs::write(&file, p.db.to_string());
        say(
            out,
            &Msg::Prepared {
                db: p.db,
                file: file.display().to_string(),
            },
        );
        return 0;
    }
    if let Err(e) = std::fs::metadata(&args.file) {
        say(
            out,
            &Msg::Error {
                kind: Problem::Unreadable,
                detail: e.to_string(),
            },
        );
        return 1;
    }
    if std::fs::create_dir_all(&args.cache)
        .and_then(|_| std::fs::write(args.cache.join("clean.f32"), b""))
        .is_err()
    {
        say(
            out,
            &Msg::Error {
                kind: Problem::Cache,
                detail: args.cache.display().to_string(),
            },
        );
        return 1;
    }
    let opened = Msg::Opened {
        rate: RATE,
        channels: 1,
        stored: 1,
        frames: Some(FRAMES),
        video: false,
        codec: "fake".into(),
        ffmpeg: false,
        reduced: false,
        tracks: 1,
        cleaning: 1,
        dropped: 0,
        track: 0,
        titles: Vec::new(),
    };
    say(out, &opened);
    say(
        out,
        &Msg::Output {
            ext: "wav".into(),
            same_format: true,
            sound_only: false,
        },
    );
    for k in 1..=20 {
        if rx.recv_timeout(Duration::from_millis(50)).is_ok() {
            return 0;
        }
        let ranges = vec![(0, FRAMES * k / 20)];
        if !say(out, &Msg::Lit { ranges }) {
            return 0;
        }
    }
    say(out, &Msg::Length { frames: FRAMES });
    say(out, &Msg::Done);
    let _ = rx.recv();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_round_trip() {
        let a = Args {
            file: "/music/a b.flac".into(),
            cache: "/c/1".into(),
            model: "dpdfnet8_48khz_hr".into(),
            inference: Inference::Onnx,
            ffmpeg: Some("/usr/bin/ffmpeg".into()),
            track: Pick::One(1),
            fake: true,
            probe: false,
            prepare: Some(Prepare {
                db: 12.5,
                frames: 480_000,
                stored: 2,
                more: vec![Some((470_000, 1)), None],
            }),
        };
        assert_eq!(Args::parse(&a.to_args()), Ok(a.clone()));
        let plain = Args {
            inference: Inference::Auto,
            ffmpeg: None,
            fake: false,
            probe: false,
            prepare: None,
            ..a
        };
        assert_eq!(Args::parse(&plain.to_args()), Ok(plain.clone()));
        assert!(Args::parse(&["--cache".into()]).is_err());
        let mut half = plain.to_args();
        half.splice(0..0, ["--prepare".to_string(), "6".into()]);
        assert!(Args::parse(&half).is_err(), "--prepare needs --cleaned");
        assert!(Args::parse(&["x.wav".into(), "y.wav".into()]).is_err());
    }

    #[test]
    fn commands_parse_strictly() {
        assert_eq!(Command::parse("front 48000"), Some(Command::Front(48_000)));
        assert_eq!(Command::parse("stop"), Some(Command::Stop));
        assert_eq!(
            Command::parse(Command::Front(7).line().trim()),
            Some(Command::Front(7))
        );
        for bad in ["front", "front -1", "front 1 2", "stop now", "", "go 1"] {
            assert_eq!(Command::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn messages_are_tagged_json_lines() {
        let m = Msg::Lit {
            ranges: vec![(0, 48_000)],
        };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"ev":"lit","ranges":[[0,48000]]}"#
        );
        let e = Msg::Error {
            kind: Problem::NoSound,
            detail: "x".into(),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(s, r#"{"ev":"error","kind":"no_sound","detail":"x"}"#);
        assert_eq!(serde_json::from_str::<Msg>(&s).unwrap(), e);
        assert_eq!(
            serde_json::to_string(&Msg::Done).unwrap(),
            r#"{"ev":"done"}"#
        );
        let f = Msg::SaveFailed {
            kind: Problem::Moved,
            detail: "x".into(),
        };
        let s = serde_json::to_string(&f).unwrap();
        assert_eq!(s, r#"{"ev":"save_failed","kind":"moved","detail":"x"}"#);
    }

    #[test]
    fn every_failed_save_has_a_kind_the_page_names() {
        use std::path::PathBuf;
        let io = |n| SaveError::Io(std::io::Error::from_raw_os_error(n));
        for (e, kind) in [
            (SaveError::NoSpace, Problem::NoSpace),
            (SaveError::OriginalGone, Problem::Moved),
            (SaveError::FolderGone, Problem::FolderGone),
            (SaveError::NoWrite(PathBuf::from("/ro")), Problem::NoWrite),
            (
                SaveError::Ffmpeg("Invalid argument".into()),
                Problem::Encode,
            ),
            (SaveError::Decode("truncated".into()), Problem::Unreadable),
            (io(libc::EIO), Problem::Failed),
            (SaveError::Cancelled, Problem::Failed),
        ] {
            assert_eq!(save_problem(&e), kind, "{e}");
        }
        assert_eq!(
            short_reason(&io(libc::EIO).to_string()),
            "Input/output error"
        );
        assert_eq!(short_reason(" no free name "), "no free name");
        let long = "x".repeat(200);
        let s = short_reason(&long);
        assert_eq!(s.chars().count(), SHORT_REASON);
        assert!(s.ends_with('…'));
    }
}
