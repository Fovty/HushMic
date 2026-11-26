//! `hushmic clean FILE…`: clean recordings from a terminal, in this
//! process (no daemon), with the Files page's output and naming: the
//! original's format next to it as `name (cleaned).ext`, never over a file,
//! its voice track cleaned and every other one kept as it is (with
//! ffmpeg; `--track` picks another, or all). One line per file;
//! `--json` makes it one JSON object per file. On a terminal, how far each
//! file is shows on stderr as it goes (`--quiet`: not).

use super::cache::{bytes_needed, free_bytes, root_or_temp};
use super::decode::which;
use super::engine::{init_runtime, EngineSpec};
use super::ffmpeg::Ffmpeg;
use super::mix::alpha;
use super::pass::{clean_more, Event, More, Pass, RATE};
use super::resample::converted_len;
use super::save;
use super::worker::{save_problem, start_problem, Problem};
use crate::appstate::{Model, Strength};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// `hushmic clean`'s scratch folders in the cache root: this prefix and a
/// name of their own, each held through `NAME.lock` beside it while its
/// run lives (swept once it is not, see `jobs::sweep_stale`).
pub const SCRATCH_PREFIX: &str = "clean-";

/// A scratch folder of this run's own in `root`, held while the result
/// lives. Its name is unique: two runs at once never share one, also in
/// the Flatpak where both are pid 2.
pub fn scratch_folder(root: &Path) -> std::io::Result<(PathBuf, super::held::Held)> {
    std::fs::create_dir_all(root)?;
    loop {
        let name = super::held::unique(SCRATCH_PREFIX);
        let lock = root.join(format!("{name}.lock"));
        // held before the folder exists, so a sweep never sees it unheld
        let held = super::held::hold(&lock)?;
        match std::fs::create_dir(root.join(&name)) {
            Ok(()) => return Ok((root.join(name), held)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(&lock);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&lock);
                return Err(e);
            }
        }
    }
}

pub const USAGE: &str = "usage: hushmic clean FILE|FOLDER… [-r] \
                         [--strength gentle|medium|strong|maximum] [--model quality|light] \
                         [--track N|all] [--out DIR] [--json] [--quiet]";

/// `hushmic clean --help`.
pub const HELP: &str = "\
Usage: hushmic clean FILE|FOLDER… [OPTIONS]

Clean recordings (audio or video) here, without the running app. A folder
gives the recordings in it (its top level; -r: its subfolders too), but
not HushMic's own cleaned copies of files beside them. Each
cleaned copy goes next to its original as \"NAME (cleaned).EXT\" (then
\"(cleaned 2)\" and so on), in the original's format, never over a file.
The original is never changed. With ffmpeg installed, a video keeps its
picture and every sound track: one is cleaned (the one whose title names a
microphone or a voice, such as OBS's \"Mic/Aux\", else the first) and the
others are kept exactly as they are. Without ffmpeg, a video saves as its
sound alone (FLAC), and only its first sound track.

Options:
  --strength gentle|medium|strong|maximum
                  how much noise to remove (default: strong, as on the
                  Files page; maximum also removes the room tone between
                  words)
  --model quality|light
                  the model (default: the one in Settings; light is
                  about 2.5 times faster)
  --track N|all   the sound track to clean (1 the first), or every one
                  (default: the voice's, as above)
  -r, --recursive look into the folders' subfolders too (not linked ones)
  --out DIR       put the cleaned copies in DIR (made if it isn't there)
  --json          one JSON object per file on stdout, for scripts
  --quiet         no progress on stderr
  -h, --help      this help

Exit codes: 0 every file cleaned; 1 a file could not be cleaned, or only in
part (a damaged file: its copy holds what could be read), or a usage error.
";

/// The strength `hushmic clean` uses unless told: the Files page's.
pub const DEFAULT_DB: f32 = 24.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub files: Vec<PathBuf>,
    /// The dB cap; None = Strong, as on the Files page ([`DEFAULT_DB`]).
    pub strength: Option<f32>,
    pub model: Option<Model>,
    /// The sound track to clean (`--track`): the voice's unless told.
    pub track: save::Pick,
    pub out: Option<PathBuf>,
    pub json: bool,
    /// No progress on stderr.
    pub quiet: bool,
    /// `-r`: a folder's subfolders too.
    pub recursive: bool,
    /// `--help`: the help, and nothing else.
    pub help: bool,
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        files: Vec::new(),
        strength: None,
        model: None,
        track: save::Pick::Auto,
        out: None,
        json: false,
        quiet: false,
        recursive: false,
        help: false,
    };
    // `--strength=gentle` is `--strength gentle` (not after `--`: names)
    let mut split: Vec<String> = Vec::new();
    for a in args {
        let names = split.iter().any(|w| w == "--");
        match a.split_once('=') {
            Some((k, v))
                if !names && matches!(k, "--strength" | "--model" | "--track" | "--out") =>
            {
                split.extend([k.to_string(), v.to_string()])
            }
            _ => split.push(a.clone()),
        }
    }
    let mut it = split.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--strength" => {
                let v = value()?;
                let s = match v.as_str() {
                    "gentle" => Strength::Gentle,
                    "medium" => Strength::Medium,
                    "strong" => Strength::Strong,
                    "maximum" => Strength::Maximum,
                    _ => {
                        return Err(format!(
                            "--strength: gentle, medium, strong or maximum, not {v}"
                        ))
                    }
                };
                o.strength = s.attn_limit();
            }
            "--model" => {
                o.model = Some(match value()?.as_str() {
                    "quality" => Model::Quality,
                    "light" => Model::Light,
                    v => return Err(format!("--model: quality or light, not {v}")),
                })
            }
            "--track" => {
                let v = value()?;
                o.track = match save::Pick::parse(&v) {
                    Some(save::Pick::Auto) | None => {
                        return Err(format!("--track: a number from 1, or all, not {v}"))
                    }
                    Some(p) => p,
                };
            }
            "--out" => o.out = Some(PathBuf::from(value()?)),
            "--json" => o.json = true,
            "--quiet" | "-q" => o.quiet = true,
            "--recursive" | "-r" => o.recursive = true,
            "--help" | "-h" => o.help = true,
            "--" => o.files.extend(it.by_ref().map(PathBuf::from)),
            f if f.starts_with("--") => return Err(format!("unknown option {f}")),
            f => o.files.push(PathBuf::from(f)),
        }
    }
    if o.help {
        return Ok(o);
    }
    if o.files.is_empty() {
        return Err("no file".into());
    }
    if let Some(d) = &o.out {
        if d.exists() && !d.is_dir() {
            return Err(format!("--out: not a folder: {}", d.display()));
        }
    }
    Ok(o)
}

/// `problem` in `--json`: what went wrong, or that a folder's file was
/// left out ("skipped": not a failure; `detail` says why).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Why {
    Problem(Problem),
    Said(Said),
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Said {
    Skipped,
}

/// One file's outcome, as `--json` writes it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Outcome {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Written in the original's own format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_format: Option<bool>,
    /// The file is damaged: only this much was cleaned, in seconds to the
    /// millisecond.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub damaged_at: Option<f64>,
    /// Cleaned from a lower-rate core (HE-AAC without ffmpeg): the copy
    /// keeps the file's rate but lacks its highs.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub reduced_quality: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<Why>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The strength and model it was cleaned at ("strong", "quality").
    pub strength: String,
    pub model: String,
    /// The sound tracks the copy keeps, and those of the original it
    /// lacks (without ffmpeg only the first is kept). Always written (a
    /// script needs no special case): 0 and 0 when nothing was saved.
    pub tracks: usize,
    pub tracks_dropped: usize,
    /// The sound track cleaned (1 the first), or "all"; the others are
    /// copied as they are. Always written, as `tracks`: null until the
    /// file opened.
    pub track_cleaned: Option<TrackCleaned>,
}

/// `track_cleaned`: a number, or "all".
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum TrackCleaned {
    One(usize),
    All(&'static str),
}

/// m:ss
fn clock(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

impl Outcome {
    /// The plain line.
    pub fn line(&self) -> String {
        match (&self.output, &self.detail) {
            (Some(out), _) => {
                let mut l = format!("{} → {out}", self.file);
                if let Some(at) = self.damaged_at {
                    l.push_str(&format!(" (damaged: the first {} only)", clock(at)));
                }
                l
            }
            (None, d) => format!("{}: {}", self.file, d.as_deref().unwrap_or("failed")),
        }
    }

    /// The plain notes beside the line, on stderr.
    pub fn notes(&self) -> Vec<String> {
        let mut n = Vec::new();
        if self.output.is_none() {
            return n;
        }
        if self.reduced_quality {
            n.push(format!(
                "{}: cleaned at reduced quality. Install ffmpeg for full quality.",
                self.file
            ));
        }
        if self.tracks_dropped > 0 {
            n.push(format!(
                "{}: only the first of its {} sound tracks is kept. Install ffmpeg to keep them all.",
                self.file,
                self.tracks + self.tracks_dropped
            ));
        }
        n
    }
}

/// How far a file is, on one line of stderr that is written over (a
/// terminal only, and not with `--quiet`).
pub struct Progress {
    on: bool,
    /// "[2/5] talk.m4a" (one file: its name alone).
    label: String,
    /// When the step it shows (`what`: cleaning, saving…) began: the
    /// time left is that step's, at that step's speed.
    started: Instant,
    what: String,
    shown: Option<Instant>,
    /// What the line says now (it is written over only when this changes).
    line: String,
}

/// The line is written at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

impl Progress {
    pub fn new(on: bool, label: String) -> Progress {
        Progress {
            on,
            label,
            started: Instant::now(),
            what: String::new(),
            shown: None,
            line: String::new(),
        }
    }

    /// The line for `what` (\"cleaning\") at `done` (0 to 1): the percent
    /// and, after 10 s, the time left at the speed so far.
    pub fn line(&self, what: &str, done: Option<f32>, elapsed: Duration) -> String {
        let mut l = format!("{}: {what}", self.label);
        if let Some(f) = done.map(|f| f.clamp(0.0, 1.0)) {
            l.push_str(&format!(" {}%", (f * 100.0).floor() as u32));
            let secs = elapsed.as_secs_f64();
            if secs >= 10.0 && f > 0.01 && f < 1.0 {
                let left = secs / f as f64 - secs;
                l.push_str(&if left < 60.0 {
                    ", under a minute left".to_string()
                } else {
                    format!(", about {} min left", (left / 60.0).round() as u64)
                });
            }
        }
        l
    }

    /// Show how far it is (at most every [`PROGRESS_EVERY`]).
    pub fn show(&mut self, what: &str, done: Option<f32>) {
        if !self.on || self.shown.is_some_and(|t| t.elapsed() < PROGRESS_EVERY) {
            return;
        }
        let now = Instant::now();
        self.shown = Some(now);
        let elapsed = self.step(what, now);
        let line = self.line(what, done, elapsed);
        if line != self.line {
            let mut e = std::io::stderr().lock();
            let _ = write!(e, "\r{line}\x1b[K");
            let _ = e.flush();
            self.line = line;
        }
    }

    /// How long the step `what` has run at `now`; a new step starts the
    /// clock over (a save after a long clean must not look slow).
    pub fn step(&mut self, what: &str, now: Instant) -> Duration {
        if what != self.what {
            self.what = what.to_string();
            self.started = now;
        }
        now.saturating_duration_since(self.started)
    }

    /// The line goes: what follows is written where it was.
    pub fn clear(&mut self) {
        if self.on && !self.line.is_empty() {
            let mut e = std::io::stderr().lock();
            let _ = write!(e, "\r\x1b[K");
            let _ = e.flush();
            self.line.clear();
        }
    }
}

/// What every file is cleaned with.
struct Setup {
    model_dir: PathBuf,
    model: String,
    /// Its word for the JSON ("quality", "light").
    model_word: String,
    inference: hushmic_denoiser::Inference,
    db: f32,
    track: save::Pick,
    ffmpeg: Option<PathBuf>,
    out: Option<PathBuf>,
    /// The cache root (scratch folders, the partial copies' notes).
    root: PathBuf,
    /// Free bytes where a folder is (tests give their own).
    free: fn(&Path) -> std::io::Result<u64>,
}

fn failed(file: &Path, problem: Problem, detail: String) -> Outcome {
    Outcome {
        file: file.display().to_string(),
        output: None,
        same_format: None,
        damaged_at: None,
        reduced_quality: false,
        problem: Some(Why::Problem(problem)),
        detail: Some(detail),
        strength: String::new(),
        model: String::new(),
        tracks: 0,
        tracks_dropped: 0,
        track_cleaned: None,
    }
}

/// The JSON's word for a dB cap.
fn strength_word(db: f32) -> String {
    match Strength::from_attn(db) {
        Strength::Gentle => "gentle".into(),
        Strength::Medium => "medium".into(),
        Strength::Strong => "strong".into(),
        Strength::Maximum => "maximum".into(),
        Strength::Custom => format!("{db} dB"),
    }
}

/// The cleaned sound must fit where it is kept while it cleans, as the
/// daemon checks once a file opens (a length the file does not state is
/// not checked; the write then fails on its own).
fn room(file: &Path, need: u64, free: std::io::Result<u64>) -> Result<(), Box<Outcome>> {
    let free = free.unwrap_or(u64::MAX);
    if need <= free {
        return Ok(());
    }
    let mb = |b: u64| b.div_ceil(1_000_000);
    Err(Box::new(failed(
        file,
        Problem::NoSpace,
        format!(
            "not enough space to clean it: it needs {} MB, {} MB are free",
            mb(need),
            mb(free)
        ),
    )))
}

fn clean_one(file: &Path, s: &Setup, scratch: &Path, show: &mut Progress) -> Outcome {
    let mut o = clean_tracks(file, s, scratch, show);
    show.clear();
    (o.strength, o.model) = (strength_word(s.db), s.model_word.clone());
    o
}

/// [`clean_one`]: the sound track it cleans, any other one it cleans too
/// (`--track all`), then the copy (how far each is on `show`).
fn clean_tracks(file: &Path, s: &Setup, scratch: &Path, show: &mut Progress) -> Outcome {
    let spec = EngineSpec::new(&s.model_dir, &s.model, s.inference);
    let ff = s.ffmpeg.as_deref().and_then(Ffmpeg::at);
    let tracks = match s.track.resolve(&save::track_titles(file, ff.as_ref())) {
        Ok(t) => t,
        Err(count) => {
            let has = match count {
                1 => "it has one sound track".to_string(),
                n => format!("it has {n} sound tracks"),
            };
            let detail = format!("there is no sound track {} ({has})", s.track.word());
            return failed(file, Problem::Failed, detail);
        }
    };
    let (tx, rx) = channel();
    let started = Pass::start_track(
        file,
        s.ffmpeg.as_deref(),
        spec.clone(),
        scratch,
        tracks.main,
        tx,
    );
    let pass = match started {
        Ok(p) => p,
        Err(e) => {
            let (kind, detail) = start_problem(file, &e, s.ffmpeg.is_some());
            return failed(file, kind, detail);
        }
    };
    let i = pass.info().clone();
    let reduced = i.full_rate.is_some();
    let plan = save::plan(file, &i, ff.as_ref(), tracks);
    let more = plan.cleaned_tracks();
    let stated = i.frames.map(|f| converted_len(f, i.rate, RATE));
    if let Some(f) = stated {
        let need = bytes_needed(f, pass.stored_channels()) * (1 + more.len()) as u64;
        if let Err(o) = room(file, need, (s.free)(scratch)) {
            pass.stop();
            return *o;
        }
    }
    // each sound track counts the same in how far the file is
    let share = 1.0 / (1 + more.len()) as f32;
    let (mut length, mut damaged) = (None, None);
    let end = loop {
        match rx.recv_timeout(PROGRESS_EVERY) {
            Ok(Event::Length(f)) => length = Some(f),
            Ok(Event::Damaged(f)) => damaged = Some(f),
            Ok(Event::Lit(r)) => {
                let lit: u64 = r.iter().map(|(a, b)| b - a).sum();
                let of = length.or(stated).filter(|&t| t > 0);
                show.show(
                    "cleaning",
                    of.map(|t| (lit as f64 / t as f64) as f32 * share),
                );
            }
            Ok(Event::Done) => break Ok(()),
            Ok(Event::Failed(d)) => break Err(d),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Err("the pass ended early".into()),
        }
    };
    let stored = pass.stored_channels();
    pass.stop();
    if let Err(d) = end {
        return failed(file, Problem::Failed, d);
    }
    let Some(total) = damaged.or(length) else {
        return failed(file, Problem::Failed, "no length".into());
    };
    // the other sound tracks it cleans, one after the other
    let mut cleaned: Vec<More> = vec![None; plan.tracks.saturating_sub(1)];
    if !more.is_empty() {
        let mut n = 0;
        let mut last = None;
        let done = clean_more(
            file,
            s.ffmpeg.as_deref(),
            &spec,
            scratch,
            &more,
            &|| false,
            &mut |track, lit, frames| {
                if last != Some(track) {
                    n += usize::from(last.is_some());
                    last = Some(track);
                }
                let f = frames.or(Some(total)).filter(|&t| t > 0);
                show.show(
                    "cleaning",
                    f.map(|t| share * (1.0 + n as f32 + lit as f32 / t as f32)),
                );
            },
        )
        .unwrap_or_default();
        for (&k, m) in more.iter().zip(done) {
            if let Some(c) = plan.slot(k).and_then(|slot| cleaned.get_mut(slot)) {
                *c = m;
            }
        }
    }
    // the copy, off this thread so how far it is shows
    let no = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicU32::new(0));
    let (txp, rxp) = channel();
    {
        let (file, scratch, ff, no, done) = (
            file.to_path_buf(),
            scratch.to_path_buf(),
            ff.clone(),
            no.clone(),
            done.clone(),
        );
        let alpha = alpha(s.db);
        std::thread::spawn(move || {
            let r = save::prepare_tracks(
                &file,
                &scratch,
                stored,
                total,
                tracks,
                &cleaned,
                alpha,
                ff.as_ref(),
                &no,
                done,
            );
            let _ = txp.send(r);
        });
    }
    let prepared = loop {
        match rxp.recv_timeout(PROGRESS_EVERY) {
            Ok(r) => break r,
            Err(RecvTimeoutError::Timeout) => {
                show.show("saving", Some(f32::from_bits(done.load(Ordering::Relaxed))));
            }
            Err(RecvTimeoutError::Disconnected) => break Err(save::SaveError::Cancelled),
        }
    };
    let res = prepared.and_then(|p| {
        let journal = s.root.join(save::PARTIALS);
        let out = save::deliver_noted(&p.file, file, s.out.as_deref(), None, &no, Some(&journal))?;
        Ok((out, p))
    });
    match res {
        Ok((out, p)) => Outcome {
            file: file.display().to_string(),
            output: Some(out.display().to_string()),
            same_format: Some(p.plan.same_format),
            damaged_at: damaged.map(|f| (f as f64 / 48.0).round() / 1000.0),
            reduced_quality: reduced,
            problem: None,
            detail: None,
            strength: String::new(),
            model: String::new(),
            tracks: p.plan.tracks.max(1),
            tracks_dropped: p.plan.dropped,
            track_cleaned: Some(match tracks.all && p.plan.tracks > 1 {
                true => TrackCleaned::All("all"),
                false => TrackCleaned::One(p.plan.main + 1),
            }),
        },
        Err(e) => failed(file, save_problem(&e), e.to_string()),
    }
}

/// `hushmic clean FILE…`. Exit codes: 0 all cleaned, 1 some not or only
/// in part (a damaged file: its copy is written, up to where it could be
/// read), or a usage error (as every command's).
pub fn main(args: &[String]) -> i32 {
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("hushmic clean: {e}\n{USAGE}");
            return 1;
        }
    };
    if o.help {
        print!("{HELP}");
        return 0;
    }
    if let Some(d) = &o.out {
        if let Err(e) = std::fs::create_dir_all(d) {
            eprintln!("hushmic clean: --out {}: {e}", d.display());
            return 1;
        }
    }
    // Beside a call, the microphone's engine comes first, as for the
    // daemon's workers (C-1): idle priority, the efficiency cores.
    super::worker::step_aside();
    let cfg = crate::config::Config::load();
    let paths = crate::controller::Paths::resolve();
    // The native engine needs no runtime; the load error says when one is
    // missing.
    let _ = init_runtime(&paths.dylib);
    let model = o.model.unwrap_or_else(|| Model::from_id(&cfg.model));
    let setup = Setup {
        model_dir: paths.model_dir,
        model: o.model.map_or(cfg.model.clone(), |m| m.id().to_string()),
        model_word: match model {
            Model::Quality => "quality".into(),
            Model::Light => "light".into(),
        },
        inference: match cfg.inference {
            crate::config::Inference::Auto => hushmic_denoiser::Inference::Auto,
            crate::config::Inference::Onnx => hushmic_denoiser::Inference::Onnx,
        },
        db: o.strength.unwrap_or(DEFAULT_DB),
        track: o.track,
        ffmpeg: which("ffmpeg"),
        out: o.out.clone(),
        root: root_or_temp(),
        free: free_bytes,
    };
    // What an earlier run that was interrupted left (C-2).
    super::jobs::sweep_stale(&setup.root);
    let (base, held) = match scratch_folder(&setup.root) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "hushmic clean: no scratch folder in {}: {e}",
                setup.root.display()
            );
            return 1;
        }
    };
    // how far each file is, on a terminal
    // SAFETY: isatty on a descriptor number.
    let tty = unsafe { libc::isatty(libc::STDERR_FILENO) } == 1;
    // folders open into their recordings, as a drop on the page does
    let g = super::gather::gather(&o.files, o.recursive);
    let mut failures = 0;
    // what this run cleaned them at, also on the lines of files it didn't
    let at = |mut out: Outcome| {
        (out.strength, out.model) = (strength_word(setup.db), setup.model_word.clone());
        out
    };
    for folder in &g.empty {
        let none = at(failed(
            folder,
            Problem::NotMedia,
            "no audio or video files in it".into(),
        ));
        failures += 1;
        if o.json {
            println!("{}", serde_json::to_string(&none).unwrap_or_default());
        } else {
            eprintln!("{}", none.line());
        }
    }
    if o.json {
        // a folder's left-out files: a line each, not failures
        for (file, why) in left_out(&g) {
            let mut skipped = at(failed(&file, Problem::NotMedia, why.into()));
            skipped.problem = Some(Why::Said(Said::Skipped));
            println!("{}", serde_json::to_string(&skipped).unwrap_or_default());
        }
    } else {
        for note in gathered_notes(&g) {
            eprintln!("{note}");
        }
    }
    let n = g.files.len();
    for (k, file) in g.files.iter().enumerate() {
        let scratch = base.join(k.to_string());
        let name = file.file_name().map_or_else(
            || file.display().to_string(),
            |f| f.to_string_lossy().into_owned(),
        );
        let label = if n > 1 {
            format!("[{}/{n}] {name}", k + 1)
        } else {
            name
        };
        let mut show = Progress::new(tty && !o.quiet, label);
        let outcome = clean_one(file, &setup, &scratch, &mut show);
        let _ = std::fs::remove_dir_all(&scratch);
        // A damaged file's copy holds only what could be read: a script
        // must not take it for the whole (C-4).
        failures += usize::from(outcome.output.is_none() || outcome.damaged_at.is_some());
        if o.json {
            println!("{}", serde_json::to_string(&outcome).unwrap_or_default());
        } else if outcome.output.is_some() {
            println!("{}", outcome.line());
        } else {
            eprintln!("{}", outcome.line());
        }
        if !o.json {
            for note in outcome.notes() {
                eprintln!("{note}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_file(base.with_extension("lock"));
    drop(held);
    i32::from(failures > 0)
}

/// The files a command line's folders left out, and why.
fn left_out(g: &super::gather::Gathered) -> Vec<(PathBuf, &'static str)> {
    let not = g
        .not_media
        .iter()
        .map(|p| (p.clone(), "not an audio or video file"));
    let copies = g
        .skipped
        .iter()
        .map(|p| (p.clone(), "HushMic's cleaned copy of a file beside it"));
    not.chain(copies).collect()
}

/// What a command line's folders left out, one line each (stderr).
pub fn gathered_notes(g: &super::gather::Gathered) -> Vec<String> {
    let mut n: Vec<String> = left_out(g)
        .into_iter()
        .map(|(p, why)| format!("{}: left out, {why}", p.display()))
        .collect();
    if g.capped {
        n.push(format!(
            "only the first {} recordings in the folders are taken",
            super::gather::MOST
        ));
    }
    n
}

/// `hushmic open-files --help` (it starts nothing).
pub const OPEN_FILES_HELP: &str = "\
Usage: hushmic open-files FILE|FOLDER… [-r]

Put recordings (audio or video files) on the Files page of the running
HushMic and show its window there: what the file manager's \"Open with
HushMic\" does. HushMic starts when it isn't running. A folder gives the
recordings in it (its top level; -r, --recursive: its subfolders too, not
linked ones), but not HushMic's own cleaned copies of files beside them.

Exit codes: 0 every file is on the page; 1 a file could not be added (it
says why), or no file was given.
";

/// How long a HushMic started by [`open_files`] gets to take requests.
const START_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// `hushmic open-files FILE…` ("Open with"): each file onto the running
/// instance's Files page, then the window there. Without a running one, it
/// starts one (tray only, the window comes with the files). Exit 0 when
/// every file is on the page, 1 otherwise (or for no file).
pub fn open_files(args: &[String]) -> i32 {
    use crate::control::client_run;
    if args.iter().any(|h| h == "--help" || h == "-h") {
        print!("{OPEN_FILES_HELP}");
        return 0;
    }
    let recursive = args.iter().any(|a| a == "-r" || a == "--recursive");
    let given: Vec<PathBuf> = args
        .iter()
        .filter(|a| *a != "-r" && *a != "--recursive")
        .map(PathBuf::from)
        .collect();
    if let Some(bad) = given
        .iter()
        .find(|p| p.to_string_lossy().starts_with('-') && !p.exists())
    {
        eprintln!(
            "hushmic open-files: unknown option {}\nusage: hushmic open-files FILE|FOLDER… [-r]",
            bad.display()
        );
        return 1;
    }
    let g = super::gather::gather(&given, recursive);
    for note in gathered_notes(&g) {
        eprintln!("{note}");
    }
    for folder in &g.empty {
        eprintln!("{}: no audio or video files in it", folder.display());
    }
    let args: Vec<String> = g.files.iter().map(|p| p.display().to_string()).collect();
    if args.is_empty() {
        if given.is_empty() {
            eprintln!("usage: hushmic open-files FILE|FOLDER… [-r]");
        }
        return 1;
    }
    let add = |f: &String| {
        let p = std::path::absolute(f).unwrap_or_else(|_| f.into());
        let json = serde_json::to_string(&p.to_string_lossy()).unwrap_or_default();
        client_run(&["files".to_string(), "add".into(), json])
    };
    let mut first = add(&args[0]);
    if first.0 == 2 {
        if let Err(e) = start_detached() {
            eprintln!("hushmic open-files: could not start HushMic: {e}");
            return 1;
        }
        let deadline = std::time::Instant::now() + START_WAIT;
        while first.0 == 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(250));
            first = add(&args[0]);
        }
        if first.0 == 2 {
            eprintln!(
                "hushmic open-files: HushMic did not start; what it said is in {}",
                start_log().display()
            );
        }
    }
    let mut failed = 0;
    for (k, f) in args.iter().enumerate() {
        let (code, out) = if k == 0 { first.clone() } else { add(f) };
        if code != 0 {
            eprintln!("{f}: {out}");
            failed += 1;
        }
    }
    if failed < args.len() {
        let _ = client_run(&["files".to_string(), "open".into()]);
    }
    i32::from(failed > 0 || !g.empty.is_empty())
}

/// Set on the instance [`open_files`] starts: its output goes to
/// [`start_log`], which it keeps small (see [`keep_start_log_small`]).
pub const START_LOG_ENV: &str = "HUSHMIC_START_LOG";

/// What an instance started by "Open with" prints, beside the
/// diagnostics log (a terminal or the desktop's journal has it otherwise).
pub fn start_log() -> PathBuf {
    crate::diagnostics::log_path().with_file_name("open-files.log")
}

/// The most [`start_log`] grows to before it starts over.
const START_LOG_CAP: u64 = 1024 * 1024;

/// In the instance [`open_files`] started: a thread that empties its log
/// once it passes [`START_LOG_CAP`] (it is opened for appending, so the
/// writes go on at the new end).
pub fn keep_start_log_small() {
    let _ = std::thread::Builder::new()
        .name("start-log".into())
        .spawn(|| loop {
            trim_if_large(libc::STDERR_FILENO, START_LOG_CAP);
            std::thread::sleep(std::time::Duration::from_secs(60));
        });
}

/// Empty the regular file behind `fd` once it is larger than `cap`.
fn trim_if_large(fd: libc::c_int, cap: u64) -> bool {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fstat and ftruncate on a descriptor, no memory handed over
    // but the zeroed stat.
    unsafe {
        libc::fstat(fd, &mut st) == 0
            && st.st_mode & libc::S_IFMT == libc::S_IFREG
            && st.st_size as u64 > cap
            && libc::ftruncate(fd, 0) == 0
    }
}

/// `hushmic --tray` in a session of its own, so it outlives this process.
/// Its output goes to [`start_log`], so a start that fails says why.
fn start_detached() -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let log = start_log();
    let out = log
        .parent()
        .map(std::fs::create_dir_all)
        .and_then(Result::ok)
        .and_then(|_| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
                .ok()
        });
    let (stdout, stderr) = match out.and_then(|f| {
        // A fresh log for each start.
        f.set_len(0).ok()?;
        Some((f.try_clone().ok()?, f))
    }) {
        Some((a, b)) => (Stdio::from(a), Stdio::from(b)),
        None => (Stdio::null(), Stdio::null()),
    };
    let mut c = crate::selfexe::command();
    c.arg("--tray")
        .env(START_LOG_ENV, "1")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    // SAFETY: one plain syscall between fork and exec.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c.spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn options_parse_and_usage_errors_say_why() {
        let o = parse(&args(&["a.wav", "--strength", "strong", "b.mp4", "--json"])).unwrap();
        assert_eq!(o.files, [PathBuf::from("a.wav"), PathBuf::from("b.mp4")]);
        assert_eq!((o.strength, o.json, o.model), (Some(24.0), true, None));
        assert!(!o.quiet && !o.help);
        assert!(parse(&args(&["a.wav", "--quiet"])).unwrap().quiet);
        // --help alone is fine; it prints the help and nothing else runs
        assert!(parse(&args(&["--help"])).unwrap().help);
        assert!(parse(&args(&["-h"])).unwrap().help);
        // --out may name a folder that isn't there yet (it is made)
        let o = parse(&args(&["a.wav", "--out", "/nonexistent/dir"])).unwrap();
        assert_eq!(o.out.as_deref(), Some(Path::new("/nonexistent/dir")));
        assert!(HELP.contains("default: strong"));
        let o = parse(&args(&["--model", "light", "--", "--odd name.wav"])).unwrap();
        assert_eq!((o.model, o.files.len()), (Some(Model::Light), 1));
        // GNU style, but not for a name after `--`
        let o = parse(&args(&[
            "--strength=gentle",
            "--model=light",
            "--",
            "--out=x.wav",
        ]))
        .unwrap();
        assert_eq!(o.strength, Strength::Gentle.attn_limit());
        assert_eq!(o.model, Some(Model::Light));
        assert_eq!(o.files, [PathBuf::from("--out=x.wav")]);
        assert!(parse(&args(&["a.wav", "--strength=loud"])).is_err());
        for bad in [
            &["--strength", "loud", "a.wav"][..],
            &["--model", "big", "a.wav"][..],
            &["--json"][..],
            &["a.wav", "--out", "/etc/hostname"][..],
            &["a.wav", "--fast"][..],
            &["a.wav", "--strength"][..],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn lines_are_calm_and_say_what_happened() {
        let mut o = Outcome {
            file: "talk.m4a".into(),
            output: Some("talk (cleaned).m4a".into()),
            same_format: Some(true),
            damaged_at: None,
            reduced_quality: false,
            problem: None,
            detail: None,
            strength: "strong".into(),
            model: "quality".into(),
            track_cleaned: None,
            tracks: 1,
            tracks_dropped: 0,
        };
        assert_eq!(o.line(), "talk.m4a → talk (cleaned).m4a");
        assert!(o.notes().is_empty());
        let json = serde_json::to_string(&o).unwrap();
        assert!(!json.contains("reduced"), "{json}");
        // the track fields always, defaults too (a stable schema)
        assert!(
            json.ends_with(
                r#""strength":"strong","model":"quality","tracks":1,"tracks_dropped":0,"track_cleaned":null}"#
            ),
            "{json}"
        );
        o.reduced_quality = true;
        assert_eq!(
            o.notes(),
            ["talk.m4a: cleaned at reduced quality. Install ffmpeg for full quality."]
        );
        assert!(serde_json::to_string(&o)
            .unwrap()
            .contains(r#""reduced_quality":true"#));
        o.reduced_quality = false;
        // an OBS recording: every track kept, or (no ffmpeg) the first only
        o.tracks = 2;
        assert!(serde_json::to_string(&o).unwrap().contains(r#""tracks":2"#));
        (o.tracks, o.tracks_dropped) = (1, 1);
        assert_eq!(
            o.notes(),
            ["talk.m4a: only the first of its 2 sound tracks is kept. Install ffmpeg to keep them all."]
        );
        o.tracks_dropped = 0;
        o.damaged_at = Some(192.4);
        assert_eq!(
            o.line(),
            "talk.m4a → talk (cleaned).m4a (damaged: the first 3:12 only)"
        );
        let f = failed(
            Path::new("x.xyz"),
            Problem::Unsupported,
            "not supported".into(),
        );
        assert_eq!(f.line(), "x.xyz: not supported");
        assert_eq!(
            serde_json::to_string(&f).unwrap(),
            r#"{"file":"x.xyz","problem":"unsupported","detail":"not supported","strength":"","model":"","tracks":0,"tracks_dropped":0,"track_cleaned":null}"#
        );
        assert_eq!(strength_word(24.0), "strong");
        assert_eq!(strength_word(100.0), "maximum");
    }

    #[test]
    fn progress_says_how_far_and_how_long_is_left() {
        let p = Progress::new(true, "[2/5] talk.m4a".into());
        let at = |s| Duration::from_secs(s);
        assert_eq!(
            p.line("cleaning", Some(0.02), at(1)),
            "[2/5] talk.m4a: cleaning 2%"
        );
        // after 10 s, the time left at the speed so far
        assert_eq!(
            p.line("cleaning", Some(0.25), at(60)),
            "[2/5] talk.m4a: cleaning 25%, about 3 min left"
        );
        assert_eq!(
            p.line("saving", Some(0.5), at(20)),
            "[2/5] talk.m4a: saving 50%, under a minute left"
        );
        assert_eq!(p.line("cleaning", None, at(30)), "[2/5] talk.m4a: cleaning");
    }

    /// SF-C3: a save after a long clean counts from when saving began
    /// (it said "about 55 min left" for a 33 s save).
    #[test]
    fn the_time_left_is_the_steps_own() {
        let mut p = Progress::new(true, "long.wav".into());
        let t0 = Instant::now();
        let at = |s| t0 + Duration::from_secs(s);
        assert_eq!(p.step("cleaning", at(0)), Duration::ZERO);
        assert_eq!(p.step("cleaning", at(60)), Duration::from_secs(60));
        // saving starts at 60 s: 20 s into it, half done
        assert_eq!(p.step("saving", at(60)), Duration::ZERO);
        let into = p.step("saving", at(80));
        assert_eq!(into, Duration::from_secs(20));
        assert_eq!(
            p.line("saving", Some(0.5), into),
            "long.wav: saving 50%, under a minute left"
        );
        // and the 10 s gate is the step's too: no guess 5 s into a save
        let mut q = Progress::new(true, "long.wav".into());
        q.step("cleaning", at(0));
        q.step("saving", at(60));
        let early = q.step("saving", at(65));
        assert_eq!(q.line("saving", Some(0.05), early), "long.wav: saving 5%");
    }

    #[test]
    fn clean_one_checks_the_space_once_the_file_is_open() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let models = repo.join("assets/models");
        if !models.join("dpdfnet2_48khz_hr.onnx").exists() {
            eprintln!("SKIP: models not provisioned");
            return;
        }
        let root = std::env::temp_dir().join(format!("hushmic-cli-room-{}", std::process::id()));
        let setup = Setup {
            model_dir: models,
            model: crate::controller::LIGHT_MODEL.into(),
            model_word: "light".into(),
            inference: hushmic_denoiser::Inference::Auto,
            db: 12.0,
            track: save::Pick::Auto,
            ffmpeg: None,
            out: Some(root.clone()),
            root: root.clone(),
            free: |_| Ok(1_000),
        };
        let file = repo.join("tests/fixtures/files/master_48k_mono.wav");
        let o = clean_one(
            &file,
            &setup,
            &root.join("scratch"),
            &mut Progress::new(false, String::new()),
        );
        assert_eq!(
            o.problem,
            Some(Why::Problem(Problem::NoSpace)),
            "{:?}",
            o.detail
        );
        assert!(o.output.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_file_that_would_not_fit_is_not_cleaned() {
        let f = Path::new("long.flac");
        assert!(room(f, 5_000_000, Ok(5_000_000)).is_ok());
        // A folder whose free space cannot be read is not held against it.
        assert!(room(f, 5_000_000, Err(std::io::ErrorKind::NotFound.into())).is_ok());
        let o = room(f, 1_200_000_000, Ok(300_000_000)).unwrap_err();
        assert_eq!(o.problem, Some(Why::Problem(Problem::NoSpace)));
        assert_eq!(
            o.line(),
            "long.flac: not enough space to clean it: it needs 1200 MB, 300 MB are free"
        );
    }

    #[test]
    fn the_start_log_starts_over_once_it_is_large() {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        let p = std::env::temp_dir().join(format!("hushmic-start-log-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .unwrap();
        f.write_all(&[b'x'; 100]).unwrap();
        assert!(!trim_if_large(f.as_raw_fd(), 100), "at the cap: kept");
        f.write_all(b"y").unwrap();
        assert!(trim_if_large(f.as_raw_fd(), 100));
        // Appending goes on at the new end, with no hole before it.
        f.write_all(b"after\n").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"after\n");
        // Not a file (a terminal, a pipe, /dev/null): left alone.
        let null = std::fs::File::open("/dev/null").unwrap();
        assert!(!trim_if_large(null.as_raw_fd(), 0));
        let _ = std::fs::remove_file(p);
    }
}
