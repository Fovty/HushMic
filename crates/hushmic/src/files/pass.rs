//! The cleaning pass: the whole file once, at full strength, into the
//! cache. Every strength, the hold and Save are mixes of this pass and the
//! original, so one pass serves them all.
//!
//! The pass starts at frame 0 as one front. A seek past the cleaned part
//! starts a second front there with a 0.5 s pre-roll, so playback can go
//! on in a moment. When the earlier front reaches the later one's start it
//! runs on 2 s over it and crossfades in 20 ms, so the later front's first,
//! less warmed-up seconds never reach the saved file. A front that is
//! through (at the end, or handed over) fills what is left, starting 2.5 s
//! early and crossfading in. Every join therefore has 2.5 s of warm-up.
//!
//! Mono recorded as stereo is cleaned once and written to both channels;
//! eight 1 s windows across the file decide it at once, and the full read
//! that follows (the exact length, the original's lane of the file's
//! picture, Matroska's seek index) confirms it or starts the pass over
//! with every channel.
//!
//! Every front adds the cleaned lane's columns of the picture
//! (`super::picture`) for the part it writes.

use super::cache::Cache;
use super::decode::{DecodeError, Decoder, Info, Via};
use super::engine::EngineSpec;
use super::picture;
use super::resample::{converted_len, Converter};
use crate::miccheck::spectrum::ROWS;
use hushmic_denoiser::{StreamDenoiser, HOP};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;

/// The engine's rate.
pub const RATE: u32 = 48_000;
/// Warm-up before a seek front's first frame: 0.5 s.
pub const PREROLL: u64 = 24_000;
/// How far a front runs over the next one's start before handing over: 2 s.
pub const JOIN: u64 = 96_000;
/// The crossfade at every hand-over: 20 ms.
pub const XFADE: u64 = 960;
/// A request this close ahead of a running front is left to it: 10 s.
pub const NEAR: u64 = 480_000;
/// Fronts at once; a further request replaces the newest.
pub const MAX_FRONTS: usize = 2;
/// Engine input per step: 250 ms.
const STEP: usize = 12_000;
/// The channel check: windows across the file, 1 s each.
const WINDOWS: u64 = 8;
/// Channels whose difference is this far below their level are the same.
const SAME_BELOW: f64 = 1e-6;

/// What the pass reports, in 48 kHz frames.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The cleaned parts so far, after every step of any front.
    Lit(Vec<(u64, u64)>),
    /// The exact length, once the full read has counted it.
    Length(u64),
    /// The file stops early: this much could be read.
    Damaged(u64),
    /// Every frame is cleaned.
    Done,
    Failed(String),
}

/// Sorted, non-overlapping cleaned ranges `[start, end)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Segments(Vec<(u64, u64)>);

impl Segments {
    pub fn ranges(&self) -> &[(u64, u64)] {
        &self.0
    }

    /// Add `[s, e)`, merging with what it touches.
    pub fn add(&mut self, s: u64, e: u64) {
        if e <= s {
            return;
        }
        let (mut s, mut e) = (s, e);
        self.0.retain(|&(a, b)| {
            if b < s || a > e {
                true
            } else {
                s = s.min(a);
                e = e.max(b);
                false
            }
        });
        let i = self.0.partition_point(|&(a, _)| a < s);
        self.0.insert(i, (s, e));
    }

    /// The range holding `at`.
    pub fn containing(&self, at: u64) -> Option<(u64, u64)> {
        self.0.iter().copied().find(|&(a, b)| a <= at && at < b)
    }

    /// The first range starting at or after `at`.
    pub fn first_from(&self, at: u64) -> Option<(u64, u64)> {
        self.0.iter().copied().find(|&(a, _)| a >= at)
    }

    /// What is not cleaned in `[0, total)`.
    pub fn gaps(&self, total: u64) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let mut at = 0;
        for &(a, b) in &self.0 {
            if a > at {
                out.push((at, a.min(total)));
            }
            at = at.max(b);
        }
        if at < total {
            out.push((at, total));
        }
        out.retain(|&(a, b)| b > a);
        out
    }

    pub fn complete(&self, total: u64) -> bool {
        self.gaps(total).is_empty()
    }
}

/// Matroska's packets by time, from the full read (see [`Decoder::seek_index`]).
type SeekIndex = Arc<Vec<(u64, u64)>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// From a seek: 0.5 s pre-roll, writes from its start.
    Seek,
    /// Filling a gap after the end: 2.5 s pre-roll, crossfades in.
    Gap,
}

/// Where a front is writing: the step it is about to add, `[from, to)`
/// (`from == to` before its first step).
type Span = Arc<Mutex<(u64, u64)>>;

struct Front {
    id: u64,
    span: Span,
    stop: Arc<AtomicBool>,
}

/// A front writing `span` is inside the gap `[a, b)`: its step overlaps
/// it, or ends where the gap begins (it goes on into it). Its end alone
/// is not enough: a step that crosses the gap's end (into the next
/// front's range) leaves the end past the gap while the step still fills
/// it.
fn inside(span: (u64, u64), (a, b): (u64, u64)) -> bool {
    span.0 < b && a <= span.1
}

struct Shared {
    path: PathBuf,
    ffmpeg: Option<PathBuf>,
    /// The sound track cleaned (0: the first).
    track: usize,
    spec: EngineSpec,
    cache_dir: PathBuf,
    info: Info,
    rate: u32,
    channels: usize,
    /// 48 kHz frames per column of the file's picture.
    span: u64,
    stored: AtomicUsize,
    cache: RwLock<Arc<Cache>>,
    segments: Mutex<Segments>,
    total: Mutex<Option<u64>>,
    index: Mutex<Option<SeekIndex>>,
    fronts: Mutex<Vec<Front>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    next_id: AtomicU64,
    /// The full read has finished and settled the channels.
    read: AtomicBool,
    done: AtomicBool,
    stopped: AtomicBool,
    events: Mutex<Sender<Event>>,
    /// Frames no front steps onto until [`Pass::release`] (tests; none
    /// in the app).
    holds: Mutex<Vec<u64>>,
    released: Condvar,
}

/// A file being cleaned.
pub struct Pass {
    shared: Arc<Shared>,
}

#[derive(Debug)]
pub enum PassError {
    Decode(DecodeError),
    Cache(std::io::Error),
    Engine(String),
}

impl std::fmt::Display for PassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PassError::Decode(e) => e.fmt(f),
            PassError::Cache(e) => write!(f, "cache: {e}"),
            PassError::Engine(e) => write!(f, "engine: {e}"),
        }
    }
}

impl std::error::Error for PassError {}

impl Pass {
    /// Open `path`, decide the channels, create the cache in `cache_dir`
    /// and start cleaning from the start. Events go to `events`.
    pub fn start(
        path: &Path,
        ffmpeg: Option<&Path>,
        spec: EngineSpec,
        cache_dir: &Path,
        events: Sender<Event>,
    ) -> Result<Pass, PassError> {
        Pass::start_track(path, ffmpeg, spec, cache_dir, 0, events)
    }

    /// Tests: [`Pass::start`] with every front held where its next step
    /// would take in one of the frames `holds`, until [`Pass::release`].
    /// A test puts its fronts where it wants them with this, whatever
    /// the threads' pace: a front runs up to its hold and no further.
    #[doc(hidden)]
    pub fn start_held(
        path: &Path,
        ffmpeg: Option<&Path>,
        spec: EngineSpec,
        cache_dir: &Path,
        events: Sender<Event>,
        holds: &[u64],
    ) -> Result<Pass, PassError> {
        Pass::start_with(path, ffmpeg, spec, cache_dir, 0, events, holds)
    }

    /// Let the fronts held by [`Pass::start_held`] go on.
    #[doc(hidden)]
    pub fn release(&self) {
        self.shared.holds.lock().unwrap().clear();
        self.shared.released.notify_all();
    }

    /// [`Pass::start`] for the file's `track`th sound track (see
    /// `Decoder::open_track`).
    pub fn start_track(
        path: &Path,
        ffmpeg: Option<&Path>,
        spec: EngineSpec,
        cache_dir: &Path,
        track: usize,
        events: Sender<Event>,
    ) -> Result<Pass, PassError> {
        Pass::start_with(path, ffmpeg, spec, cache_dir, track, events, &[])
    }

    fn start_with(
        path: &Path,
        ffmpeg: Option<&Path>,
        spec: EngineSpec,
        cache_dir: &Path,
        track: usize,
        events: Sender<Event>,
        holds: &[u64],
    ) -> Result<Pass, PassError> {
        let d = Decoder::open_track(path, ffmpeg, track).map_err(PassError::Decode)?;
        let info = d.info().clone();
        drop(d);
        // The engine must load before anything is promised.
        spec.build().map_err(PassError::Engine)?;
        let stored = if info.channels > 1 && channels_match(path, ffmpeg, track) {
            1
        } else {
            info.channels
        };
        let cache = Cache::create(cache_dir, stored).map_err(PassError::Cache)?;
        let span = picture::span_for(info.frames.map(|f| converted_len(f, info.rate, RATE)));
        cache.start_picture(span).map_err(PassError::Cache)?;
        let shared = Arc::new(Shared {
            path: path.to_path_buf(),
            ffmpeg: ffmpeg.map(Path::to_path_buf),
            track,
            spec,
            cache_dir: cache_dir.to_path_buf(),
            rate: info.rate,
            channels: info.channels,
            info,
            span,
            stored: AtomicUsize::new(stored),
            cache: RwLock::new(Arc::new(cache)),
            segments: Mutex::new(Segments::default()),
            total: Mutex::new(None),
            index: Mutex::new(None),
            fronts: Mutex::new(Vec::new()),
            threads: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
            read: AtomicBool::new(false),
            done: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            events: Mutex::new(events),
            holds: Mutex::new(holds.to_vec()),
            released: Condvar::new(),
        });
        let sh = shared.clone();
        let read = std::thread::Builder::new()
            .name("hushmic-file-read".into())
            .spawn(move || full_read(&sh))
            .map_err(|e| PassError::Engine(e.to_string()))?;
        shared.threads.lock().unwrap().push(read);
        spawn_front(&shared, 0, Kind::Seek);
        Ok(Pass { shared })
    }

    /// Channels kept in the cache: 1 when the file's channels are the same.
    pub fn stored_channels(&self) -> usize {
        self.shared.stored.load(Ordering::Acquire)
    }

    pub fn channels(&self) -> usize {
        self.shared.channels
    }

    /// What the file is, as the decoder first saw it.
    pub fn info(&self) -> &Info {
        &self.shared.info
    }

    pub fn segments(&self) -> Segments {
        self.shared.segments.lock().unwrap().clone()
    }

    pub fn total(&self) -> Option<u64> {
        *self.shared.total.lock().unwrap()
    }

    pub fn cache(&self) -> Arc<Cache> {
        self.shared.cache.read().unwrap().clone()
    }

    /// Playback wants `at` (48 kHz frames) and it is not cleaned: start a
    /// front there, unless a running one gets there soon on its own.
    pub fn request(&self, at: u64) {
        let sh = &self.shared;
        if sh.stopped.load(Ordering::Acquire)
            || sh.segments.lock().unwrap().containing(at).is_some()
        {
            return;
        }
        if sh.total.lock().unwrap().is_some_and(|t| at >= t) {
            return;
        }
        {
            let mut fronts = sh.fronts.lock().unwrap();
            if fronts.iter().any(|f| {
                let p = f.span.lock().unwrap().1;
                p <= at && at < p + NEAR
            }) {
                return;
            }
            if fronts.len() >= MAX_FRONTS {
                // Replace the newest: the oldest is the one that will
                // close the gaps behind playback.
                if let Some(f) = fronts.iter().max_by_key(|f| f.id) {
                    f.stop.store(true, Ordering::Release);
                }
                fronts.retain(|f| !f.stop.load(Ordering::Acquire));
            }
        }
        spawn_front(sh, at, Kind::Seek);
    }

    /// Stop every front and the full read, and wait for them.
    pub fn stop(&self) {
        let sh = &self.shared;
        sh.stopped.store(true, Ordering::Release);
        for f in sh.fronts.lock().unwrap().iter() {
            f.stop.store(true, Ordering::Release);
        }
        loop {
            let t = sh.threads.lock().unwrap().pop();
            match t {
                Some(t) => {
                    let _ = t.join();
                }
                None => break,
            }
        }
    }
}

impl Drop for Pass {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What became of a sound track after the first: its cleaned sound's
/// length (48 kHz frames) and channels in its cache folder, or None where
/// it could not be cleaned (Save then copies it as it is).
pub type More = Option<(u64, usize)>;

/// Clean the sound tracks `tracks` of `path` (their numbers, 1 the
/// second) one after the other, each into its own folder in `cache_dir`
/// (`cache::track_dir`), once the first is done. `progress(track, cleaned,
/// length)` hears every step; `stop()` ends it early (None). A track that
/// can't be cleaned (its decoder fails, it stops early on an error) is
/// left as it is, said in the log: the file still saves, with that track
/// copied.
pub fn clean_more(
    path: &Path,
    ffmpeg: Option<&Path>,
    spec: &EngineSpec,
    cache_dir: &Path,
    tracks: &[usize],
    stop: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(usize, u64, Option<u64>),
) -> Option<Vec<More>> {
    use std::sync::mpsc::{channel, RecvTimeoutError};
    let mut out = Vec::with_capacity(tracks.len());
    for &k in tracks {
        let dir = super::cache::track_dir(cache_dir, k);
        let (tx, rx) = channel();
        let pass = match Pass::start_track(path, ffmpeg, spec.clone(), &dir, k, tx) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[hushmic] {}: sound track {}: {e}", path.display(), k + 1);
                out.push(None);
                continue;
            }
        };
        let (mut length, mut damaged) = (None, None);
        let got = loop {
            if stop() {
                pass.stop();
                return None;
            }
            match rx.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(Event::Lit(r)) => progress(k, r.iter().map(|(a, b)| b - a).sum(), length),
                Ok(Event::Length(f)) => length = Some(f),
                Ok(Event::Damaged(f)) => damaged = Some(f),
                Ok(Event::Done) => break damaged.or(length),
                Ok(Event::Failed(e)) => {
                    eprintln!("[hushmic] {}: sound track {}: {e}", path.display(), k + 1);
                    break None;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break None,
            }
        };
        let stored = pass.stored_channels();
        pass.stop();
        out.push(got.map(|f| (f, stored)));
    }
    Some(out)
}

fn send(sh: &Shared, e: Event) {
    let _ = sh.events.lock().unwrap().send(e);
}

fn spawn_front(sh: &Arc<Shared>, start: u64, kind: Kind) {
    let id = sh.next_id.fetch_add(1, Ordering::AcqRel);
    let span: Span = Arc::new(Mutex::new((start, start)));
    let stop = Arc::new(AtomicBool::new(false));
    sh.fronts.lock().unwrap().push(Front {
        id,
        span: span.clone(),
        stop: stop.clone(),
    });
    let s2 = sh.clone();
    let t = std::thread::Builder::new()
        .name("hushmic-file-front".into())
        .spawn(move || run_front(&s2, id, start, kind, &span, &stop));
    match t {
        Ok(t) => sh.threads.lock().unwrap().push(t),
        Err(e) => {
            sh.fronts.lock().unwrap().retain(|f| f.id != id);
            send(sh, Event::Failed(e.to_string()));
        }
    }
}

enum Outcome {
    /// Reached the end, or handed over to the next front.
    Reached,
    Stopped,
}

fn run_front(
    sh: &Arc<Shared>,
    id: u64,
    start: u64,
    kind: Kind,
    pos: &Mutex<(u64, u64)>,
    stop: &AtomicBool,
) {
    let (mut start, mut kind) = (start, kind);
    loop {
        match render(sh, start, kind, pos, stop) {
            Err(e) => {
                if !stop.load(Ordering::Acquire) {
                    send(sh, Event::Failed(e));
                }
                break;
            }
            Ok(Outcome::Stopped) => break,
            Ok(Outcome::Reached) => {
                // Before the length is known, a range no front extends
                // still ends in a gap.
                let total = *sh.total.lock().unwrap();
                if let Some(g) = unclaimed_gap(sh, id, total.unwrap_or(u64::MAX), pos) {
                    start = g;
                    kind = Kind::Gap;
                    continue;
                }
                break;
            }
        }
    }
    sh.fronts.lock().unwrap().retain(|f| f.id != id);
    maybe_done(sh);
}

/// Done once every frame is cleaned, no front is still writing (a
/// hand-over overwrites cleaned frames) and the full read has settled the
/// channels. The last of the fronts and the read to finish sends it.
fn maybe_done(sh: &Shared) {
    let Some(total) = *sh.total.lock().unwrap() else {
        return;
    };
    if sh.read.load(Ordering::SeqCst)
        && sh.fronts.lock().unwrap().is_empty()
        && sh.segments.lock().unwrap().complete(total)
        && !sh.done.swap(true, Ordering::AcqRel)
    {
        send(sh, Event::Done);
    }
}

/// The first gap no other running front is inside of, claimed for this
/// front (its position moves there while the fronts are locked, so two
/// fronts never take the same gap).
fn unclaimed_gap(sh: &Shared, me: u64, total: u64, pos: &Mutex<(u64, u64)>) -> Option<u64> {
    let gaps = sh.segments.lock().unwrap().gaps(total);
    let fronts = sh.fronts.lock().unwrap();
    let g = gaps
        .into_iter()
        .find(|&(a, b)| {
            !fronts.iter().any(|f| {
                f.id != me
                    && !f.stop.load(Ordering::Acquire)
                    && inside(*f.span.lock().unwrap(), (a, b))
            })
        })
        .map(|(a, _)| a)?;
    *pos.lock().unwrap() = (g, g);
    Some(g)
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// One front, from `start` until the end or the next front's start (plus
/// the hand-over).
fn render(
    sh: &Shared,
    start: u64,
    kind: Kind,
    pos: &Mutex<(u64, u64)>,
    stop: &AtomicBool,
) -> Result<Outcome, String> {
    let rate = sh.rate as u64;
    let g = gcd(rate, RATE as u64);
    let (step48, step_src) = (RATE as u64 / g, rate / g);
    let lead_in = match kind {
        Kind::Seek => PREROLL,
        Kind::Gap => JOIN + PREROLL,
    };
    // Start on a frame where both rates meet and on the engine's hop grid,
    // so this front's samples and frames line up with every other front's.
    // Off the hop grid the sound stays further from the straight run (27.8
    // against 38.5 dB, 8 s in, measured on the Light model).
    let grid = step48 / gcd(step48, HOP as u64) * HOP as u64;
    let from48 = start.saturating_sub(lead_in) / grid * grid;
    let src_from = from48 / step48 * step_src;
    let mut dec =
        Decoder::open_track(&sh.path, sh.ffmpeg.as_deref(), sh.track).map_err(|e| e.to_string())?;
    if let Some(ix) = sh.index.lock().unwrap().clone() {
        dec.set_seek_index(ix);
    }
    if src_from > 0 {
        dec.seek(src_from).map_err(|e| e.to_string())?;
    }
    let stored = sh.stored.load(Ordering::Acquire);
    let cache = sh.cache.read().unwrap().clone();
    let mut conv = Converter::new(sh.rate, RATE, stored).ok_or("no converter for this rate")?;
    let mut engines = (0..stored)
        .map(|_| sh.spec.build())
        .collect::<Result<Vec<_>, _>>()?;
    let latency = engines[0].denoiser().latency_samples() as u64;
    // Frames fed to the engines, and given back (after the latency).
    let (mut fed, mut back) = (0u64, 0u64);
    let mut at = from48;
    let write_from = match kind {
        Kind::Seek => start,
        Kind::Gap => start.saturating_sub(XFADE),
    };
    // The picture's cleaned lane: the original (its channels' mean, from
    // frame `at` on) next to what comes back for it.
    let mut lane = picture::Cleaned::new(sh.span, write_from);
    let mut original: Vec<f32> = Vec::new();
    let put = |col: u64, hops: u16, levels: &[u8; ROWS], like: &[i8; ROWS]| {
        let _ = cache.put_cleaned(col, hops, levels, like);
    };
    let mean = |x: &[Vec<f32>]| -> Vec<f32> {
        let k = 1.0 / x.len().max(1) as f32;
        (0..x[0].len())
            .map(|i| x.iter().map(|c| c[i]).sum::<f32>() * k)
            .collect()
    };
    let mut join_at: Option<u64> = None;
    let mut input_done = false;
    while !input_done {
        wait_released(sh, at, stop);
        if stop.load(Ordering::Acquire) || sh.stopped.load(Ordering::Acquire) {
            return Ok(Outcome::Stopped);
        }
        // Up to one step of 48 kHz input.
        let mut input = vec![Vec::new(); stored];
        while input[0].len() < STEP && !input_done {
            match dec.next_block().map_err(|e| e.to_string())? {
                Some(b) => {
                    let b = if stored == 1 {
                        vec![b.into_iter().next().unwrap_or_default()]
                    } else {
                        b
                    };
                    for (i, c) in input.iter_mut().zip(conv.push(&b)) {
                        i.extend_from_slice(&c);
                    }
                }
                None => {
                    for (i, c) in input.iter_mut().zip(conv.finish()) {
                        i.extend_from_slice(&c);
                    }
                    input_done = true;
                }
            }
        }
        fed += input[0].len() as u64;
        original.extend(mean(&input));
        let mut out = run_engines(&mut engines, &input, input_done)?;
        // The engine's latency comes off the front; at the end the flush
        // brings the output to exactly the input's length.
        let skip = latency.saturating_sub(back).min(out[0].len() as u64) as usize;
        back += out[0].len() as u64;
        for o in &mut out {
            o.drain(..skip);
        }
        let got = (back.saturating_sub(latency)).min(fed);
        let keep = (got - (at - from48)) as usize;
        for o in &mut out {
            o.truncate(keep);
        }
        let n = out[0].len() as u64;
        if n == 0 {
            continue;
        }
        // Ahead of the range it is about to add, so no other front ever
        // sees this one's end as an unclaimed gap.
        *pos.lock().unwrap() = (at, at + n);
        let (handed_over, upto) =
            place(sh, &cache, write_from, start, kind, at, &out, &mut join_at)?;
        // What stands in the cache from this step, or warms the front up.
        let k = (upto.saturating_sub(at) as usize).min(original.len());
        lane.push(at, &original[..k], &mean(&out)[..k], &mut { put });
        original.drain(..(n as usize).min(original.len()));
        if handed_over {
            // To the next front, whose columns go on from here; or the
            // file ends here (the full read has counted it).
            if *sh.total.lock().unwrap() == Some(upto) {
                lane.finish(&mut { put });
            } else {
                lane.flush(&mut { put });
            }
            return Ok(Outcome::Reached);
        }
        at += n;
    }
    lane.finish(&mut { put });
    // The end of the file: this front knows the exact length.
    {
        let mut t = sh.total.lock().unwrap();
        if t.is_none() {
            *t = Some(at);
            send(sh, Event::Length(at));
        }
    }
    Ok(Outcome::Reached)
}

/// A front about to step from `at` waits while that step would take in a
/// held frame (see [`Pass::start_held`]), or until it is told to stop.
fn wait_released(sh: &Shared, at: u64, stop: &AtomicBool) {
    let mut holds = sh.holds.lock().unwrap();
    while holds.iter().any(|h| (at..at + STEP as u64).contains(h))
        && !stop.load(Ordering::Acquire)
        && !sh.stopped.load(Ordering::Acquire)
    {
        // (the stops are flags, not signals: look again in a moment)
        let wait = std::time::Duration::from_millis(20);
        holds = sh.released.wait_timeout(holds, wait).unwrap().0;
    }
}

/// Run each channel's engine on its input (in parallel for several), and
/// at the end flush them.
fn run_engines(
    engines: &mut [StreamDenoiser],
    input: &[Vec<f32>],
    flush: bool,
) -> Result<Vec<Vec<f32>>, String> {
    fn one(e: &mut StreamDenoiser, x: &[f32], flush: bool) -> Result<Vec<f32>, String> {
        let mut out = e.process(x).to_vec();
        if flush {
            let z = vec![0f32; e.pending()];
            out.extend_from_slice(e.process(&z));
        }
        match e.take_error() {
            Some(err) => Err(err.to_string()),
            None => Ok(out),
        }
    }
    if engines.len() == 1 {
        return Ok(vec![one(&mut engines[0], &input[0], flush)?]);
    }
    let jobs: Vec<_> = engines.iter_mut().zip(input).collect();
    in_groups(jobs, parallel_limit(), |(e, x)| one(e, x, flush))
        .into_iter()
        .collect()
}

/// How many channels one front runs at once: the CPUs this process may use
/// (the efficiency cores, when it steps aside), less one, shared by the
/// fronts. A 5.1 file on a small machine takes turns instead of starting
/// six threads per front.
fn parallel_limit() -> usize {
    static LIMIT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        (cpus.saturating_sub(1) / MAX_FRONTS).max(1)
    })
}

/// `f` over `items`, at most `limit` at a time, each in a thread of its
/// own when more than one runs; the results in the items' order.
fn in_groups<T: Send, R: Send>(
    items: Vec<T>,
    limit: usize,
    f: impl Fn(T) -> Result<R, String> + Sync,
) -> Vec<Result<R, String>> {
    let mut out = Vec::with_capacity(items.len());
    let mut items = items.into_iter().peekable();
    while items.peek().is_some() {
        let group: Vec<T> = items.by_ref().take(limit.max(1)).collect();
        if group.len() == 1 {
            out.extend(group.into_iter().map(&f));
            continue;
        }
        let f = &f;
        std::thread::scope(|s| {
            let hs: Vec<_> = group.into_iter().map(|x| s.spawn(move || f(x))).collect();
            out.extend(hs.into_iter().map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("engine thread panicked".into()))
            }));
        });
    }
    out
}

/// Write the cleaned frames `out` (they start at frame `at`) into the
/// cache: nothing before `write_from`, a crossfade in (gap fronts) and out
/// (at the hand-over). True when this front has handed over; and the frame
/// its sound from this step goes up to (the step's end while it warms up,
/// its start when it wrote nothing).
#[allow(clippy::too_many_arguments)]
fn place(
    sh: &Shared,
    cache: &Cache,
    write_from: u64,
    start: u64,
    kind: Kind,
    at: u64,
    out: &[Vec<f32>],
    join_at: &mut Option<u64>,
) -> Result<(bool, u64), String> {
    let n = out[0].len() as u64;
    let end = at + n;
    if end <= write_from {
        return Ok((false, end));
    }
    // The next front's start, fixed once seen (our own range grows into it).
    let (stop_at, next_end) = {
        let segs = sh.segments.lock().unwrap();
        if join_at.is_none() {
            let ahead = at.max(start);
            *join_at = segs
                .first_from(ahead)
                .map(|(a, _)| a)
                .filter(|&a| a > start || kind == Kind::Gap && a > write_from);
        }
        match *join_at {
            Some(j) => {
                let next_end = segs.containing(j).map_or(j, |(_, b)| b);
                ((j + JOIN).min(next_end.max(j)), Some(next_end))
            }
            None => (u64::MAX, None),
        }
    };
    let total = sh.total.lock().unwrap().unwrap_or(u64::MAX);
    let stop_at = stop_at.min(total);
    let lo = at.max(write_from);
    let hi = end.min(stop_at);
    if hi <= lo {
        return Ok((stop_at <= at, at));
    }
    let a = (lo - at) as usize;
    let b = (hi - at) as usize;
    let mut block: Vec<Vec<f32>> = out.iter().map(|o| o[a..b].to_vec()).collect();
    // Crossfade in (a gap front over the range before its start) and out
    // (over the next front's data before the hand-over).
    let fade_in = (kind == Kind::Gap).then_some((start.saturating_sub(XFADE), start));
    let fade_out = next_end.map(|_| (stop_at.saturating_sub(XFADE), stop_at));
    for (fs, fe) in [fade_in, fade_out].into_iter().flatten() {
        let (s, e) = (fs.max(lo), fe.min(hi));
        if e <= s {
            continue;
        }
        let old = cache.read(s, (e - s) as usize).map_err(|e| e.to_string())?;
        let coming_in = Some((fs, fe)) == fade_in;
        let k = (s - lo) as usize;
        for (ch, old) in block.iter_mut().zip(&old) {
            for (i, (v, o)) in ch[k..k + old.len()].iter_mut().zip(old).enumerate() {
                let t = (s + i as u64 - fs) as f32 / (fe - fs).max(1) as f32;
                // Ours rises over a fade in, falls over a fade out.
                let w = if coming_in { t } else { 1.0 - t };
                *v = w * *v + (1.0 - w) * o;
            }
        }
    }
    cache.write(lo, &block).map_err(|e| e.to_string())?;
    let lit = {
        let mut segs = sh.segments.lock().unwrap();
        segs.add(lo, hi);
        segs.ranges().to_vec()
    };
    send(sh, Event::Lit(lit));
    Ok((hi == stop_at, hi))
}

/// Whether the channels carry the same sound: eight 1 s windows across the
/// file (or its first 10 s when its length is not stated).
fn channels_match(path: &Path, ffmpeg: Option<&Path>, track: usize) -> bool {
    let Ok(mut d) = Decoder::open_track(path, ffmpeg, track) else {
        return false;
    };
    let rate = d.info().rate as u64;
    // (ffmpeg seeks by decoding from the start: its first 10 s only)
    let windows: Vec<u64> = match (d.info().via, d.info().frames) {
        (Via::Native, Some(n)) if n > rate * WINDOWS => {
            (0..WINDOWS).map(|k| k * n / WINDOWS).collect()
        }
        _ => vec![0],
    };
    let span = if windows.len() == 1 { rate * 10 } else { rate };
    let (mut diff, mut level) = (0f64, 0f64);
    for w in windows {
        if w > 0 && d.seek(w).is_err() {
            return false;
        }
        let mut read = 0;
        while read < span {
            let Ok(Some(b)) = d.next_block() else { break };
            let (d2, l2) = difference(&b);
            diff += d2;
            level += l2;
            read += b[0].len() as u64;
        }
    }
    diff <= SAME_BELOW * level
}

/// Energy of each channel's difference to the first, and their level.
fn difference(b: &[Vec<f32>]) -> (f64, f64) {
    let (mut diff, mut level) = (0f64, 0f64);
    for c in &b[1..] {
        for (x, y) in b[0].iter().zip(c) {
            let (x, y) = (*x as f64, *y as f64);
            diff += (x - y) * (x - y);
            level += x * x + y * y;
        }
    }
    (diff, level)
}

/// The full read: the exact length, the picture's original lane,
/// Matroska's seek index, whether the file is cut, and the channel check
/// over everything.
fn full_read(sh: &Arc<Shared>) {
    let mut d = match Decoder::open_track(&sh.path, sh.ffmpeg.as_deref(), sh.track) {
        Ok(d) => d,
        Err(e) => {
            send(sh, Event::Failed(e.to_string()));
            return;
        }
    };
    let mut pos = 0u64;
    let (mut diff, mut level) = (0f64, 0f64);
    let cache = sh.cache.read().unwrap().clone();
    let mut lane = picture::Original::new(sh.rate, sh.span);
    let mut put = |col: u64, hops: u16, levels: &[u8; ROWS]| {
        let _ = cache.put_original(col, hops, levels);
    };
    loop {
        if sh.stopped.load(Ordering::Acquire) {
            return;
        }
        let b = match d.next_block() {
            Ok(Some(b)) => b,
            Ok(None) => break,
            Err(e) => {
                send(sh, Event::Failed(e.to_string()));
                return;
            }
        };
        if b.len() > 1 {
            let (d2, l2) = difference(&b);
            diff += d2;
            level += l2;
        }
        pos += b[0].len() as u64;
        lane.push(&b, &mut put);
    }
    lane.finish(&mut put);
    if let Some(ix) = d.seek_index() {
        *sh.index.lock().unwrap() = Some(ix);
    }
    let total = converted_len(pos, sh.rate, RATE);
    if let Some(cut) = d.damaged() {
        send(sh, Event::Damaged(converted_len(cut, sh.rate, RATE)));
    }
    {
        let mut t = sh.total.lock().unwrap();
        if t.is_none() {
            *t = Some(total);
            send(sh, Event::Length(total));
        }
    }
    let same = sh.channels > 1 && diff <= SAME_BELOW * level;
    if sh.stored.load(Ordering::Acquire) == 1 && sh.channels > 1 && !same {
        restart_all_channels(sh);
    }
    sh.read.store(true, Ordering::SeqCst);
    maybe_done(sh);
}

/// The windows said the channels are the same; the full read says they
/// are not. Start over with every channel (the picture's original lane
/// stays).
fn restart_all_channels(sh: &Arc<Shared>) {
    let was = sh.cache.read().unwrap().picture_bytes().unwrap_or_default();
    loop {
        let fronts = sh.fronts.lock().unwrap();
        if fronts.is_empty() {
            // Swapped while the fronts are locked: a front requested
            // meanwhile starts on the new cache.
            let cache = match Cache::create(&sh.cache_dir, sh.channels) {
                Ok(c) => c,
                Err(e) => {
                    send(sh, Event::Failed(e.to_string()));
                    return;
                }
            };
            let _ = cache.start_picture(sh.span);
            for (col, hops, levels) in picture::originals(&was) {
                let _ = cache.put_original(col, hops, &levels);
            }
            *sh.cache.write().unwrap() = Arc::new(cache);
            sh.stored.store(sh.channels, Ordering::Release);
            *sh.segments.lock().unwrap() = Segments::default();
            break;
        }
        for f in fronts.iter() {
            f.stop.store(true, Ordering::Release);
        }
        drop(fronts);
        if sh.stopped.load(Ordering::Acquire) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    send(sh, Event::Lit(Vec::new()));
    spawn_front(sh, 0, Kind::Seek);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_take_turns_beyond_the_limit_and_keep_their_order() {
        let (now, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let r = in_groups((0..6).collect(), 4, |k: usize| {
            let n = now.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(n, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(20));
            now.fetch_sub(1, Ordering::SeqCst);
            if k == 4 {
                Err("four".to_string())
            } else {
                Ok(k * 10)
            }
        });
        assert!(most.load(Ordering::SeqCst) <= 4);
        assert_eq!(
            r,
            [Ok(0), Ok(10), Ok(20), Ok(30), Err("four".into()), Ok(50)]
        );
        // One at a time runs here, in order.
        let r = in_groups(vec![1, 2, 3], 1, |k: i32| Ok::<_, String>(k));
        assert_eq!(r, [Ok(1), Ok(2), Ok(3)]);
    }

    #[test]
    fn a_front_whose_step_crosses_a_gaps_end_is_inside_it() {
        // The first front about to add [949921, 961921) while the gap it
        // fills is [949921, 960123] (a seek front's range starts there):
        // its end alone lies past the gap, and another front took the gap
        // and wrote over the straight run before the hand-over.
        assert!(inside((949_921, 961_921), (949_921, 960_123)));
        assert!(inside((5, 10), (0, 20)), "within");
        assert!(inside((0, 0), (0, 20)), "before its first step");
        assert!(!inside((30, 40), (0, 20)), "past it");
        assert!(!inside((0, 10), (20, 30)), "before it");
        assert!(inside((10, 20), (20, 30)), "ends where it begins");
        assert!(!inside((30, 40), (20, 30)), "begins where it ends");
    }

    #[test]
    fn segments_merge_and_report_gaps() {
        let mut s = Segments::default();
        s.add(10, 20);
        s.add(30, 40);
        s.add(20, 25);
        assert_eq!(s.ranges(), &[(10, 25), (30, 40)]);
        assert_eq!(s.gaps(50), vec![(0, 10), (25, 30), (40, 50)]);
        s.add(0, 35);
        assert_eq!(s.ranges(), &[(0, 40)]);
        assert_eq!(s.containing(39), Some((0, 40)));
        assert_eq!(s.containing(40), None);
        assert_eq!(s.first_from(1), None);
        s.add(45, 50);
        assert_eq!(s.first_from(40), Some((45, 50)));
        assert!(!s.complete(50));
        s.add(40, 45);
        assert!(s.complete(50));
    }
}
