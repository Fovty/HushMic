//! Mic check C ("Instant replay"): the last 10 seconds of the raw and the
//! clean microphone, replayed with same-word switching between the two.
//!
//! [`MicCheck`] is the page's handle: it runs one backend thread that
//! listens (the engine tap, or two recording streams as the fallback),
//! keeps the rolling buffer and its timeline columns, judges the level,
//! freezes and plays takes, and publishes a [`View`] snapshot. Every
//! decision is made by the pure [`state::CheckState`]; this module only
//! carries them out.
//!
//! Privacy: the audio lives in memory only while the page is visible. The
//! tap ring has no file name once the plugin mapped it; leaving or hiding
//! the page releases it (the plugin unmaps within 2 s) and drops the
//! buffer. A window that stops drawing (minimised where the desktop says
//! nothing, or hung) counts as hidden: when the page has not read a
//! changed view for [`LOOK_TIMEOUT`], the backend lets go as if it got
//! `Visible(false)`, and starts again on the page's next read. A frozen
//! take is never written to disk; a re-created window starts without it.
//! The picture's lanes ([`spectrum`]) are made from that audio and go
//! with it: cleared with the buffer, dropped with the take.

pub mod clock;
pub mod demo;
pub mod level;
pub mod metrics;
pub mod outlat;
pub mod player;
pub mod probe;
pub(crate) mod proc;
pub mod result;
pub mod source;
pub mod spectrum;
pub mod state;
pub mod stream;
pub mod take;
pub mod types;
pub mod xcorr;

pub use types::*;

use level::LevelCheck;
use player::{Player, PlayerState};
use source::{KeepAlive, StreamsPoll, StreamsSource, TapPoll, TapSource};
use state::{Action, CheckState, Event};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use take::{FrozenTake, Rolling};

/// Asks the window for a repaint after the given delay (egui's
/// `request_repaint_after`).
pub type Repaint = Arc<dyn Fn(Duration) + Send + Sync>;

/// The backend's clock: two ticks per timeline column (50 ms), so a column
/// is drawn at most 25 ms after it is complete (a strength change shows in
/// the column it falls in within about one column). Reading twice as often
/// costs about 0.15 % of a core.
const TICK: Duration = Duration::from_millis(25);
/// A chain that has not come up in this long is reported missing (as the
/// A/B window's settle window).
const SETTLE: Duration = Duration::from_secs(8);
/// A stream fallback that died restarts after this.
const RETRY: Duration = Duration::from_secs(1);
/// The page has not read a changed view for this long: the window is not
/// drawing, so nobody is looking (see the module docs).
pub const LOOK_TIMEOUT: Duration = Duration::from_millis(1_500);

/// Delete the files the window's players may have left in the runtime
/// dir: Mic check's restart player's WAV of a replay (the user's voice)
/// and the Files page's restart WAVs (a stretch of a recording). The
/// window removes them itself; the daemon calls this at start and exit,
/// and when the window ends, for a window that crashed.
pub fn remove_files() {
    if let Some(d) = proc::work_dir() {
        let _ = std::fs::remove_file(d.join(player::PLAY_FILE));
        crate::files::player::remove_files(&d);
    }
}

enum Msg {
    Cmd(Command),
    /// The page read the view again after the backend paused.
    Resume,
    Quit,
}

/// What the page and the backend share.
struct Shared {
    view: Mutex<View>,
    /// CLOCK_MONOTONIC ms of the page's last `view()`.
    read_ms: AtomicU64,
    /// The backend paused because the page stopped reading.
    paused: AtomicBool,
    /// The running replay's clock and the take's length (frames): the
    /// playhead is read from it on every frame the page draws, not at the
    /// backend's tick.
    clock: Mutex<Option<(Arc<clock::PlayClock>, usize)>>,
    /// The two lanes of the last 10 seconds, or of the take a replay
    /// plays.
    picture: Arc<spectrum::Picture>,
}

/// The page's handle. See the module docs.
pub struct MicCheck {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl MicCheck {
    /// Start the backend for a visible page. `repaint` is called whenever
    /// the view changed.
    pub fn start(setup: Setup, repaint: Repaint) -> MicCheck {
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = Arc::new(Shared {
            view: Mutex::new(View {
                mic_name: setup.info.mic_name.clone(),
                ..View::default()
            }),
            read_ms: AtomicU64::new(hushmic_shm::monotonic_ms()),
            paused: AtomicBool::new(false),
            clock: Mutex::new(None),
            picture: Arc::new(spectrum::Picture::new()),
        });
        let s = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("miccheck".into())
            .spawn(move || Backend::new(setup, s, repaint).run(&rx))
            .ok();
        let _ = tx.send(Msg::Cmd(Command::Visible(true)));
        MicCheck { tx, shared, thread }
    }

    pub fn send(&self, c: Command) {
        let _ = self.tx.send(Msg::Cmd(c));
    }

    /// The latest snapshot. Call it from the frames that draw the page:
    /// the reads are how the backend knows someone is looking.
    pub fn view(&self) -> View {
        self.shared
            .read_ms
            .store(hushmic_shm::monotonic_ms(), Ordering::Release);
        if self.shared.paused.swap(false, Ordering::AcqRel) {
            let _ = self.tx.send(Msg::Resume);
        }
        let mut v = self.shared.view.lock().unwrap().clone();
        if v.timeline.position.is_some() {
            if let Some((clock, frames)) = self.shared.clock.lock().unwrap().as_ref() {
                if *frames > 0 {
                    // the replay loops: the playhead starts over each lap
                    let heard = clock.heard(std::time::Instant::now());
                    v.timeline.position = Some(player::in_lap(heard, *frames));
                }
            }
        }
        v
    }

    /// The picture of what the view's timeline covers. The backend
    /// analyses the audio for it from the first call on.
    pub fn picture(&self) -> Arc<spectrum::Picture> {
        self.shared.picture.want();
        Arc::clone(&self.shared.picture)
    }
}

impl Drop for MicCheck {
    /// Never waits: the backend fades a running replay out and lets go of
    /// the microphone on its own thread, within about 200 ms.
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
        // Detached: the thread owns everything it still needs.
        self.thread.take();
    }
}

enum Src {
    Off,
    /// Waiting for the chain's meter (the plugin is starting).
    Waiting {
        since: Instant,
    },
    Tap(TapSource),
    Streams(StreamsSource),
    /// The fallback died; try again at `at`.
    Retry {
        at: Instant,
    },
}

struct Backend {
    state: CheckState,
    rolling: Rolling,
    level: LevelCheck,
    src: Src,
    /// Records hushmic_source while the page listens (see KeepAlive).
    keep_alive: Option<KeepAlive>,
    frozen: Option<Arc<FrozenTake>>,
    /// The frozen take's spectrum, while a page draws the picture.
    frozen_lanes: Option<spectrum::Frozen>,
    analyzer: Option<spectrum::Analyzer>,
    /// The frozen take's sides are the same moments to the sample (the
    /// tap, or streams that aligned): it can be re-rendered at another
    /// strength.
    frozen_exact: bool,
    player: Option<Player>,
    /// The seamless player failed before playing anything once: this
    /// PipeWire cannot play from a pipe.
    restart_only: bool,
    shared: Arc<Shared>,
    repaint: Repaint,
    done: Vec<(Column, take::ColumnStats)>,
    published: Option<u64>,
    columns_moved: bool,
    /// Wake the page for new columns too ([`Setup::columns`]).
    wake_on_columns: bool,
    /// A replay plays once ([`Setup::once`]).
    once: bool,
    /// When the oldest view the page has not read yet was published (ms).
    unread_since: Option<u64>,
    /// Paused for want of reads (see the module docs).
    paused: bool,
}

impl Backend {
    fn new(setup: Setup, shared: Arc<Shared>, repaint: Repaint) -> Backend {
        let wake_on_columns = setup.columns;
        Backend {
            wake_on_columns,
            once: setup.once,
            state: CheckState::new(&setup),
            rolling: Rolling::with_picture(Arc::clone(&shared.picture)),
            level: LevelCheck::new(),
            src: Src::Off,
            keep_alive: None,
            frozen: None,
            frozen_lanes: None,
            analyzer: None,
            frozen_exact: false,
            player: None,
            restart_only: false,
            shared,
            repaint,
            done: Vec::new(),
            published: None,
            columns_moved: false,
            unread_since: None,
            paused: false,
        }
    }

    fn run(mut self, rx: &Receiver<Msg>) {
        let mut next_tick = Instant::now();
        loop {
            let wait = next_tick.saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(Msg::Cmd(c)) => {
                    if matches!(c, Command::Visible(_)) {
                        // The page says it: no pause of ours stands.
                        self.paused = false;
                        self.shared.paused.store(false, Ordering::Release);
                    }
                    let actions = self.state.on_command(&c);
                    self.execute(actions);
                    self.publish();
                    continue;
                }
                Ok(Msg::Resume) => {
                    if std::mem::take(&mut self.paused) {
                        let actions = self.state.on_command(&Command::Visible(true));
                        self.execute(actions);
                        self.publish();
                    }
                    continue;
                }
                Ok(Msg::Quit) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            next_tick = Instant::now() + TICK;
            self.tick(Instant::now());
            self.publish();
        }
        self.player = None;
        self.src = Src::Off;
        self.keep_alive = None;
        // the page may outlive this thread by a frame: nothing of the
        // audio stays behind in what they share
        self.rolling.clear();
        self.shared.picture.unfreeze();
    }

    /// The keep-alive runs while the page listens, from whichever source
    /// (none while blocked): a source stopped and started again (a new
    /// chain, a lost tap, the fallback) keeps the same one.
    fn keep_alive(&mut self, now: Instant) {
        let wanted = self.state.wants_source() && !matches!(self.src, Src::Off);
        match (wanted, self.keep_alive.as_mut()) {
            (true, Some(k)) => k.poll(now),
            (true, None) => self.keep_alive = Some(KeepAlive::start(now)),
            (false, Some(_)) => self.keep_alive = None,
            (false, None) => {}
        }
    }

    fn execute(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::StartSource => {
                    self.clear_buffer();
                    self.src = Src::Waiting {
                        since: Instant::now(),
                    };
                }
                Action::StopSource => {
                    self.src = Src::Off;
                    self.clear_buffer();
                }
                Action::Replay { side } => self.replay(side),
                Action::Switch(side) => {
                    if let Some(p) = self.player.as_mut() {
                        p.set_side(side);
                    }
                }
                Action::Seek(at) => {
                    if let Some(p) = self.player.as_mut() {
                        p.seek(at);
                    }
                }
                Action::StopPlayback => self.player = None,
                Action::DropTake => {
                    self.frozen = None;
                    self.frozen_lanes = None;
                    self.shared.picture.unfreeze();
                }
                Action::Restrength => self.restrength(),
            }
        }
        self.keep_alive(Instant::now());
    }

    /// Forget the rolling buffer and the level, and tell the state (it
    /// must never offer to replay audio that is gone).
    fn clear_buffer(&mut self) {
        self.rolling.clear();
        self.level.reset();
        self.columns_moved = true;
        self.state.on_event(Event::Buffered(0.0));
    }

    /// The strength moved: what is recorded from now on is at it, and the
    /// frozen take plays and shows as cleaned at it.
    fn restrength(&mut self) {
        let info = self.state.info().clone();
        self.rolling.set_strength(info.strength_db);
        if !self.frozen_exact {
            return;
        }
        let Some(take) = self
            .frozen
            .as_ref()
            .and_then(|t| t.at_strength(info.strength_db))
        else {
            return;
        };
        let take = Arc::new(take);
        self.state
            .on_event(Event::Restrength(result::evaluate(&take, info.engine)));
        if let Some(p) = self.player.as_mut() {
            p.set_take(Arc::clone(&take));
        }
        // the picture's clean lane follows, from the cells it kept
        if let (Some(lanes), Some(old)) = (self.frozen_lanes.as_mut(), self.frozen.as_ref()) {
            lanes.restrength(&old.strengths, info.strength_db);
            self.shared.picture.freeze(lanes.columns());
        }
        self.frozen = Some(take);
        self.columns_moved = true;
    }

    /// The frozen take's lanes for the picture (after the replay started:
    /// the sound never waits for them).
    fn freeze_picture(&mut self, take: &FrozenTake) {
        if !self.shared.picture.wanted() {
            self.frozen_lanes = None;
            self.shared.picture.unfreeze();
            return;
        }
        let analyzer = self.analyzer.get_or_insert_with(spectrum::Analyzer::new);
        let lanes = spectrum::Frozen::of(analyzer, take);
        self.shared.picture.freeze(lanes.columns());
        self.frozen_lanes = Some(lanes);
    }

    /// Freeze what the timeline shows and play it.
    fn replay(&mut self, side: Listen) {
        let engine = self.state.info().engine;
        let mut kind = PlaybackKind::Seamless;
        let fresh = if self.rolling.held_secs() >= MIN_REPLAY_SECS {
            match &mut self.src {
                Src::Streams(s) => s.freeze(TAKE_FRAMES).map(|(mut take, aligned, changed)| {
                    // Re-aligned audio, the same moments: the live
                    // columns' modes hold for it.
                    let modes = self.rolling.modes_tail(take.columns.len());
                    if modes.len() == take.columns.len() {
                        take.modes = modes;
                    }
                    let strengths = self.rolling.strengths_tail(take.columns.len());
                    if strengths.len() == take.columns.len() {
                        take.strengths = strengths;
                    }
                    if changed {
                        // The pairing lag moved: the live view starts over.
                        self.rolling.clear();
                        self.columns_moved = true;
                    }
                    if !aligned {
                        kind = PlaybackKind::Sequential;
                    }
                    take
                }),
                _ => self.rolling.freeze(),
            }
        } else {
            None
        };
        self.state
            .on_event(Event::Buffered(self.rolling.held_secs()));
        if kind == PlaybackKind::Seamless && self.restart_only {
            kind = PlaybackKind::Restart;
        }
        let Some(take) = fresh else {
            self.state.on_event(Event::NothingToReplay);
            return;
        };
        let take = Arc::new(take);
        self.state.on_event(Event::Frozen {
            result: result::evaluate(&take, engine),
            playback: kind,
        });
        self.frozen = Some(Arc::clone(&take));
        self.frozen_exact = kind != PlaybackKind::Sequential;
        self.play(Arc::clone(&take), side, 0.0, kind);
        self.freeze_picture(&take);
    }

    fn play(&mut self, take: Arc<FrozenTake>, side: Listen, at: f32, kind: PlaybackKind) {
        let kind = match kind {
            PlaybackKind::Seamless if self.restart_only => PlaybackKind::Restart,
            k => k,
        };
        let dir = proc::work_dir();
        self.player = Player::start(take, side, at, kind, dir.as_deref(), self.once);
        if self.player.is_none() {
            self.state.on_event(Event::PlaybackFailed);
        }
    }

    fn tick(&mut self, now: Instant) {
        self.check_looking();
        if self.state.wants_source() {
            self.poll_source(now);
        }
        self.keep_alive(now);
        self.poll_player();
    }

    /// Pause when the page stopped reading a changing view.
    fn check_looking(&mut self) {
        let read = self.shared.read_ms.load(Ordering::Acquire);
        if self.unread_since.is_some_and(|since| read >= since) {
            self.unread_since = None;
        }
        let now = hushmic_shm::monotonic_ms();
        let stale = self
            .unread_since
            .is_some_and(|since| now.saturating_sub(since) > LOOK_TIMEOUT.as_millis() as u64);
        if stale && self.state.wants_source() {
            let actions = self.state.on_command(&Command::Visible(false));
            self.execute(actions);
            self.paused = true;
            self.unread_since = None;
            self.shared.paused.store(true, Ordering::Release);
        }
    }

    fn poll_source(&mut self, now: Instant) {
        self.done.clear();
        let info = self.state.info().clone();
        self.rolling.set_engine(info.engine);
        self.rolling.set_strength(info.strength_db);
        let mut event = None;
        match &mut self.src {
            Src::Off => {}
            Src::Waiting { since } => {
                let meter_ok = info
                    .meter
                    .as_deref()
                    .is_some_and(|m| hushmic_shm::meter::MeterReader::open(m).is_ok());
                if meter_ok {
                    match TapSource::request(info.meter.as_deref().unwrap()) {
                        Ok(t) => self.src = Src::Tap(t),
                        Err(_) => self.src = streams_or_blocked(&info),
                    }
                } else if now.saturating_duration_since(*since) > SETTLE {
                    // No meter (a plugin without the engine view, no
                    // runtime dir, or none known yet): try the recording
                    // streams.
                    self.src = streams_or_blocked(&info);
                    if matches!(self.src, Src::Off) {
                        event = Some(Event::Blocked(Blocker::NoMicrophone));
                    }
                } else {
                    event = Some(Event::Connecting);
                }
            }
            Src::Tap(t) => match t.poll(now, &mut self.rolling, &mut self.done) {
                TapPoll::Waiting => event = Some(Event::Connecting),
                TapPoll::Running => event = Some(Event::Ready(Source::Tap)),
                TapPoll::Unavailable => {
                    self.src = streams_or_blocked(&info);
                    if matches!(self.src, Src::Off) {
                        event = Some(Event::Blocked(Blocker::NoMicrophone));
                    }
                }
                TapPoll::Lost => {
                    // The plugin let go while we look: ask again.
                    self.rolling.clear();
                    self.columns_moved = true;
                    self.src = Src::Waiting { since: now };
                    event = Some(Event::Connecting);
                }
            },
            Src::Streams(s) => match s.poll(&mut self.rolling, &mut self.done) {
                StreamsPoll::Waiting => event = Some(Event::Connecting),
                StreamsPoll::Running => event = Some(Event::Ready(Source::Streams)),
                StreamsPoll::Died { unsupported: true } => {
                    self.src = Src::Off;
                    event = Some(Event::Blocked(Blocker::NeedsNewerPipewire));
                }
                StreamsPoll::Died { unsupported: false } => {
                    self.src = Src::Retry { at: now + RETRY };
                    self.rolling.clear();
                    self.columns_moved = true;
                    event = Some(Event::CaptureStopped);
                }
            },
            Src::Retry { at } => {
                if now >= *at {
                    self.src = streams_or_blocked(&info);
                }
            }
        }
        if let Some(e) = event {
            self.state.on_event(e);
        }
        if !self.done.is_empty() {
            let engine = info.engine;
            for (_, stats) in &self.done {
                self.level.feed(stats, engine, self.rolling.zero_run());
            }
            self.columns_moved = true;
            self.state.on_event(Event::Level(self.level.level()));
        }
        // Every tick, also after a clear (a gap, a lost source): the state
        // only moves when the page would see a difference.
        self.state
            .on_event(Event::Buffered(self.rolling.held_secs()));
    }

    fn poll_player(&mut self) {
        let Some(p) = self.player.as_mut() else {
            return;
        };
        match p.poll() {
            PlayerState::Playing => {
                let pos = p.position();
                self.state.on_event(Event::Position(pos));
            }
            PlayerState::Ended => {
                self.player = None;
                self.state.on_event(Event::PlaybackEnded);
            }
            PlayerState::Failed { early } => {
                let kind = p.kind();
                let at = p.position();
                self.player = None;
                if early && kind == PlaybackKind::Seamless && !self.restart_only {
                    // This pw-cat cannot play from a pipe: restart mode,
                    // from the same place.
                    self.restart_only = true;
                    if let Some(t) = self.frozen.clone() {
                        let side = self.state.listen();
                        self.play(t, side, at, PlaybackKind::Restart);
                        self.state.on_event(Event::Frozen {
                            result: self.state.result(),
                            playback: PlaybackKind::Restart,
                        });
                        return;
                    }
                }
                self.state.on_event(Event::PlaybackFailed);
            }
        }
    }

    fn publish(&mut self) {
        if self.rolling.take_woke() {
            // the picture moved for a page that had stopped looking
            (self.repaint)(Duration::from_millis(5));
        }
        *self.shared.clock.lock().unwrap() = self
            .player
            .as_ref()
            .and_then(|p| Some((p.clock()?, p.frames())));
        let serial = self.state.serial();
        let changed = self.published != Some(serial);
        if !changed && !self.columns_moved {
            return;
        }
        self.published = Some(serial);
        self.columns_moved = false;
        let replaying = self.state.replaying();
        self.shared
            .picture
            .show_frozen(replaying && self.frozen.is_some());
        let columns = match (&self.frozen, replaying) {
            (Some(t), true) => t.columns.clone(),
            _ => self.rolling.columns().collect(),
        };
        *self.shared.view.lock().unwrap() = self.state.view(columns);
        if self.unread_since.is_none() {
            self.unread_since = Some(hushmic_shm::monotonic_ms());
        }
        if !changed && !self.wake_on_columns {
            // only new columns, which the page does not draw: no reason to
            // wake it (it reads them on its next frame, at least once a
            // second, so a window that stopped drawing still lets go)
            return;
        }
        (self.repaint)(Duration::from_millis(5));
    }
}

/// The recording-stream fallback, if a microphone is known.
fn streams_or_blocked(info: &EngineInfo) -> Src {
    match info.mic_node.as_deref() {
        Some(node) => Src::Streams(StreamsSource::start(node)),
        None => Src::Off,
    }
}
