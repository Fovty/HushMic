//! The pure state machine of the Mic check page: commands from the page
//! and events from the backend's sources and player go in, the backend's
//! actions and the page's [`View`] come out. No audio, no processes, no
//! clock: everything the backend does is decided here and tested here.

use super::types::*;

/// What the backend's sources and player report.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The buffer now holds this many seconds of complete columns.
    Buffered(f32),
    Level(Level),
    /// The source is starting (chain coming up, tap requested, capture
    /// re-routed).
    Connecting,
    /// Audio flows from `Source`.
    Ready(Source),
    /// Nothing can run.
    Blocked(Blocker),
    /// A take was frozen for a replay: its result and how it can be played.
    Frozen {
        result: Option<TakeResult>,
        playback: PlaybackKind,
    },
    /// The frozen take was re-rendered at the new strength: its result.
    Restrength(Option<TakeResult>),
    /// Freezing found nothing to replay.
    NothingToReplay,
    /// The playhead, 0..=1.
    Position(f32),
    PlaybackEnded,
    PlaybackFailed,
    /// The capture stream ended unexpectedly; the source restarts.
    CaptureStopped,
}

/// What the backend must do.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Start listening (tap or fallback) for the current chain.
    StartSource,
    /// Stop listening: release the tap, end the keep-alive and the
    /// recorders, drop the rolling buffer.
    StopSource,
    /// Freeze the rolling buffer into a take and start playing it.
    Replay {
        side: Listen,
    },
    Switch(Listen),
    Seek(f32),
    StopPlayback,
    /// Forget the frozen take and the result (settings changed).
    DropTake,
    /// The strength moved on the running chain: the frozen take plays
    /// and shows as cleaned at it (re-rendered, the result with it).
    Restrength,
}

fn switch_if_changed(before: Listen, now: Listen) -> Vec<Action> {
    if before == now {
        vec![]
    } else {
        vec![Action::Switch(now)]
    }
}

pub struct CheckState {
    info: EngineInfo,
    visible: bool,
    status: Status,
    source: Source,
    replaying: bool,
    after_replay: bool,
    /// The side a replay plays while nothing is held: the latch, set on
    /// purpose by the page ([`Command::Listen`]) and kept until it sends
    /// another (the page clears it when a new take starts). A hold never
    /// changes it.
    latch: Listen,
    /// The hold control (or Space) is down: the original plays.
    holding: bool,
    /// The replay playing was started by a hold: its release ends it
    /// (UX panel round 2, A3).
    held_replay: bool,
    position: Option<f32>,
    result: Option<TakeResult>,
    settings_changed: bool,
    held_secs: f32,
    level: Level,
    playback: PlaybackKind,
    warning: Option<Warning>,
    serial: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Connecting,
    Ready,
    Blocked(Blocker),
}

impl CheckState {
    pub fn new(setup: &Setup) -> CheckState {
        CheckState {
            info: setup.info.clone(),
            visible: false,
            status: Status::Connecting,
            source: Source::Pending,
            replaying: false,
            after_replay: false,
            latch: Listen::HushMic,
            holding: false,
            held_replay: false,
            position: None,
            result: None,
            settings_changed: false,
            held_secs: 0.0,
            level: Level::default(),
            playback: PlaybackKind::Seamless,
            warning: None,
            serial: 0,
        }
    }

    fn engine_runs(&self) -> bool {
        self.info.engine != Engine::Off
    }

    pub fn can_replay(&self) -> bool {
        if self.replaying || !self.engine_runs() || !self.info.pipewire {
            return false;
        }
        self.status == Status::Ready
            && self.level.verdict != LevelVerdict::NoSound
            && self.held_secs >= MIN_REPLAY_SECS
    }

    pub fn phase(&self) -> Phase {
        if self.info.engine == Engine::Off {
            return Phase::Blocked(Blocker::Off);
        }
        if !self.info.pipewire {
            return Phase::Blocked(Blocker::NoPipewire);
        }
        if let Status::Blocked(b) = self.status {
            return Phase::Blocked(b);
        }
        if self.replaying {
            return Phase::Replaying;
        }
        if self.level.verdict == LevelVerdict::NoSound {
            return Phase::MicProblem;
        }
        if self.status == Status::Connecting {
            return Phase::Connecting;
        }
        if self.after_replay {
            Phase::Result
        } else {
            Phase::Listening
        }
    }

    pub fn notice(&self) -> Option<Notice> {
        if self.settings_changed {
            return Some(Notice::SettingsChanged);
        }
        match self.info.engine {
            Engine::Bypass => Some(Notice::Bypass),
            Engine::Muted => Some(Notice::Muted),
            _ => None,
        }
    }

    /// Whether the page wants audio: the backend runs a source only then.
    pub fn wants_source(&self) -> bool {
        self.visible && self.engine_runs() && self.info.pipewire
    }

    pub fn info(&self) -> &EngineInfo {
        &self.info
    }

    fn changed(&mut self) {
        self.serial += 1;
    }

    /// The side playing: the original while held, else the latch.
    fn side(&self) -> Listen {
        if self.holding {
            Listen::Original
        } else {
            self.latch
        }
    }

    fn start_replay(&mut self, holding: bool) -> Vec<Action> {
        self.replaying = true;
        self.after_replay = false;
        self.holding = holding;
        self.held_replay = holding;
        self.position = Some(0.0);
        vec![Action::Replay { side: self.side() }]
    }

    /// The replay is over (Stop, or what ends the source: the replay
    /// itself loops); so is a hold (its release may follow and is then
    /// ignored). The latch stays.
    fn end_replay(&mut self) {
        self.replaying = false;
        self.holding = false;
        self.held_replay = false;
        self.position = None;
    }

    pub fn on_command(&mut self, c: &Command) -> Vec<Action> {
        self.warning = None;
        self.changed();
        match c {
            Command::Visible(true) => {
                if self.visible {
                    return vec![];
                }
                self.visible = true;
                self.status = Status::Connecting;
                if self.wants_source() {
                    vec![Action::StartSource]
                } else {
                    vec![]
                }
            }
            Command::Visible(false) => {
                if !self.visible {
                    return vec![];
                }
                self.visible = false;
                self.end_replay();
                self.held_secs = 0.0;
                self.level = Level::default();
                self.status = Status::Connecting;
                vec![Action::StopPlayback, Action::StopSource]
            }
            Command::Replay => {
                if self.can_replay() {
                    self.start_replay(false)
                } else {
                    vec![]
                }
            }
            Command::Stop => {
                if self.replaying {
                    self.end_replay();
                    self.after_replay = true;
                    vec![Action::StopPlayback]
                } else {
                    vec![]
                }
            }
            Command::Hold(true) => {
                if self.holding {
                    vec![]
                } else if self.replaying {
                    let before = self.side();
                    self.holding = true;
                    switch_if_changed(before, self.side())
                } else if self.can_replay() {
                    self.start_replay(true)
                } else {
                    vec![]
                }
            }
            Command::Hold(false) => {
                if !self.holding {
                    return vec![];
                }
                if self.held_replay && self.replaying {
                    // a hold with nothing playing heard the original for
                    // as long as it was held, and nothing after it
                    self.end_replay();
                    self.after_replay = true;
                    return vec![Action::StopPlayback];
                }
                self.holding = false;
                // Back to the latch (HushMic unless the page latched).
                if self.replaying {
                    switch_if_changed(Listen::Original, self.side())
                } else {
                    vec![]
                }
            }
            Command::Listen(side) => {
                // A latch set on purpose ends a hold (a tap's release may
                // follow and is then ignored), and keeps the replay going.
                let before = self.side();
                self.holding = false;
                self.held_replay = false;
                self.latch = *side;
                if self.replaying {
                    switch_if_changed(before, self.side())
                } else {
                    vec![]
                }
            }
            Command::Seek(at) => {
                if self.replaying && at.is_finite() {
                    let at = at.clamp(0.0, 1.0);
                    self.position = Some(at);
                    vec![Action::Seek(at)]
                } else {
                    vec![]
                }
            }
            Command::Engine(info) => self.on_engine(info.clone()),
        }
    }

    fn on_engine(&mut self, info: EngineInfo) -> Vec<Action> {
        let old = std::mem::replace(&mut self.info, info);
        // The strength is not among them: the daemon writes it to the
        // running chain, which goes on (the take stays comparable, and the
        // live picture shows the change in its next column).
        let chain_changed = old.meter != self.info.meter
            || old.mic_node != self.info.mic_node
            || old.model != self.info.model;
        let restrength = !chain_changed && old.strength_db != self.info.strength_db;
        let was_wanted = self.visible && old.engine != Engine::Off && old.pipewire;
        let mut actions = Vec::new();
        if chain_changed {
            // Another microphone or model (or a restarted chain): the take
            // would compare the old with the new.
            let had_audio = self.held_secs > 0.0 || self.replaying || self.result.is_some();
            self.end_replay();
            self.after_replay = false;
            self.result = None;
            self.held_secs = 0.0;
            self.level = Level::default();
            self.status = Status::Connecting;
            self.settings_changed = had_audio && self.engine_runs();
            actions.extend([Action::StopPlayback, Action::DropTake]);
            if was_wanted {
                actions.push(Action::StopSource);
            }
            if self.wants_source() {
                actions.push(Action::StartSource);
            }
        } else if was_wanted && !self.wants_source() {
            // The same chain, but PipeWire did not answer a probe (or the
            // engine stopped without a new chain yet): let go of the
            // microphone, keep what was measured, claim no change.
            if self.replaying {
                self.end_replay();
                self.after_replay = true;
            }
            self.held_secs = 0.0;
            self.level = Level::default();
            self.status = Status::Connecting;
            actions.extend([Action::StopPlayback, Action::StopSource]);
        } else if !was_wanted && self.wants_source() {
            self.status = Status::Connecting;
            actions.push(Action::StartSource);
        }
        if restrength {
            actions.push(Action::Restrength);
        }
        actions
    }

    /// Apply a backend event. The serial only moves when the page would
    /// see a difference, so the backend may repeat events every tick.
    pub fn on_event(&mut self, e: Event) {
        let before = self.view(Vec::new());
        self.apply(e);
        let mut after = self.view(Vec::new());
        after.serial = before.serial;
        if after != before {
            self.changed();
        }
    }

    fn apply(&mut self, e: Event) {
        match e {
            Event::Buffered(secs) => {
                self.held_secs = secs;
                if secs >= MIN_REPLAY_SECS {
                    self.settings_changed = false;
                }
            }
            Event::Level(l) => self.level = l,
            Event::Connecting => self.status = Status::Connecting,
            Event::Ready(src) => {
                self.status = Status::Ready;
                self.source = src;
            }
            Event::Blocked(b) => {
                self.status = Status::Blocked(b);
                self.end_replay();
            }
            Event::Frozen { result, playback } => {
                self.result = result;
                self.playback = playback;
            }
            Event::Restrength(result) => {
                // a take without a result (muted, too short) stays so
                if self.result.is_some() {
                    self.result = result;
                }
            }
            Event::NothingToReplay => {
                self.end_replay();
            }
            Event::Position(p) => {
                if self.replaying {
                    self.position = Some(p.clamp(0.0, 1.0));
                }
            }
            Event::PlaybackEnded => {
                if self.replaying {
                    self.end_replay();
                    self.after_replay = true;
                }
            }
            Event::PlaybackFailed => {
                if self.replaying {
                    self.end_replay();
                    self.after_replay = self.result.is_some();
                }
                self.warning = Some(Warning::PlaybackFailed);
            }
            Event::CaptureStopped => {
                self.status = Status::Connecting;
                self.held_secs = 0.0;
                self.warning = Some(Warning::CaptureStopped);
            }
        }
    }

    pub fn replaying(&self) -> bool {
        self.replaying
    }

    pub fn serial(&self) -> u64 {
        self.serial
    }

    pub fn result(&self) -> Option<TakeResult> {
        self.result
    }

    /// The side playing (or the next replay starts on).
    pub fn listen(&self) -> Listen {
        self.side()
    }

    /// The page's snapshot; `columns` and `live` come from the backend
    /// (the rolling buffer, or the frozen take while replaying).
    pub fn view(&self, columns: Vec<Column>) -> View {
        View {
            phase: self.phase(),
            notice: self.notice(),
            listen: self.side(),
            holding: self.holding,
            timeline: Timeline {
                columns,
                live: !self.replaying,
                position: if self.replaying { self.position } else { None },
            },
            can_replay: self.can_replay(),
            collected: (self.held_secs / MIN_REPLAY_SECS).clamp(0.0, 1.0),
            level: self.level,
            result: self.result,
            source: self.source,
            playback: self.playback,
            mic_name: self.info.mic_name.clone(),
            warning: self.warning,
            serial: self.serial,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn info(engine: Engine, meter: &str) -> EngineInfo {
        EngineInfo {
            engine,
            meter: Some(PathBuf::from(meter)),
            mic_node: Some("alsa_input.usb".into()),
            mic_name: "RODE NT-USB".into(),
            pipewire: true,
            model: "dpdfnet8_48khz_hr".into(),
            strength_db: 100.0,
        }
    }

    fn listening() -> CheckState {
        let mut s = CheckState::new(&Setup {
            info: info(Engine::Filtering, "/run/m-1"),
            columns: true,
            once: false,
        });
        assert_eq!(
            s.on_command(&Command::Visible(true)),
            vec![Action::StartSource]
        );
        assert_eq!(s.phase(), Phase::Connecting);
        s.on_event(Event::Ready(Source::Tap));
        s.on_event(Event::Buffered(1.0));
        assert_eq!(s.phase(), Phase::Listening);
        assert!(!s.can_replay(), "not before two seconds");
        s.on_event(Event::Buffered(10.0));
        assert!(s.can_replay());
        s
    }

    fn result() -> TakeResult {
        TakeResult {
            between_words: Quieter::Much,
            between_words_db: Some(26.0),
            voice: Voice::Kept,
            voice_db: Some(0.0),
        }
    }

    #[test]
    fn replay_hold_and_result() {
        let mut s = listening();
        assert_eq!(
            s.on_command(&Command::Replay),
            vec![Action::Replay {
                side: Listen::HushMic
            }]
        );
        s.on_event(Event::Frozen {
            result: Some(result()),
            playback: PlaybackKind::Seamless,
        });
        let v = s.view(vec![]);
        assert_eq!(
            (v.phase, v.holding, v.listen),
            (Phase::Replaying, false, Listen::HushMic)
        );
        assert!(!v.timeline.live && !v.can_replay);
        assert_eq!(v.timeline.position, Some(0.0));
        // Hold: the original at the same position; release: back.
        assert_eq!(
            s.on_command(&Command::Hold(true)),
            vec![Action::Switch(Listen::Original)]
        );
        assert!(s.on_command(&Command::Hold(true)).is_empty(), "key repeat");
        let v = s.view(vec![]);
        assert_eq!(
            (v.phase, v.holding, v.listen),
            (Phase::Replaying, true, Listen::Original)
        );
        assert_eq!(
            s.on_command(&Command::Hold(false)),
            vec![Action::Switch(Listen::HushMic)]
        );
        s.on_event(Event::Position(0.4));
        assert_eq!(s.on_command(&Command::Seek(0.7)), vec![Action::Seek(0.7)]);
        assert_eq!(s.view(vec![]).timeline.position, Some(0.7));
        s.on_event(Event::PlaybackEnded);
        let v = s.view(vec![]);
        assert_eq!(v.phase, Phase::Result);
        assert!(v.timeline.live, "after a replay the timeline is live again");
        assert_eq!(v.result, Some(result()));
        assert!(v.can_replay);
    }

    /// A hold with nothing playing plays the original while held, and its
    /// release ends that replay (UX panel round 2, A3: no loop in the
    /// headset after letting go).
    #[test]
    fn hold_outside_a_replay_plays_the_original_while_held() {
        let mut s = listening();
        assert_eq!(
            s.on_command(&Command::Hold(true)),
            vec![Action::Replay {
                side: Listen::Original
            }]
        );
        assert!(s.view(vec![]).holding);
        assert_eq!(s.phase(), Phase::Replaying);
        assert_eq!(
            s.on_command(&Command::Hold(false)),
            vec![Action::StopPlayback]
        );
        assert_eq!(s.phase(), Phase::Result);
        // a replay started with Replay keeps going after a hold over it
        s.on_command(&Command::Replay);
        s.on_command(&Command::Hold(true));
        assert_eq!(
            s.on_command(&Command::Hold(false)),
            vec![Action::Switch(Listen::HushMic)]
        );
        assert_eq!(s.phase(), Phase::Replaying);
        s.on_command(&Command::Stop);
        // a tap latches the original: the replay it started goes on
        s.on_command(&Command::Hold(true));
        assert_eq!(s.on_command(&Command::Listen(Listen::Original)), vec![]);
        assert!(s.on_command(&Command::Hold(false)).is_empty());
        assert_eq!(s.phase(), Phase::Replaying);
        assert_eq!(s.view(vec![]).listen, Listen::Original);
    }

    fn replay_side(actions: &[Action]) -> Option<Listen> {
        actions.iter().find_map(|a| match a {
            Action::Replay { side } => Some(*side),
            _ => None,
        })
    }

    #[test]
    fn a_hold_that_outlasts_the_replay_leaves_the_next_on_hushmic() {
        // Held during a replay, and a hold that started one: either way the
        // replay ends while the button is still down, and its release
        // comes after.
        for start_by_holding in [false, true] {
            let mut s = listening();
            if start_by_holding {
                let a = s.on_command(&Command::Hold(true));
                assert_eq!(replay_side(&a), Some(Listen::Original));
            } else {
                let a = s.on_command(&Command::Replay);
                assert_eq!(replay_side(&a), Some(Listen::HushMic));
                assert_eq!(
                    s.on_command(&Command::Hold(true)),
                    vec![Action::Switch(Listen::Original)]
                );
            }
            s.on_event(Event::PlaybackEnded);
            assert!(s.on_command(&Command::Hold(false)).is_empty());
            let v = s.view(vec![]);
            assert_eq!((v.holding, v.listen), (false, Listen::HushMic));
            let a = s.on_command(&Command::Replay);
            assert_eq!(
                replay_side(&a),
                Some(Listen::HushMic),
                "started by holding: {start_by_holding}"
            );
        }
    }

    #[test]
    fn a_latched_original_stays_for_the_next_replay_and_a_hold_returns_to_it() {
        let mut s = listening();
        // A tap: the press plays the original at once, the quick release
        // latches it (the page sends the latch before the release).
        let a = s.on_command(&Command::Hold(true));
        assert_eq!(replay_side(&a), Some(Listen::Original));
        assert!(s.on_command(&Command::Listen(Listen::Original)).is_empty());
        assert!(s.on_command(&Command::Hold(false)).is_empty(), "stays");
        assert_eq!(s.view(vec![]).listen, Listen::Original);
        // A hold over the latch changes nothing, and its release returns to
        // the latch, not to HushMic.
        assert!(s.on_command(&Command::Hold(true)).is_empty());
        assert!(s.on_command(&Command::Hold(false)).is_empty());
        assert_eq!(s.view(vec![]).listen, Listen::Original);
        // The latch outlives the replay.
        s.on_event(Event::PlaybackEnded);
        let a = s.on_command(&Command::Replay);
        assert_eq!(replay_side(&a), Some(Listen::Original));
        // The next tap goes back to HushMic while it plays.
        assert!(s.on_command(&Command::Hold(true)).is_empty());
        assert_eq!(
            s.on_command(&Command::Listen(Listen::HushMic)),
            vec![Action::Switch(Listen::HushMic)]
        );
        assert!(s.on_command(&Command::Hold(false)).is_empty());
        assert_eq!(s.view(vec![]).listen, Listen::HushMic);
        // Unlatched, a hold peeks and its release comes back.
        assert_eq!(
            s.on_command(&Command::Hold(true)),
            vec![Action::Switch(Listen::Original)]
        );
        assert_eq!(
            s.on_command(&Command::Hold(false)),
            vec![Action::Switch(Listen::HushMic)]
        );
    }

    #[test]
    fn the_listen_control_switches_or_sets_the_next_side() {
        let mut s = listening();
        assert!(s.on_command(&Command::Listen(Listen::Original)).is_empty());
        assert_eq!(
            s.on_command(&Command::Replay),
            vec![Action::Replay {
                side: Listen::Original
            }]
        );
        assert_eq!(
            s.on_command(&Command::Listen(Listen::HushMic)),
            vec![Action::Switch(Listen::HushMic)]
        );
        assert!(s.on_command(&Command::Seek(f32::NAN)).is_empty());
    }

    #[test]
    fn a_new_chain_drops_the_take_and_says_so() {
        let mut s = listening();
        s.on_command(&Command::Replay);
        let actions = s.on_command(&Command::Engine(info(Engine::Filtering, "/run/m-2")));
        assert_eq!(
            actions,
            vec![
                Action::StopPlayback,
                Action::DropTake,
                Action::StopSource,
                Action::StartSource
            ]
        );
        let v = s.view(vec![]);
        assert_eq!(v.phase, Phase::Connecting);
        assert_eq!(v.notice, Some(Notice::SettingsChanged));
        assert_eq!(v.result, None);
        s.on_event(Event::Ready(Source::Tap));
        s.on_event(Event::Buffered(2.5));
        assert_eq!(s.notice(), None, "cleared once new audio is in");
        // A mode change keeps the take; the notice says what is going on.
        assert!(s
            .on_command(&Command::Engine(info(Engine::Bypass, "/run/m-2")))
            .is_empty());
        assert_eq!(s.notice(), Some(Notice::Bypass));
        assert!(s
            .on_command(&Command::Engine(info(Engine::Muted, "/run/m-2")))
            .is_empty());
        assert_eq!(s.notice(), Some(Notice::Muted));
    }

    #[test]
    fn a_pipewire_blip_keeps_the_result() {
        let mut s = listening();
        s.on_command(&Command::Replay);
        s.on_event(Event::Frozen {
            result: Some(result()),
            playback: PlaybackKind::Seamless,
        });
        s.on_event(Event::PlaybackEnded);
        assert_eq!(s.phase(), Phase::Result);
        let mut nopw = info(Engine::Filtering, "/run/m-1");
        nopw.pipewire = false;
        assert_eq!(
            s.on_command(&Command::Engine(nopw)),
            vec![Action::StopPlayback, Action::StopSource]
        );
        assert_eq!(s.phase(), Phase::Blocked(Blocker::NoPipewire));
        assert_eq!(
            s.on_command(&Command::Engine(info(Engine::Filtering, "/run/m-1"))),
            vec![Action::StartSource]
        );
        s.on_event(Event::Ready(Source::Tap));
        let v = s.view(vec![]);
        assert_eq!(v.phase, Phase::Result);
        assert_eq!(v.result, Some(result()));
        assert_eq!(v.notice, None, "nothing the user changed");
        // A blip during a replay ends it, with the result kept.
        s.on_event(Event::Buffered(10.0));
        s.on_command(&Command::Replay);
        let mut nopw = info(Engine::Filtering, "/run/m-1");
        nopw.pipewire = false;
        s.on_command(&Command::Engine(nopw));
        assert!(!s.replaying());
        assert_eq!(s.result(), Some(result()));
    }

    #[test]
    fn a_model_change_drops_the_take() {
        let mut s = listening();
        s.on_command(&Command::Replay);
        s.on_event(Event::Frozen {
            result: Some(result()),
            playback: PlaybackKind::Seamless,
        });
        let mut next = info(Engine::Filtering, "/run/m-1");
        next.model = "dpdfnet2_48khz_hr".into();
        let a = s.on_command(&Command::Engine(next));
        assert!(a.contains(&Action::DropTake), "{a:?}");
        assert_eq!(s.notice(), Some(Notice::SettingsChanged));
        assert_eq!(s.result(), None);
    }

    /// The strength is written to the running chain: the replay plays on,
    /// listening goes on, nothing is dropped.
    #[test]
    fn a_strength_change_keeps_the_replay_and_the_take() {
        let mut s = listening();
        s.on_command(&Command::Replay);
        s.on_event(Event::Frozen {
            result: Some(result()),
            playback: PlaybackKind::Seamless,
        });
        let mut next = info(Engine::Filtering, "/run/m-1");
        next.strength_db = 24.0;
        let a = s.on_command(&Command::Engine(next));
        assert_eq!(a, vec![Action::Restrength]);
        assert!(s.replaying());
        assert_eq!(s.phase(), Phase::Replaying);
        assert_eq!(s.notice(), None);
        assert_eq!(s.result(), Some(result()));
        // listening, too: no source restart
        let mut s = listening();
        let mut next = info(Engine::Filtering, "/run/m-1");
        next.strength_db = 6.0;
        assert_eq!(
            s.on_command(&Command::Engine(next)),
            vec![Action::Restrength]
        );
        assert_eq!(s.phase(), Phase::Listening);
    }

    /// The re-rendered take's result replaces the one shown; a take that
    /// had none (muted, too short) gets none.
    #[test]
    fn a_rerendered_take_brings_its_result() {
        let mut s = listening();
        s.on_command(&Command::Replay);
        s.on_event(Event::Frozen {
            result: Some(result()),
            playback: PlaybackKind::Seamless,
        });
        let serial = s.serial();
        let mut other = result();
        other.between_words_db = Some(32.0);
        s.on_event(Event::Restrength(Some(other)));
        assert_eq!(s.result(), Some(other));
        assert_ne!(s.serial(), serial, "the page sees it");
        assert!(s.replaying());
        let mut s = listening();
        s.on_command(&Command::Replay);
        s.on_event(Event::Frozen {
            result: None,
            playback: PlaybackKind::Seamless,
        });
        s.on_event(Event::Restrength(Some(result())));
        assert_eq!(s.result(), None);
    }

    #[test]
    fn off_blocks_and_on_starts_again() {
        let mut s = listening();
        let mut off = info(Engine::Off, "");
        off.meter = None;
        let a = s.on_command(&Command::Engine(off));
        assert!(a.contains(&Action::StopSource) && !a.contains(&Action::StartSource));
        assert_eq!(s.phase(), Phase::Blocked(Blocker::Off));
        assert!(!s.can_replay());
        let a = s.on_command(&Command::Engine(info(Engine::Filtering, "/run/m-3")));
        assert_eq!(a.last(), Some(&Action::StartSource));
        assert_eq!(s.phase(), Phase::Connecting);
        let mut nopw = info(Engine::Filtering, "/run/m-3");
        nopw.pipewire = false;
        s.on_command(&Command::Engine(nopw));
        assert_eq!(s.phase(), Phase::Blocked(Blocker::NoPipewire));
    }

    #[test]
    fn hidden_page_lets_go_and_a_silent_mic_is_a_problem() {
        let mut s = listening();
        s.on_event(Event::Level(Level {
            verdict: LevelVerdict::NoSound,
            speech: None,
        }));
        assert_eq!(s.phase(), Phase::MicProblem);
        assert!(!s.can_replay());
        assert_eq!(
            s.on_command(&Command::Visible(false)),
            vec![Action::StopPlayback, Action::StopSource]
        );
        assert!(s.on_command(&Command::Visible(false)).is_empty());
        assert_eq!(
            s.on_command(&Command::Visible(true)),
            vec![Action::StartSource]
        );
        assert_eq!(s.phase(), Phase::Connecting);
    }

    #[test]
    fn blocked_sources_and_failed_playback() {
        let mut s = listening();
        s.on_event(Event::Blocked(Blocker::NeedsNewerPipewire));
        assert_eq!(s.phase(), Phase::Blocked(Blocker::NeedsNewerPipewire));
        let mut s = listening();
        s.on_command(&Command::Replay);
        s.on_event(Event::PlaybackFailed);
        let v = s.view(vec![]);
        assert_eq!(v.phase, Phase::Listening);
        assert_eq!(v.warning, Some(Warning::PlaybackFailed));
        s.on_command(&Command::Listen(Listen::Original));
        assert_eq!(s.view(vec![]).warning, None, "shown once");
    }
}
