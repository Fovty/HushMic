//! The daemon's side of [`AppState`]: the facts the main loop owns, turned
//! into one snapshot ([`build`]), and the bookkeeping for where the chain
//! is in its life ([`Lifecycle`]). Pure: main.rs gathers, this decides.

use crate::appstate::{
    AppRef, AppState, AppsState, DaemonInfo, DesktopLook, DirectUse, Engine, MicDevice, MicProfile,
    MicState, Mode, Phase, PipeWireState, Prefs, ShortcutsState, Sound, SoundState, StartReason,
    Theme, TrayHost, UiState,
};
use crate::config::{ActiveProfile, Config};
use crate::controller::RunMode;
use crate::desktop::{Look, Scheme};
use crate::diagnostics::EngineTier;
use crate::pipewire::Source;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// A chain that has not registered its node this long after a spawn is no
/// longer "starting": it is down (the watchdog takes over).
pub const START_LIMIT: Duration = Duration::from_secs(8);

/// Why a chain was (re)spawned, as the main loop knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Launch,
    TurnOn,
    Settings,
    Recovery,
    /// The watchdog re-created a node that went away.
    Watchdog,
}

/// Where the chain is in its life: which spawn is current, whether its node
/// was seen up, and the last failure.
#[derive(Debug, Default)]
pub struct Lifecycle {
    /// A start announced before the spawn (a launch opens the window
    /// first): Starting until the spawn or its failure.
    pending: Option<(Start, Instant)>,
    /// The current spawn: its engine generation, why, and when.
    spawn: Option<(u64, Start, Instant)>,
    /// The generation whose node was seen up.
    ready: Option<u64>,
    /// The last failure, and the engine generation it happened under.
    error: Option<(String, u64)>,
    /// The generation the main loop last noted (see [`Lifecycle::note`]).
    noted: u64,
}

impl Lifecycle {
    /// Bookkeeping from now on, the engine at `generation` (no spawn of
    /// this run yet).
    pub fn since(generation: u64) -> Self {
        Lifecycle {
            noted: generation,
            ..Lifecycle::default()
        }
    }

    /// A start is about to begin (the window may already be watching).
    pub fn begin(&mut self, why: Start, now: Instant) {
        self.pending = Some((why, now));
    }

    /// A chain was spawned under `generation` (after a successful enable).
    /// An older failure is history now; one recorded for this very spawn
    /// (its node never came) stays.
    pub fn started(&mut self, generation: u64, why: Start, now: Instant) {
        self.pending = None;
        self.spawn = Some((generation, why, now));
        if self.error.as_ref().is_some_and(|(_, g)| *g != generation) {
            self.error = None;
        }
    }

    /// A start failed with `err` while the engine was at `generation`
    /// (shown in diagnostics, cleared when a chain comes up).
    pub fn failed(&mut self, err: &str, generation: u64) {
        self.pending = None;
        self.error = Some((err.to_string(), generation));
    }

    /// The main loop's turn: a chain generation it has not seen yet, with a
    /// chain running, is a spawn for `why`. True then (the caller watches
    /// for its node). `why` goes back to Settings either way: the next
    /// spawn's handler sets its own reason.
    pub fn note(&mut self, generation: u64, running: bool, why: &mut Start, now: Instant) -> bool {
        if generation == self.noted {
            return false;
        }
        self.noted = generation;
        let reason = std::mem::replace(why, Start::Settings);
        if running {
            self.started(generation, reason, now);
        }
        running
    }

    /// A probe saw the node `present` (or not) while `generation` runs.
    pub fn observe(&mut self, generation: u64, present: Option<bool>) {
        if present == Some(true) && self.spawn.is_some_and(|(g, _, _)| g == generation) {
            self.ready = Some(generation);
            self.error = None;
        }
    }

    /// Whether the node of `generation` has been seen up.
    pub fn is_ready(&self, generation: u64) -> bool {
        self.ready == Some(generation)
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_ref().map(|(e, _)| e.as_str())
    }

    /// The phase for the window. `present` is the last node probe.
    pub fn phase(
        &self,
        enabled: bool,
        running: bool,
        generation: u64,
        present: Option<bool>,
        now: Instant,
    ) -> (Phase, Option<StartReason>) {
        if !enabled {
            return (Phase::Off, None);
        }
        if let Some((why, at)) = self.pending {
            if now.duration_since(at) < START_LIMIT {
                return starting(why);
            }
        }
        let current = self.spawn.filter(|(g, _, _)| *g == generation);
        if running && self.ready == Some(generation) {
            return match present {
                // Seen up, now gone: the watchdog is on it.
                Some(false) => (Phase::Reconnecting, None),
                _ => (Phase::Ready, None),
            };
        }
        match current {
            Some((_, why, at)) if running && now.duration_since(at) < START_LIMIT => starting(why),
            _ => (Phase::Down, None),
        }
    }
}

fn starting(why: Start) -> (Phase, Option<StartReason>) {
    match why {
        Start::Watchdog => (Phase::Reconnecting, None),
        Start::Launch => (Phase::Starting, Some(StartReason::Launch)),
        Start::TurnOn => (Phase::Starting, Some(StartReason::TurnOn)),
        Start::Settings => (Phase::Starting, Some(StartReason::Settings)),
        Start::Recovery => (Phase::Starting, Some(StartReason::Recovery)),
    }
}

/// Everything a snapshot is made of, borrowed from the main loop.
pub struct Facts<'a> {
    pub cfg: &'a Config,
    pub headless: bool,
    pub flatpak: bool,
    pub mode: RunMode,
    pub unmute_to: RunMode,
    pub phase: (Phase, Option<StartReason>),
    pub generation: u64,
    pub tier: Option<EngineTier>,
    pub configured_light: bool,
    pub error: Option<&'a str>,
    /// The failure gate: the chain keeps restarting (until stable again).
    pub flapping: bool,
    /// The failure gate: times the chain ran again after a failure.
    pub recovered: u64,
    /// The running chain's engine-meter file.
    pub meter: Option<&'a str>,
    /// When the current use began on the meter's clock (`usemark`).
    pub use_began_ms: Option<u64>,
    /// The settings in effect and whose they are.
    pub profile: &'a ActiveProfile,
    /// The device the running chain listens to.
    pub active_mic: Option<&'a str>,
    pub fallback: bool,
    pub mics: &'a [Source],
    /// Descriptions of devices seen this run (an unplugged chosen mic keeps
    /// its name).
    pub seen: &'a HashMap<String, String>,
    /// Where each device stood in the last list it was in (an unplugged
    /// chosen mic keeps its place).
    pub slots: &'a HashMap<String, usize>,
    pub system_default: Option<&'a str>,
    pub rerouted: bool,
    /// The capture healer gave up restarting a stream fed by nothing.
    pub mic_stuck: bool,
    /// The microphone sends exact digital silence to an app (10 s).
    pub mic_silent: bool,
    pub using: &'a [AppRef],
    pub direct: &'a [DirectUse],
    pub can_set_default: bool,
    pub default_released: Option<&'a str>,
    pub shortcuts: &'a ShortcutsState,
    pub tray: TrayHost,
    pub pipewire_reachable: bool,
    pub live_capture: Option<bool>,
    pub ui: &'a UiState,
    /// What the desktop said about its look (the settings portal).
    pub desktop: Look,
}

fn audible(m: RunMode) -> Mode {
    match m {
        RunMode::Bypass => Mode::Bypass,
        _ => Mode::Suppress,
    }
}

/// The names microphones are listed by: each device's description, and
/// where several share one, the first by node name keeps it and the others
/// are numbered in that order ("RODE NT-USB (2)"), so a device keeps its
/// name while the same set is around. `devices` are (node name,
/// description) pairs, a node listed again keeping its first description;
/// the result maps node names to their names.
pub fn display_names<'a>(
    devices: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> HashMap<String, String> {
    let mut listed = HashSet::new();
    let mut by_description: HashMap<&str, Vec<&str>> = HashMap::new();
    for (name, description) in devices {
        if listed.insert(name) {
            by_description.entry(description).or_default().push(name);
        }
    }
    let mut names = HashMap::new();
    for (description, mut same) in by_description {
        same.sort_unstable();
        for (i, name) in same.into_iter().enumerate() {
            let shown = if i == 0 {
                description.to_string()
            } else {
                crate::tr!("name-repeat", name = description, n = (i + 1).to_string())
            };
            names.insert(name.to_string(), shown);
        }
    }
    names
}

/// A device's description: as PipeWire lists it now, else as last seen
/// this run, else its node name.
fn describe(mics: &[Source], seen: &HashMap<String, String>, name: &str) -> String {
    crate::pipewire::description_of(mics, name)
        .map(str::to_string)
        .or_else(|| seen.get(name).cloned())
        .unwrap_or_else(|| name.to_string())
}

/// The name of every microphone the user can meet (see [`display_names`]):
/// the ones plugged in, the chosen one, the ones with saved settings and
/// the ones seen this run, numbered together, so a twin unplugged mid-run
/// does not renumber the others. The snapshot and the daemon's
/// notifications name devices from here.
pub fn mic_names(
    cfg: &Config,
    mics: &[Source],
    seen: &HashMap<String, String>,
) -> HashMap<String, String> {
    let others: Vec<(&str, String)> = cfg
        .mic
        .iter()
        .chain(cfg.mic_prefs.keys())
        .map(|name| (name.as_str(), describe(mics, seen, name)))
        .collect();
    display_names(
        mics.iter()
            .map(|m| (m.name.as_str(), m.description.as_str()))
            .chain(others.iter().map(|(n, d)| (*n, d.as_str())))
            .chain(seen.iter().map(|(n, d)| (n.as_str(), d.as_str()))),
    )
}

pub fn build(f: &Facts) -> AppState {
    let cfg = f.cfg;
    let describe = |name: &str| describe(f.mics, f.seen, name);
    let mut devices: Vec<MicDevice> = f
        .mics
        .iter()
        .map(|m| MicDevice {
            name: m.name.clone(),
            description: m.description.clone(),
            present: true,
            saved_settings: cfg.mic_prefs.contains_key(&m.name),
        })
        .collect();
    if let Some(sel) = cfg.mic.as_deref() {
        if !devices.iter().any(|d| d.name == sel) {
            let at = f
                .slots
                .get(sel)
                .map_or(devices.len(), |&i| i.min(devices.len()));
            devices.insert(
                at,
                MicDevice {
                    name: sel.to_string(),
                    description: describe(sel),
                    present: false,
                    saved_settings: cfg.mic_prefs.contains_key(sel),
                },
            );
        }
    }
    // Same-name microphones are told apart wherever they are listed.
    let shown = mic_names(cfg, f.mics, f.seen);
    for d in &mut devices {
        if let Some(name) = shown.get(&d.name) {
            d.description = name.clone();
        }
    }
    let name_of = |device: &str| {
        shown
            .get(device)
            .cloned()
            .unwrap_or_else(|| describe(device))
    };
    let enabled = cfg.enabled;
    AppState {
        daemon: DaemonInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            headless: f.headless,
            flatpak: f.flatpak,
        },
        mode: Mode::from_selection(enabled.then_some(f.mode)),
        unmute_to: audible(f.unmute_to),
        engine: Engine {
            phase: f.phase.0,
            start_reason: f.phase.1,
            tier: f.tier.filter(|_| f.phase.0 == Phase::Ready),
            configured_light: f.configured_light,
            generation: f.generation,
            error: f.error.map(str::to_string),
            flapping: f.flapping,
            recovered: f.recovered,
            meter: f.meter.map(str::to_string),
            use_began_ms: f.use_began_ms.filter(|_| enabled && !f.using.is_empty()),
        },
        mic: MicState {
            selected: cfg.mic.clone(),
            fallback: f.fallback,
            active: f.active_mic.map(str::to_string),
            system_default: f.system_default.map(str::to_string),
            devices,
            rerouted: f.rerouted,
            stuck: f.mic_stuck,
            silent: f.mic_silent,
        },
        sound: SoundState {
            current: Sound::new(&f.profile.model, f.profile.attn_limit),
            owner: f.profile.device.clone(),
            saved: f.profile.saved,
            defaults: Sound::new(&cfg.model, cfg.attn_limit),
            profiles: cfg
                .mic_prefs
                .iter()
                .map(|(device, p)| MicProfile {
                    device: device.clone(),
                    description: name_of(device),
                    sound: Sound::new(&p.model, p.attn_limit),
                })
                .collect(),
        },
        apps: AppsState {
            using: if enabled {
                f.using.to_vec()
            } else {
                Vec::new()
            },
            direct: if enabled {
                f.direct.to_vec()
            } else {
                Vec::new()
            },
            // The setting: a default the user moved away stays theirs only
            // until a deliberate start, which `default_released` reports.
            set_default: cfg.set_default,
            can_set_default: f.can_set_default,
            default_released: f.default_released.map(name_of),
        },
        shortcuts: ShortcutsState {
            set_up: cfg.shortcuts_setup,
            ..f.shortcuts.clone()
        },
        prefs: Prefs {
            autostart: cfg.autostart,
            notifications: cfg.notifications,
            appearance: cfg.appearance,
            tray: cfg.tray,
            tray_icon: cfg.tray_icon,
            inference: cfg.inference,
            voice_hold: cfg.voice_hold,
            hidden_notices: cfg.hidden_notices.clone(),
        },
        tray: f.tray,
        pipewire: PipeWireState {
            reachable: f.pipewire_reachable,
            live_capture: f.live_capture,
        },
        ui: f.ui.clone(),
        desktop: desktop_look(cfg, f.desktop),
        // The daemon's file jobs fill it in (`files::jobs`).
        files: Default::default(),
        // and its pending question (the tray's Quit or Off)
        ask: None,
        // the speakers cleaner fills it in (`Speakers::app_state`)
        #[cfg(feature = "speakers")]
        speakers: Default::default(),
    }
}

/// The desktop's look with the Appearance setting applied (the snapshot's
/// `desktop`, and what a new window paints its first frame in).
pub fn desktop_look(cfg: &Config, look: Look) -> DesktopLook {
    let system_theme = match look.scheme {
        Some(Scheme::Dark) => Some(Theme::Dark),
        Some(Scheme::Light) => Some(Theme::Light),
        Some(Scheme::NoPreference) | None => None,
    };
    DesktopLook {
        system_theme,
        effective_theme: Theme::resolve(cfg.appearance, system_theme),
        reduced_motion: look.reduced_motion.unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::{Keys, ShortcutsBackend, Strength};

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_launch_is_starting_until_the_node_is_seen_then_ready() {
        let now = t0();
        let mut l = Lifecycle::default();
        assert_eq!(l.phase(true, false, 0, None, now).0, Phase::Down);
        l.started(4, Start::Launch, now);
        assert_eq!(
            l.phase(true, true, 4, Some(false), now),
            (Phase::Starting, Some(StartReason::Launch))
        );
        l.observe(4, Some(true));
        assert_eq!(
            l.phase(true, true, 4, Some(true), now),
            (Phase::Ready, None)
        );
        // The node goes away under a running child: reconnecting.
        assert_eq!(
            l.phase(true, true, 4, Some(false), now).0,
            Phase::Reconnecting
        );
        // Off wins over everything.
        assert_eq!(l.phase(false, true, 4, Some(true), now).0, Phase::Off);
    }

    #[test]
    fn a_start_that_never_comes_up_is_down_after_the_limit() {
        let now = t0();
        let mut l = Lifecycle::default();
        l.started(2, Start::TurnOn, now);
        assert_eq!(l.phase(true, true, 2, None, now).0, Phase::Starting);
        let later = now + START_LIMIT + Duration::from_millis(1);
        assert_eq!(l.phase(true, true, 2, None, later).0, Phase::Down);
        // A dead child is down at once.
        assert_eq!(l.phase(true, false, 2, None, now).0, Phase::Down);
    }

    #[test]
    fn a_start_announced_before_the_spawn_is_starting_already() {
        let now = t0();
        let mut l = Lifecycle::default();
        l.begin(Start::Launch, now);
        assert_eq!(
            l.phase(true, false, 0, None, now),
            (Phase::Starting, Some(StartReason::Launch))
        );
        l.started(3, Start::Launch, now);
        assert_eq!(l.phase(true, true, 3, Some(false), now).0, Phase::Starting);
        // A failed start ends it at once.
        let mut l = Lifecycle::default();
        l.begin(Start::Launch, now);
        l.failed("no plugin", 0);
        assert_eq!(l.phase(true, false, 0, None, now).0, Phase::Down);
        // Never forever.
        let mut l = Lifecycle::default();
        l.begin(Start::Launch, now);
        let later = now + START_LIMIT + Duration::from_millis(1);
        assert_eq!(l.phase(true, false, 0, None, later).0, Phase::Down);
    }

    #[test]
    fn a_watchdog_respawn_reads_as_reconnecting() {
        let now = t0();
        let mut l = Lifecycle::default();
        l.started(1, Start::Launch, now);
        l.observe(1, Some(true));
        l.started(3, Start::Watchdog, now);
        assert_eq!(l.phase(true, true, 3, None, now).0, Phase::Reconnecting);
        l.observe(3, Some(true));
        assert_eq!(l.phase(true, true, 3, Some(true), now).0, Phase::Ready);
    }

    #[test]
    fn a_probe_of_an_old_generation_proves_nothing() {
        let now = t0();
        let mut l = Lifecycle::default();
        l.started(5, Start::Settings, now);
        l.observe(4, Some(true));
        assert!(!l.is_ready(5));
        assert_eq!(
            l.phase(true, true, 5, Some(true), now),
            (Phase::Starting, Some(StartReason::Settings))
        );
    }

    #[test]
    fn errors_last_until_a_chain_comes_up() {
        let now = t0();
        let mut l = Lifecycle::default();
        l.failed("model file not found", 0);
        assert_eq!(l.error(), Some("model file not found"));
        l.started(1, Start::Launch, now);
        assert_eq!(l.error(), None, "a new start clears it");
        l.failed("node never appeared", 1);
        l.observe(1, Some(true));
        assert_eq!(l.error(), None);
    }

    /// The review's nit: the watchdog respawns (the generation moves), its
    /// node never comes, and the loop head notes the spawn only after: the
    /// failure of that very spawn must stay.
    #[test]
    fn a_respawn_whose_node_never_came_keeps_its_error() {
        let now = t0();
        let mut l = Lifecycle::default();
        let mut why = Start::Launch;
        assert!(l.note(1, true, &mut why, now));
        l.observe(1, Some(true));
        // the watchdog's respawn (generation 2), then the 2 s wait fails
        why = Start::Watchdog;
        l.failed("the HushMic microphone did not appear in PipeWire", 2);
        assert!(l.note(2, true, &mut why, now));
        assert_eq!(
            l.error(),
            Some("the HushMic microphone did not appear in PipeWire")
        );
        assert_eq!(l.phase(true, true, 2, None, now).0, Phase::Reconnecting);
    }

    /// The loop head's bookkeeping: a new generation with a chain running
    /// starts it for the reason the handler set, the reason goes back to
    /// Settings, an unchanged generation does nothing, a stop is no start.
    #[test]
    fn the_loop_head_notes_each_spawn_once_with_its_reason() {
        let now = t0();
        let mut l = Lifecycle::default();
        let mut why = Start::TurnOn;
        assert!(l.note(4, true, &mut why, now));
        assert_eq!(why, Start::Settings, "the reason is spent");
        assert_eq!(
            l.phase(true, true, 4, None, now),
            (Phase::Starting, Some(StartReason::TurnOn))
        );
        assert!(!l.note(4, true, &mut why, now), "same generation: nothing");
        // A settings restart: the default reason.
        assert!(l.note(5, true, &mut why, now));
        assert_eq!(
            l.phase(true, true, 5, None, now).1,
            Some(StartReason::Settings)
        );
        // A stop moves the generation too, but is no start.
        why = Start::Recovery;
        assert!(!l.note(6, false, &mut why, now));
        assert_eq!(why, Start::Settings);
        // A node seen up for an older generation proves nothing.
        assert!(l.note(7, true, &mut why, now));
        l.observe(5, Some(true));
        assert_eq!(l.phase(true, true, 7, Some(true), now).0, Phase::Starting);
        l.observe(7, Some(true));
        assert_eq!(l.phase(true, true, 7, Some(true), now).0, Phase::Ready);
    }

    fn facts<'a>(
        cfg: &'a Config,
        profile: &'a ActiveProfile,
        mics: &'a [Source],
        seen: &'a HashMap<String, String>,
        shortcuts: &'a ShortcutsState,
        ui: &'a UiState,
    ) -> Facts<'a> {
        Facts {
            cfg,
            headless: false,
            flatpak: false,
            mode: RunMode::Mute,
            unmute_to: RunMode::Bypass,
            phase: (Phase::Ready, None),
            generation: 9,
            tier: Some(EngineTier::Light),
            configured_light: false,
            error: None,
            flapping: false,
            recovered: 0,
            meter: None,
            use_began_ms: None,
            profile,
            active_mic: Some("rode"),
            fallback: false,
            mics,
            seen,
            slots: Box::leak(Box::default()),
            system_default: Some("builtin"),
            rerouted: false,
            mic_stuck: false,
            mic_silent: false,
            using: &[],
            direct: &[],
            can_set_default: true,
            default_released: None,
            shortcuts,
            tray: TrayHost::Registered,
            pipewire_reachable: true,
            live_capture: Some(true),
            ui,
            desktop: Look::default(),
        }
    }

    #[test]
    fn the_snapshot_reflects_config_and_loop_state() {
        let mut cfg = Config {
            mic: Some("rode".into()),
            attn_limit: 24.0,
            ..Config::default()
        };
        cfg.set_attn_for(Some("rode"), 12.0);
        cfg.shortcuts_setup = true;
        let profile = cfg.profile_for(Some("rode"));
        let mics = vec![
            Source {
                name: "builtin".into(),
                description: "Built-in Audio".into(),
            },
            Source {
                name: "rode".into(),
                description: "RODE NT-USB".into(),
            },
        ];
        let seen = HashMap::new();
        let sc = ShortcutsState {
            available: true,
            backend: ShortcutsBackend::Portal,
            set_up: false,
            keys: ShortcutsState::unreported(),
        };
        let ui = UiState::default();
        let s = build(&facts(&cfg, &profile, &mics, &seen, &sc, &ui));
        assert_eq!(s.mode, Mode::Mute);
        assert_eq!(s.unmute_to, Mode::Bypass);
        assert_eq!(s.engine.tier, Some(EngineTier::Light));
        assert_eq!(s.sound.current.strength, Strength::Medium);
        assert_eq!(s.sound.defaults.strength, Strength::Strong);
        assert!(s.sound.saved);
        assert_eq!(s.sound.owner.as_deref(), Some("rode"));
        assert_eq!(s.sound.profiles.len(), 1);
        assert_eq!(s.sound.profiles[0].description, "RODE NT-USB");
        assert!(s.mic.devices[1].saved_settings);
        assert!(s.mic.devices.iter().all(|d| d.present));
        assert!(s.shortcuts.set_up, "set_up comes from the config");
        assert_eq!(s.shortcuts.keys[0].keys, Keys::NotReported);
        assert_eq!(s.daemon.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn an_unplugged_choice_keeps_its_place_and_its_remembered_name() {
        let cfg = Config {
            mic: Some("rode".into()),
            ..Config::default()
        };
        let profile = cfg.profile_for(Some("builtin"));
        let mics = vec![
            Source {
                name: "builtin".into(),
                description: "Built-in Audio".into(),
            },
            Source {
                name: "headset".into(),
                description: "USB Headset".into(),
            },
        ];
        let mut seen = HashMap::new();
        seen.insert("rode".to_string(), "RODE NT-USB".to_string());
        let sc = ShortcutsState::default();
        let ui = UiState::default();
        let mut f = facts(&cfg, &profile, &mics, &seen, &sc, &ui);
        f.fallback = true;
        // Never seen in a list this run: last.
        let s = build(&f);
        let last = s.mic.devices.last().unwrap();
        assert_eq!(
            (last.name.as_str(), last.description.as_str(), last.present),
            ("rode", "RODE NT-USB", false)
        );
        assert!(s.mic.fallback);
        // It stood second: it stays second.
        let slots = HashMap::from([
            ("builtin".to_string(), 0),
            ("rode".to_string(), 1),
            ("headset".to_string(), 2),
        ]);
        f.slots = &slots;
        let names: Vec<String> = build(&f).mic.devices.into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["builtin", "rode", "headset"]);
        // A place past the end of a shorter list: last.
        let far = HashMap::from([("rode".to_string(), 7)]);
        f.slots = &far;
        let names: Vec<String> = build(&f).mic.devices.into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["builtin", "headset", "rode"]);
    }

    #[test]
    fn the_desktop_look_resolves_with_the_appearance_setting() {
        let mut cfg = Config::default();
        let profile = cfg.profile_for(None);
        let seen = HashMap::new();
        let sc = ShortcutsState::default();
        let ui = UiState::default();
        // Nothing said: dark, full motion.
        let s = build(&facts(&cfg, &profile, &[], &seen, &sc, &ui));
        assert_eq!(s.desktop, DesktopLook::default());
        let mut f = facts(&cfg, &profile, &[], &seen, &sc, &ui);
        f.desktop = Look {
            scheme: Some(Scheme::Light),
            reduced_motion: Some(true),
        };
        let d = build(&f).desktop;
        assert_eq!(d.system_theme, Some(Theme::Light));
        assert_eq!(d.effective_theme, Theme::Light);
        assert!(d.reduced_motion);
        // No preference reads as not said.
        f.desktop.scheme = Some(Scheme::NoPreference);
        assert_eq!(build(&f).desktop.system_theme, None);
        assert_eq!(build(&f).desktop.effective_theme, Theme::Dark);
        // The setting wins over the desktop.
        cfg.appearance = crate::config::Appearance::Dark;
        let mut f = facts(&cfg, &profile, &[], &seen, &sc, &ui);
        f.desktop.scheme = Some(Scheme::Light);
        assert_eq!(build(&f).desktop.effective_theme, Theme::Dark);
    }

    #[test]
    fn a_default_moved_away_keeps_the_setting_and_names_the_device() {
        let cfg = Config {
            set_default: true,
            ..Config::default()
        };
        let profile = cfg.profile_for(None);
        let mics = vec![Source {
            name: "builtin".into(),
            description: "Built-in Audio".into(),
        }];
        let seen = HashMap::new();
        let sc = ShortcutsState::default();
        let ui = UiState::default();
        let mut f = facts(&cfg, &profile, &mics, &seen, &sc, &ui);
        assert_eq!(build(&f).apps.default_released, None);
        f.default_released = Some("builtin");
        let s = build(&f);
        // The next start takes it back, so the switch stays on.
        assert!(s.apps.set_default);
        assert_eq!(s.apps.default_released.as_deref(), Some("Built-in Audio"));
    }

    #[test]
    fn off_shows_no_apps_and_no_tier() {
        let cfg = Config {
            enabled: false,
            ..Config::default()
        };
        let profile = cfg.profile_for(None);
        let seen = HashMap::new();
        let sc = ShortcutsState::default();
        let ui = UiState::default();
        let using = [AppRef {
            id: "firefox".into(),
            name: Some("Firefox".into()),
        }];
        let mut f = facts(&cfg, &profile, &[], &seen, &sc, &ui);
        f.using = &using;
        f.use_began_ms = Some(5_000);
        f.phase = (Phase::Off, None);
        let s = build(&f);
        assert_eq!(s.mode, Mode::Off);
        assert!(s.apps.using.is_empty());
        assert_eq!(s.engine.tier, None);
        assert_eq!(s.engine.use_began_ms, None);
    }

    #[test]
    fn the_use_mark_comes_with_the_apps_using_hushmic() {
        let cfg = Config::default();
        let profile = cfg.profile_for(None);
        let seen = HashMap::new();
        let sc = ShortcutsState::default();
        let ui = UiState::default();
        let using = [AppRef {
            id: "firefox".into(),
            name: Some("Firefox".into()),
        }];
        let mut f = facts(&cfg, &profile, &[], &seen, &sc, &ui);
        f.use_began_ms = Some(5_000);
        assert_eq!(build(&f).engine.use_began_ms, None, "no app, no mark");
        f.using = &using;
        let s = build(&f);
        assert!(s.in_use());
        assert_eq!(s.engine.use_began_ms, Some(5_000));
    }

    #[test]
    fn same_name_microphones_are_numbered_by_node_name() {
        crate::i18n::pin_english();
        let names = display_names([
            ("usb-rode-2", "RODE NT-USB"),
            ("builtin", "Built-in Audio"),
            ("usb-rode-1", "RODE NT-USB"),
            ("usb-rode-1", "RODE NT-USB"),
            ("usb-rode-3", "RODE NT-USB"),
        ]);
        assert_eq!(names.len(), 4, "one name per device");
        assert_eq!(names["builtin"], "Built-in Audio");
        assert_eq!(names["usb-rode-1"], "RODE NT-USB");
        assert_eq!(names["usb-rode-2"], "RODE NT-USB (2)");
        assert_eq!(names["usb-rode-3"], "RODE NT-USB (3)");
        // The order they are listed in does not matter.
        let again = display_names([
            ("usb-rode-3", "RODE NT-USB"),
            ("usb-rode-1", "RODE NT-USB"),
            ("usb-rode-2", "RODE NT-USB"),
        ]);
        assert_eq!(again["usb-rode-2"], "RODE NT-USB (2)");
        // A node listed again keeps its first description.
        let renamed = display_names([("usb-rode-1", "RODE NT-USB"), ("usb-rode-1", "Podcast")]);
        assert_eq!(renamed["usb-rode-1"], "RODE NT-USB");
    }

    #[test]
    fn an_unplugged_twin_does_not_renumber_the_others() {
        crate::i18n::pin_english();
        let cfg = Config::default();
        let profile = cfg.profile_for(None);
        let twin = |name: &str| Source {
            name: name.into(),
            description: "RODE NT-USB".into(),
        };
        let all = vec![twin("usb-rode-a"), twin("usb-rode-b"), twin("usb-rode-c")];
        let seen: HashMap<String, String> = all
            .iter()
            .map(|m| (m.name.clone(), m.description.clone()))
            .collect();
        let sc = ShortcutsState::default();
        let ui = UiState::default();
        let shown = |mics: &[Source]| -> Vec<String> {
            build(&facts(&cfg, &profile, mics, &seen, &sc, &ui))
                .mic
                .devices
                .into_iter()
                .map(|d| d.description)
                .collect()
        };
        assert_eq!(
            shown(&all),
            ["RODE NT-USB", "RODE NT-USB (2)", "RODE NT-USB (3)"]
        );
        // The first one unplugged, no settings saved for it: the others
        // keep their numbers for the rest of the run.
        assert_eq!(shown(&all[1..]), ["RODE NT-USB (2)", "RODE NT-USB (3)"]);
        assert_eq!(
            mic_names(&cfg, &all[1..], &seen)["usb-rode-a"],
            "RODE NT-USB",
            "the daemon's notifications name devices the same way"
        );
    }

    #[test]
    fn same_name_microphones_are_told_apart_in_the_snapshot() {
        crate::i18n::pin_english();
        let mut cfg = Config {
            mic: Some("usb-rode-b".into()),
            ..Config::default()
        };
        cfg.set_attn_for(Some("usb-rode-b"), 12.0);
        // Settings saved for a third one, not plugged in now.
        cfg.mic_prefs.insert(
            "usb-rode-c".into(),
            crate::config::MicPrefs {
                model: cfg.model.clone(),
                attn_limit: 18.0,
            },
        );
        let profile = cfg.profile_for(Some("usb-rode-b"));
        let mics = vec![
            Source {
                name: "usb-rode-b".into(),
                description: "RODE NT-USB".into(),
            },
            Source {
                name: "usb-rode-a".into(),
                description: "RODE NT-USB".into(),
            },
        ];
        let seen = HashMap::from([("usb-rode-c".to_string(), "RODE NT-USB".to_string())]);
        let sc = ShortcutsState::default();
        let ui = UiState::default();
        let mut f = facts(&cfg, &profile, &mics, &seen, &sc, &ui);
        f.default_released = Some("usb-rode-a");
        let s = build(&f);
        let devices: Vec<(&str, &str)> = s
            .mic
            .devices
            .iter()
            .map(|d| (d.name.as_str(), d.description.as_str()))
            .collect();
        assert_eq!(
            devices,
            [
                ("usb-rode-b", "RODE NT-USB (2)"),
                ("usb-rode-a", "RODE NT-USB")
            ],
            "the list keeps its order; only the names change"
        );
        let mut profiles: Vec<(&str, &str)> = s
            .sound
            .profiles
            .iter()
            .map(|p| (p.device.as_str(), p.description.as_str()))
            .collect();
        profiles.sort_unstable();
        assert_eq!(
            profiles,
            [
                ("usb-rode-b", "RODE NT-USB (2)"),
                ("usb-rode-c", "RODE NT-USB (3)")
            ]
        );
        assert_eq!(s.apps.default_released.as_deref(), Some("RODE NT-USB"));
    }
}
