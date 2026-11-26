//! Replay of a frozen take with same-word switching.
//!
//! The seamless player feeds one `pw-cat` playback stream itself, 10 ms at
//! a time, keeping only about 40 ms queued in the pipe: switching sides is
//! "the next block comes from the other take" with a 20 ms crossfade, a
//! seek is a 5 ms fade out and in, and the playhead is what is heard on
//! the audio's own clock ([`super::clock`]: how far pw-cat has read, less
//! the measured output latency, smooth between readings). Where `pw-cat`
//! cannot play from a pipe, the restart player plays a WAV of the rest of
//! the chosen side and starts over at the same position on every switch (a
//! short gap). A replay loops: past the take's end it starts over (5 ms
//! out, 5 ms in) until Stop. A take re-rendered at another strength takes
//! over at the same position (a 20 ms crossfade; the restart player starts
//! over there).

use super::clock::PlayClock;
use super::proc;
use super::take::FrozenTake;
use super::types::{Listen, PlaybackKind, SAMPLE_RATE};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Side switch: 20 ms crossfade.
pub const CROSSFADE: usize = 960;
/// Seek: 5 ms fade out, jump, 5 ms fade in.
pub const SEEK_FADE: usize = 240;
const BLOCK: usize = 480;
/// Samples kept queued in the pipe ahead of pw-cat.
const LEAD: usize = 1920;
/// Pipe capacity: the lead plus one block always fits, so writes never
/// block.
const PIPE_BYTES: libc::c_int = 16_384;
/// No progress for this long while playing: no output device.
const STALL: Duration = Duration::from_secs(3);
/// Stop: the queued audio and a 5 ms fade play out within this.
const STOP_GRACE: Duration = Duration::from_millis(150);
/// The restart player's file in the runtime dir.
pub const PLAY_FILE: &str = "miccheck-play.wav";

/// The sample generator. Pure: unit-tested without any process.
pub struct Mixer {
    take: Arc<FrozenTake>,
    pos: usize,
    side: Listen,
    /// Crossfading from this side; samples done of [`CROSSFADE`].
    fade: Option<(Listen, usize)>,
    /// Crossfading from this take (the one before a new strength);
    /// samples done of [`CROSSFADE`].
    retake: Option<(Arc<FrozenTake>, usize)>,
    /// Fading out before a jump to `.0`; samples done of [`SEEK_FADE`].
    seek: Option<(usize, usize)>,
    /// Fade-in samples left after a jump or at the start.
    fade_in: usize,
    /// Times the take started over (the replay loops until Stop).
    laps: u64,
    /// Stop: the take ends where the fade out lands, no lap follows.
    ending: bool,
    /// The take plays once: no lap follows its end.
    once: bool,
}

impl Mixer {
    /// Start at `pos`, fading in (a take never starts with a click).
    pub fn new(take: Arc<FrozenTake>, side: Listen, pos: usize) -> Mixer {
        Mixer {
            pos: pos.min(take.frames()),
            take,
            side,
            fade: None,
            retake: None,
            seek: None,
            fade_in: SEEK_FADE,
            laps: 0,
            ending: false,
            once: false,
        }
    }

    /// The take plays once, then ends (no lap).
    pub fn once(mut self, once: bool) -> Mixer {
        self.once = once;
        self
    }

    /// Fade out from here and end (Stop).
    pub fn finish(&mut self) {
        self.ending = true;
        self.seek(self.take.frames());
    }

    /// Frames played since the start, every lap counted: the playhead's
    /// clock runs on this (it never steps back at a lap).
    pub fn unrolled(&self) -> u64 {
        self.laps * self.take.frames() as u64 + self.pos as u64
    }

    /// Where a jump to `to` lands in [`Self::unrolled`] frames.
    pub fn unrolled_at(&self, to: usize) -> u64 {
        self.laps * self.take.frames() as u64 + to.min(self.take.frames()) as u64
    }

    fn sample(take: &FrozenTake, side: Listen, i: usize) -> f32 {
        match side {
            Listen::HushMic => take.clean[i],
            Listen::Original => take.raw[i],
        }
    }

    /// Sample `i` of `take` on the side heard, mid side-crossfade too.
    fn heard(&self, take: &FrozenTake, i: usize) -> f32 {
        let v = Self::sample(take, self.side, i);
        match self.fade {
            Some((from, done)) => {
                let g = (done + 1) as f32 / CROSSFADE as f32;
                Self::sample(take, from, i) * (1.0 - g) + v * g
            }
            None => v,
        }
    }

    pub fn side(&self) -> Listen {
        self.side
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn frames(&self) -> usize {
        self.take.frames()
    }

    /// Continue on `side` from the next sample, crossfading. A switch back
    /// during a running crossfade reverses it from where it stands, so the
    /// output never jumps.
    pub fn set_side(&mut self, side: Listen) {
        if side == self.side {
            return;
        }
        self.fade = match self.fade {
            Some((from, done)) if from == side => Some((self.side, CROSSFADE - done)),
            _ => Some((self.side, 0)),
        };
        self.side = side;
    }

    /// Go on with `take` (the same audio at another strength, the same
    /// length) from the next sample, crossfading from the one playing. A
    /// take of another length is not this one: it is ignored.
    pub fn set_take(&mut self, take: Arc<FrozenTake>) {
        if take.frames() != self.take.frames() {
            return;
        }
        let old = std::mem::replace(&mut self.take, take);
        self.retake = Some((old, 0));
    }

    /// Jump to frame `to` after a short fade out.
    pub fn seek(&mut self, to: usize) {
        let to = to.min(self.take.frames());
        self.seek = Some(match self.seek {
            Some((_, done)) => (to, done),
            None => (to, 0),
        });
    }

    /// Fill `out` from the current position; returns the samples written
    /// (fewer than `out.len()` only at the end, after Stop: past the take's
    /// end it starts over).
    pub fn render(&mut self, out: &mut [f32]) -> usize {
        let mut n = 0;
        while n < out.len() {
            if let Some((to, done)) = self.seek {
                if done >= SEEK_FADE {
                    self.pos = to;
                    self.seek = None;
                    self.fade = None;
                    self.fade_in = SEEK_FADE;
                    continue;
                }
            }
            if self.pos >= self.take.frames() {
                if self.ending || self.once || self.take.frames() == 0 {
                    break;
                }
                // the last 5 ms faded out: start over, fading in
                self.pos = 0;
                self.laps += 1;
                self.fade_in = SEEK_FADE;
                continue;
            }
            let mut v = self.heard(&self.take, self.pos);
            if let Some((old, done)) = self.retake.take() {
                let g = (done + 1) as f32 / CROSSFADE as f32;
                v = self.heard(&old, self.pos) * (1.0 - g) + v * g;
                self.retake = (done + 1 < CROSSFADE).then_some((old, done + 1));
            }
            if let Some((from, done)) = self.fade {
                self.fade = (done + 1 < CROSSFADE).then_some((from, done + 1));
            }
            if let Some((to, done)) = self.seek {
                v *= 1.0 - (done + 1) as f32 / SEEK_FADE as f32;
                self.seek = Some((to, done + 1));
            }
            if self.fade_in > 0 {
                v *= (SEEK_FADE - self.fade_in + 1) as f32 / SEEK_FADE as f32;
                self.fade_in -= 1;
            }
            // The take's last 5 ms fade out: it ends without a click.
            let left = self.take.frames() - self.pos;
            if left < SEEK_FADE {
                v *= left as f32 / SEEK_FADE as f32;
            }
            out[n] = v;
            n += 1;
            self.pos += 1;
        }
        n
    }
}

/// A playhead in frames heard, every lap counted, as 0..1 of the take:
/// the replay loops, so it starts over at each lap.
pub fn in_lap(heard: u64, frames: usize) -> f32 {
    if frames == 0 {
        return 0.0;
    }
    (heard % frames as u64) as f32 / frames as f32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerState {
    Playing,
    Ended,
    /// The player could not play (no pw-cat, no output, a pw-cat that
    /// cannot read a pipe). `early`: before any audio went out, so another
    /// player may be tried.
    Failed {
        early: bool,
    },
}

#[derive(Default)]
struct FeedCtl {
    side: Option<Listen>,
    take: Option<Arc<FrozenTake>>,
    seek: Option<usize>,
    stop: bool,
    /// Frames heard, every lap counted ([`Mixer::unrolled`]).
    heard: u64,
    state: Option<PlayerState>,
}

struct Seamless {
    ctl: Arc<Mutex<FeedCtl>>,
    clock: Arc<PlayClock>,
    thread: Option<JoinHandle<()>>,
    side: Listen,
}

impl Seamless {
    fn start(take: Arc<FrozenTake>, side: Listen, pos: usize, once: bool) -> Seamless {
        let ctl = Arc::new(Mutex::new(FeedCtl {
            heard: pos as u64,
            ..FeedCtl::default()
        }));
        let clock = Arc::new(PlayClock::new(1, pos as u64));
        let feeder_ctl = Arc::clone(&ctl);
        let feeder_clock = Arc::clone(&clock);
        let thread = std::thread::Builder::new()
            .name("miccheck-play".into())
            .spawn(move || {
                let mixer = Mixer::new(take, side, pos).once(once);
                let state = feed(mixer, &feeder_ctl, &feeder_clock);
                feeder_ctl.lock().unwrap().state = Some(state);
            })
            .ok();
        if thread.is_none() {
            ctl.lock().unwrap().state = Some(PlayerState::Failed { early: true });
        }
        Seamless {
            ctl,
            clock,
            thread,
            side,
        }
    }

    fn stop(&mut self) {
        self.ctl.lock().unwrap().stop = true;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Seamless {
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

/// The feeder thread: see the module docs. Owns and reaps the child.
fn feed(mut mixer: Mixer, ctl: &Mutex<FeedCtl>, clock: &Arc<PlayClock>) -> PlayerState {
    let mut child = match proc::spawn_feed() {
        Ok(c) => c,
        Err(_) => return PlayerState::Failed { early: true },
    };
    // The output latency, measured while this stream plays.
    let _latency = super::outlat::Watch::start(child.id(), Arc::clone(clock));
    let Some(mut stdin) = child.stdin.take() else {
        proc::stop_child(&mut child);
        return PlayerState::Failed { early: true };
    };
    let fd = stdin.as_raw_fd();
    // SAFETY: plain fcntl on our pipe end; failure keeps the default size,
    // which is larger and only loosens the lead.
    unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, PIPE_BYTES) };
    let started = Instant::now();
    let mut block = [0f32; BLOCK];
    let mut bytes = Vec::with_capacity(BLOCK * 4);
    let mut written_any = false;
    // samples written into the pipe, so far
    let mut written = 0;
    let mut last_heard = (mixer.unrolled(), Instant::now());
    // Never read back before the last seek's target: audio from before it
    // can still be queued in the pipe. (Frames as unrolled: every lap
    // counted, so the clock never steps back at a lap.)
    let mut floor = mixer.unrolled();
    let mut fed_all = false;
    // Stop: the rest fades out and plays, then pw-cat goes.
    let mut stopping: Option<Instant> = None;
    loop {
        {
            let mut c = ctl.lock().unwrap();
            if c.stop && stopping.is_none() {
                if !written_any {
                    drop(c);
                    proc::stop_child(&mut child);
                    return PlayerState::Ended;
                }
                mixer.finish();
                fed_all = false;
                stopping = Some(Instant::now());
            }
            if stopping.is_none() {
                if let Some(side) = c.side.take() {
                    mixer.set_side(side);
                }
                if let Some(take) = c.take.take() {
                    mixer.set_take(take);
                }
                if let Some(to) = c.seek.take() {
                    mixer.seek(to);
                    floor = mixer.unrolled_at(to);
                    // the playhead starts over there (the reading lags
                    // the jump while the pipe drains)
                    clock.reset(floor);
                    fed_all = false;
                }
            }
        }
        if stopping.is_some_and(|t| t.elapsed() > STOP_GRACE) {
            proc::stop_child(&mut child);
            return PlayerState::Ended;
        }
        if let Ok(Some(_)) = child.try_wait() {
            // Early: nothing went out (it never read the pipe; on a loaded
            // machine it can take a while to end), or it ended at once.
            let read_none = queued_bytes(fd) / 4 >= written;
            let early = !written_any || read_none || started.elapsed() < Duration::from_millis(500);
            return if fed_all {
                PlayerState::Ended
            } else {
                PlayerState::Failed { early }
            };
        }
        let mut queued = queued_bytes(fd) / 4;
        while !fed_all && queued < LEAD {
            let n = mixer.render(&mut block);
            if n == 0 {
                fed_all = true;
                break;
            }
            bytes.clear();
            for s in &block[..n] {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            if stdin.write_all(&bytes).is_err() {
                proc::stop_child(&mut child);
                return PlayerState::Failed {
                    early: !written_any,
                };
            }
            written_any = true;
            written += n;
            queued += n;
        }
        let now = Instant::now();
        let read = mixer.unrolled().saturating_sub(queued as u64).max(floor);
        clock.observe(read, now);
        let heard = clock.heard(now);
        ctl.lock().unwrap().heard = heard;
        if heard != last_heard.0 {
            last_heard = (heard, Instant::now());
        } else if last_heard.1.elapsed() > STALL && !fed_all {
            proc::stop_child(&mut child);
            return PlayerState::Failed { early: false };
        }
        if fed_all && queued == 0 {
            // Everything is with pw-cat: close the pipe and let it drain.
            drop(stdin);
            let deadline = match stopping {
                Some(t) => t + STOP_GRACE,
                None => Instant::now() + Duration::from_secs(1),
            };
            loop {
                let stop_now = stopping.is_none() && ctl.lock().unwrap().stop;
                if stop_now || Instant::now() > deadline {
                    proc::stop_child(&mut child);
                    break;
                }
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            ctl.lock().unwrap().heard = mixer.unrolled();
            return PlayerState::Ended;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The restart fallback: a WAV of the rest of one side per start.
struct Restart {
    take: Arc<FrozenTake>,
    side: Listen,
    from: usize,
    started: Instant,
    child: Option<Child>,
    file: PathBuf,
    failed: bool,
    /// The take plays once: its end ends the replay.
    once: bool,
}

impl Restart {
    fn start(take: Arc<FrozenTake>, side: Listen, pos: usize, dir: &Path, once: bool) -> Restart {
        let mut r = Restart {
            take,
            side,
            from: pos,
            started: Instant::now(),
            child: None,
            file: dir.join(PLAY_FILE),
            failed: false,
            once,
        };
        r.play_from(pos);
        r
    }

    fn play_from(&mut self, pos: usize) {
        if let Some(mut c) = self.child.take() {
            proc::stop_child(&mut c);
        }
        let pos = pos.min(self.take.frames());
        let mut src = match self.side {
            Listen::HushMic => self.take.clean[pos..].to_vec(),
            Listen::Original => self.take.raw[pos..].to_vec(),
        };
        // 5 ms in and out, as the seamless player.
        let n = src.len();
        let fade = SEEK_FADE.min(n / 2);
        for k in 0..fade {
            let g = (k + 1) as f32 / SEEK_FADE as f32;
            src[k] *= g;
            src[n - 1 - k] *= g;
        }
        self.from = pos;
        self.started = Instant::now();
        match proc::write_wav(&self.file, &src).and_then(|()| proc::spawn_play(&self.file)) {
            Ok(c) => self.child = Some(c),
            Err(_) => self.failed = true,
        }
    }

    fn position(&self) -> usize {
        let played = (self.started.elapsed().as_secs_f32() * SAMPLE_RATE as f32) as usize;
        (self.from + played).min(self.take.frames())
    }

    fn poll(&mut self) -> PlayerState {
        if self.failed {
            return PlayerState::Failed { early: false };
        }
        match self.child.as_mut().map(Child::try_wait) {
            Some(Ok(None)) => PlayerState::Playing,
            Some(Ok(Some(status))) if !status.success() => PlayerState::Failed { early: false },
            Some(Ok(Some(_))) if self.once => PlayerState::Ended,
            // the take played to its end: it loops, from the start
            Some(Ok(Some(_))) => {
                self.play_from(0);
                if self.failed {
                    PlayerState::Failed { early: false }
                } else {
                    PlayerState::Playing
                }
            }
            _ => PlayerState::Ended,
        }
    }
}

impl Drop for Restart {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            proc::stop_child(&mut c);
        }
        let _ = std::fs::remove_file(&self.file);
    }
}

enum Inner {
    Seamless(Seamless),
    Restart(Restart),
}

/// A replay in progress. Dropping it stops the sound.
pub struct Player {
    inner: Inner,
    frames: usize,
    /// Switching restarts the side from the start (unaligned take).
    sequential: bool,
}

impl Player {
    /// Start `take` at `at` (0..=1) on `side`, the way `kind` says
    /// (`Sequential` plays seamlessly but switches by starting over).
    /// `dir` holds the restart player's file. It loops until stopped, or
    /// with `once` ends at the take's end.
    pub fn start(
        take: Arc<FrozenTake>,
        side: Listen,
        at: f32,
        kind: PlaybackKind,
        dir: Option<&Path>,
        once: bool,
    ) -> Option<Player> {
        let frames = take.frames();
        let pos = (at.clamp(0.0, 1.0) * frames as f32) as usize;
        let inner = match (kind, dir) {
            (PlaybackKind::Restart, Some(d)) => {
                Inner::Restart(Restart::start(take, side, pos, d, once))
            }
            (PlaybackKind::Restart, None) => return None,
            _ => Inner::Seamless(Seamless::start(take, side, pos, once)),
        };
        Some(Player {
            inner,
            frames,
            sequential: kind == PlaybackKind::Sequential,
        })
    }

    pub fn set_side(&mut self, side: Listen) {
        if self.sequential {
            match &mut self.inner {
                Inner::Seamless(s) => {
                    let mut c = s.ctl.lock().unwrap();
                    c.side = Some(side);
                    c.seek = Some(0);
                    s.side = side;
                }
                Inner::Restart(r) => {
                    r.side = side;
                    r.play_from(0);
                }
            }
            return;
        }
        match &mut self.inner {
            Inner::Seamless(s) => {
                s.ctl.lock().unwrap().side = Some(side);
                s.side = side;
            }
            Inner::Restart(r) if r.side != side => {
                let at = r.position();
                r.side = side;
                r.play_from(at);
            }
            Inner::Restart(_) => {}
        }
    }

    /// Play `take` (this one at another strength) from where it is.
    pub fn set_take(&mut self, take: Arc<FrozenTake>) {
        if take.frames() != self.frames {
            return;
        }
        match &mut self.inner {
            Inner::Seamless(s) => s.ctl.lock().unwrap().take = Some(take),
            Inner::Restart(r) => {
                let at = r.position();
                r.take = take;
                r.play_from(at);
            }
        }
    }

    pub fn seek(&mut self, at: f32) {
        let to = (at.clamp(0.0, 1.0) * self.frames as f32) as usize;
        match &mut self.inner {
            Inner::Seamless(s) => s.ctl.lock().unwrap().seek = Some(to),
            Inner::Restart(r) => r.play_from(to),
        }
    }

    /// The take's length (frames).
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// The playhead's clock, for a reader that wants it every frame
    /// (None for the restart player, whose playhead is the wall clock).
    pub fn clock(&self) -> Option<Arc<PlayClock>> {
        match &self.inner {
            Inner::Seamless(s) => Some(Arc::clone(&s.clock)),
            Inner::Restart(_) => None,
        }
    }

    /// The playhead, 0..=1 of the take (in the lap it is in).
    pub fn position(&self) -> f32 {
        let heard = match &self.inner {
            Inner::Seamless(s) => {
                let c = s.ctl.lock().unwrap();
                match c.state {
                    // playing: what is heard right now
                    None => s.clock.heard(Instant::now()),
                    Some(_) => c.heard,
                }
            }
            Inner::Restart(r) => r.position() as u64,
        };
        in_lap(heard, self.frames)
    }

    pub fn poll(&mut self) -> PlayerState {
        match &mut self.inner {
            Inner::Seamless(s) => s.ctl.lock().unwrap().state.unwrap_or(PlayerState::Playing),
            Inner::Restart(r) => r.poll(),
        }
    }

    pub fn kind(&self) -> PlaybackKind {
        match (&self.inner, self.sequential) {
            (_, true) => PlaybackKind::Sequential,
            (Inner::Seamless(_), _) => PlaybackKind::Seamless,
            (Inner::Restart(_), _) => PlaybackKind::Restart,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// raw = +1 everywhere, clean = -1 everywhere: the mix shows exactly
    /// how much of each side is in every sample.
    fn take(frames: usize) -> Arc<FrozenTake> {
        Arc::new(FrozenTake {
            raw: vec![1.0; frames],
            clean: vec![-1.0; frames],
            columns: Vec::new(),
            modes: Vec::new(),
            strengths: Vec::new(),
        })
    }

    fn render(m: &mut Mixer, n: usize) -> Vec<f32> {
        let mut out = vec![0f32; n];
        let k = m.render(&mut out);
        out.truncate(k);
        out
    }

    #[test]
    fn a_replay_fades_in_and_out() {
        let mut m = Mixer::new(take(2_000), Listen::Original, 0);
        let x = render(&mut m, 2_000);
        assert_eq!(x.len(), 2_000);
        assert!(
            (x[0] - 1.0 / SEEK_FADE as f32).abs() < 1e-6,
            "starts near 0"
        );
        assert!(x[..SEEK_FADE].windows(2).all(|w| w[1] > w[0]));
        assert!(x[SEEK_FADE..2_000 - SEEK_FADE].iter().all(|&v| v == 1.0));
        assert!(x[2_000 - SEEK_FADE..].windows(2).all(|w| w[1] < w[0]));
        assert!(
            (x[1_999] - 1.0 / SEEK_FADE as f32).abs() < 1e-6,
            "ends near 0"
        );
        // Stop: a 5 ms fade from where it stands, then nothing.
        let mut m = Mixer::new(take(48_000), Listen::Original, 0);
        render(&mut m, 1_000);
        m.finish();
        let x = render(&mut m, 1_000);
        assert_eq!(x.len(), SEEK_FADE);
        assert!(x.windows(2).all(|w| w[1] < w[0]));
        assert_eq!(*x.last().unwrap(), 0.0);
    }

    #[test]
    fn a_switch_crossfades_in_twenty_ms_at_the_same_position() {
        let mut m = Mixer::new(take(48_000), Listen::HushMic, 1000);
        render(&mut m, SEEK_FADE);
        assert_eq!(render(&mut m, 10), vec![-1.0; 10]);
        m.set_side(Listen::Original);
        let x = render(&mut m, CROSSFADE + 10);
        assert_eq!(
            m.position(),
            1010 + SEEK_FADE + CROSSFADE + 10,
            "no jump in position"
        );
        assert!(x.windows(2).all(|w| w[1] >= w[0]), "monotonic crossfade");
        assert!((x[CROSSFADE / 2] - 0.0).abs() < 0.01);
        assert_eq!(x[CROSSFADE - 1], 1.0);
        assert!(x[CROSSFADE..].iter().all(|&v| v == 1.0));
    }

    #[test]
    fn switching_back_mid_fade_reverses_without_a_jump() {
        let mut m = Mixer::new(take(48_000), Listen::HushMic, 0);
        m.set_side(Listen::Original);
        let a = render(&mut m, 300);
        m.set_side(Listen::HushMic);
        let b = render(&mut m, CROSSFADE);
        let step = 2.0 / CROSSFADE as f32 + 1e-6;
        assert!((b[0] - a[299]).abs() <= step, "{} -> {}", a[299], b[0]);
        assert!(b.windows(2).all(|w| w[1] <= w[0]));
        assert_eq!(*b.last().unwrap(), -1.0);
        assert_eq!(render(&mut m, 5), vec![-1.0; 5]);
    }

    /// A new take (another strength) takes over at the same position
    /// with a 20 ms crossfade, on the side heard; the original side is the
    /// same audio and does not move.
    #[test]
    fn a_new_take_crossfades_in_where_it_plays() {
        let quieter = |frames: usize| {
            Arc::new(FrozenTake {
                raw: vec![1.0; frames],
                clean: vec![-0.5; frames],
                columns: Vec::new(),
                modes: Vec::new(),
                strengths: Vec::new(),
            })
        };
        let mut m = Mixer::new(take(48_000), Listen::HushMic, 1000);
        render(&mut m, SEEK_FADE + 100);
        let at = m.position();
        m.set_take(quieter(48_000));
        let x = render(&mut m, CROSSFADE + 10);
        assert_eq!(m.position(), at + CROSSFADE + 10, "the same place");
        assert!((x[0] - -1.0).abs() < 0.01, "{}", x[0]);
        assert!((x[CROSSFADE / 2] - -0.75).abs() < 0.01);
        assert!(x.windows(2).all(|w| w[1] >= w[0] - 1e-6), "a ramp, no jump");
        assert_eq!(x[CROSSFADE + 5], -0.5);
        // the original side is the same in both
        m.set_take(take(48_000));
        m.set_side(Listen::Original);
        let y = render(&mut m, CROSSFADE * 2);
        assert_eq!(*y.last().unwrap(), 1.0);
        // another length is another take: ignored
        m.set_take(quieter(100));
        assert_eq!(m.frames(), 48_000);
    }

    #[test]
    fn a_seek_fades_out_jumps_and_fades_in() {
        let t = Arc::new(FrozenTake {
            raw: (0..48_000).map(|i| i as f32).collect(),
            clean: (0..48_000).map(|i| i as f32).collect(),
            columns: Vec::new(),
            modes: Vec::new(),
            strengths: Vec::new(),
        });
        let mut m = Mixer::new(Arc::clone(&t), Listen::HushMic, 100);
        m.seek(30_000);
        let x = render(&mut m, SEEK_FADE * 2 + 10);
        // Fade-out over the old position (under the start's fade-in),
        // then the new one fading in.
        assert!(x[..SEEK_FADE].iter().enumerate().all(|(k, &v)| {
            let g = (k + 1) as f32 / SEEK_FADE as f32;
            let want = (100 + k) as f32 * (1.0 - g) * g;
            (v - want).abs() < 1e-2
        }));
        assert_eq!(x[SEEK_FADE - 1], 0.0);
        assert!((x[SEEK_FADE] - 30_000.0 / SEEK_FADE as f32).abs() < 1e-2);
        assert_eq!(x[2 * SEEK_FADE + 5], (30_000 + SEEK_FADE + 5) as f32);
        assert_eq!(m.position(), 30_000 + SEEK_FADE + 10);
    }

    /// A replay loops until Stop: past the end (its last 5 ms faded out)
    /// it starts over, fading in, on the side it plays, and the unrolled
    /// count the playhead's clock runs on goes on rising.
    #[test]
    fn a_replay_loops_until_stop() {
        let mut m = Mixer::new(take(1000), Listen::Original, 900);
        let x = render(&mut m, 480);
        assert_eq!(x.len(), 480, "no end: it starts over");
        assert_eq!((m.position(), m.unrolled()), (380, 1380));
        assert!(x[99].abs() < 0.01, "faded out at the end: {}", x[99]);
        assert!(x[100].abs() < 0.01, "fades in at the start: {}", x[100]);
        assert_eq!(x[479], 1.0);
        // a hold (the other side) plays on across the next lap
        m.set_side(Listen::HushMic);
        let y = render(&mut m, 1000);
        assert_eq!(*y.last().unwrap(), -1.0);
        assert_eq!(m.side(), Listen::HushMic);
        assert_eq!(m.unrolled(), 2380);
        assert_eq!(in_lap(m.unrolled(), 1000), 0.38);
        // Stop: fades out and ends
        m.finish();
        let end = render(&mut m, 2000);
        assert_eq!(end.len(), SEEK_FADE);
        assert_eq!(render(&mut m, 480).len(), 0);
        let m = Mixer::new(take(1000), Listen::Original, 5000);
        assert_eq!(m.position(), 1000, "clamped");
    }

    /// First run's replay plays the take once (UX panel round 2, A3): it
    /// ends at the take's end, its last 5 ms faded out, with no lap.
    #[test]
    fn a_take_played_once_ends_at_its_end() {
        let mut m = Mixer::new(take(1000), Listen::Original, 900).once(true);
        let x = render(&mut m, 480);
        assert_eq!(x.len(), 100, "the rest of the take, no more");
        assert!(x[99].abs() < 0.01, "faded out at the end: {}", x[99]);
        assert_eq!(render(&mut m, 480).len(), 0);
        assert_eq!(m.unrolled(), 1000);
    }
}
