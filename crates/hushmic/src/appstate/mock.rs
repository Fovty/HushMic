//! Board states as `AppState` values, for building and screenshotting the
//! window without a daemon (`hushmic --window --mock <scenario>`), and for
//! UI tests. The names and devices are the boards' (App-Main, App-Settings,
//! App-Shortcuts, App-FirstRun), so a screenshot lines up with the board's
//! same state.
//!
//! [`apply`] plays the daemon's part for a mock window: an action changes
//! the state the way the real daemon would answer it, so the switch, Mute,
//! the pickers and the settings can be clicked through. Turning on from Off
//! passes through Starting; [`settle`] ends it (the mock feed calls it
//! after [`START_SECS`]).

use super::*;

/// How long a mock start takes (appux: Off → Starting for 1.5 s → on).
pub const START_SECS: f32 = 1.5;

/// Every state the boards show, by board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    // App-Main
    OnInCall,
    OnNoApp,
    /// On, in a call, with a silent microphone. Silence is the engine
    /// meter's to report; the daemon state is the same as OnInCall.
    MicSilent,
    Bypass,
    Muted,
    LightForNow,
    UnfilteredForNow,
    Starting,
    MicUnplugged,
    DirectMic,
    DirectMicFollowsDefault,
    DirectMicNoName,
    PipewireUnreachable,
    Off,
    /// Not on the board: the chain is down and retried ("Needs attention").
    NeedsAttention,
    /// Not on the board: the watchdog is bringing the node back.
    Reconnecting,
    // App-Shortcuts ("Keys set" is every scenario above)
    ShortcutsNotSetUp,
    KeysNotReported,
    PlasmaBefore64,
    ShortcutsUnavailable,
    // App-Settings ("Tray found" is every scenario above)
    NoTray,
    // App-FirstRun: a fresh install at each step, waiting for an app
    FirstRun1,
    FirstRun2,
    FirstRun3,
    /// Step 3 once Firefox uses HushMic.
    FirstRun3Connected,
    /// First run on PipeWire too old for the live level.
    FirstRunOldPipewire,
}

impl Scenario {
    pub const ALL: [Scenario; 26] = [
        Scenario::OnInCall,
        Scenario::OnNoApp,
        Scenario::MicSilent,
        Scenario::Bypass,
        Scenario::Muted,
        Scenario::LightForNow,
        Scenario::UnfilteredForNow,
        Scenario::Starting,
        Scenario::MicUnplugged,
        Scenario::DirectMic,
        Scenario::DirectMicFollowsDefault,
        Scenario::DirectMicNoName,
        Scenario::PipewireUnreachable,
        Scenario::Off,
        Scenario::NeedsAttention,
        Scenario::Reconnecting,
        Scenario::ShortcutsNotSetUp,
        Scenario::KeysNotReported,
        Scenario::PlasmaBefore64,
        Scenario::ShortcutsUnavailable,
        Scenario::NoTray,
        Scenario::FirstRun1,
        Scenario::FirstRun2,
        Scenario::FirstRun3,
        Scenario::FirstRun3Connected,
        Scenario::FirstRunOldPipewire,
    ];

    /// The `--mock` word.
    pub fn word(self) -> &'static str {
        match self {
            Scenario::OnInCall => "on-in-call",
            Scenario::OnNoApp => "on-no-app",
            Scenario::MicSilent => "mic-silent",
            Scenario::Bypass => "bypass",
            Scenario::Muted => "muted",
            Scenario::LightForNow => "light-for-now",
            Scenario::UnfilteredForNow => "unfiltered-for-now",
            Scenario::Starting => "starting",
            Scenario::MicUnplugged => "mic-unplugged",
            Scenario::DirectMic => "direct-mic",
            Scenario::DirectMicFollowsDefault => "direct-mic-follows-default",
            Scenario::DirectMicNoName => "direct-mic-no-name",
            Scenario::PipewireUnreachable => "pipewire-unreachable",
            Scenario::Off => "off",
            Scenario::NeedsAttention => "needs-attention",
            Scenario::Reconnecting => "reconnecting",
            Scenario::ShortcutsNotSetUp => "shortcuts-not-set-up",
            Scenario::KeysNotReported => "keys-not-reported",
            Scenario::PlasmaBefore64 => "plasma-before-6-4",
            Scenario::ShortcutsUnavailable => "shortcuts-unavailable",
            Scenario::NoTray => "no-tray",
            Scenario::FirstRun1 => "first-run-1",
            Scenario::FirstRun2 => "first-run-2",
            Scenario::FirstRun3 => "first-run-3",
            Scenario::FirstRun3Connected => "first-run-3-connected",
            Scenario::FirstRunOldPipewire => "first-run-old-pipewire",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.word() == word)
    }
}

const BUILTIN: &str = "alsa_input.pci-0000_00_1f.3.analog-stereo";
const RODE: &str = "alsa_input.usb-RODE_Microphones_RODE_NT-USB-00.analog-stereo";
const HEADSET: &str = "alsa_input.usb-Generic_USB_Headset-00.mono-fallback";

fn device(name: &str, description: &str, saved_settings: bool) -> MicDevice {
    MicDevice {
        name: name.into(),
        description: description.into(),
        present: true,
        saved_settings,
    }
}

fn app(id: &str, name: Option<&str>) -> AppRef {
    AppRef {
        id: id.into(),
        name: name.map(str::to_string),
    }
}

fn keys(texts: [Option<&str>; 4]) -> Vec<ShortcutKeys> {
    ShortcutAction::ALL
        .iter()
        .zip(texts)
        .map(|(a, t)| ShortcutKeys {
            action: *a,
            keys: match t {
                Some(t) => Keys::Text(t.into()),
                None => Keys::NotSet,
            },
        })
        .collect()
}

/// The board's everyday setup: on, RODE NT-USB chosen (with its own
/// Maximum / Quality settings; the USB Headset has Medium / Light), Firefox
/// in a call, shortcuts bound, tray up.
fn base() -> AppState {
    let rode = Sound::new(QUALITY_MODEL, 100.0);
    AppState {
        daemon: DaemonInfo {
            version: env!("CARGO_PKG_VERSION").into(),
            headless: false,
            flatpak: false,
        },
        mode: Mode::Suppress,
        unmute_to: Mode::Suppress,
        engine: Engine {
            phase: Phase::Ready,
            start_reason: None,
            tier: Some(EngineTier::Quality),
            configured_light: false,
            generation: 7,
            error: None,
            flapping: false,
            recovered: 0,
            meter: None,
            use_began_ms: None,
        },
        mic: MicState {
            selected: Some(RODE.into()),
            fallback: false,
            active: Some(RODE.into()),
            system_default: Some(BUILTIN.into()),
            // The boards' order: RODE NT-USB, Built-in Audio, USB Headset.
            devices: vec![
                device(RODE, "RODE NT-USB", true),
                device(BUILTIN, "Built-in Audio", false),
                device(HEADSET, "USB Headset", true),
            ],
            rerouted: false,
            stuck: false,
            silent: false,
        },
        sound: SoundState {
            current: rode.clone(),
            owner: Some(RODE.into()),
            saved: true,
            defaults: Sound::default(),
            profiles: vec![
                MicProfile {
                    device: RODE.into(),
                    description: "RODE NT-USB".into(),
                    sound: rode,
                },
                MicProfile {
                    device: HEADSET.into(),
                    description: "USB Headset".into(),
                    sound: Sound::new(LIGHT_MODEL, 12.0),
                },
            ],
        },
        apps: AppsState {
            using: vec![app("firefox", Some("Firefox"))],
            direct: vec![],
            set_default: false,
            can_set_default: true,
            default_released: None,
        },
        shortcuts: ShortcutsState {
            available: true,
            backend: ShortcutsBackend::Portal,
            set_up: true,
            keys: keys([
                Some("Ctrl+Alt+M"),
                Some("Ctrl+Alt+B"),
                Some("Ctrl+Alt+Space"),
                None,
            ]),
        },
        prefs: Prefs {
            autostart: true,
            notifications: true,
            appearance: Appearance::System,
            tray: true,
            tray_icon: TrayIcon::Auto,
            inference: Inference::Auto,
            voice_hold: false,
            hidden_notices: vec![HiddenNotice {
                app: "com.obsproject.Studio".into(),
                name: Some("OBS Studio".into()),
            }],
        },
        tray: TrayHost::Registered,
        pipewire: PipeWireState {
            reachable: true,
            live_capture: Some(true),
        },
        ui: UiState {
            page: Page::Live,
            first_run_step: None,
            beam_used: true,
            size: None,
            file: None,
        },
        // The boards are dark; the desktop says nothing.
        desktop: DesktopLook::default(),
        files: Default::default(),
        // The speakers cleaner starts off (the switch is saved off); its
        // outputs are there to pick.
        #[cfg(feature = "speakers")]
        speakers: SpeakersState {
            outputs: vec![
                output(SPEAKERS_BUILTIN, "Built-in Audio Analog Stereo"),
                output(SPEAKERS_HEADSET, "USB Headset"),
                output(SPEAKERS_HDMI, "HDMI / DisplayPort 1"),
            ],
            system_default: Some(SPEAKERS_BUILTIN.into()),
            ..SpeakersState::default()
        },
        ask: None,
    }
}

#[cfg(feature = "speakers")]
const SPEAKERS_BUILTIN: &str = "alsa_output.pci-0000_00_1f.3.analog-stereo";
#[cfg(feature = "speakers")]
const SPEAKERS_HEADSET: &str = "alsa_output.usb-Generic_USB_Headset-00.analog-stereo";
#[cfg(feature = "speakers")]
const SPEAKERS_HDMI: &str = "alsa_output.pci-0000_01_00.1.hdmi-stereo";

#[cfg(feature = "speakers")]
fn output(name: &str, description: &str) -> OutputDevice {
    OutputDevice {
        name: name.into(),
        description: description.into(),
        present: true,
    }
}

/// The Speakers page's states (no board: built from the page patterns).
#[cfg(feature = "speakers")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeakersScene {
    /// The switch off (the default).
    Off,
    /// Starting after the switch went on.
    Starting,
    /// On, no app plays through it.
    Idle,
    /// On, two apps play through it, on the USB Headset.
    Playing,
    /// "Hold to hear the original" held while apps play.
    Held,
    /// The chosen output (the USB Headset) is unplugged: the sound plays
    /// on the system default meanwhile.
    Unplugged,
    /// The chain should run and does not (it keeps failing to start).
    Down,
    /// The light model for now (a busy computer).
    Light,
    /// HushMic Speakers is the system output.
    IsDefault,
    /// A restart left Discord on another output; Firefox plays here.
    Left,
}

#[cfg(feature = "speakers")]
impl SpeakersScene {
    pub const ALL: [SpeakersScene; 10] = [
        SpeakersScene::Off,
        SpeakersScene::Starting,
        SpeakersScene::Idle,
        SpeakersScene::Playing,
        SpeakersScene::Held,
        SpeakersScene::Unplugged,
        SpeakersScene::Down,
        SpeakersScene::Light,
        SpeakersScene::IsDefault,
        SpeakersScene::Left,
    ];

    pub fn word(self) -> &'static str {
        match self {
            SpeakersScene::Off => "off",
            SpeakersScene::Starting => "starting",
            SpeakersScene::Idle => "idle",
            SpeakersScene::Playing => "playing",
            SpeakersScene::Held => "held",
            SpeakersScene::Unplugged => "unplugged",
            SpeakersScene::Down => "down",
            SpeakersScene::Light => "light",
            SpeakersScene::IsDefault => "is-default",
            SpeakersScene::Left => "left",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.word() == word)
    }
}

/// `s` with the speakers cleaner in `sc` (HushMic's own state as it is:
/// a scenario with HushMic off stops the cleaner, as the daemon does).
#[cfg(feature = "speakers")]
pub fn with_speakers(mut s: AppState, sc: SpeakersScene) -> AppState {
    let sp = &mut s.speakers;
    if sc != SpeakersScene::Off {
        sp.enabled = true;
        sp.phase = Phase::Ready;
        sp.tier = Some(EngineTier::Quality);
        sp.playing_on = sp.system_default.clone();
        sp.generation = 3;
    }
    let playing = || {
        vec![
            app("com.discordapp.Discord", Some("Discord")),
            app("firefox", Some("Firefox")),
        ]
    };
    match sc {
        SpeakersScene::Off | SpeakersScene::Idle => {}
        SpeakersScene::Starting => {
            sp.phase = Phase::Starting;
            sp.tier = None;
            sp.playing_on = None;
        }
        SpeakersScene::Playing | SpeakersScene::Light => {
            sp.output = Some(SPEAKERS_HEADSET.into());
            sp.playing_on = sp.output.clone();
            sp.strength = Strength::Strong;
            sp.attn_limit = 24.0;
            sp.playing = playing();
            if sc == SpeakersScene::Light {
                sp.tier = Some(EngineTier::Light);
            }
        }
        SpeakersScene::Held => {
            sp.playing = playing();
            sp.held = true;
        }
        SpeakersScene::Unplugged => {
            sp.output = Some(SPEAKERS_HEADSET.into());
            for d in sp.outputs.iter_mut().filter(|d| d.name == SPEAKERS_HEADSET) {
                d.present = false;
            }
            sp.playing = playing();
        }
        SpeakersScene::Down => {
            sp.phase = Phase::Down;
            sp.tier = None;
            sp.playing_on = None;
            sp.failing = true;
            sp.error = Some("hushmic_sink never appeared".into());
        }
        SpeakersScene::IsDefault => {
            sp.is_default = true;
            sp.playing = playing();
        }
        SpeakersScene::Left => {
            sp.playing = vec![app("firefox", Some("Firefox"))];
            sp.left = vec![app("com.discordapp.Discord", Some("Discord"))];
        }
    }
    if s.mode == Mode::Off || !s.pipewire.reachable {
        speakers_stop(&mut s.speakers);
    }
    s
}

/// The cleaner stops (HushMic off, or the switch off).
#[cfg(feature = "speakers")]
fn speakers_stop(sp: &mut SpeakersState) {
    if sp.phase != Phase::Off {
        sp.generation += 1;
    }
    sp.phase = Phase::Off;
    sp.tier = None;
    sp.held = false;
    sp.playing.clear();
    sp.left.clear();
    sp.playing_on = None;
    sp.failing = false;
    sp.error = None;
}

/// The cleaner starts if HushMic is on and the switch is: true when it did
/// (the caller settles it).
#[cfg(feature = "speakers")]
fn speakers_start(s: &mut AppState) -> bool {
    if s.mode == Mode::Off || !s.speakers.enabled || s.speakers.phase != Phase::Off {
        return false;
    }
    s.speakers.phase = Phase::Starting;
    s.speakers.generation += 1;
    true
}

/// A fresh install: nothing chosen, nothing set up, first run at `step`.
fn first_run(step: u8) -> AppState {
    let mut s = base();
    s.mic.selected = None;
    s.mic.active = Some(BUILTIN.into());
    s.mic
        .devices
        .iter_mut()
        .for_each(|d| d.saved_settings = false);
    s.sound = SoundState {
        current: Sound::default(),
        owner: Some(BUILTIN.into()),
        saved: false,
        defaults: Sound::default(),
        profiles: vec![],
    };
    s.apps.using.clear();
    s.shortcuts.set_up = false;
    s.shortcuts.keys = ShortcutsState::unreported();
    s.prefs.autostart = false;
    s.prefs.hidden_notices.clear();
    s.ui = UiState {
        page: Page::FirstRun,
        first_run_step: Some(step),
        beam_used: false,
        size: None,
        file: None,
    };
    s
}

pub fn state(sc: Scenario) -> AppState {
    let mut s = base();
    match sc {
        Scenario::OnInCall | Scenario::MicSilent => {}
        Scenario::OnNoApp => s.apps.using.clear(),
        Scenario::Bypass => {
            s.mode = Mode::Bypass;
            s.unmute_to = Mode::Bypass;
        }
        Scenario::Muted => s.mode = Mode::Mute,
        Scenario::LightForNow => s.engine.tier = Some(EngineTier::Light),
        Scenario::UnfilteredForNow => s.engine.tier = Some(EngineTier::Passthrough),
        Scenario::Starting => {
            s.engine.phase = Phase::Starting;
            s.engine.start_reason = Some(StartReason::Launch);
            s.engine.tier = None;
            s.apps.using.clear();
        }
        Scenario::MicUnplugged => {
            s.mic.fallback = true;
            s.mic.active = Some(BUILTIN.into());
            // Listed where it was (the daemon remembers the place).
            for d in s.mic.devices.iter_mut().filter(|d| d.name == RODE) {
                d.present = false;
            }
            s.sound.current = Sound::default();
            s.sound.owner = Some(BUILTIN.into());
            s.sound.saved = false;
        }
        Scenario::DirectMic => {
            // Zoom records the microphone itself: nobody uses HushMic.
            s.apps.using.clear();
            s.apps.direct = vec![DirectUse {
                app: app("us.zoom.Zoom", Some("Zoom")),
                follows_default: false,
            }];
        }
        Scenario::DirectMicFollowsDefault => {
            s.apps.using.clear();
            s.apps.direct = vec![DirectUse {
                app: app("us.zoom.Zoom", Some("Zoom")),
                follows_default: true,
            }];
        }
        Scenario::DirectMicNoName => {
            s.apps.using.clear();
            s.apps.direct = vec![DirectUse {
                app: app("chromium", None),
                follows_default: false,
            }];
        }
        Scenario::PipewireUnreachable => {
            s.pipewire.reachable = false;
            s.pipewire.live_capture = None;
            s.engine = Engine {
                phase: Phase::Down,
                error: Some("hushmic_source never appeared (is PipeWire running?)".into()),
                generation: s.engine.generation,
                ..Engine::default()
            };
            s.apps.using.clear();
            s.mic.devices.clear();
            s.mic.active = None;
            s.mic.system_default = None;
        }
        Scenario::Off => {
            s.mode = Mode::Off;
            s.engine = Engine {
                generation: s.engine.generation,
                ..Engine::default()
            };
            s.apps.using.clear();
            s.apps.direct.clear();
            s.mic.active = None;
        }
        Scenario::NeedsAttention => {
            s.engine.phase = Phase::Down;
            s.engine.tier = None;
            s.engine.error = Some("the filter chain exited during startup".into());
            s.apps.using.clear();
        }
        Scenario::Reconnecting => {
            s.engine.phase = Phase::Reconnecting;
            s.engine.tier = None;
            s.apps.using.clear();
        }
        Scenario::ShortcutsNotSetUp => {
            s.shortcuts.set_up = false;
            s.shortcuts.keys = ShortcutsState::unreported();
        }
        Scenario::KeysNotReported => s.shortcuts.keys = ShortcutsState::unreported(),
        Scenario::PlasmaBefore64 => {
            s.shortcuts.backend = ShortcutsBackend::KdeService;
            s.shortcuts.keys = keys([Some("Meta+M"), None, Some("Meta+Space"), None]);
        }
        Scenario::ShortcutsUnavailable => {
            s.shortcuts = ShortcutsState {
                available: false,
                backend: ShortcutsBackend::Unsupported,
                set_up: false,
                keys: ShortcutsState::unreported(),
            };
        }
        Scenario::NoTray => s.tray = TrayHost::Waiting,
        Scenario::FirstRun1 => s = first_run(1),
        Scenario::FirstRun2 => s = first_run(2),
        Scenario::FirstRun3 => s = first_run(3),
        Scenario::FirstRun3Connected => {
            s = first_run(3);
            s.apps.using = vec![app("firefox", Some("Firefox"))];
        }
        Scenario::FirstRunOldPipewire => {
            s = first_run(1);
            s.pipewire.live_capture = Some(false);
        }
    }
    s
}

/// The daemon's answer to `action`, applied to a mock state. True when the
/// action started the chain (the caller settles it after [`START_SECS`]).
pub fn apply(s: &mut AppState, action: &Action) -> bool {
    let set_mode = |s: &mut AppState, m: Mode| -> bool {
        let was_off = s.mode == Mode::Off;
        if matches!(m, Mode::Suppress | Mode::Bypass) {
            s.unmute_to = m;
        }
        s.mode = m;
        if m == Mode::Off {
            #[cfg(feature = "speakers")]
            speakers_stop(&mut s.speakers);
            s.engine.phase = Phase::Off;
            s.engine.start_reason = None;
            s.engine.tier = None;
            s.engine.generation += 1;
            s.apps.using.clear();
            s.apps.direct.clear();
            false
        } else if was_off {
            s.engine.phase = Phase::Starting;
            s.engine.start_reason = Some(StartReason::TurnOn);
            s.engine.generation += 1;
            #[cfg(feature = "speakers")]
            speakers_start(s);
            true
        } else {
            false
        }
    };
    match action {
        Action::SetMode(m) => return set_mode(s, *m),
        Action::ToggleMute => {
            let next = if s.mode == Mode::Mute {
                s.unmute_to
            } else {
                Mode::Mute
            };
            return set_mode(s, next);
        }
        // The daemon refuses it unless muted; a mock ignores it then.
        Action::SetUnmuteTo(m) => {
            if s.mode == Mode::Mute && matches!(m, Mode::Suppress | Mode::Bypass) {
                s.unmute_to = *m;
            }
        }
        Action::ToggleBypass => {
            let next = if s.mode == Mode::Bypass {
                Mode::Suppress
            } else {
                Mode::Bypass
            };
            return set_mode(s, next);
        }
        Action::SelectMic(m) => {
            s.mic.selected = m.clone();
            s.mic.fallback = false;
            s.mic.active = m.clone().or_else(|| s.mic.system_default.clone());
            let owner = s.mic.active.clone();
            let own = s
                .sound
                .profiles
                .iter()
                .find(|p| Some(&p.device) == owner.as_ref())
                .map(|p| p.sound.clone());
            s.sound.saved = own.is_some();
            s.sound.current = own.unwrap_or_else(|| s.sound.defaults.clone());
            s.sound.owner = owner;
            s.engine.generation += 1;
        }
        Action::SetStrength(st) => {
            if let Some(v) = st.attn_limit() {
                let model = s.sound.current.model_id.clone();
                edit_sound(s, Sound::new(&model, v));
            }
        }
        Action::SetModel(m) => {
            let attn = s.sound.current.attn_limit;
            edit_sound(s, Sound::new(m.id(), attn));
            s.engine.configured_light = *m == Model::Light;
            s.engine.generation += 1;
        }
        Action::SetDefaultMic(b) => {
            s.apps.set_default = *b;
            // The default moves to HushMic, and with it every app that
            // follows the default ("moves the call to HushMic").
            if *b && s.mode != Mode::Off {
                let (moved, stay): (Vec<_>, Vec<_>) =
                    s.apps.direct.drain(..).partition(|d| d.follows_default);
                s.apps.direct = stay;
                for d in moved {
                    if !s.apps.using.iter().any(|a| a.id == d.app.id) {
                        s.apps.using.push(d.app);
                    }
                }
            }
        }
        Action::SetAutostart(b) => s.prefs.autostart = *b,
        Action::SetNotifications(b) => s.prefs.notifications = *b,
        Action::SetVoiceHold(b) => s.prefs.voice_hold = *b,
        Action::SetAppearance(a) => {
            s.prefs.appearance = *a;
            s.desktop.effective_theme = Theme::resolve(*a, s.desktop.system_theme);
        }
        Action::SetTray(b) => s.prefs.tray = *b,
        Action::SetTrayIcon(i) => s.prefs.tray_icon = *i,
        Action::SetInference(i) => s.prefs.inference = *i,
        Action::ResetProfile(d) => {
            s.sound.profiles.retain(|p| &p.device != d);
            if let Some(dev) = s.mic.devices.iter_mut().find(|x| &x.name == d) {
                dev.saved_settings = false;
            }
            if s.sound.owner.as_deref() == Some(d.as_str()) {
                s.sound.saved = false;
                s.sound.current = s.sound.defaults.clone();
            }
        }
        Action::SetupShortcuts => s.shortcuts.set_up = true,
        Action::Refresh | Action::Retry | Action::Quit => {}
        Action::Answer(yes) => {
            if s.ask.take() == Some(Ask::TurnOff) && *yes {
                return set_mode(s, Mode::Off);
            }
        }
        Action::UiPage(p) => s.ui.page = *p,
        Action::UiFirstRun(step) => {
            s.ui.first_run_step = *step;
            if step.is_some() {
                s.ui.page = Page::FirstRun;
            }
        }
        Action::UiBeamUsed => s.ui.beam_used = true,
        Action::UiSize(w, h) => s.ui.size = Some([*w, *h]),
        Action::UiFocused(_) | Action::UiShows(_) => {}
        Action::UiFile(f) => s.ui.file = *f,
        // No file jobs on the mock boards yet.
        Action::FilesAdd(_)
        | Action::FilesPlay(..)
        | Action::FilesRetry(_)
        | Action::FilesSave(_)
        | Action::FilesSaveTo(..)
        | Action::FilesCancelSave(_)
        | Action::FilesReplace(..)
        | Action::FilesCleanFirst(_) => {}
        Action::FilesQuit(_) | Action::FilesSaveAndQuit => s.files.ask_quit = false,
        Action::FilesAskQuit => s.files.ask_quit = true,
        Action::FilesCancelQuit => s.files.quitting = None,
        Action::FilesFolder(d) => s.files.folder = Some(d.display().to_string()),
        Action::FilesNextToOriginal => s.files.folder = None,
        Action::FilesStrength(db, id) => {
            s.files.strength = *db;
            if let Some(j) = s.files.jobs.iter_mut().find(|j| Some(j.id) == *id) {
                j.db = *db;
            }
        }
        Action::FilesTrack(id, t) => {
            if let Some(j) = s.files.jobs.iter_mut().find(|j| j.id == *id) {
                j.chosen = Some(*t);
            }
        }
        Action::FilesCleanAnyway => s.files.yielding = false,
        Action::FilesCleanNext(id) => s.files.next_up = Some(*id),
        Action::FilesRemove(id) => s.files.jobs.retain(|j| j.id != *id),
        Action::FilesClear => {
            s.files.cleared = s.files.jobs.len();
            s.files.jobs.clear();
        }
        Action::FilesUndoClear => s.files.cleared = 0,
        #[cfg(feature = "speakers")]
        Action::SpeakersOn(on) => {
            s.speakers.enabled = *on;
            if *on {
                return speakers_start(s);
            }
            speakers_stop(&mut s.speakers);
        }
        #[cfg(feature = "speakers")]
        Action::SpeakersOutput(o) => {
            let sp = &mut s.speakers;
            sp.output = o.clone();
            if sp.phase == Phase::Ready {
                let present = o
                    .as_ref()
                    .is_none_or(|o| sp.outputs.iter().any(|d| &d.name == o && d.present));
                sp.playing_on = if present {
                    o.clone().or_else(|| sp.system_default.clone())
                } else {
                    sp.system_default.clone()
                };
            }
        }
        #[cfg(feature = "speakers")]
        Action::SpeakersStrength(st) => {
            if let Some(v) = st.attn_limit() {
                s.speakers.strength = *st;
                s.speakers.attn_limit = v;
            }
        }
        #[cfg(feature = "speakers")]
        Action::SpeakersHold(on) => {
            s.speakers.held = *on && s.speakers.phase == Phase::Ready;
        }
        #[cfg(feature = "speakers")]
        Action::SpeakersRetry => {
            if s.speakers.phase == Phase::Down {
                s.speakers.phase = Phase::Starting;
                s.speakers.failing = false;
                s.speakers.error = None;
                s.speakers.generation += 1;
                return true;
            }
        }
    }
    false
}

/// A strength or model edit lands where the daemon puts it: the device in
/// use when it has its own settings, else the defaults.
fn edit_sound(s: &mut AppState, sound: Sound) {
    if s.sound.saved {
        if let Some(p) = s
            .sound
            .profiles
            .iter_mut()
            .find(|p| Some(&p.device) == s.sound.owner.as_ref())
        {
            p.sound = sound.clone();
        }
    } else {
        s.sound.defaults = sound.clone();
    }
    s.sound.current = sound;
}

/// End a mock start: the chain is up.
pub fn settle(s: &mut AppState) {
    #[cfg(feature = "speakers")]
    let sp = &mut s.speakers;
    #[cfg(feature = "speakers")]
    if sp.phase == Phase::Starting {
        sp.phase = Phase::Ready;
        sp.tier = Some(EngineTier::Quality);
        let present = sp
            .output
            .as_ref()
            .is_none_or(|o| sp.outputs.iter().any(|d| &d.name == o && d.present));
        sp.playing_on = if present {
            sp.output.clone().or_else(|| sp.system_default.clone())
        } else {
            sp.system_default.clone()
        };
    }
    if s.engine.phase == Phase::Starting {
        s.engine.phase = Phase::Ready;
        s.engine.start_reason = None;
        s.engine.tier = Some(if s.engine.configured_light {
            EngineTier::Light
        } else {
            EngineTier::Quality
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_words_are_unique_and_parse_back() {
        let mut seen = std::collections::HashSet::new();
        for sc in Scenario::ALL {
            assert!(seen.insert(sc.word()), "duplicate word {}", sc.word());
            assert_eq!(Scenario::parse(sc.word()), Some(sc));
        }
        assert_eq!(Scenario::parse("nope"), None);
    }

    #[test]
    fn turning_on_from_off_passes_through_starting() {
        let mut s = state(Scenario::Off);
        assert!(apply(&mut s, &Action::SetMode(Mode::Suppress)));
        assert_eq!(s.engine.phase, Phase::Starting);
        assert_eq!(s.engine.start_reason, Some(StartReason::TurnOn));
        settle(&mut s);
        assert_eq!(s.engine.phase, Phase::Ready);
        assert_eq!(s.vis(), Vis::Filtering);
        // A live switch is no start.
        assert!(!apply(&mut s, &Action::SetMode(Mode::Bypass)));
        assert_eq!(s.vis(), Vis::Raw);
    }

    #[test]
    fn mute_returns_to_where_it_came_from() {
        let mut s = state(Scenario::Bypass);
        apply(&mut s, &Action::ToggleMute);
        assert_eq!(s.mode, Mode::Mute);
        apply(&mut s, &Action::ToggleMute);
        assert_eq!(s.mode, Mode::Bypass);
    }

    #[test]
    fn the_switch_while_muted_picks_where_unmute_returns() {
        let mut s = state(Scenario::Bypass);
        apply(&mut s, &Action::ToggleMute);
        apply(&mut s, &Action::SetUnmuteTo(Mode::Suppress));
        assert_eq!(s.mode, Mode::Mute, "stays muted");
        assert_eq!(s.unmute_to, Mode::Suppress);
        apply(&mut s, &Action::ToggleMute);
        assert_eq!(s.mode, Mode::Suppress);
        // Not muted: nothing changes.
        apply(&mut s, &Action::SetUnmuteTo(Mode::Bypass));
        assert_eq!((s.mode, s.unmute_to), (Mode::Suppress, Mode::Suppress));
    }

    #[test]
    fn edits_land_in_the_profile_in_use_or_the_defaults() {
        let mut s = state(Scenario::OnInCall);
        apply(&mut s, &Action::SetStrength(Strength::Strong));
        assert_eq!(s.sound.current.strength, Strength::Strong);
        assert_eq!(s.sound.profiles[0].sound.strength, Strength::Strong);
        assert_eq!(s.sound.defaults.strength, Strength::Maximum);
        apply(&mut s, &Action::SelectMic(None));
        assert!(!s.sound.saved);
        assert_eq!(s.sound.current, s.sound.defaults);
        apply(&mut s, &Action::SetStrength(Strength::Gentle));
        assert_eq!(s.sound.defaults.strength, Strength::Gentle);
        apply(&mut s, &Action::ResetProfile(RODE.into()));
        assert!(!s.sound.profiles.iter().any(|p| p.device == RODE));
    }

    #[test]
    fn the_board_profiles_and_direct_states() {
        let s = state(Scenario::OnInCall);
        assert_eq!(s.sound.current.strength, Strength::Maximum);
        let order: Vec<&str> = s
            .mic
            .devices
            .iter()
            .map(|d| d.description.as_str())
            .collect();
        assert_eq!(order, ["RODE NT-USB", "Built-in Audio", "USB Headset"]);
        let headset = s
            .sound
            .profiles
            .iter()
            .find(|p| p.description == "USB Headset")
            .unwrap();
        assert_eq!(headset.sound.strength, Strength::Medium);
        assert_eq!(headset.sound.model, Model::Light);
        assert_eq!(s.sound.defaults, Sound::default());
        for sc in [
            Scenario::DirectMic,
            Scenario::DirectMicFollowsDefault,
            Scenario::DirectMicNoName,
        ] {
            assert!(!state(sc).in_use(), "{sc:?}: nobody uses HushMic");
        }
        // The default switch moves a follower to HushMic, not a pinned app.
        let mut s = state(Scenario::DirectMicFollowsDefault);
        apply(&mut s, &Action::SetDefaultMic(true));
        assert!(s.apps.direct.is_empty());
        assert_eq!(s.apps.using[0].name.as_deref(), Some("Zoom"));
        let mut s = state(Scenario::DirectMic);
        apply(&mut s, &Action::SetDefaultMic(true));
        assert_eq!(s.apps.direct.len(), 1);
        assert!(s.apps.using.is_empty());
    }

    #[test]
    fn the_mock_answers_like_the_daemon() {
        let mut s = state(Scenario::OnInCall);
        apply(&mut s, &Action::UiFirstRun(Some(2)));
        assert_eq!(s.ui.page, Page::FirstRun);
        let mut s = state(Scenario::DirectMic);
        apply(&mut s, &Action::SetMode(Mode::Off));
        assert!(s.apps.direct.is_empty());
        assert_eq!(s.engine.start_reason, None);
        assert!(state(Scenario::Off).apps.direct.is_empty());
    }

    #[test]
    fn first_run_scenarios_start_fresh() {
        for sc in [
            Scenario::FirstRun1,
            Scenario::FirstRun2,
            Scenario::FirstRun3,
            Scenario::FirstRunOldPipewire,
        ] {
            let s = state(sc);
            assert_eq!(s.ui.page, Page::FirstRun);
            assert!(s.ui.first_run_step.is_some());
            assert!(!s.ui.beam_used);
            assert!(!s.in_use(), "{sc:?}");
        }
        assert!(state(Scenario::FirstRun3Connected).in_use());
    }
}
