//! Playing a file on the Files page: the cleaned sound from the cache,
//! mixed with the original (decoded again as it plays) at the strength on
//! screen, and the original alone while held.
//!
//! Like Mic check's player it feeds one `pw-cat` stream itself, 10 ms at a
//! time with about 40 ms queued, so the hold and the strength take effect
//! within a block and the playhead is the feeder's position minus what is
//! still queued. Unlike a take, the file is still being cleaned: playback
//! that reaches the end of the cleaned part fades out there and waits
//! until max(2 s, 30 s × (1 − speed)) beyond it is cleaned (the speed is
//! the pass's, in real time), or the pass is done. Held, it plays the
//! original past the cleaned part.
//!
//! Where `pw-cat` cannot play from a pipe (it ends at once), the restart
//! player takes over from the playhead, as Mic check's does: it writes a
//! WAV of the next stretch (up to the end of the cleaned part, 2 s after a
//! start, 20 s after that) and plays it with `pw-play`, rendering the
//! stretch after it into a second file meanwhile. It starts over from the
//! playhead on a seek, or on a hold or strength change that changes what
//! is heard. Each start-over, and each move to the next stretch, is a
//! short gap (pw-play starting). Once a restart player has played, every
//! later file in this process starts there.

use super::cache::Cache;
use super::decode::{DecodeError, Decoder};
use super::mix::Blend;
use super::resample::Converter;
use crate::appstate::{FileJob, JobState};
use crate::miccheck::clock::PlayClock;
use crate::miccheck::player::{PlayerState, SEEK_FADE};
use crate::miccheck::proc::{self, Purpose};
use std::collections::VecDeque;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const RATE: u64 = 48_000;
/// Frames per block fed: 10 ms.
const BLOCK: usize = 480;
/// Frames kept queued in the pipe ahead of pw-cat.
const LEAD: usize = 1920;
/// The lead plus a block of stereo f32 always fits.
const PIPE_BYTES: libc::c_int = 32_768;
/// No progress for this long while playing (and not waiting): no output.
const STALL: Duration = Duration::from_secs(3);
/// Stop: the queued audio and a 5 ms fade play out within this.
const STOP_GRACE: Duration = Duration::from_millis(150);
/// The original's converter starts this far before a seek target and
/// settles there.
const WARM: u64 = 4_800;
/// The pass's speed is judged over this much of its recent progress.
const SPEED_WINDOW: Duration = Duration::from_secs(4);
/// The restart player's longest stretch: 20 s.
const CHUNK: usize = 960_000;
/// Its first stretch after a start or a start-over: 2 s, so it sounds
/// soon (rendering 20 s takes about 0.15 s in a release build).
const FIRST_CHUNK: usize = 96_000;
/// While one stretch plays, the next is rendered this much at a time
/// (1 s), so the thread keeps answering a stop or a seek.
const STEP: usize = 48_000;
/// The restart player starts over at most this often while the strength
/// is dragged.
const MIN_RESTART: Duration = Duration::from_millis(200);
/// A pw-cat (or pw-play) that ends within this of its start never played.
const EARLY: Duration = Duration::from_millis(500);
/// The restart players' WAVs in the runtime dir start with this; see
/// [`remove_files`].
const PLAY_FILE_PREFIX: &str = "files-play-";
/// A pw-cat could not play from a pipe and the restart player could: every
/// file plays through the restart player from then on. Set only once a
/// restart player has played, so a pw-cat that failed for another reason
/// (no PipeWire for a moment) does not make it stick.
static RESTART_ONLY: AtomicBool = AtomicBool::new(false);

/// Whether files play through the restart player in this process.
pub fn restart_only() -> bool {
    RESTART_ONLY.load(Ordering::Relaxed)
}

/// Tests: start with or without the restart player.
#[doc(hidden)]
pub fn set_restart_only(on: bool) {
    RESTART_ONLY.store(on, Ordering::Relaxed);
}

/// Delete the restart players' WAVs a crashed window left in `dir` (the
/// runtime dir): what was playing of the user's recording must not
/// outlive the window. The daemon calls this through
/// `miccheck::remove_files` at start, at exit and when the window ends.
pub fn remove_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(PLAY_FILE_PREFIX) && name.ends_with(".wav") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// How far past the playhead the cleaned part must reach before playback
/// that caught up with the pass goes on: max(2 s, 30 s × (1 − speed)),
/// so a pass slower than real time is not caught up with again at once.
pub fn resume_ahead(speed: f64) -> u64 {
    let s = 30.0 * (1.0 - speed.clamp(0.0, 1.0));
    (s.max(2.0) * RATE as f64) as u64
}

/// The channels played: mono as mono, everything else as stereo.
pub fn out_channels(channels: usize) -> usize {
    if channels <= 1 {
        1
    } else {
        2
    }
}

/// Down to the channels played: more than two go to left (the even ones)
/// and right (the odd ones), for listening only; Save keeps them all.
fn downmix(x: Vec<Vec<f32>>) -> Vec<Vec<f32>> {
    if x.len() <= 2 {
        return x;
    }
    let n = x[0].len();
    let side = |odd: usize| {
        let chs: Vec<&Vec<f32>> = x.iter().skip(odd).step_by(2).collect();
        let k = chs.len() as f32;
        (0..n)
            .map(|i| chs.iter().map(|c| c[i]).sum::<f32>() / k)
            .collect()
    };
    vec![side(0), side(1)]
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// The original at 48 kHz, read in order and seekable to the frame.
struct Original {
    dec: Decoder,
    rate: u32,
    channels: usize,
    conv: Converter,
    /// Converted frames from `at`.
    ready: Vec<Vec<f32>>,
    at: u64,
    ended: bool,
}

impl Original {
    /// Its `track`th sound track (0: the first), the one cleaned.
    fn open(path: &Path, ffmpeg: Option<&Path>, track: usize) -> Result<Original, DecodeError> {
        let dec = Decoder::open_track(path, ffmpeg, track)?;
        let (rate, channels) = (dec.info().rate, dec.info().channels);
        let conv = Converter::new(rate, RATE as u32, channels)
            .ok_or_else(|| DecodeError::Unsupported(format!("{rate} Hz")))?;
        Ok(Original {
            dec,
            rate,
            channels,
            conv,
            ready: vec![Vec::new(); channels],
            at: 0,
            ended: false,
        })
    }

    /// Start over at `to` (48 kHz): the converter starts a little before,
    /// on a frame where both rates meet, and settles by then.
    fn seek(&mut self, to: u64) -> Result<(), DecodeError> {
        let rate = self.rate as u64;
        let g = gcd(rate, RATE);
        let (step48, step_src) = (RATE / g, rate / g);
        let from48 = to.saturating_sub(WARM) / step48 * step48;
        self.dec.seek(from48 / step48 * step_src)?;
        self.conv = Converter::new(self.rate, RATE as u32, self.channels)
            .ok_or_else(|| DecodeError::Unsupported(format!("{} Hz", self.rate)))?;
        self.ready = vec![Vec::new(); self.channels];
        self.at = from48;
        self.ended = false;
        self.drop_to(to)
    }

    fn drop_to(&mut self, to: u64) -> Result<(), DecodeError> {
        while self.at < to {
            let skip = ((to - self.at) as usize).min(self.ready[0].len());
            if skip == 0 && !self.decode()? {
                self.at = to;
                break;
            }
            for c in &mut self.ready {
                c.drain(..skip);
            }
            self.at += skip as u64;
        }
        Ok(())
    }

    /// One more block into `ready`; false at the end.
    fn decode(&mut self) -> Result<bool, DecodeError> {
        if self.ended {
            return Ok(false);
        }
        let out = match self.dec.next_block()? {
            Some(b) => self.conv.push(&b),
            None => {
                self.ended = true;
                self.conv.finish()
            }
        };
        for (r, o) in self.ready.iter_mut().zip(out) {
            r.extend_from_slice(&o);
        }
        Ok(true)
    }

    /// Frames `[to, to + n)`, silence past the end.
    fn read(&mut self, to: u64, n: usize) -> Result<Vec<Vec<f32>>, DecodeError> {
        if to < self.at || to > self.at + RATE {
            self.seek(to)?;
        } else {
            self.drop_to(to)?;
        }
        while self.ready[0].len() < n && self.decode()? {}
        let mut out = vec![vec![0f32; n]; self.channels];
        for (o, r) in out.iter_mut().zip(&mut self.ready) {
            let k = n.min(r.len());
            o[..k].copy_from_slice(&r[..k]);
            r.drain(..k);
        }
        self.at += n as u64;
        Ok(out)
    }
}

/// What plays: the sample generator, without any process (unit-tested).
pub struct FileSource {
    cache: Cache,
    cache_dir: PathBuf,
    orig: Original,
    blend: Blend,
    channels: usize,
    pos: u64,
    /// The length (48 kHz), as known so far.
    frames: u64,
    lit: Vec<(u64, u64)>,
    done: bool,
    /// Recent progress of the pass: (when, frames cleaned).
    progress: VecDeque<(Instant, u64)>,
    waiting: bool,
    /// Fading out before a jump to `.0` (`None`: a stop); frames done.
    seek: Option<(Option<u64>, usize)>,
    fade_in: usize,
    /// Fading out at the end of the cleaned part; frames done.
    tip_fade: Option<usize>,
    /// The gain of the last frame out: 0 after a fade out (or before the
    /// first), when stopping needs no fade.
    last_gain: f32,
    stopped: bool,
}

impl FileSource {
    /// `job` as the daemon reports it, from frame `at`, at `strength` (the
    /// original's share, see `mix::alpha`).
    pub fn open(
        job: &FileJob,
        ffmpeg: Option<&Path>,
        at: u64,
        strength: f32,
    ) -> Result<FileSource, String> {
        let cache =
            Cache::open(Path::new(&job.cache), job.stored).map_err(|e| format!("cache: {e}"))?;
        let mut orig =
            Original::open(Path::new(&job.path), ffmpeg, job.track).map_err(|e| e.to_string())?;
        let frames = job.frames.unwrap_or(0);
        let at = at.min(frames);
        if at > 0 {
            orig.seek(at).map_err(|e| e.to_string())?;
        }
        let mut s = FileSource {
            cache,
            cache_dir: PathBuf::from(&job.cache),
            channels: out_channels(orig.channels),
            orig,
            blend: Blend::new(strength),
            pos: at,
            frames,
            lit: Vec::new(),
            done: false,
            progress: VecDeque::new(),
            waiting: false,
            seek: None,
            fade_in: SEEK_FADE,
            tip_fade: None,
            last_gain: 0.0,
            stopped: false,
        };
        s.set_lit(
            &job.lit,
            job.state == JobState::Done,
            job.frames,
            Instant::now(),
        );
        Ok(s)
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Caught up with the pass: nothing plays until it is ahead again.
    pub fn waiting(&self) -> bool {
        self.waiting
    }

    /// The end of the file, or a finished stop.
    pub fn ended(&self) -> bool {
        (self.stopped && self.seek.is_none()) || (self.done && self.pos >= self.frames)
    }

    /// The pass's progress (the daemon's snapshot). `frames` is the length
    /// as known now.
    pub fn set_lit(&mut self, lit: &[(u64, u64)], done: bool, frames: Option<u64>, now: Instant) {
        let total: u64 = lit.iter().map(|(a, b)| b - a).sum();
        if self.progress.back().is_some_and(|&(_, t)| t > total) {
            self.progress.clear(); // started over
        }
        self.progress.push_back((now, total));
        while self
            .progress
            .front()
            .is_some_and(|&(t, _)| now.duration_since(t) > SPEED_WINDOW)
        {
            self.progress.pop_front();
        }
        self.lit = lit.to_vec();
        self.done = done;
        if let Some(f) = frames {
            self.frames = f;
        }
    }

    /// The channels in the cache, as the daemon reports them: a pass that
    /// started over with every channel writes a new cache.
    pub fn set_stored(&mut self, stored: usize) {
        if stored != self.cache.channels() {
            match Cache::open(&self.cache_dir, stored) {
                Ok(c) => self.cache = c,
                Err(e) => eprintln!("[hushmic] file playback: cache: {e}"),
            }
        }
    }

    /// The pass's speed in real time over its recent progress (0 before it
    /// has any).
    pub fn speed(&self) -> f64 {
        match (self.progress.front(), self.progress.back()) {
            (Some(&(t0, a)), Some(&(t1, b))) if t1 > t0 => {
                (b - a) as f64 / RATE as f64 / (t1 - t0).as_secs_f64()
            }
            _ => 0.0,
        }
    }

    pub fn set_strength(&mut self, a: f32) {
        self.blend.set_strength(a);
    }

    pub fn hold(&mut self, on: bool) {
        self.blend.hold(on);
    }

    /// The original's share once any ramp is through: what is heard.
    pub fn heard_alpha(&self) -> f32 {
        self.blend.target()
    }

    /// Jump to `to` after a 5 ms fade out.
    pub fn seek(&mut self, to: u64) {
        let to = to.min(self.frames);
        let done = self.seek.map_or(0, |(_, d)| d);
        self.seek = Some((Some(to), done));
    }

    /// Go on from `to` at once, fading in, with a hold or a new strength
    /// already in full (the restart player: nothing sounds in between).
    fn jump(&mut self, to: u64) {
        self.pos = to.min(self.frames);
        self.seek = None;
        self.tip_fade = None;
        self.fade_in = SEEK_FADE;
        self.last_gain = 0.0;
        self.waiting = false;
        self.blend.settle();
    }

    /// Fade out from here and end (Stop).
    pub fn finish(&mut self) {
        let done = self.seek.map_or(0, |(_, d)| d);
        self.seek = Some((None, done));
        self.stopped = true;
    }

    /// The end of the cleaned part from `at`: the end of its range, or
    /// `at` where nothing is cleaned.
    fn lit_end(&self, at: u64) -> u64 {
        if self.done {
            return self.frames;
        }
        self.lit
            .iter()
            .find(|&&(a, b)| a <= at && at < b)
            .map_or(at, |&(_, b)| b)
    }

    /// How far playback may go from `pos` right now.
    fn playable_end(&self) -> u64 {
        // Held all the way, the original alone plays on.
        if self.blend.held_fully() {
            return self.frames;
        }
        self.lit_end(self.pos)
    }

    /// Waiting at the end of the cleaned part: go on once enough beyond
    /// it is cleaned, the pass is done, or the original is held.
    fn may_resume(&self) -> bool {
        if self.done || self.blend.held_fully() {
            return true;
        }
        let end = self.lit_end(self.pos);
        end >= self.frames || end - self.pos >= resume_ahead(self.speed())
    }

    /// Up to `max` frames, interleaved. Empty while waiting and at the end.
    pub fn render(&mut self, max: usize) -> Result<Vec<f32>, String> {
        // A jump or a stop, once faded out (at once while nothing sounds).
        if let Some((to, done)) = self.seek {
            if done >= SEEK_FADE || self.waiting || self.last_gain == 0.0 {
                self.seek = None;
                self.tip_fade = None;
                self.last_gain = 0.0;
                match to {
                    Some(to) => {
                        self.pos = to;
                        self.fade_in = SEEK_FADE;
                        self.waiting = false;
                    }
                    None => return Ok(Vec::new()),
                }
            }
        }
        if self.stopped && self.seek.is_none() {
            return Ok(Vec::new());
        }
        if self.waiting {
            // Nothing sounds: a hold or a new strength applies at once.
            self.blend.settle();
            if !self.may_resume() {
                return Ok(Vec::new());
            }
            self.waiting = false;
            self.fade_in = SEEK_FADE;
        }
        if self.done && self.pos >= self.frames {
            return Ok(Vec::new());
        }
        let playable = self.playable_end().min(self.frames);
        let room = playable.saturating_sub(self.pos);
        // 5 ms are kept in hand before the end of the cleaned part: if
        // playback has to wait there, it fades out over them.
        let reserve = if playable < self.frames {
            SEEK_FADE as u64
        } else {
            0
        };
        if self.seek.is_none() && self.tip_fade.is_none() && room <= reserve {
            if room == 0 && self.last_gain == 0.0 {
                self.waiting = true;
                return Ok(Vec::new());
            }
            // Past the end of it while sounding (a hold let go): the fade
            // runs on the original.
            self.tip_fade = Some(0);
        }
        let mut n = max.min((self.frames - self.pos) as usize);
        n = match (self.seek, self.tip_fade) {
            (Some((_, d)), _) | (None, Some(d)) => n.min(SEEK_FADE - d),
            (None, None) => n.min((room - reserve) as usize),
        };
        if n == 0 {
            // The stated length reached before the pass knows the real one.
            self.waiting = true;
            return Ok(Vec::new());
        }
        let clean = self.cache.read(self.pos, n).map_err(|e| e.to_string())?;
        let orig = self.orig.read(self.pos, n).map_err(|e| e.to_string())?;
        let mixed = downmix(self.blend.mix(&clean, &orig));
        let mut out = Vec::with_capacity(n * self.channels);
        let mut g = 1.0f32;
        for i in 0..n {
            g = 1.0;
            if let Some((_, done)) = self.seek.as_mut() {
                *done += 1;
                g *= 1.0 - *done as f32 / SEEK_FADE as f32;
            }
            if let Some(done) = self.tip_fade.as_mut() {
                *done += 1;
                g *= 1.0 - *done as f32 / SEEK_FADE as f32;
            }
            if self.fade_in > 0 {
                g *= (SEEK_FADE - self.fade_in + 1) as f32 / SEEK_FADE as f32;
                self.fade_in -= 1;
            }
            // The file's last 5 ms fade out: it ends without a click.
            let left = self.frames - self.pos - i as u64;
            if left < SEEK_FADE as u64 {
                g *= left as f32 / SEEK_FADE as f32;
            }
            for ch in &mixed {
                out.push(ch[i] * g);
            }
        }
        self.last_gain = g;
        self.pos += n as u64;
        if self.tip_fade.is_some_and(|d| d >= SEEK_FADE) {
            // Faded out: wait there, unless more is cleaned by now.
            self.tip_fade = None;
            self.fade_in = SEEK_FADE;
        }
        Ok(out)
    }
}

#[derive(Default)]
struct Ctl {
    seek: Option<u64>,
    hold: Option<bool>,
    strength: Option<f32>,
    /// The daemon's latest word on the job.
    job: Option<FileJob>,
    stop: bool,
    heard: u64,
    waiting: bool,
    state: Option<PlayerState>,
    /// The playhead's clock while the pipe player plays (not the restart
    /// player's: its playhead is the wall clock).
    clock: Option<Arc<PlayClock>>,
}

/// A file playing. Dropping it stops the sound.
pub struct FilePlayer {
    ctl: Arc<Mutex<Ctl>>,
    thread: Option<JoinHandle<()>>,
}

impl FilePlayer {
    /// Play `source` (opened at its start position).
    pub fn start(source: FileSource) -> FilePlayer {
        let ctl = Arc::new(Mutex::new(Ctl {
            heard: source.position(),
            ..Ctl::default()
        }));
        let feeder_ctl = Arc::clone(&ctl);
        let thread = std::thread::Builder::new()
            .name("hushmic-file-play".into())
            .spawn(move || {
                let state = if restart_only() {
                    feed_restart(source, &feeder_ctl, true)
                } else {
                    feed(source, &feeder_ctl)
                };
                feeder_ctl.lock().unwrap().state = Some(state);
            })
            .ok();
        if thread.is_none() {
            ctl.lock().unwrap().state = Some(PlayerState::Failed { early: true });
        }
        FilePlayer { ctl, thread }
    }

    pub fn seek(&self, to: u64) {
        self.ctl.lock().unwrap().seek = Some(to);
    }

    pub fn hold(&self, on: bool) {
        self.ctl.lock().unwrap().hold = Some(on);
    }

    pub fn set_strength(&self, a: f32) {
        self.ctl.lock().unwrap().strength = Some(a);
    }

    /// The daemon's latest word on the pass.
    pub fn set_lit(&self, job: &FileJob) {
        self.ctl.lock().unwrap().job = Some(job.clone());
    }

    /// The playhead (48 kHz frames): what is heard now, read off the
    /// audio's clock while the pipe player plays.
    pub fn position(&self) -> u64 {
        let c = self.ctl.lock().unwrap();
        match (&c.clock, c.state) {
            (Some(clock), None) => clock.heard(Instant::now()),
            _ => c.heard,
        }
    }

    /// Waiting for the pass at the end of the cleaned part.
    pub fn waiting(&self) -> bool {
        self.ctl.lock().unwrap().waiting
    }

    pub fn poll(&self) -> PlayerState {
        self.ctl
            .lock()
            .unwrap()
            .state
            .unwrap_or(PlayerState::Playing)
    }

    pub fn stop(&mut self) {
        self.ctl.lock().unwrap().stop = true;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for FilePlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn queued_bytes(fd: i32) -> usize {
    let mut n: libc::c_int = 0;
    // SAFETY: FIONREAD writes one int; on Linux it reports the bytes in
    // the pipe from either end.
    if unsafe { libc::ioctl(fd, libc::FIONREAD, &mut n) } == 0 {
        n.max(0) as usize
    } else {
        0
    }
}

/// The feeder thread (Mic check's, with a source that can wait). Owns and
/// reaps the child.
fn feed(mut src: FileSource, ctl: &Mutex<Ctl>) -> PlayerState {
    let ch = src.channels();
    let mut child = match proc::spawn_feed_as(Purpose::File, ch) {
        Ok(c) => c,
        Err(_) => return PlayerState::Failed { early: true },
    };
    let Some(mut stdin) = child.stdin.take() else {
        proc::stop_child(&mut child);
        return PlayerState::Failed { early: true };
    };
    let fd = stdin.as_raw_fd();
    // SAFETY: plain fcntl on our pipe end; failure keeps the default size.
    unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, PIPE_BYTES) };
    let started = Instant::now();
    let clock = Arc::new(PlayClock::new(ch, src.position()));
    ctl.lock().unwrap().clock = Some(Arc::clone(&clock));
    let _latency = crate::miccheck::outlat::Watch::start(child.id(), Arc::clone(&clock));
    // The restart player's place if this pw-cat ends at once: the playhead,
    // never before the last seek's target (blocks from before the seek can
    // still be queued; nor is what pw-cat has read counted from before it).
    let mut floor = src.position();
    let mut heard = src.position();
    let mut bytes = Vec::with_capacity(BLOCK * ch * 4);
    let mut written_any = false;
    // frames written into the pipe, so far
    let mut written = 0;
    let mut last_heard = (src.position(), Instant::now());
    let mut stopping: Option<Instant> = None;
    let fail = |child: &mut std::process::Child, early: bool| {
        proc::stop_child(child);
        PlayerState::Failed { early }
    };
    loop {
        {
            let mut c = ctl.lock().unwrap();
            if c.stop && stopping.is_none() {
                src.finish();
                stopping = Some(Instant::now());
            }
            if stopping.is_none() {
                if let Some(to) = c.seek.take() {
                    src.seek(to);
                    floor = to.min(src.frames());
                    heard = floor;
                    // the playhead starts over there (the reading lags
                    // the jump while the pipe drains)
                    clock.reset(floor);
                }
                if let Some(on) = c.hold.take() {
                    src.hold(on);
                }
                if let Some(a) = c.strength.take() {
                    src.set_strength(a);
                }
            }
            if let Some(j) = c.job.take() {
                src.set_stored(j.stored);
                src.set_lit(&j.lit, j.state == JobState::Done, j.frames, Instant::now());
            }
        }
        if stopping.is_some_and(|t| t.elapsed() > STOP_GRACE) {
            proc::stop_child(&mut child);
            return PlayerState::Ended;
        }
        // pw-cat gone: it exited, or a write found the pipe closed (EPIPE),
        // whichever is seen first.
        let mut gone = matches!(child.try_wait(), Ok(Some(_)));
        let mut queued = 0;
        if !gone {
            queued = queued_bytes(fd) / (4 * ch);
            while queued < LEAD {
                let block = match src.render(BLOCK) {
                    Ok(b) if !b.is_empty() => b,
                    Ok(_) => break,
                    Err(e) => {
                        eprintln!("[hushmic] file playback: {e}");
                        return fail(&mut child, !written_any);
                    }
                };
                bytes.clear();
                for s in &block {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                if stdin.write_all(&bytes).is_err() {
                    gone = true;
                    break;
                }
                written_any = true;
                written += block.len() / ch;
                queued += block.len() / ch;
            }
        }
        if gone {
            // Early: nothing went out (it never read the pipe; on a loaded
            // machine it can take a while to end), or it ended at once.
            let read_none = queued_bytes(fd) / (4 * ch) >= written;
            proc::stop_child(&mut child);
            let early = !written_any || read_none || started.elapsed() < EARLY;
            if early && stopping.is_none() {
                // This pw-cat cannot play from a pipe: the restart player,
                // from the same place.
                src.jump(heard.max(floor));
                ctl.lock().unwrap().clock = None;
                return feed_restart(src, ctl, false);
            }
            if src.ended() {
                return PlayerState::Ended;
            }
            return PlayerState::Failed { early };
        }
        let now = Instant::now();
        clock.observe(src.position().saturating_sub(queued as u64).max(floor), now);
        heard = clock.heard(now);
        {
            let mut c = ctl.lock().unwrap();
            c.heard = heard;
            c.waiting = src.waiting();
        }
        if heard != last_heard.0 || src.waiting() || stopping.is_some() {
            last_heard = (heard, Instant::now());
        } else if last_heard.1.elapsed() > STALL {
            return fail(&mut child, false);
        }
        if src.ended() && queued == 0 {
            // Everything is with pw-cat: close the pipe and let it drain.
            drop(stdin);
            let deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            proc::stop_child(&mut child);
            ctl.lock().unwrap().heard = src.position();
            return PlayerState::Ended;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The next stretch for the restart player, rendered a piece at a time
/// (so the thread keeps answering): up to the end of the cleaned part or
/// the file, at most `want` frames. A stretch cut at `want` fades out over
/// its last 5 ms, and the next one fades in.
struct Next {
    /// Interleaved.
    samples: Vec<f32>,
    from: u64,
    want: usize,
    /// Rendered all it will be.
    complete: bool,
    /// Written, into which of the two files: the one not playing when it
    /// was written, which is the one to play (the stretch that played
    /// there may have ended since).
    written: Option<usize>,
}

impl Next {
    fn new(from: u64, want: usize) -> Next {
        Next {
            samples: Vec::new(),
            from,
            want,
            complete: false,
            written: None,
        }
    }

    fn frames(&self, ch: usize) -> usize {
        self.samples.len() / ch
    }

    /// Render up to `step` frames more. Once complete, an empty stretch
    /// means nothing plays now (caught up, or the end).
    fn render(&mut self, src: &mut FileSource, step: usize) -> Result<(), String> {
        let ch = src.channels();
        let stop = self.frames(ch).saturating_add(step).min(self.want);
        while self.frames(ch) < stop {
            let b = src.render((stop - self.frames(ch)).min(BLOCK * 10))?;
            if b.is_empty() {
                self.complete = true;
                return Ok(());
            }
            self.samples.extend(b);
        }
        if self.frames(ch) >= self.want {
            self.complete = true;
            if src.position() < src.frames() {
                let n = self.frames(ch);
                for k in 0..SEEK_FADE.min(n) {
                    let g = k as f32 / SEEK_FADE as f32;
                    for v in &mut self.samples[(n - 1 - k) * ch..(n - k) * ch] {
                        *v *= g;
                    }
                }
                src.jump(src.position());
            }
        }
        Ok(())
    }
}

/// The restart player's thread: one `pw-play` of a WAV at a time, the
/// playhead counted from when it started, the next stretch rendered into
/// the other of two files while one plays. Owns and reaps the child.
/// `confirmed`: restart players have played in this process before.
fn feed_restart(mut src: FileSource, ctl: &Mutex<Ctl>, confirmed: bool) -> PlayerState {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let Some(dir) = proc::work_dir() else {
        return PlayerState::Failed { early: true };
    };
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let files = [0, 1].map(|k| dir.join(format!("{PLAY_FILE_PREFIX}{pid}-{n}-{k}.wav")));
    let state = restart_loop(&mut src, ctl, &files, confirmed);
    for f in &files {
        let _ = std::fs::remove_file(f);
    }
    state
}

/// One WAV playing: its `pw-play`, from which frame, since when, how long,
/// and which of the two files.
struct Stretch {
    child: Child,
    from: u64,
    at: Instant,
    len: u64,
    file: usize,
}

impl Stretch {
    fn heard(&self) -> u64 {
        let played = (self.at.elapsed().as_secs_f64() * RATE as f64) as u64;
        self.from + played.min(self.len)
    }

    fn stop(mut self) {
        proc::stop_child(&mut self.child);
    }
}

fn restart_loop(
    src: &mut FileSource,
    ctl: &Mutex<Ctl>,
    files: &[PathBuf; 2],
    mut confirmed: bool,
) -> PlayerState {
    let ch = src.channels();
    let mut playing: Option<Stretch> = None;
    // A start (or a start-over) plays a short stretch first, so it sounds
    // soon; the ones after it are long.
    let mut next = Next::new(src.position(), FIRST_CHUNK);
    let mut seek: Option<u64> = None;
    let mut changed = false;
    // A restart player has played: files start here from now on.
    fn confirm(confirmed: &mut bool) {
        if !*confirmed {
            *confirmed = true;
            RESTART_ONLY.store(true, Ordering::Relaxed);
        }
    }
    // A stretch this player ends: it played if it ran past EARLY (one that
    // fails on its own says so by its exit status instead, however long it
    // took to fail on a loaded machine).
    fn stop_played(p: Stretch, confirmed: &mut bool) {
        if p.at.elapsed() >= EARLY {
            confirm(confirmed);
        }
        p.stop();
    }
    loop {
        {
            let mut c = ctl.lock().unwrap();
            if c.stop {
                drop(c);
                if let Some(p) = playing.take() {
                    stop_played(p, &mut confirmed);
                }
                return PlayerState::Ended;
            }
            if let Some(to) = c.seek.take() {
                seek = Some(to.min(src.frames()));
            }
            // Only what changes what is heard starts over: the same hold
            // or strength again (as the page sends when a player arrives),
            // or a strength under a hold, does not.
            let was = src.heard_alpha();
            if let Some(on) = c.hold.take() {
                src.hold(on);
            }
            if let Some(a) = c.strength.take() {
                src.set_strength(a);
            }
            changed |= src.heard_alpha() != was;
            if let Some(j) = c.job.take() {
                src.set_stored(j.stored);
                src.set_lit(&j.lit, j.state == JobState::Done, j.frames, Instant::now());
            }
        }
        // A seek, a hold or a new strength: start over from the playhead
        // (or the seek's target), at most every MIN_RESTART.
        let since = playing.as_ref().map(|p| p.at.elapsed());
        if (seek.is_some() || changed) && since.is_none_or(|t| t >= MIN_RESTART) {
            let at = seek
                .take()
                .unwrap_or_else(|| playing.as_ref().map_or(next.from, Stretch::heard));
            if let Some(p) = playing.take() {
                stop_played(p, &mut confirmed);
            }
            src.jump(at);
            next = Next::new(at, FIRST_CHUNK);
            changed = false;
        }
        if let Some(p) = playing.as_mut() {
            match p.child.try_wait() {
                Ok(None) => {}
                Ok(Some(status)) if status.success() => {
                    let end = p.from + p.len;
                    playing = None;
                    ctl.lock().unwrap().heard = end;
                    confirm(&mut confirmed);
                }
                _ => {
                    let ran = p.at.elapsed() >= EARLY;
                    if let Some(p) = playing.take() {
                        p.stop();
                    }
                    return PlayerState::Failed {
                        early: !confirmed && !ran,
                    };
                }
            }
        }
        // The next stretch: a piece at a time while one plays, all of it
        // at once while nothing does.
        let step = if playing.is_some() { STEP } else { usize::MAX };
        if !next.complete {
            if let Err(e) = next.render(src, step) {
                eprintln!("[hushmic] file playback: {e}");
                return PlayerState::Failed { early: false };
            }
        }
        if next.complete && next.written.is_none() && !next.samples.is_empty() {
            let other = playing.as_ref().map_or(0, |p| 1 - p.file);
            if let Err(e) = proc::write_wav_as(&files[other], &next.samples, ch) {
                eprintln!("[hushmic] file playback: {e}");
                return PlayerState::Failed { early: !confirmed };
            }
            next.written = Some(other);
        }
        if playing.is_none() && next.complete {
            if let Some(file) = next.written {
                match proc::spawn_play_as(&files[file], Purpose::File) {
                    Ok(child) => {
                        let len = next.frames(ch) as u64;
                        playing = Some(Stretch {
                            child,
                            from: next.from,
                            at: Instant::now(),
                            len,
                            file,
                        });
                        next = Next::new(next.from + len, CHUNK);
                    }
                    Err(e) => {
                        eprintln!("[hushmic] file playback: {e}");
                        return PlayerState::Failed { early: !confirmed };
                    }
                }
            } else if src.ended() {
                ctl.lock().unwrap().heard = src.position();
                return PlayerState::Ended;
            } else {
                // Caught up with the pass: look again in a moment, and
                // sound soon once it is there, as at a start.
                next = Next::new(src.position(), FIRST_CHUNK);
            }
        }
        {
            let mut c = ctl.lock().unwrap();
            // A seek held back by MIN_RESTART shows where it goes.
            c.heard = match (seek, &playing) {
                (Some(to), _) => to,
                (None, Some(p)) => p.heard(),
                (None, None) => next.from,
            };
            c.waiting = playing.is_none() && src.waiting();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// ffmpeg for the originals symphonia cannot read.
pub fn ffmpeg() -> Option<PathBuf> {
    super::decode::which("ffmpeg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caught_up_playback_waits_for_more_the_slower_the_pass() {
        assert_eq!(resume_ahead(2.0), 96_000, "faster than real time: 2 s");
        assert_eq!(resume_ahead(1.0), 96_000);
        assert_eq!(resume_ahead(0.5), 720_000, "half: 15 s");
        assert_eq!(resume_ahead(0.0), 1_440_000, "stalled: 30 s");
        assert_eq!(resume_ahead(0.95), 96_000, "1.5 s is under the 2 s floor");
    }

    #[test]
    fn the_restart_players_stretches_meet_with_a_fade() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/files/master_48k_mono.wav");
        let dir = std::env::temp_dir().join(format!("hushmic-chunk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = Cache::create(&dir, 1).unwrap();
        cache.write(0, &[vec![0.25f32; 48_000]]).unwrap();
        let job = FileJob {
            id: 1,
            path: fixture.display().to_string(),
            state: JobState::Done,
            cache: dir.display().to_string(),
            stored: 1,
            channels: 1,
            frames: Some(48_000),
            exact: true,
            lit: vec![(0, 48_000)],
            ..FileJob::default()
        };
        // The cleaned sound alone: 0.25 wherever no fade applies.
        let stretch = |s: &mut FileSource, want: usize, step: usize| {
            let mut n = Next::new(s.position(), want);
            while !n.complete {
                n.render(s, step).unwrap();
            }
            n.samples
        };
        let mut s = FileSource::open(&job, None, 0, 0.0).unwrap();
        let a = stretch(&mut s, 20_000, 7_000);
        assert_eq!(a.len(), 20_000, "in pieces, to the length it wants");
        assert!(a[0] < 0.01, "the first fades in");
        assert_eq!(a[10_000], 0.25);
        assert!(a[19_999] < 0.01 && a[19_759] > 0.24, "cut: 5 ms fade out");
        let b = stretch(&mut s, 20_000, usize::MAX);
        assert_eq!(b.len(), 20_000);
        assert!(b[0] < 0.01 && b[240] == 0.25, "the next fades in");
        let c = stretch(&mut s, 20_000, usize::MAX);
        assert_eq!(c.len(), 8_000, "up to the end");
        assert!(
            c[7_999] < 0.01 && c[7_000] == 0.25,
            "the file's own end fade"
        );
        assert!(s.ended());
        assert!(stretch(&mut s, 20_000, usize::MAX).is_empty());
        // A stretch ends at the end of the cleaned part, and waits there.
        let mut s = FileSource::open(&job, None, 0, 0.0).unwrap();
        s.set_lit(&[(0, 12_000)], false, None, Instant::now());
        assert_eq!(stretch(&mut s, 20_000, usize::MAX).len(), 12_000);
        assert!(s.waiting());
        assert!(stretch(&mut s, 20_000, usize::MAX).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_crashed_windows_restart_files_are_removed() {
        let dir = std::env::temp_dir().join(format!("hushmic-playfiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            "files-play-12-0.wav",
            "files-play-12-3.wav",
            "miccheck-play.wav",
            "keep.wav",
        ] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        remove_files(&dir);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["keep.wav", "miccheck-play.wav"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn more_than_two_channels_are_heard_as_two() {
        assert_eq!(out_channels(1), 1);
        assert_eq!(out_channels(6), 2);
        let x = vec![
            vec![1.0],
            vec![2.0],
            vec![3.0],
            vec![4.0],
            vec![5.0],
            vec![6.0],
        ];
        assert_eq!(downmix(x), vec![vec![3.0], vec![4.0]]);
        assert_eq!(
            downmix(vec![vec![1.0], vec![2.0]]),
            vec![vec![1.0], vec![2.0]]
        );
    }
}
