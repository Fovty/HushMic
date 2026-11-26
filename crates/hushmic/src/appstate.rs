//! The window's view of the daemon. One snapshot type, [`AppState`], that
//! the daemon pushes over the control socket's `watch` channel whenever
//! anything in it changes, and the requests, [`Action`], the window sends
//! back on ordinary one-shot control connections. Pure data: the daemon
//! fills it from the state its main loop owns, the window renders it, and
//! [`mock`] builds every board state for UI work without a daemon.
//!
//! Wire stability: every struct reads with `#[serde(default)]` and unknown
//! fields are ignored, so a field added later never breaks a reader built
//! before it. A change that would break one is a new [`PROTOCOL`].
//!
//! No UI copy lives here: the window turns these facts into words.

use crate::config::{Appearance, HiddenNotice, Inference, TrayIcon};
use crate::controller::{RunMode, LIGHT_MODEL};
use crate::diagnostics::EngineTier;
use crate::shortcuts::Action as ShortcutAction;
use serde::{Deserialize, Serialize};

pub mod mock;

/// The watch protocol version every line carries (see `watch.rs`).
pub const PROTOCOL: u32 = 1;

/// The quality model's id (the light one is [`LIGHT_MODEL`]).
pub const QUALITY_MODEL: &str = "dpdfnet8_48khz_hr";

/// Everything the window shows, as one value. The daemon sends a fresh one
/// on every change; the window never asks.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    pub daemon: DaemonInfo,
    /// What apps using HushMic get: suppress / bypass / mute, or off (no
    /// virtual microphone at all).
    pub mode: Mode,
    /// Where Unmute and the toggles return: suppress or bypass (never mute
    /// or off). Lets the window draw the switch while muted.
    pub unmute_to: Mode,
    pub engine: Engine,
    pub mic: MicState,
    pub sound: SoundState,
    pub apps: AppsState,
    pub shortcuts: ShortcutsState,
    pub prefs: Prefs,
    /// Whether HushMic has an icon in the panel.
    pub tray: TrayHost,
    pub pipewire: PipeWireState,
    /// The window's own state, kept by the daemon across window re-creation
    /// and restarts.
    pub ui: UiState,
    /// Light or dark, and less motion: the desktop's wishes with the
    /// Appearance setting applied.
    pub desktop: DesktopLook,
    /// "Clean a file": the files on the page and how far each is.
    pub files: FilesState,
    /// HushMic Speakers: what you hear, cleaned.
    #[cfg(feature = "speakers")]
    pub speakers: SpeakersState,
    /// The tray's Quit or Off was chosen while an app uses HushMic: the
    /// window asks first (a tray menu has no dialogs), and the daemon acts
    /// on the answer. Until it is answered or the window closes.
    pub ask: Option<Ask>,
}

/// What the daemon has the window ask before it acts (UX panel A3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ask {
    Quit,
    TurnOff,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonInfo {
    pub version: String,
    /// Started with `--headless` (the systemd unit): no tray this run.
    pub headless: bool,
    pub flatpak: bool,
}

/// The four states of the microphone HushMic offers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Suppress,
    Bypass,
    Mute,
    #[default]
    Off,
}

impl Mode {
    /// From the daemon's selection (None = off).
    pub fn from_selection(sel: Option<RunMode>) -> Self {
        match sel {
            Some(RunMode::Suppress) => Mode::Suppress,
            Some(RunMode::Bypass) => Mode::Bypass,
            Some(RunMode::Mute) => Mode::Mute,
            None => Mode::Off,
        }
    }

    pub fn selection(self) -> Option<RunMode> {
        match self {
            Mode::Suppress => Some(RunMode::Suppress),
            Mode::Bypass => Some(RunMode::Bypass),
            Mode::Mute => Some(RunMode::Mute),
            Mode::Off => None,
        }
    }

    /// The word `hushmic mode` takes and prints.
    pub fn word(self) -> &'static str {
        crate::control::mode_word(self.selection())
    }
}

/// Where the virtual microphone is in its life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Turned off: no virtual microphone.
    #[default]
    Off,
    /// The chain was just started and its node is not up yet. See
    /// [`Engine::start_reason`] for why.
    Starting,
    /// Up and usable.
    Ready,
    /// The node went away and the watchdog is bringing it back.
    Reconnecting,
    /// Should be on but is not, and no start is under way: "Needs
    /// attention" (the watchdog keeps retrying in the background).
    Down,
}

/// Why the chain is starting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StartReason {
    /// The daemon itself just started (a launch or login).
    Launch,
    /// The user turned HushMic on from Off.
    TurnOn,
    /// A microphone, model or default-microphone change restarted it.
    Settings,
    /// The chosen microphone went away or came back.
    Recovery,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Engine {
    pub phase: Phase,
    /// Set while `phase` is Starting.
    pub start_reason: Option<StartReason>,
    /// The tier the running chain reports; None before it said anything or
    /// without a chain.
    pub tier: Option<EngineTier>,
    /// The chain runs the light model by choice, so a light tier is not
    /// "Light for now".
    pub configured_light: bool,
    /// Changes whenever the chain is started or stopped: anything the
    /// window measured on the old chain (a Mic check take) is stale.
    pub generation: u64,
    /// The last start failure, in English (for diagnostics; the window
    /// words the state itself). Cleared when the chain comes up.
    pub error: Option<String>,
    /// The chain keeps restarting: from the moment the daemon says so
    /// (the flapping notification) until it has been stable for a while,
    /// or fails outright instead.
    pub flapping: bool,
    /// Bumped as soon as the chain runs again after a failure, or after it
    /// kept restarting once it is stable again (never mid-flap; the desktop
    /// notification waits until it has been stable a while). It only grows,
    /// so a change while the window is open is a new recovery.
    pub recovered: u64,
    /// The running chain's engine-meter file (M2), which the window maps
    /// read-only while visible; None without a chain or a meter. A new
    /// path is a new chain.
    pub meter: Option<String>,
    /// While an app uses HushMic: the CLOCK_MONOTONIC ms before which the
    /// meter's hops reached no app (the engine also runs for Mic check or a
    /// volume meter); 0 when the whole current run was heard. Cut the
    /// meter's history there (`EngineMeter::history_since`). None while no
    /// app uses HushMic.
    pub use_began_ms: Option<u64>,
}

/// The strength names. Each is a dB cap on the plugin; anything else (set
/// from the command line) is Custom.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strength {
    Gentle,
    Medium,
    Strong,
    #[default]
    Maximum,
    Custom,
}

impl Strength {
    /// The four presets in ascending order, with their dB caps (the tray's
    /// and the CLI's words for the same values).
    pub const PRESETS: [(Strength, f32); 4] = [
        (Strength::Gentle, 6.0),
        (Strength::Medium, 12.0),
        (Strength::Strong, 24.0),
        (Strength::Maximum, 100.0),
    ];

    pub fn from_attn(attn_limit: f32) -> Self {
        Self::PRESETS
            .iter()
            .find(|(_, v)| (v - attn_limit).abs() < 0.5)
            .map(|(s, _)| *s)
            .unwrap_or(Strength::Custom)
    }

    /// The dB cap; None for Custom.
    pub fn attn_limit(self) -> Option<f32> {
        Self::PRESETS
            .iter()
            .find(|(s, _)| *s == self)
            .map(|(_, v)| *v)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Model {
    #[default]
    Quality,
    Light,
}

impl Model {
    pub fn from_id(id: &str) -> Self {
        if id == LIGHT_MODEL {
            Model::Light
        } else {
            Model::Quality
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Model::Quality => QUALITY_MODEL,
            Model::Light => LIGHT_MODEL,
        }
    }
}

/// One set of noise-removal settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sound {
    pub strength: Strength,
    pub attn_limit: f32,
    pub model: Model,
    /// The model file stem, as configured (a hand-edited id that is neither
    /// model shows as Quality here and fails at start).
    pub model_id: String,
}

impl Sound {
    pub fn new(model_id: &str, attn_limit: f32) -> Self {
        Sound {
            strength: Strength::from_attn(attn_limit),
            attn_limit,
            model: Model::from_id(model_id),
            model_id: model_id.to_string(),
        }
    }
}

impl Default for Sound {
    fn default() -> Self {
        Sound::new(QUALITY_MODEL, 100.0)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundState {
    /// The settings in effect (Strength and Model on the Live page).
    pub current: Sound,
    /// The device they belong to (node.name); None = no device can own
    /// settings right now, the defaults apply.
    pub owner: Option<String>,
    /// `owner` has its own saved settings (else it runs on the defaults).
    pub saved: bool,
    /// "Other microphones": what every device without its own settings
    /// uses.
    pub defaults: Sound,
    /// Every device with its own saved settings (Settings › Microphones).
    pub profiles: Vec<MicProfile>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MicProfile {
    pub device: String,
    /// Its display name when known (else the node name).
    pub description: String,
    pub sound: Sound,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MicState {
    /// The chosen microphone (node.name); None = System default.
    pub selected: Option<String>,
    /// The chosen microphone is gone and the chain runs on the system
    /// default until it is back.
    pub fallback: bool,
    /// The device the running chain listens to (node.name); None = not
    /// known or no chain.
    pub active: Option<String>,
    /// The device "System default" stands for right now (node.name).
    pub system_default: Option<String>,
    /// The capture devices, in PipeWire's order. A chosen device that is
    /// not plugged in keeps the place it last had (last if it was not seen
    /// this run), with `present: false`.
    pub devices: Vec<MicDevice>,
    /// Another app keeps re-routing HushMic's capture (EasyEffects'
    /// "process all input streams").
    pub rerouted: bool,
    /// The microphone does not connect: HushMic's capture stream stays
    /// fed by nothing though chain restarts tried (pipewire::Repin).
    pub stuck: bool,
    /// An app uses HushMic and the microphone has sent exact digital
    /// silence for 10 s (the daemon's look, every tick; Live sees it
    /// sooner from its own picture).
    pub silent: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MicDevice {
    pub name: String,
    pub description: String,
    pub present: bool,
    /// The device has its own saved strength and model.
    pub saved_settings: bool,
}

/// HushMic Speakers, as the Speakers page shows it.
#[cfg(feature = "speakers")]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeakersState {
    /// The saved switch (`speakers.enabled`).
    pub enabled: bool,
    /// The speakers chain: Off while not wanted (switched off, or HushMic
    /// off), Starting from a start until its nodes are up, Ready up,
    /// Reconnecting while it comes back after its sink went away, Down
    /// wanted but not up and no start under way (retries go on).
    pub phase: Phase,
    /// Several starts in a row failed ("keeps failing").
    pub failing: bool,
    /// The last start failure, in English (for diagnostics; cleared when
    /// a start works).
    pub error: Option<String>,
    /// The saved Play to (node.name); None = System default.
    pub output: Option<String>,
    /// The real output the sound reaches now (node.name), past HushMic
    /// Speakers; None = not known or not running.
    pub playing_on: Option<String>,
    /// The real output "System default" stands for now (node.name): the
    /// system default, or the last real one while the user made HushMic
    /// Speakers the system output.
    pub system_default: Option<String>,
    /// The real outputs in PipeWire's order, without HushMic Speakers. A
    /// chosen output that is not plugged in keeps the place it last had
    /// (last if it was not seen this run), with `present: false`.
    pub outputs: Vec<OutputDevice>,
    pub strength: Strength,
    /// `speakers.attn_limit`, dB.
    pub attn_limit: f32,
    /// "Hold to hear the original" is on in the running chain.
    pub held: bool,
    /// The user made HushMic Speakers the system output.
    pub is_default: bool,
    /// The apps playing through it now (HushMic's own streams left out).
    pub playing: Vec<AppRef>,
    /// Apps a restart of HushMic Speakers left on another output: they
    /// named the old one by number or a mixer moved them in, so only kept
    /// metadata could hold them on the new one, and this WirePlumber (0.4.8,
    /// or one HushMic cannot tell) would save it as the pick of every app.
    /// Elsewhere HushMic holds them and this stays empty. Shown until they
    /// end or play through it again: "<App> left HushMic Speakers when it
    /// restarted. Pick HushMic Speakers for it again."
    pub left: Vec<AppRef>,
    /// What its engine reports; None before it said, or without a chain.
    pub tier: Option<EngineTier>,
    /// Bumped on every start and stop.
    pub generation: u64,
}

#[cfg(feature = "speakers")]
impl Default for SpeakersState {
    fn default() -> Self {
        SpeakersState {
            enabled: false,
            phase: Phase::Off,
            failing: false,
            error: None,
            output: None,
            playing_on: None,
            system_default: None,
            outputs: Vec::new(),
            strength: Strength::Maximum,
            attn_limit: 100.0,
            held: false,
            is_default: false,
            playing: Vec::new(),
            left: Vec::new(),
            tier: None,
            generation: 0,
        }
    }
}

/// A real output (an `Audio/Sink`).
#[cfg(feature = "speakers")]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputDevice {
    pub name: String,
    pub description: String,
    pub present: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppsState {
    /// Apps recording HushMic right now. The Live page only asks whether
    /// there are any; first run names the first one.
    pub using: Vec<AppRef>,
    /// Apps recording the microphone HushMic cleans, directly, for some
    /// seconds. The window no longer warns about them (round 3b); the
    /// snapshot and `status --json` keep them. Notices hidden with
    /// `hushmic notice hide` are left out; see [`Prefs::hidden_notices`].
    pub direct: Vec<DirectUse>,
    /// "Use as default microphone", the setting. It stays on when the user
    /// moves the default away (see `default_released`).
    pub set_default: bool,
    /// Whether HushMic may change the default here (a sandbox without the
    /// permission: the switch hides).
    pub can_set_default: bool,
    /// The user moved the system default away from HushMic, to this device
    /// (display name): HushMic leaves it there until the next deliberate
    /// start (a new daemon, turning HushMic on from Off, or setting
    /// `set_default` on again). A takeover whose write never landed is not
    /// reported here: nothing was taken, so nothing was moved away.
    pub default_released: Option<String>,
}

/// An app as PipeWire describes it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppRef {
    /// A stable key without whitespace (Flatpak app id, binary, or name):
    /// what a hidden notice is stored under.
    pub id: String,
    /// A name fit to show; None when the app reports nothing usable
    /// ("An app").
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DirectUse {
    pub app: AppRef,
    /// The app records whatever the system default is, so turning on "Use
    /// as default microphone" moves it to HushMic at once.
    pub follows_default: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShortcutsBackend {
    /// Not known yet (the desktop session is still starting).
    #[default]
    Unknown,
    /// The desktop's GlobalShortcuts portal.
    Portal,
    /// KDE's own shortcut service (Plasma before 6.4): the keys are set in
    /// System Settings.
    KdeService,
    /// No shortcuts on this desktop.
    Unsupported,
}

/// The keys bound to one action, as the desktop describes them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "text", rename_all = "kebab-case")]
pub enum Keys {
    /// The desktop did not say (some never do).
    #[default]
    NotReported,
    /// No key is bound.
    NotSet,
    /// The desktop's own text ("Ctrl+Alt+M"): one piece, never split.
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutKeys {
    pub action: ShortcutAction,
    #[serde(default)]
    pub keys: Keys,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutsState {
    /// A shortcuts session is up: keys work and the dialog can open.
    pub available: bool,
    pub backend: ShortcutsBackend,
    /// The user went through the desktop's dialog once.
    pub set_up: bool,
    /// One entry per action, in the order of `shortcuts::Action::ALL`.
    pub keys: Vec<ShortcutKeys>,
}

impl ShortcutsState {
    /// Every action with unknown keys.
    pub fn unreported() -> Vec<ShortcutKeys> {
        ShortcutAction::ALL
            .iter()
            .map(|a| ShortcutKeys {
                action: *a,
                keys: Keys::NotReported,
            })
            .collect()
    }
}

/// The settings the window shows as switches and choices.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub autostart: bool,
    pub notifications: bool,
    pub appearance: Appearance,
    /// "Show tray icon" (takes effect at the next start).
    pub tray: bool,
    pub tray_icon: TrayIcon,
    pub inference: Inference,
    /// "Stay on my voice": the plugin's voice hold.
    pub voice_hold: bool,
    /// Apps whose "Direct mic" notice the user hid (`hushmic notice`;
    /// no longer shown in the window).
    pub hidden_notices: Vec<HiddenNotice>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrayHost {
    /// No icon wanted this run (`--headless`, or `tray = false`).
    #[default]
    Disabled,
    /// Wanted, but the desktop has no tray (yet): retried in the background.
    Waiting,
    Registered,
}

/// Light or dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

impl Theme {
    /// Settings › Appearance applied: System follows the desktop, and is
    /// dark when the desktop does not say.
    pub fn resolve(appearance: Appearance, system: Option<Theme>) -> Theme {
        match appearance {
            Appearance::Dark => Theme::Dark,
            Appearance::Light => Theme::Light,
            Appearance::System => system.unwrap_or(Theme::Dark),
        }
    }
}

/// What the desktop asks of the window's look (the daemon reads the
/// settings portal; the window opens no bus for it). The daemon starts
/// reading with the first window it opens: until then (a tray or headless
/// run without a window yet) this holds the defaults, dark and full motion,
/// with the Appearance setting applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopLook {
    /// The style the desktop prefers; None = no preference, or nothing
    /// says (no settings portal).
    pub system_theme: Option<Theme>,
    /// The theme to draw: `Theme::resolve(prefs.appearance, system_theme)`.
    pub effective_theme: Theme,
    /// The desktop asks for less motion (false when nothing says).
    pub reduced_motion: bool,
}

#[cfg(feature = "speakers")]
impl SpeakersState {
    /// An output's display name (node.name when it is not listed).
    pub fn description_of<'a>(&'a self, name: &'a str) -> &'a str {
        self.outputs
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.description.as_str())
            .unwrap_or(name)
    }

    /// The chosen output is not plugged in (the sound plays on the system
    /// default meanwhile).
    pub fn output_absent(&self) -> bool {
        self.output
            .as_ref()
            .is_some_and(|o| self.outputs.iter().any(|d| &d.name == o && !d.present))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PipeWireState {
    /// PipeWire answered the last probe. False: the "can't reach PipeWire"
    /// page.
    pub reachable: bool,
    /// PipeWire can stream a capture to a pipe (old PipeWire cannot: no
    /// live level in first run). None = not known yet.
    pub live_capture: Option<bool>,
}

impl Default for PipeWireState {
    fn default() -> Self {
        PipeWireState {
            reachable: true,
            live_capture: None,
        }
    }
}

/// The window's pages, as routed by `--page` and remembered by the daemon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Page {
    /// Live; Mic check was merged into it (an old window.json or wire
    /// word "mic-check" opens Live).
    #[default]
    #[serde(alias = "mic-check")]
    #[cfg_attr(not(feature = "speakers"), serde(alias = "speakers"))]
    Live,
    /// The speakers cleaner (HushMic Speakers). A build without it reads
    /// its word in a saved window.json as Live.
    #[cfg(feature = "speakers")]
    Speakers,
    /// Clean a file.
    Files,
    /// Settings, scrolled to Shortcuts.
    Shortcuts,
    Settings,
    /// Settings, scrolled to About.
    About,
    FirstRun,
}

impl Page {
    pub const ALL: [Page; 6 + cfg!(feature = "speakers") as usize] = [
        Page::Live,
        #[cfg(feature = "speakers")]
        Page::Speakers,
        Page::Files,
        Page::Shortcuts,
        Page::Settings,
        Page::About,
        Page::FirstRun,
    ];

    /// The word on the command line (`--page files`) and the wire.
    pub fn word(self) -> &'static str {
        match self {
            Page::Live => "live",
            #[cfg(feature = "speakers")]
            Page::Speakers => "speakers",
            Page::Files => "files",
            Page::Shortcuts => "shortcuts",
            Page::Settings => "settings",
            Page::About => "about",
            Page::FirstRun => "first-run",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        if word == "mic-check" {
            // Mic check is part of Live now
            return Some(Page::Live);
        }
        Self::ALL.into_iter().find(|p| p.word() == word)
    }
}

/// The window's state the daemon keeps for it (in a small state file, not
/// the user's config): restored when the window is re-created on a
/// relaunch, and across restarts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// The page last shown.
    pub page: Page,
    /// First run is under way at this step (1 to 3); None = done, or never
    /// needed (an existing install).
    pub first_run_step: Option<u8>,
    /// The first-start beam has been used up (once per install).
    pub beam_used: bool,
    /// The window's last size (logical points), restored on the next open.
    pub size: Option<[u16; 2]>,
    /// The file selected on the Files page and its playhead (48 kHz
    /// frames). The ids are this run's: a stale one names no file.
    pub file: Option<(u64, u64)>,
}

/// The daemon's file jobs (`files::jobs`). The bulk (the cleaned sound,
/// the file's picture) stays in each job's cache folder, which the window
/// reads itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesState {
    /// In the order they were added.
    pub jobs: Vec<FileJob>,
    /// Cleaning waits: the microphone's engine has stepped down for want
    /// of CPU, and the microphone comes first.
    pub yielding: bool,
    /// The strength the page plays and saves at: a dB cap, as in the
    /// Settings (which it starts from).
    pub strength: f32,
    /// Quit was chosen (in the tray or the window) while files clean or
    /// save: the window asks first ("Quit HushMic? 2 files are still
    /// cleaning.", the quit sheet over Files). It goes when the
    /// user answers, or when nothing is left to stop.
    pub ask_quit: bool,
    /// Cleaned files cannot go next to their originals (a sandbox sees
    /// only the files it was given): Save goes into `folder`, which the
    /// page asks for once.
    pub needs_folder: bool,
    /// Where cleaned files go when not next to the original.
    pub folder: Option<String>,
    /// That folder did not answer when last looked at (a share that mounts
    /// slowly): kept, but a Save asks where meanwhile (in a sandbox).
    pub folder_unreachable: bool,
    /// Clear all took this many files off the page a moment ago: Undo can
    /// still bring them back (the page's toast).
    pub cleared: usize,
    /// A row's × took this one (its name) a moment ago, instead.
    pub cleared_name: Option<String>,
    /// The file that starts cleaning next, when one waits (its row offers
    /// no "Clean next").
    pub next_up: Option<u64>,
    /// "Save all and quit" was chosen: HushMic quits once this many files
    /// are saved (the page's header, with Cancel).
    pub quitting: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileJob {
    pub id: u64,
    pub path: String,
    pub state: JobState,
    /// The folder of the cleaned sound and the picture (see `files::cache`).
    pub cache: String,
    /// The file's own rate and channels; `stored` is 1 for mono recorded
    /// as stereo. Zero until the worker has opened the file.
    pub rate: u32,
    pub channels: usize,
    pub stored: usize,
    pub video: bool,
    pub codec: String,
    /// Read through ffmpeg (symphonia cannot).
    pub ffmpeg: bool,
    /// Read below its quality (HE-AAC without ffmpeg: its core only).
    pub reduced: bool,
    /// The length in 48 kHz frames: as the file states it until `exact`.
    pub frames: Option<u64>,
    pub exact: bool,
    /// The length the file states (48 kHz frames), kept after the full
    /// read counted `frames`: a damaged file says how long it should be.
    pub stated: Option<u64>,
    /// The cleaned parts, `[start, end)` in 48 kHz frames.
    pub lit: Vec<(u64, u64)>,
    /// The file stops early: this much can be cleaned.
    pub damaged: Option<u64>,
    /// Why it needs attention.
    pub problem: Option<crate::files::worker::Problem>,
    /// What Save writes: the extension, whether in the original's own
    /// format, and whether a video saves as its sound only.
    pub out_ext: String,
    pub same_format: bool,
    pub sound_only: bool,
    /// The strength the output for Save is ready at.
    pub prepared: Option<f32>,
    /// Save was asked for and is under way.
    pub saving: bool,
    /// Where the last Save put the cleaned file, and at what strength (a
    /// dB cap): at another strength than the page's now, the row offers
    /// Save again ("Saved at Maximum · Save Strong").
    pub saved: Option<String>,
    pub saved_db: Option<f32>,
    /// Why the last Save did not happen.
    pub save_problem: Option<crate::files::worker::Problem>,
    /// What the page names with `save_problem`: the folder that can't be
    /// written (`NoWrite`), the encoder's own reason (`Encode`), or the
    /// short reason of a Save that failed otherwise (`Failed`).
    pub save_detail: Option<String>,
    /// The last Save went next to the original instead of the chosen
    /// folder: why (the page says so with where it went).
    pub folder_miss: Option<FolderMiss>,
    /// How far the output for Save is being prepared, 0 to 1, while it is.
    pub preparing: Option<f32>,
    /// The pass stopped on an error (or its worker ended early) this many
    /// times in a row; a cleaned file starts over at 0.
    pub stops: u32,
    /// The user chose to clean the part before the damage (`damaged`).
    pub damage_ok: bool,
    /// The sound tracks Save keeps, how many of them are cleaned (the
    /// main one, then any other one after another), and the file's tracks
    /// Save can't keep (only one, without ffmpeg). 0 until opened.
    pub tracks: usize,
    pub cleaning: usize,
    pub dropped: usize,
    /// How far the other tracks are, in tracks (1.5: one is through, the
    /// next half).
    pub more_done: f32,
    /// The other tracks, in order, as cleaned: each one's length and
    /// channels (in its cache folder), or None (copied as it is).
    pub more: Vec<Option<(u64, usize)>>,
    /// The sound track cleaned and played (0: the first; the voice's
    /// unless the user chose), and every sound track's title (empty
    /// without one; none until opened, or without ffmpeg).
    pub track: usize,
    pub titles: Vec<String>,
    /// The track the user chose on the row (None: the voice's).
    pub chosen: Option<usize>,
    /// The file's own strength (a dB cap): what it plays and saves at.
    /// A file takes the page's strength when added; a change on the page
    /// is the selected file's (and the next ones').
    pub db: f32,
}

impl FileJob {
    /// Cleaned, and no copy of it saved (none asked for, or the Save
    /// failed): quitting loses the cleaning.
    pub fn unsaved(&self) -> bool {
        self.state == JobState::Done && self.saved.is_none() && !self.saving
    }

    /// How much of the cleaning is done, 0 to 1: every track Save cleans
    /// counts the same.
    pub fn cleaned_fraction(&self) -> f32 {
        if self.state == JobState::Done {
            return 1.0;
        }
        let first = match self.frames {
            Some(f) if f > 0 => {
                let lit: u64 = self.lit.iter().map(|(a, b)| b - a).sum();
                (lit as f64 / f as f64).min(1.0) as f32
            }
            _ => 0.0,
        };
        ((first + self.more_done) / self.cleaning.max(1) as f32).min(1.0)
    }
}

/// Why a Save did not go into the chosen folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FolderMiss {
    /// It is gone (deleted, unmounted, a grant that did not last).
    Gone,
    /// A look at it did not answer in time (a hung mount or portal).
    NoAnswer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobState {
    /// Not started, or stopped behind the file being played.
    #[default]
    Waiting,
    Cleaning,
    Done,
    NeedsAttention,
}

/// What the Live picture shows, as far as the switch-on moment cares (the
/// beam gate's input).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vis {
    Starting,
    Off,
    Filtering,
    /// Your raw voice: bypass, or "Noise suppression paused for now".
    Raw,
    Muted,
}

impl AppState {
    /// An app is recording HushMic right now.
    pub fn in_use(&self) -> bool {
        !self.apps.using.is_empty()
    }

    /// The picture's state for the switch-on moment. A chain that is not up
    /// (starting, reconnecting, down) reads as Starting or Off; an up chain
    /// as its mode.
    pub fn vis(&self) -> Vis {
        if self.mode == Mode::Off {
            return Vis::Off;
        }
        match self.engine.phase {
            Phase::Off | Phase::Down => Vis::Off,
            Phase::Starting | Phase::Reconnecting => Vis::Starting,
            Phase::Ready => match self.mode {
                Mode::Suppress if self.engine.tier == Some(EngineTier::Passthrough) => Vis::Raw,
                Mode::Suppress => Vis::Filtering,
                Mode::Bypass => Vis::Raw,
                Mode::Mute => Vis::Muted,
                Mode::Off => Vis::Off,
            },
        }
    }

    /// A device's display name: its description when listed, else the one
    /// its remembered settings carry.
    pub fn description_of(&self, name: &str) -> Option<&str> {
        self.mic
            .devices
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.description.as_str())
            // a microphone that isn't listed now (unplugged, PipeWire
            // gone) keeps the name its settings remember
            .or_else(|| {
                self.sound
                    .profiles
                    .iter()
                    .find(|p| p.device == name && p.description != p.device)
                    .map(|p| p.description.as_str())
            })
    }
}

/// A request from the window. Each is one line on the control socket, the
/// same grammar the CLI speaks; [`Action::request_line`] writes it and
/// `control::parse_request` reads it.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    SetMode(Mode),
    /// Mute, or back to `unmute_to`.
    ToggleMute,
    /// While muted: set `unmute_to` (Suppress or Bypass) and stay muted
    /// (the switch on a muted Live page). The daemon refuses it when the
    /// mode is not Mute.
    SetUnmuteTo(Mode),
    ToggleBypass,
    /// None = System default.
    SelectMic(Option<String>),
    /// A preset (Custom is not settable from the window).
    SetStrength(Strength),
    SetModel(Model),
    SetDefaultMic(bool),
    SetAutostart(bool),
    SetNotifications(bool),
    /// "Stay on my voice" (restarts the filter).
    SetVoiceHold(bool),
    SetAppearance(Appearance),
    SetTray(bool),
    SetTrayIcon(TrayIcon),
    SetInference(Inference),
    /// Forget a microphone's own settings (Settings › Microphones › Reset).
    ResetProfile(String),
    /// Open the desktop's shortcut dialog (or System Settings on Plasma
    /// before 6.4).
    SetupShortcuts,
    /// Re-read PipeWire now instead of at the next tick (first run waits
    /// for an app this way).
    Refresh,
    /// "Try again": retry a chain that is down now, not at the next backoff
    /// step.
    Retry,
    /// "Quit HushMic".
    Quit,
    /// Remember the page shown.
    UiPage(Page),
    /// First run at this step; None = done (or skipped).
    UiFirstRun(Option<u8>),
    UiBeamUsed,
    /// The user resized the window (logical points).
    UiSize(u16, u16),
    /// The window gained (true) or lost (false) keyboard focus: while it
    /// has it, problems it shows are not also desktop notifications.
    UiFocused(bool),
    /// The problem Live's Status card shows (of those that are also
    /// notifications); None = none of them. Only it is held back while
    /// Live has focus.
    UiShows(Option<crate::notify::Problem>),
    /// The file selected on the Files page and its playhead; None = none.
    UiFile(Option<(u64, u64)>),
    /// Add a file to the Files page (cleaning starts when it is its turn).
    FilesAdd(std::path::PathBuf),
    /// Take a file off the page: its cleaning stops and its cache goes.
    FilesRemove(u64),
    /// The user plays this file from this frame (48 kHz): it cleans first,
    /// from there.
    FilesPlay(u64, u64),
    /// Try a file that needs attention again.
    FilesRetry(u64),
    /// A strength (a dB cap): the file's (what it plays and saves at)
    /// and the page's for files added next; None: the page's only.
    FilesStrength(f32, Option<u64>),
    /// Clean another of the file's sound tracks (0: the first).
    FilesTrack(u64, usize),
    /// Clean files while an app uses HushMic on a busy computer ("Clean
    /// anyway"), until no app uses it.
    FilesCleanAnyway,
    /// "Clean next": this waiting file goes before the others that wait.
    FilesCleanNext(u64),
    /// Save next to the original; a file still cleaning saves when cleaned.
    FilesSave(u64),
    /// Save into this folder ("Save elsewhere…").
    FilesSaveTo(u64, std::path::PathBuf),
    /// Cancel a Save that waits or runs.
    FilesCancelSave(u64),
    /// The same file found again here ("Choose it again…"): it keeps its
    /// id and place and starts over.
    FilesReplace(u64, std::path::PathBuf),
    /// A damaged file: clean (and save) the part before the damage.
    FilesCleanFirst(u64),
    /// The answer to the quit sheet while files clean: stop them and quit, or keep
    /// cleaning (and running).
    FilesQuit(bool),
    /// The quit sheet's "Save all and quit": every file it would lose is
    /// saved (once cleaned), then HushMic quits.
    FilesSaveAndQuit,
    /// Cancel on "Quits once 2 files are saved": HushMic stays.
    FilesCancelQuit,
    /// Clear all: every file off the page, for a moment undoable.
    FilesClear,
    /// Undo Clear all: the files come back as they were.
    FilesUndoClear,
    /// Quit in the window (More › Quit, Ctrl+Q) while files clean or save:
    /// the daemon asks (the quit sheet) as for the tray's Quit, or
    /// quits when there is nothing left to stop.
    FilesAskQuit,
    /// Where cleaned files go from now on (chosen through the portal).
    FilesFolder(std::path::PathBuf),
    /// Cleaned files go next to their originals again (Settings › Files).
    FilesNextToOriginal,
    /// The answer to the daemon's question ([`AppState::ask`]): go ahead
    /// (Quit, Turn off), or not.
    Answer(bool),
    /// HushMic Speakers on or off (saved).
    #[cfg(feature = "speakers")]
    SpeakersOn(bool),
    /// Where it plays (saved); None = System default.
    #[cfg(feature = "speakers")]
    SpeakersOutput(Option<String>),
    /// A preset (Custom is not settable from the window).
    #[cfg(feature = "speakers")]
    SpeakersStrength(Strength),
    /// "Hold to hear the original": the speakers chain passes its input
    /// through while held. Never saved.
    #[cfg(feature = "speakers")]
    SpeakersHold(bool),
    /// "Try again": start a speakers chain that is down now.
    #[cfg(feature = "speakers")]
    SpeakersRetry,
}

impl Action {
    /// The control-socket request line, without the newline.
    pub fn request_line(&self) -> String {
        let on = |b: bool| if b { "true" } else { "false" };
        match self {
            Action::SetMode(m) => format!("mode {}", m.word()),
            Action::ToggleMute => "toggle mute".into(),
            // Mute and Off are not targets: the daemon refuses the word.
            Action::SetUnmuteTo(m) => format!("unmute-to {}", m.word()),
            Action::ToggleBypass => "toggle bypass".into(),
            Action::SelectMic(None) => "config set mic default".into(),
            Action::SelectMic(Some(m)) => format!("config set mic {m}"),
            // Custom has no value to send: the daemon refuses the word
            // rather than turning it into a preset.
            Action::SetStrength(s) => match s.attn_limit() {
                Some(v) => format!("config set attn_limit {v}"),
                None => "config set attn_limit custom".into(),
            },
            Action::SetModel(m) => format!("config set model {}", m.id()),
            Action::SetDefaultMic(b) => format!("config set set_default {}", on(*b)),
            Action::SetAutostart(b) => format!("config set autostart {}", on(*b)),
            Action::SetNotifications(b) => format!("config set notifications {}", on(*b)),
            Action::SetVoiceHold(b) => format!("config set voice_hold {}", on(*b)),
            Action::SetAppearance(a) => format!("config set appearance {}", a.as_str()),
            Action::SetTray(b) => format!("config set tray {}", on(*b)),
            Action::SetTrayIcon(i) => format!("config set tray_icon {}", i.as_str()),
            Action::SetInference(i) => format!("config set inference {}", i.as_str()),
            Action::ResetProfile(d) => format!("profile reset {d}"),
            Action::SetupShortcuts => "shortcuts setup".into(),
            Action::Refresh => "refresh".into(),
            Action::Retry => "retry".into(),
            // after the window's own question (files it would stop, an
            // app using HushMic): the daemon asks nothing more
            Action::Quit => "quit --force".into(),
            Action::UiPage(p) => format!("ui page {}", p.word()),
            Action::UiFirstRun(Some(step)) => format!("ui first-run {step}"),
            Action::UiFirstRun(None) => "ui first-run done".into(),
            Action::UiBeamUsed => "ui beam-used".into(),
            Action::UiSize(w, h) => format!("ui size {w} {h}"),
            Action::UiFocused(v) => format!("ui focused {}", if *v { "on" } else { "off" }),
            Action::UiShows(p) => format!(
                "ui shows {}",
                p.and_then(crate::notify::Problem::word).unwrap_or("none")
            ),
            Action::UiFile(None) => "ui file none".into(),
            Action::UiFile(Some((id, at))) => format!("ui file {id} {at}"),
            // A JSON string: any path fits on the line, spaces and all.
            Action::FilesAdd(p) => format!(
                "files add {}",
                serde_json::to_string(&p.to_string_lossy()).unwrap_or_default()
            ),
            Action::FilesRemove(id) => format!("files remove {id}"),
            Action::FilesPlay(id, at) => format!("files play {id} {at}"),
            Action::FilesRetry(id) => format!("files retry {id}"),
            Action::FilesStrength(db, None) => format!("files strength {db}"),
            Action::FilesStrength(db, Some(id)) => format!("files strength {db} {id}"),
            Action::FilesTrack(id, t) => format!("files track {id} {}", t + 1),
            Action::FilesCleanAnyway => "files clean-anyway".into(),
            Action::FilesCleanNext(id) => format!("files clean-next {id}"),
            Action::FilesSave(id) => format!("files save {id}"),
            Action::FilesSaveTo(id, d) => format!(
                "files save {id} {}",
                serde_json::to_string(&d.to_string_lossy()).unwrap_or_default()
            ),
            Action::FilesCancelSave(id) => format!("files cancel-save {id}"),
            Action::FilesReplace(id, p) => format!(
                "files replace {id} {}",
                serde_json::to_string(&p.to_string_lossy()).unwrap_or_default()
            ),
            Action::FilesCleanFirst(id) => format!("files clean-first {id}"),
            Action::FilesQuit(true) => "files quit-stop".into(),
            Action::FilesQuit(false) => "files quit-keep".into(),
            Action::FilesSaveAndQuit => "files quit-save".into(),
            Action::FilesCancelQuit => "files quit-cancel".into(),
            Action::FilesAskQuit => "files quit-ask".into(),
            Action::FilesClear => "files clear".into(),
            Action::FilesUndoClear => "files undo-clear".into(),
            Action::FilesFolder(d) => format!(
                "files folder {}",
                serde_json::to_string(&d.to_string_lossy()).unwrap_or_default()
            ),
            Action::FilesNextToOriginal => "files folder none".into(),
            Action::Answer(yes) => format!("ui answer {}", if *yes { "yes" } else { "no" }),
            #[cfg(feature = "speakers")]
            Action::SpeakersOn(b) => format!("speakers {}", if *b { "on" } else { "off" }),
            #[cfg(feature = "speakers")]
            Action::SpeakersOutput(None) => "speakers output default".into(),
            #[cfg(feature = "speakers")]
            Action::SpeakersOutput(Some(o)) => format!("speakers output {o}"),
            #[cfg(feature = "speakers")]
            Action::SpeakersStrength(s) => match s.attn_limit() {
                Some(v) => format!("speakers strength {v}"),
                None => "speakers strength custom".into(),
            },
            #[cfg(feature = "speakers")]
            Action::SpeakersHold(b) => format!("speakers hold {}", if *b { "on" } else { "off" }),
            #[cfg(feature = "speakers")]
            Action::SpeakersRetry => "speakers retry".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlisted_microphone_keeps_its_remembered_name() {
        let mut s = AppState::default();
        s.sound.profiles = vec![
            MicProfile {
                device: "alsa_input.usb-rode".into(),
                description: "RODE NT-USB".into(),
                sound: Sound::default(),
            },
            MicProfile {
                device: "alsa_input.usb-bare".into(),
                description: "alsa_input.usb-bare".into(),
                sound: Sound::default(),
            },
        ];
        assert_eq!(s.description_of("alsa_input.usb-rode"), Some("RODE NT-USB"));
        // a profile that only knows the node name names nothing
        assert_eq!(s.description_of("alsa_input.usb-bare"), None);
        s.mic.devices = vec![MicDevice {
            name: "alsa_input.usb-rode".into(),
            description: "RODE NT-USB Mini".into(),
            present: true,
            saved_settings: true,
        }];
        // the live list wins
        assert_eq!(
            s.description_of("alsa_input.usb-rode"),
            Some("RODE NT-USB Mini")
        );
    }

    #[test]
    fn strength_names_map_to_the_tray_presets() {
        assert_eq!(Strength::from_attn(100.0), Strength::Maximum);
        assert_eq!(Strength::from_attn(24.0), Strength::Strong);
        assert_eq!(Strength::from_attn(12.2), Strength::Medium);
        assert_eq!(Strength::from_attn(6.0), Strength::Gentle);
        assert_eq!(Strength::from_attn(40.0), Strength::Custom);
        assert_eq!(Strength::from_attn(0.0), Strength::Custom);
        for (s, v) in Strength::PRESETS {
            assert_eq!(s.attn_limit(), Some(v));
            assert_eq!(Strength::from_attn(v), s);
        }
        assert_eq!(Strength::Custom.attn_limit(), None);
    }

    #[test]
    fn models_map_by_id() {
        assert_eq!(Model::from_id(LIGHT_MODEL), Model::Light);
        assert_eq!(Model::from_id(QUALITY_MODEL), Model::Quality);
        assert_eq!(Model::Light.id(), LIGHT_MODEL);
        let s = Sound::new(LIGHT_MODEL, 12.0);
        assert_eq!((s.model, s.strength), (Model::Light, Strength::Medium));
    }

    #[test]
    fn modes_round_trip_with_the_daemon_selection() {
        for sel in [
            Some(RunMode::Suppress),
            Some(RunMode::Bypass),
            Some(RunMode::Mute),
            None,
        ] {
            assert_eq!(Mode::from_selection(sel).selection(), sel);
        }
        assert_eq!(Mode::Off.word(), "off");
        assert_eq!(Mode::Suppress.word(), "suppress");
    }

    #[test]
    fn system_follows_the_desktop_and_is_dark_when_it_does_not_say() {
        assert_eq!(Theme::resolve(Appearance::System, None), Theme::Dark);
        assert_eq!(
            Theme::resolve(Appearance::System, Some(Theme::Light)),
            Theme::Light
        );
        assert_eq!(
            Theme::resolve(Appearance::Dark, Some(Theme::Light)),
            Theme::Dark
        );
        assert_eq!(Theme::resolve(Appearance::Light, None), Theme::Light);
    }

    #[test]
    fn pages_round_trip_through_their_words() {
        for p in Page::ALL {
            assert_eq!(Page::parse(p.word()), Some(p));
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.word()));
        }
        assert_eq!(Page::parse("bogus"), None);
        // Mic check is part of Live: its old word (window.json, a wire
        // from an older window or daemon, `--page`) opens Live
        assert_eq!(Page::parse("mic-check"), Some(Page::Live));
        assert_eq!(
            serde_json::from_str::<Page>("\"mic-check\"").unwrap(),
            Page::Live
        );
        // so does a window.json a build with the Speakers page saved on it
        #[cfg(not(feature = "speakers"))]
        assert_eq!(
            serde_json::from_str::<Page>("\"speakers\"").unwrap(),
            Page::Live
        );
    }

    /// The JSON is a contract for the window and for status-bar scripts:
    /// the shape is pinned here.
    #[test]
    fn the_snapshot_serializes_to_the_documented_shape() {
        let s = mock::state(mock::Scenario::OnInCall);
        let v: serde_json::Value = serde_json::to_value(&s).unwrap();
        assert_eq!(v["mode"], "suppress");
        assert_eq!(v["unmute_to"], "suppress");
        assert_eq!(v["engine"]["phase"], "ready");
        assert_eq!(v["engine"]["tier"], "quality");
        assert_eq!(v["sound"]["current"]["strength"], "maximum");
        assert_eq!(v["sound"]["current"]["model"], "quality");
        assert_eq!(v["mic"]["devices"][0]["present"], true);
        assert_eq!(v["apps"]["using"][0]["name"], "Firefox");
        assert_eq!(v["shortcuts"]["keys"][0]["action"], "toggle-mute");
        assert_eq!(v["shortcuts"]["keys"][0]["keys"]["state"], "text");
        assert_eq!(v["shortcuts"]["keys"][0]["keys"]["text"], "Ctrl+Alt+M");
        assert_eq!(v["prefs"]["appearance"], "system");
        assert_eq!(v["prefs"]["tray_icon"], "auto");
        assert_eq!(v["tray"], "registered");
        assert_eq!(v["pipewire"]["reachable"], true);
        assert_eq!(v["ui"]["page"], "live");
        assert_eq!(v["desktop"]["system_theme"], serde_json::Value::Null);
        assert_eq!(v["desktop"]["effective_theme"], "dark");
        assert_eq!(v["desktop"]["reduced_motion"], false);
        let back: AppState = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn every_scenario_round_trips_through_json() {
        for sc in mock::Scenario::ALL {
            let s = mock::state(sc);
            let json = serde_json::to_string(&s).unwrap();
            let back: AppState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s, "{sc:?}");
        }
    }

    #[test]
    fn readers_tolerate_missing_and_unknown_fields() {
        // An older daemon (missing fields) and a newer one (extra fields)
        // both read.
        let s: AppState =
            serde_json::from_str(r#"{"mode":"mute","future":{"x":1},"engine":{"phase":"ready"}}"#)
                .unwrap();
        assert_eq!(s.mode, Mode::Mute);
        assert_eq!(s.engine.phase, Phase::Ready);
        assert!(
            s.pipewire.reachable,
            "unknown reachability is not an outage"
        );
        assert_eq!(s.sound.current, Sound::default());
        let s: AppState = serde_json::from_str("{}").unwrap();
        assert_eq!(s, AppState::default());
    }

    #[test]
    fn vis_follows_mode_phase_and_tier() {
        use mock::Scenario as S;
        for (sc, want) in [
            (S::OnInCall, Vis::Filtering),
            (S::OnNoApp, Vis::Filtering),
            (S::Bypass, Vis::Raw),
            (S::Muted, Vis::Muted),
            (S::LightForNow, Vis::Filtering),
            (S::UnfilteredForNow, Vis::Raw),
            (S::Starting, Vis::Starting),
            (S::Off, Vis::Off),
            (S::NeedsAttention, Vis::Off),
        ] {
            assert_eq!(mock::state(sc).vis(), want, "{sc:?}");
        }
    }

    #[test]
    fn actions_are_lines_the_control_parser_accepts() {
        let all = [
            Action::SetMode(Mode::Suppress),
            Action::SetMode(Mode::Off),
            Action::ToggleMute,
            Action::SetUnmuteTo(Mode::Suppress),
            Action::SetUnmuteTo(Mode::Bypass),
            Action::ToggleBypass,
            Action::SelectMic(None),
            Action::SelectMic(Some("alsa_input.usb-RODE".into())),
            Action::SetStrength(Strength::Gentle),
            Action::SetModel(Model::Light),
            Action::SetDefaultMic(true),
            Action::SetAutostart(false),
            Action::SetNotifications(true),
            Action::SetAppearance(Appearance::Light),
            Action::SetTray(false),
            Action::SetTrayIcon(TrayIcon::Symbolic),
            Action::SetInference(Inference::Onnx),
            Action::ResetProfile("alsa_input.usb-RODE".into()),
            Action::SetupShortcuts,
            Action::Refresh,
            Action::Retry,
            Action::Quit,
            Action::UiPage(Page::Files),
            Action::UiFirstRun(Some(2)),
            Action::UiFirstRun(None),
            Action::UiBeamUsed,
            Action::UiSize(1024, 700),
            Action::UiFocused(false),
            Action::UiFocused(true),
            Action::UiFile(None),
            Action::UiFile(Some((3, 48_000))),
            Action::FilesAdd("/home/a/My  Recordings/talk \"1\".m4a".into()),
            Action::FilesRemove(3),
            Action::FilesPlay(3, 96_000),
            Action::FilesRetry(3),
            Action::FilesStrength(12.5, None),
            Action::FilesStrength(6.0, Some(3)),
            Action::FilesTrack(3, 1),
            Action::FilesCleanAnyway,
            Action::FilesCleanNext(3),
            Action::FilesSave(3),
            Action::FilesSaveTo(3, "/home/a/My  Music".into()),
            Action::FilesCancelSave(3),
            Action::FilesReplace(3, "/home/a/new place/talk.m4a".into()),
            Action::FilesCleanFirst(3),
            Action::FilesQuit(true),
            Action::FilesQuit(false),
            Action::FilesAskQuit,
            Action::FilesCancelQuit,
            Action::FilesClear,
            Action::FilesUndoClear,
            Action::FilesFolder("/home/a/Cleaned files".into()),
            Action::FilesNextToOriginal,
            Action::Answer(true),
            Action::Answer(false),
            #[cfg(feature = "speakers")]
            Action::SpeakersOn(true),
            #[cfg(feature = "speakers")]
            Action::SpeakersOn(false),
            #[cfg(feature = "speakers")]
            Action::SpeakersOutput(None),
            #[cfg(feature = "speakers")]
            Action::SpeakersOutput(Some("alsa_output.usb-dac".into())),
            #[cfg(feature = "speakers")]
            Action::SpeakersStrength(Strength::Medium),
            #[cfg(feature = "speakers")]
            Action::SpeakersHold(true),
            #[cfg(feature = "speakers")]
            Action::SpeakersHold(false),
            #[cfg(feature = "speakers")]
            Action::SpeakersRetry,
        ];
        for a in all {
            let line = a.request_line();
            let words: Vec<&str> = line.split_whitespace().collect();
            assert!(
                crate::control::parse_request(&words).is_ok(),
                "{a:?} -> {line:?}"
            );
        }
        assert_eq!(
            Action::SetStrength(Strength::Strong).request_line(),
            "config set attn_limit 24"
        );
        assert_eq!(
            Action::SetUnmuteTo(Mode::Bypass).request_line(),
            "unmute-to bypass"
        );
        assert!(crate::control::parse_request(&["unmute-to", "mute"]).is_err());
        // Custom is refused by the daemon, never sent as a preset.
        assert_eq!(
            Action::SetStrength(Strength::Custom).request_line(),
            "config set attn_limit custom"
        );
        assert!(crate::config_cli::parse_value(
            crate::config_cli::Key::AttnLimit,
            "custom",
            std::path::Path::new("/nonexistent")
        )
        .is_err());
        #[cfg(feature = "speakers")]
        assert_eq!(
            Action::SpeakersStrength(Strength::Strong).request_line(),
            "speakers strength 24"
        );
        #[cfg(feature = "speakers")]
        assert_eq!(
            Action::SpeakersOutput(None).request_line(),
            "speakers output default"
        );
        // A path goes over exactly, spaces and quotes and all.
        let path = "/home/a/My  Recordings/talk \"1\".m4a";
        let line = Action::FilesAdd(path.into()).request_line();
        assert_eq!(crate::control::files_add_path(&line).as_deref(), Some(path));
    }
}
