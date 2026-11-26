//! The contract between the Mic check page and its backend: what the page
//! sends ([`Command`]), what it draws ([`View`]), and how the backend is
//! set up ([`Setup`]). Plain data, serde-ready; a page test builds a
//! [`View`] by hand instead of running a backend.

use crate::meter::EngineMode;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Samples per second of every take.
pub const SAMPLE_RATE: u32 = 48_000;
/// One timeline column: 50 ms.
pub const COLUMN_FRAMES: usize = 2400;
/// The timeline: the last 10 seconds, 200 columns.
pub const COLUMNS: usize = 200;
pub const TAKE_FRAMES: usize = COLUMNS * COLUMN_FRAMES;
/// The comfortable speech lane on the timeline, on the level scale
/// (`(dBFS + 60) / 60`): -27 to -12 dBFS. "Your voice reaches into the
/// light band."
pub const BAND: (f32, f32) = (0.55, 0.8);
/// Replay becomes available once this much audio is in the buffer.
pub const MIN_REPLAY_SECS: f32 = 2.0;

/// What HushMic does with the microphone, as the daemon reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    Filtering,
    Bypass,
    Muted,
    /// HushMic is off: no chain, nothing to check.
    Off,
}

impl From<EngineMode> for Engine {
    fn from(m: EngineMode) -> Engine {
        match m {
            EngineMode::Filtering => Engine::Filtering,
            EngineMode::Bypass => Engine::Bypass,
            EngineMode::Muted => Engine::Muted,
        }
    }
}

/// The daemon's facts the backend needs, from the window's state stream.
/// Send a fresh one ([`Command::Engine`]) whenever any of it changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineInfo {
    pub engine: Engine,
    /// The current chain's meter file (`None` while off or starting). A
    /// different path is a different chain (new microphone, model, or a
    /// restart): the take recorded before it is dropped.
    pub meter: Option<PathBuf>,
    /// The noise-removal settings in effect (AppState `sound.current`:
    /// `model_id`, `attn_limit`). Strength is written live, without a new
    /// chain, so a change here drops the take as a new chain does.
    pub model: String,
    pub strength_db: f32,
    /// The microphone feeding the chain: its PipeWire node name (for the
    /// fallback capture) and the name to show ("No sound from {mic}").
    pub mic_node: Option<String>,
    pub mic_name: String,
    /// PipeWire answered the daemon's last probe.
    pub pipewire: bool,
}

/// How the backend starts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Setup {
    pub info: EngineInfo,
    /// The page draws the columns: each new one wakes it. Live draws none
    /// (its picture reads the engine meter itself), so only a changed
    /// state wakes it there.
    pub columns: bool,
    /// A replay plays the take once and ends (first run's "Play the last
    /// 10 seconds", UX panel round 2 A3); Live's loops until Stop.
    pub once: bool,
}

/// Which side is heard (and stands in front on the timeline).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Listen {
    /// What apps hear.
    #[default]
    HushMic,
    /// The raw microphone.
    Original,
}

/// Page → backend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// The page is visible (true) or hidden / window minimized (false).
    /// Hidden lets go of the microphone and the tap; the rolling buffer is
    /// dropped. Visible starts listening again.
    Visible(bool),
    /// "Replay the last 10 seconds": freeze the buffer and play it from
    /// the start, HushMic side (or the side [`Command::Listen`] chose).
    Replay,
    /// Stop a replay: the result shows, the timeline goes live again.
    Stop,
    /// Hold to hear the original (button pointer or Space, down = true).
    /// Outside a replay, down starts one on the original side; up goes
    /// back to the latch ([`Command::Listen`]: HushMic unless latched) at
    /// the same position. A replay that ends ends the hold too; the up
    /// that follows is ignored. Key repeats must not be sent as new downs;
    /// a window focus loss while held must send `Hold(false)`.
    Hold(bool),
    /// The latch: the side replays play while nothing is held (a tap on
    /// the hold control latches the original and the next tap HushMic;
    /// send it before the tap's `Hold(false)`). Switches at the same
    /// position during a replay, otherwise sets the side the next replay
    /// starts on. The backend keeps the side last sent, across the end of
    /// a replay and Stop; the page decides when a latch clears (it sends
    /// `Listen(HushMic)` when a new take starts). It ends a hold.
    Listen(Listen),
    /// A click on the frozen timeline, 0..=1.
    Seek(f32),
    /// The daemon's facts changed.
    Engine(EngineInfo),
}

/// Where the take comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// Not decided yet (asking the plugin for the tap).
    Pending,
    /// The engine tap: raw and clean of the same hops, exact.
    Tap,
    /// Two recording streams aligned by cross-correlation when a take is
    /// frozen (the tap was not available).
    Streams,
}

/// How switching sides sounds during a replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackKind {
    /// One stream fed by the backend: the other side continues at the same
    /// sample after a 20 ms crossfade.
    Seamless,
    /// Playback restarts at the same position on the other side: a short
    /// gap per switch (PipeWire too old to play from a pipe).
    Restart,
    /// The take could not be aligned (no speech in common, a stream
    /// dropped data): same-word switching is off; the page offers the two
    /// versions one after the other.
    Sequential,
}

/// The page's main state (appux's state table).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// The chain or the tap is starting, or the capture is being
    /// re-routed: "Connecting to your microphone…".
    Connecting,
    /// The last 10 seconds scroll by.
    Listening,
    /// The frozen take plays ([`View::listen`] says which side,
    /// [`View::holding`] whether by the hold gesture).
    Replaying,
    /// After a replay: the result shows, the timeline is live again.
    Result,
    /// The microphone delivers exact digital silence (a switched-off
    /// wireless headset): "No sound from {mic}".
    MicProblem,
    /// Nothing can run; see [`Blocker`].
    Blocked(Blocker),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Blocker {
    /// HushMic is off: "HushMic is off." [Turn on HushMic]
    Off,
    /// PipeWire is not reachable.
    NoPipewire,
    /// No microphone feeds the chain.
    NoMicrophone,
    /// No tap and this PipeWire cannot stream a capture to a pipe: the
    /// page explains; the tray's recording test still works.
    NeedsNewerPipewire,
}

/// A line above the timeline with one action (appux's edge states).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Notice {
    /// "Noise suppression is off: both sides sound the same." [Turn it on]
    Bypass,
    /// "Muted: only the original has sound." [Unmute]
    Muted,
    /// "Your settings changed. …" (microphone, model or strength changed
    /// during a check; the take was dropped). Clears once the buffer
    /// holds new audio.
    SettingsChanged,
}

/// One 50 ms column of the timeline, on the level scale (0..1). `raw` is
/// what the microphone hears, `clean` what apps hear.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub raw: f32,
    pub clean: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    /// Up to [`COLUMNS`], oldest first. The newest is at the right edge,
    /// live or frozen: a frozen take of `n` columns puts column `i` at
    /// `(COLUMNS - n + i) / COLUMNS` of the width.
    pub columns: Vec<Column>,
    /// The columns scroll (true) or show the frozen take (false).
    pub live: bool,
    /// The playhead, 0..=1 of the frozen take, while replaying.
    pub position: Option<f32>,
}

/// The level advice under the timeline, judged on the raw side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LevelVerdict {
    /// Not enough speech heard yet: "Say a few words".
    #[default]
    Waiting,
    /// "Sounds good": speech reaches into the band.
    Good,
    /// Speech stays below the band: raise the input volume.
    VeryQuiet,
    /// Speech clips or overloads: lower the input volume.
    TooLoud,
    /// Exact digital silence for 3 s.
    NoSound,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Level {
    pub verdict: LevelVerdict,
    /// Typical loud-speech level on the level scale, for a marker on the
    /// band (None while waiting).
    pub speech: Option<f32>,
}

/// "Between your words: much quieter (26 dB)".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quieter {
    /// 20 dB or more.
    Much,
    /// 10 to 20 dB.
    Clearly,
    /// 3 to 10 dB.
    Slightly,
    /// Under 3 dB: there was little noise to remove, or bypass.
    Same,
}

/// "Your voice: kept".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Voice {
    /// Within 3 dB of the original.
    Kept,
    /// 3 dB or more quieter than the original.
    Quieter,
    /// Not enough speech in the take: "Say a full sentence to check your
    /// voice". Never a made-up number.
    NotMeasured,
}

/// The measured result of the frozen take.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TakeResult {
    pub between_words: Quieter,
    /// How much quieter the pauses are, whole dB (>= 0). `None` when the
    /// take had no pauses to measure.
    pub between_words_db: Option<f32>,
    pub voice: Voice,
    /// Voice level change, whole dB (<= 0 is quieter); `None` when not
    /// measured.
    pub voice_db: Option<f32>,
}

/// Something went wrong that the page shows as a short message, then
/// forgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Warning {
    /// Playback could not start (no output device, pw-cat missing).
    PlaybackFailed,
    /// The capture stream ended unexpectedly; listening restarts.
    CaptureStopped,
}

/// Everything the page draws. A snapshot: cheap to clone (about 2 KiB).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub phase: Phase,
    pub notice: Option<Notice>,
    /// The side heard during a replay, or the side the next replay starts on.
    pub listen: Listen,
    /// The hold gesture is down.
    pub holding: bool,
    pub timeline: Timeline,
    /// "Replay the last 10 seconds" is enabled.
    pub can_replay: bool,
    /// How far the take is toward the length Replay needs
    /// ([`MIN_REPLAY_SECS`]), 0..1: the disabled button fills with it.
    pub collected: f32,
    pub level: Level,
    /// The last replayed take's result (kept while listening goes on).
    pub result: Option<TakeResult>,
    pub source: Source,
    pub playback: PlaybackKind,
    /// The microphone's display name.
    pub mic_name: String,
    /// Shown once per change of the value (compare with the previous view).
    pub warning: Option<Warning>,
    /// Bumped on every change, so a page can skip identical snapshots.
    pub serial: u64,
}

impl Default for View {
    fn default() -> Self {
        View {
            phase: Phase::Connecting,
            notice: None,
            listen: Listen::HushMic,
            holding: false,
            timeline: Timeline {
                columns: Vec::new(),
                live: true,
                position: None,
            },
            can_replay: false,
            collected: 0.0,
            level: Level::default(),
            result: None,
            source: Source::Pending,
            playback: PlaybackKind::Seamless,
            mic_name: String::new(),
            warning: None,
            serial: 0,
        }
    }
}
