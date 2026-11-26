// ksni menu literals already set every field; the trailing `..Default::default()`
// is kept intentionally as forward-compat across ksni versions.
#![allow(clippy::needless_update)]

use crate::appstate::{AppState, JobState, Mode, Phase};
use crate::config::{ActiveProfile, Config, TrayIcon};
use crate::controller::RunMode;
use crate::diagnostics::EngineTier;
use crate::pipewire::Source;
use crate::tr;
use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, Tray};
use std::sync::mpsc::Sender;

#[derive(Debug)]
pub enum TrayCmd {
    /// `Some(mode)` = a chain-alive state (suppress / bypass / mute);
    /// `None` = Off, the existing tear-the-chain-down disable path.
    SetMode(Option<RunMode>),
    SelectMic(Option<String>),
    SetAttn(f32),
    /// "Open HushMic" (and a left click on the icon): the window.
    Open,
    /// "Show" on "3 recordings cleaned.": the window, on Files.
    OpenFiles,
    Quit,
    /// "Quit HushMic" in the menu: quits, unless files are cleaning or an
    /// app uses HushMic, when the window asks first (a tray menu has no
    /// dialogs).
    AskQuit,
    /// "Turn off HushMic" in the menu: turns off, unless an app uses
    /// HushMic, when the window asks first.
    AskOff,
}

/// The tray's status line: shown on top of the menu only when something
/// is unusual (the mode radio says the normal states).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayNotice {
    /// Should run and does not (or PipeWire is unreachable).
    Attention,
    /// The watchdog is bringing the microphone back.
    Reconnecting,
    /// The chosen microphone (its display name) is not plugged in, and
    /// none stands in: apps hear nothing.
    Unplugged(String),
    /// The chosen microphone is not plugged in and another (the second
    /// name) stands in: apps hear it, cleaned.
    StandIn(String, String),
    /// The microphone (its display name) sends no sound to an app.
    Silent(String),
    /// The engine fell back to passthrough.
    Unfiltered,
    /// The engine fell back to the light model.
    Light,
}

/// The status line for a snapshot, if any; the most pressing one wins.
pub fn notice_for(s: &AppState) -> Option<TrayNotice> {
    if s.mode == Mode::Off {
        return None;
    }
    if !s.pipewire.reachable || s.engine.phase == Phase::Down {
        return Some(TrayNotice::Attention);
    }
    if s.engine.phase == Phase::Reconnecting {
        return Some(TrayNotice::Reconnecting);
    }
    if let Some(d) = s
        .mic
        .devices
        .iter()
        .find(|d| !d.present && s.mic.selected.as_deref() == Some(d.name.as_str()))
    {
        // a stand-in counts only when it is another microphone that is
        // there and has a name (as Live's, UX panel round 2, A15)
        let standin = s
            .mic
            .active
            .as_deref()
            .filter(|a| s.mic.fallback && *a != d.name)
            .and_then(|a| s.mic.devices.iter().find(|x| x.name == a && x.present))
            .map(|x| x.description.clone())
            .filter(|x| !x.trim().is_empty());
        return Some(match standin {
            Some(other) => TrayNotice::StandIn(d.description.clone(), other),
            None => TrayNotice::Unplugged(d.description.clone()),
        });
    }
    if s.mic.silent {
        let mic = s
            .mic
            .active
            .as_deref()
            .and_then(|m| s.description_of(m))
            .unwrap_or_default()
            .to_string();
        return Some(TrayNotice::Silent(mic));
    }
    if s.mode != Mode::Suppress || s.engine.phase != Phase::Ready {
        return None;
    }
    match s.engine.tier {
        Some(EngineTier::Passthrough) => Some(TrayNotice::Unfiltered),
        Some(EngineTier::Light) if !s.engine.configured_light => Some(TrayNotice::Light),
        _ => None,
    }
}

/// What the tray shows from the window's snapshot: the status line and the
/// names the Microphone submenu needs beyond the device list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayExtras {
    pub notice: Option<TrayNotice>,
    /// What "System default" stands for (display name).
    pub default_name: Option<String>,
    /// The chosen microphone's display name while it is not plugged in.
    pub absent_name: Option<String>,
    /// Its place among the microphones (where it was last seen).
    pub absent_slot: Option<usize>,
    /// The name each microphone is listed by (node name, name): the
    /// snapshot's, so same-name microphones are numbered as in the window.
    pub mic_names: Vec<(String, String)>,
    /// Files cleaning or waiting to, on the Files page.
    pub cleaning: usize,
    /// The apps using HushMic, named for a sentence ("Zoom", "Zoom and
    /// Teams", "3 apps"); None: none.
    pub apps: Option<String>,
    /// The microphone HushMic cleans, by its display name.
    pub mic: Option<String>,
    /// The desktop's light or dark style as last seen: a switch makes the
    /// host draw the icon again in the panel's new colours.
    pub system_theme: Option<crate::appstate::Theme>,
}

impl TrayExtras {
    pub fn of(s: &AppState) -> Self {
        let name_of = |n: &str| {
            s.mic
                .devices
                .iter()
                .find(|d| d.name == n)
                .map(|d| d.description.clone())
        };
        let absent = s
            .mic
            .devices
            .iter()
            .position(|d| !d.present && s.mic.selected.as_deref() == Some(d.name.as_str()));
        TrayExtras {
            notice: notice_for(s),
            default_name: s.mic.system_default.as_deref().and_then(name_of),
            absent_name: absent.map(|i| s.mic.devices[i].description.clone()),
            absent_slot: absent,
            mic_names: s
                .mic
                .devices
                .iter()
                .map(|d| (d.name.clone(), d.description.clone()))
                .collect(),
            cleaning: s
                .files
                .jobs
                .iter()
                .filter(|j| matches!(j.state, JobState::Waiting | JobState::Cleaning))
                .count(),
            apps: (!s.apps.using.is_empty()).then(|| crate::notify::name_apps(s.apps.using.iter())),
            mic: s
                .mic
                .active
                .as_deref()
                .or(s.mic.selected.as_deref())
                .and_then(|m| s.description_of(m))
                .map(str::to_string),
            system_theme: s.desktop.system_theme,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayStatus {
    Off,
    Active,
    /// Chain alive, filter bypassed (raw voice) — gray mic icon.
    Bypass,
    /// Chain alive, output muted — red slashed-mic icon (privacy state,
    /// must be visible at a glance).
    Mute,
    Error,
}

impl TrayStatus {
    pub fn icon_name(&self) -> &'static str {
        // The shipped icon set (packaging/tray/hicolor/**/status/): SNI hosts
        // resolve these from the system theme, or from icon_theme_path() when
        // HUSHMIC_TRAY_THEME_DIR points at a bundled copy (AppImage).
        match self {
            TrayStatus::Active => "hushmic-tray",
            TrayStatus::Off => "hushmic-tray-off",
            TrayStatus::Bypass => "hushmic-tray-bypass",
            TrayStatus::Mute => "hushmic-tray-mute",
            TrayStatus::Error => "hushmic-tray-error",
        }
    }

    /// The name handed to the SNI host for one icon style. The symbolic set
    /// carries the `-symbolic` suffix GTK reads to recolor the drawing, under
    /// a stem of its own (`hushmic-mono`): Plasma 6 draws `<name>-symbolic`
    /// in place of `<name>` whenever that file exists, so a shared stem would
    /// make the colored set unreachable there.
    pub fn themed_icon_name(&self, style: IconStyle) -> String {
        match style {
            IconStyle::Color => self.icon_name().to_string(),
            IconStyle::Symbolic => {
                let state = self
                    .icon_name()
                    .strip_prefix("hushmic-tray")
                    .expect("tray icon names start with 'hushmic-tray'");
                format!("hushmic-mono{state}-symbolic")
            }
        }
    }
}

/// The icon set actually in use, once `tray_icon = auto` has been resolved
/// against the desktop.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum IconStyle {
    #[default]
    Color,
    Symbolic,
}

/// Resolve the configured preference for one desktop. `desktop` is
/// XDG_CURRENT_DESKTOP as the session sets it: a colon-separated list in
/// whatever case the session chose, or None when it is unset.
///
/// KDE and GNOME are the two desktops that recolor symbolic icons for the
/// panel they are drawn on (KDE through the current-color-scheme stylesheet
/// the files carry, GTK through the `-symbolic` name), so `auto` asks for
/// the monochrome set there and keeps the coloured ladder everywhere else,
/// where a flat one-color glyph could come out invisible on its own panel.
/// The line is drawn by desktop name, not by capability: GNOME-based sessions
/// (Budgie:GNOME, ubuntu:GNOME) are in, other GTK trays that would recolor
/// (Cinnamon, XFCE, MATE) are deliberately left on the known-good colored set.
pub fn resolve_icon_style(pref: TrayIcon, desktop: Option<&str>) -> IconStyle {
    match pref {
        TrayIcon::Color => IconStyle::Color,
        TrayIcon::Symbolic => IconStyle::Symbolic,
        TrayIcon::Auto => {
            let themed = desktop.is_some_and(|d| {
                d.split(':')
                    .any(|e| e.eq_ignore_ascii_case("KDE") || e.eq_ignore_ascii_case("GNOME"))
            });
            if themed {
                IconStyle::Symbolic
            } else {
                IconStyle::Color
            }
        }
    }
}

/// The style for a config in THIS session (the env read `auto` needs).
pub fn icon_style_for(cfg: &Config) -> IconStyle {
    resolve_icon_style(
        cfg.tray_icon,
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
    )
}

/// The name handed to the SNI host, for a status, a style and a sandbox.
///
/// Inside a Flatpak only app-ID-prefixed icon names are exported to the host
/// theme (~/.local/share/flatpak/exports/share/icons on the host
/// XDG_DATA_DIRS), so the host can resolve
/// `<app-id>-tray[-state]` and `<app-id>-mono[-state]-symbolic` but never the bare `hushmic-tray`
/// set — the manifest installs both ladders under the prefixed names only.
/// The pixmap fallback stays the safety net for hosts that resolve nothing.
fn sni_icon_name(status: TrayStatus, style: IconStyle, app_id: Option<&str>) -> String {
    let name = status.themed_icon_name(style);
    match app_id {
        Some(id) => {
            let suffix = name
                .strip_prefix("hushmic")
                .expect("tray icon names start with 'hushmic'");
            format!("{id}{suffix}")
        }
        None => name,
    }
}

pub struct HushMicTray {
    pub cfg: Config,
    /// Whose strength the Strength radio shows and edits: the running
    /// chain's profile, not the defaults in `cfg`.
    pub profile: ActiveProfile,
    pub mics: Vec<Source>,
    pub cmd_tx: Sender<TrayCmd>,
    pub status: TrayStatus,
    /// The chain-alive processing mode (mirrors the Controller's). Only
    /// meaningful while `cfg.enabled`; the mode radio shows Off otherwise.
    pub mode: RunMode,
    /// The engine tier the chain reports (issue #14); shown in the title
    /// while suppressing, where a fallback is audible.
    pub engine: Option<EngineTier>,
    /// The running chain is on the light model by configuration (per-mic
    /// profile or global), so `light` is not a degradation.
    pub engine_light_configured: bool,
    /// `cfg.tray_icon` already resolved against the desktop, so the name
    /// this hands the host never depends on reading the environment inside
    /// a property call. The main loop refreshes it with the rest of the
    /// state, which is what makes `config set tray_icon` land at once.
    pub icon_style: IconStyle,
    /// The status line and the device names, from the window's snapshot;
    /// the main loop pushes them when they change.
    pub extras: TrayExtras,
}

/// The Strength submenu, weakest first: attenuation limits in dB (100 =
/// no limit). Labels come from [`strength_label`] (the message lookup
/// needs literal keys).
const STRENGTHS: &[f32] = &[6.0, 12.0, 24.0, 100.0];

fn strength_label(idx: usize) -> String {
    match idx {
        0 => tr!("tray-strength-gentle"),
        1 => tr!("tray-strength-medium"),
        2 => tr!("tray-strength-strong"),
        _ => tr!("tray-strength-maximum"),
    }
}

impl HushMicTray {
    /// The chain runs, suppressing, without its model: the icon says what
    /// the call hears (raw voice), as bypass does.
    fn unfiltered(&self) -> bool {
        self.status == TrayStatus::Active && self.engine == Some(EngineTier::Passthrough)
    }

    fn shown_status(&self) -> TrayStatus {
        if self.unfiltered() {
            return TrayStatus::Bypass;
        }
        // the attention badge (red) only while apps hear nothing: an
        // unplugged microphone with none standing in, or a silent one (UX
        // panel B6). A stand-in keeps the calls clean: no badge, the
        // status line says it (round 2, A4).
        let badge = matches!(
            self.extras.notice,
            Some(TrayNotice::Unplugged(_) | TrayNotice::Silent(_))
        );
        if badge && self.status != TrayStatus::Off {
            TrayStatus::Error
        } else {
            self.status
        }
    }

    /// The tooltip (UX panel B6): "HushMic: on · Zoom · Jabra headset",
    /// the state, who uses it, and the microphone.
    fn tip(&self) -> String {
        let state = match self.status {
            TrayStatus::Off => tr!("tray-tip-off"),
            TrayStatus::Mute => tr!("tray-tip-muted"),
            TrayStatus::Bypass => tr!("tray-tip-bypass"),
            TrayStatus::Error => tr!("tray-tip-attention"),
            TrayStatus::Active if self.unfiltered() => tr!("tray-tip-unfiltered"),
            TrayStatus::Active => tr!("tray-tip-on"),
        };
        let mut parts = vec![tr!("tray-tip", state = state)];
        if self.status != TrayStatus::Off {
            parts.extend(self.extras.apps.clone());
            parts.extend(self.extras.mic.clone());
        }
        parts.join(" · ")
    }

    fn status_text(&self) -> Option<String> {
        Some(match self.extras.notice.as_ref()? {
            TrayNotice::Attention => tr!("tray-status-attention"),
            TrayNotice::Reconnecting => tr!("tray-status-reconnecting"),
            TrayNotice::Unplugged(device) => tr!("tray-status-unplugged", device = device.as_str()),
            TrayNotice::StandIn(device, other) => tr!(
                "tray-status-standin",
                device = device.as_str(),
                standin = other.as_str()
            ),
            TrayNotice::Silent(mic) if mic.is_empty() => {
                tr!("tray-status-silent", mic = tr!("notify-your-mic"))
            }
            TrayNotice::Silent(mic) => tr!("tray-status-silent", mic = mic.as_str()),
            TrayNotice::Unfiltered => tr!("tray-status-unfiltered"),
            TrayNotice::Light => tr!("tray-status-light"),
        })
    }
}

impl Tray for HushMicTray {
    fn id(&self) -> String {
        "hushmic".into()
    }
    fn title(&self) -> String {
        match self.status {
            TrayStatus::Bypass => tr!("tray-title-bypass"),
            TrayStatus::Mute => tr!("tray-title-muted"),
            TrayStatus::Error => tr!("tray-title-attention"),
            TrayStatus::Active => match self.engine {
                Some(EngineTier::Passthrough) => tr!("tray-title-unfiltered"),
                Some(EngineTier::Light) if !self.engine_light_configured => {
                    tr!("tray-title-light")
                }
                _ => tr!("tray-title"),
            },
            TrayStatus::Off => tr!("tray-title"),
        }
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.tip(),
            ..Default::default()
        }
    }
    fn icon_name(&self) -> String {
        sni_icon_name(
            self.shown_status(),
            self.icon_style,
            crate::sandbox::flatpak_app_id(),
        )
    }
    fn icon_theme_path(&self) -> String {
        // Read per call, not cached: ksni re-queries on every property fetch
        // and the var is set once by the AppImage wrapper before launch.
        // Unset => empty string => the host falls back to the system theme.
        std::env::var("HUSHMIC_TRAY_THEME_DIR").unwrap_or_default()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        // Embedded copies of the coloured icons, for setups where the named
        // lookup cannot resolve (raw cargo-build runs without the installed
        // ladder, SNI hosts that ignore IconThemePath). These stay coloured
        // whatever `tray_icon` says: a pixmap is pixels, so nothing on the
        // host side could recolor a symbolic one for the panel it lands on.
        // Hosts prefer IconName whenever it resolves.
        crate::branding::tray_icon_rgba(self.shown_status().icon_name())
            .into_iter()
            .map(|(w, h, mut data)| {
                // RGBA -> the SNI spec's network-order ARGB32.
                for px in data.as_chunks_mut::<4>().0.iter_mut() {
                    px.rotate_right(1);
                }
                ksni::Icon {
                    width: w as i32,
                    height: h as i32,
                    data,
                }
            })
            // A switch of the desktop's light or dark style changes the
            // pixmaps by one clear pixel, so the host is told the icon
            // changed and draws the named one again in the panel's new
            // colours (Plasma kept the old colour until a restart).
            .chain(
                (self.extras.system_theme == Some(crate::appstate::Theme::Light)).then(|| {
                    ksni::Icon {
                        width: 1,
                        height: 1,
                        data: vec![0; 4],
                    }
                }),
            )
            .collect()
    }

    /// A left click, where the host sends Activate: the window.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.cmd_tx.send(TrayCmd::Open);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        // mic radio: index 0 = "System default", then each real source
        let mut mic_opts = vec![RadioItem {
            label: match &self.extras.default_name {
                Some(device) => tr!("tray-system-default-named", device = device.as_str()),
                None => tr!("tray-system-default"),
            },
            ..Default::default()
        }];
        mic_opts.extend(self.mics.iter().map(|m| {
            RadioItem {
                label: self
                    .extras
                    .mic_names
                    .iter()
                    .find(|(name, _)| *name == m.name)
                    .map_or_else(|| m.description.clone(), |(_, shown)| shown.clone()),
                ..Default::default()
            }
        }));
        // A configured mic that is currently absent (unplugged USB device) must
        // not be displayed as "System default": the rendered conf still pins
        // target.object to it. Show it truthfully as an extra, selected entry,
        // where it was last seen (else last).
        let missing_mic = match &self.cfg.mic {
            Some(name) if !self.mics.iter().any(|m| &m.name == name) => Some(name.clone()),
            _ => None,
        };
        // Its radio index ("System default" is 0).
        let absent_at = missing_mic.as_ref().map(|_| {
            1 + self
                .extras
                .absent_slot
                .map_or(self.mics.len(), |i| i.min(self.mics.len()))
        });
        if let (Some(name), Some(at)) = (&missing_mic, absent_at) {
            let device = self.extras.absent_name.as_deref().unwrap_or(name);
            mic_opts.insert(
                at,
                RadioItem {
                    label: tr!("tray-mic-absent", device = device),
                    ..Default::default()
                },
            );
        }
        let mic_selected = match &self.cfg.mic {
            None => 0,
            Some(name) => self
                .mics
                .iter()
                .position(|m| &m.name == name)
                .map(|i| i + 1)
                .or(absent_at)
                .unwrap_or(0),
        };
        let mics_for_select = self.mics.clone();

        // A strength set elsewhere (18 dB from the CLI) selects nothing:
        // an index past the end checks no entry.
        let strength_selected = STRENGTHS
            .iter()
            .position(|v| (*v - self.profile.attn_limit).abs() < 0.5)
            .unwrap_or(STRENGTHS.len());

        let mode_selected = if !self.cfg.enabled {
            3
        } else {
            match self.mode {
                RunMode::Suppress => 0,
                RunMode::Bypass => 1,
                RunMode::Mute => 2,
            }
        };

        let mut items: Vec<MenuItem<Self>> = Vec::new();
        // The status line: only while something is unusual (the mode
        // radio already says the usual states).
        let cleaning = (self.extras.cleaning > 0)
            .then(|| tr!("tray-status-cleaning", count = self.extras.cleaning));
        let lines: Vec<String> = self.status_text().into_iter().chain(cleaning).collect();
        for text in &lines {
            items.push(
                StandardItem {
                    label: text.clone(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        if !lines.is_empty() {
            items.push(MenuItem::Separator);
        }
        items.extend([
            StandardItem {
                label: tr!("tray-open"),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.cmd_tx.send(TrayCmd::Open);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            RadioGroup {
                selected: mode_selected,
                select: Box::new(|t: &mut Self, idx| {
                    let sel = match idx {
                        0 => Some(RunMode::Suppress),
                        1 => Some(RunMode::Bypass),
                        2 => Some(RunMode::Mute),
                        _ => None, // Off
                    };
                    // Optimistic local update, same pattern as the other
                    // items; the main loop pushes authoritative state back.
                    // Off may be asked about first (an app uses HushMic):
                    // it shows once it happens.
                    match sel {
                        Some(m) => {
                            t.cfg.enabled = true;
                            t.mode = m;
                            let _ = t.cmd_tx.send(TrayCmd::SetMode(sel));
                        }
                        None => {
                            let _ = t.cmd_tx.send(TrayCmd::AskOff);
                        }
                    }
                }),
                options: [
                    tr!("tray-mode-suppress"),
                    tr!("tray-mode-bypass"),
                    tr!("tray-mode-mute"),
                    tr!("tray-mode-off"),
                ]
                .into_iter()
                .map(|label| RadioItem {
                    label,
                    ..Default::default()
                })
                .collect(),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            SubMenu {
                label: tr!("tray-microphone"),
                submenu: vec![RadioGroup {
                    selected: mic_selected,
                    select: Box::new(move |t: &mut Self, idx| {
                        let pick = match (idx, absent_at) {
                            (0, _) => None,
                            // the "· unplugged" entry: already selected,
                            // nothing to change
                            (i, Some(at)) if i == at => return,
                            (i, Some(at)) if i > at => match mics_for_select.get(i - 2) {
                                Some(m) => Some(m.name.clone()),
                                None => return,
                            },
                            (i, _) => match mics_for_select.get(i - 1) {
                                Some(m) => Some(m.name.clone()),
                                None => return,
                            },
                        };
                        t.cfg.mic = pick.clone();
                        let _ = t.cmd_tx.send(TrayCmd::SelectMic(pick));
                    }),
                    options: mic_opts,
                    ..Default::default()
                }
                .into()],
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: tr!("tray-strength-menu"),
                submenu: vec![RadioGroup {
                    selected: strength_selected,
                    select: Box::new(|t: &mut Self, idx| {
                        let v = STRENGTHS[idx];
                        t.profile.attn_limit = v;
                        let _ = t.cmd_tx.send(TrayCmd::SetAttn(v));
                    }),
                    options: (0..STRENGTHS.len())
                        .map(|i| RadioItem {
                            label: strength_label(i),
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                }
                .into()],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: tr!("tray-quit-hushmic"),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.cmd_tx.send(TrayCmd::AskQuit);
                }),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_icons_distinct() {
        use super::TrayStatus::*;
        // Exact names: the PNG ladder under packaging/tray/hicolor/ ships
        // files with precisely these stems.
        assert_eq!(Active.icon_name(), "hushmic-tray");
        assert_eq!(Off.icon_name(), "hushmic-tray-off");
        assert_eq!(Error.icon_name(), "hushmic-tray-error");
        assert_eq!(Bypass.icon_name(), "hushmic-tray-bypass");
        assert_eq!(Mute.icon_name(), "hushmic-tray-mute");
        let all = [Off, Active, Bypass, Mute, Error];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.icon_name(), b.icon_name(), "{a:?} vs {b:?}");
            }
            assert!(
                a.icon_name().starts_with("hushmic-tray"),
                "{a:?} icon must come from the shipped hushmic-tray set"
            );
        }
    }

    /// Every status, in one list, for the name tests below.
    const ALL_STATUS: [TrayStatus; 5] = [
        TrayStatus::Off,
        TrayStatus::Active,
        TrayStatus::Bypass,
        TrayStatus::Mute,
        TrayStatus::Error,
    ];

    #[test]
    fn the_symbolic_set_on_disk_is_complete_and_recolorable() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/tray/hicolor");
        for dir in [
            "16x16", "16x16@2", "22x22", "22x22@2", "24x24", "24x24@2", "scalable",
        ] {
            for s in ALL_STATUS {
                let name = s.themed_icon_name(IconStyle::Symbolic);
                let path = root.join(dir).join("status").join(format!("{name}.svg"));
                let svg = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                // KDE swaps this stylesheet, GTK fills every path: no strokes,
                // no hardcoded fills.
                assert!(
                    svg.contains(r#"id="current-color-scheme""#),
                    "{}",
                    path.display()
                );
                assert!(svg.contains(".ColorScheme-Text"), "{}", path.display());
                assert!(!svg.contains("stroke"), "{}", path.display());
                let paths = svg.matches("<path ").count();
                assert!(paths > 0, "{}", path.display());
                assert_eq!(
                    svg.matches(r#"fill="currentColor""#).count(),
                    paths,
                    "{}",
                    path.display()
                );
                // GTK keys red on the `error` class: the error badge and the
                // mute slash carry it, nothing else.
                assert_eq!(
                    svg.contains(r#"class="ColorScheme-NegativeText error""#),
                    matches!(s, TrayStatus::Error | TrayStatus::Mute),
                    "{}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn symbolic_names_keep_the_state_under_their_own_stem() {
        use super::TrayStatus::*;
        // Exact names: the shipped SVG set carries precisely these stems
        // (the_symbolic_set_on_disk_is_complete_and_recolorable holds it to that).
        assert_eq!(
            Active.themed_icon_name(IconStyle::Symbolic),
            "hushmic-mono-symbolic"
        );
        assert_eq!(
            Off.themed_icon_name(IconStyle::Symbolic),
            "hushmic-mono-off-symbolic"
        );
        assert_eq!(
            Bypass.themed_icon_name(IconStyle::Symbolic),
            "hushmic-mono-bypass-symbolic"
        );
        assert_eq!(
            Mute.themed_icon_name(IconStyle::Symbolic),
            "hushmic-mono-mute-symbolic"
        );
        assert_eq!(
            Error.themed_icon_name(IconStyle::Symbolic),
            "hushmic-mono-error-symbolic"
        );
        // Colour keeps the ladder's own name, which is also the key the
        // embedded pixmaps are looked up by.
        for s in ALL_STATUS {
            assert_eq!(s.themed_icon_name(IconStyle::Color), s.icon_name(), "{s:?}");
        }
    }

    #[test]
    fn every_status_name_stays_distinct_in_both_styles() {
        let mut seen: Vec<String> = Vec::new();
        for style in [IconStyle::Color, IconStyle::Symbolic] {
            for s in ALL_STATUS {
                let n = s.themed_icon_name(style);
                assert!(n.starts_with("hushmic-"), "{s:?}/{style:?}: {n}");
                // Plasma 6 swaps in `<name>-symbolic` when it exists, so no
                // symbolic name may be a color name plus the suffix.
                assert!(
                    !seen.contains(&n.trim_end_matches("-symbolic").to_string()),
                    "{n} shadows a color name"
                );
                assert!(!seen.contains(&n), "duplicate name {n} ({s:?}/{style:?})");
                seen.push(n);
            }
        }
        assert_eq!(seen.len(), 10);
    }

    #[test]
    fn inside_a_flatpak_both_ladders_use_the_app_id_prefix() {
        let id = "io.github.fovty.HushMic";
        assert_eq!(
            sni_icon_name(TrayStatus::Active, IconStyle::Symbolic, Some(id)),
            "io.github.fovty.HushMic-mono-symbolic"
        );
        assert_eq!(
            sni_icon_name(TrayStatus::Mute, IconStyle::Symbolic, Some(id)),
            "io.github.fovty.HushMic-mono-mute-symbolic"
        );
        assert_eq!(
            sni_icon_name(TrayStatus::Mute, IconStyle::Color, Some(id)),
            "io.github.fovty.HushMic-tray-mute"
        );
        // Outside a sandbox the name is handed over unchanged.
        for style in [IconStyle::Color, IconStyle::Symbolic] {
            for s in ALL_STATUS {
                assert_eq!(
                    sni_icon_name(s, style, None),
                    s.themed_icon_name(style),
                    "{s:?}/{style:?}"
                );
            }
        }
    }

    #[test]
    fn auto_follows_the_desktop_and_the_two_words_pin_it() {
        use super::IconStyle::*;
        for (desktop, want) in [
            (Some("KDE"), Symbolic),
            (Some("kde"), Symbolic),
            (Some("ubuntu:GNOME"), Symbolic),
            (Some("GNOME-Classic:GNOME"), Symbolic),
            (Some("Budgie:GNOME"), Symbolic),
            (Some("X-Cinnamon"), Color),
            (Some("XFCE"), Color),
            (Some("sway"), Color),
            (Some(""), Color),
            (None, Color),
        ] {
            assert_eq!(
                resolve_icon_style(TrayIcon::Auto, desktop),
                want,
                "{desktop:?}"
            );
            // The explicit words ignore the desktop entirely.
            assert_eq!(resolve_icon_style(TrayIcon::Color, desktop), Color);
            assert_eq!(resolve_icon_style(TrayIcon::Symbolic, desktop), Symbolic);
        }
    }

    #[test]
    fn the_tray_hands_the_host_its_resolved_style() {
        let mut tray = test_tray();
        tray.status = TrayStatus::Active;
        assert_eq!(tray.icon_name(), "hushmic-tray");
        tray.icon_style = IconStyle::Symbolic;
        assert_eq!(tray.icon_name(), "hushmic-mono-symbolic");
        // The embedded pixmap fallback stays the coloured set either way.
        assert!(!tray.icon_pixmap().is_empty());
    }

    #[test]
    fn the_tooltip_says_the_state_the_apps_and_the_mic() {
        use crate::appstate::mock::{state, Scenario};
        let mut tray = test_tray();
        tray.status = TrayStatus::Active;
        let mut s = state(Scenario::OnInCall);
        s.apps.using.truncate(1);
        tray.extras = TrayExtras::of(&s);
        let app = s.apps.using[0].name.clone().unwrap();
        let mic = s
            .description_of(s.mic.active.as_deref().unwrap())
            .unwrap()
            .to_owned();
        assert_eq!(tray.tip(), format!("HushMic: on · {app} · {mic}"));
        tray.status = TrayStatus::Mute;
        assert!(tray.tip().starts_with("HushMic: muted · "));
        tray.status = TrayStatus::Bypass;
        assert!(tray.tip().starts_with("HushMic: noise suppression off · "));
        tray.status = TrayStatus::Off;
        assert_eq!(tray.tip(), "HushMic: off", "off: nothing else to say");
        // no app: the state and the mic
        tray.status = TrayStatus::Active;
        tray.extras.apps = None;
        assert_eq!(tray.tip(), format!("HushMic: on · {mic}"));
        assert_eq!(tray.tool_tip().title, tray.tip());
    }

    #[test]
    fn an_unplugged_or_silent_mic_badges_the_icon() {
        let mut tray = test_tray();
        tray.status = TrayStatus::Active;
        let on = tray.icon_name();
        for notice in [
            TrayNotice::Unplugged("RODE NT-USB".into()),
            TrayNotice::Silent("RODE NT-USB".into()),
        ] {
            tray.extras.notice = Some(notice.clone());
            assert_eq!(tray.shown_status(), TrayStatus::Error, "{notice:?}");
            assert_ne!(tray.icon_name(), on);
            // the title still says the state; the status line says why
            assert_eq!(tray.title(), "HushMic");
        }
        // another microphone stands in: the calls are clean, no red
        // (UX panel round 2, A4)
        tray.extras.notice = Some(TrayNotice::StandIn("RODE NT-USB".into(), "Jabra".into()));
        assert_eq!(tray.shown_status(), TrayStatus::Active);
        assert_eq!(tray.icon_name(), on);
        assert_eq!(
            tray.status_text().unwrap(),
            "RODE NT-USB unplugged · using Jabra"
        );
        // off stays off
        tray.status = TrayStatus::Off;
        assert_eq!(tray.shown_status(), TrayStatus::Off);
        // a light model or reconnecting is no badge
        tray.status = TrayStatus::Active;
        tray.extras.notice = Some(TrayNotice::Light);
        assert_eq!(tray.shown_status(), TrayStatus::Active);
    }

    #[test]
    fn a_theme_switch_tells_the_host_the_icon_changed() {
        use crate::appstate::Theme;
        let mut tray = test_tray();
        tray.status = TrayStatus::Active;
        tray.extras.system_theme = Some(Theme::Dark);
        let dark = tray.icon_pixmap();
        tray.extras.system_theme = Some(Theme::Light);
        let light = tray.icon_pixmap();
        assert_ne!(
            dark.iter().map(|i| (i.width, i.height)).collect::<Vec<_>>(),
            light
                .iter()
                .map(|i| (i.width, i.height))
                .collect::<Vec<_>>(),
            "the pixmaps differ, so ksni sends NewIcon"
        );
        // the drawn ones are the same
        assert_eq!(dark.len() + 1, light.len());
    }

    #[test]
    fn title_reflects_status() {
        let mut tray = test_tray();
        for (status, want) in [
            (TrayStatus::Active, "HushMic"),
            (TrayStatus::Off, "HushMic"),
            (TrayStatus::Bypass, "HushMic (noise suppression off)"),
            (TrayStatus::Mute, "HushMic (muted)"),
            (TrayStatus::Error, "HushMic (needs attention)"),
        ] {
            tray.status = status;
            assert_eq!(tray.title(), want, "{status:?}");
        }
        // The engine tier shows only while suppressing, and a light tier
        // only when the quality model was asked for.
        tray.status = TrayStatus::Active;
        tray.engine = Some(EngineTier::Light);
        assert_eq!(tray.title(), "HushMic (light model)");
        tray.engine = Some(EngineTier::Passthrough);
        assert_eq!(tray.title(), "HushMic (noise suppression paused)");
        tray.status = TrayStatus::Mute;
        assert_eq!(tray.title(), "HushMic (muted)");
        tray.status = TrayStatus::Active;
        tray.engine = Some(EngineTier::Light);
        tray.engine_light_configured = true;
        assert_eq!(tray.title(), "HushMic");
    }

    /// Unfiltered for now: the call hears raw voice, so the icon is the
    /// bypass one (the board's tray), in the name and in the pixmaps.
    #[test]
    fn an_unfiltered_chain_shows_the_bypass_icon() {
        let mut tray = test_tray();
        tray.status = TrayStatus::Active;
        tray.engine = Some(EngineTier::Passthrough);
        assert_eq!(tray.icon_name(), "hushmic-tray-bypass");
        assert_eq!(
            tray.icon_pixmap().len(),
            crate::branding::tray_icon_rgba("hushmic-tray-bypass").len()
        );
        tray.engine = Some(EngineTier::Quality);
        assert_eq!(tray.icon_name(), "hushmic-tray");
        // Muted stays muted whatever the engine does.
        tray.status = TrayStatus::Mute;
        tray.engine = Some(EngineTier::Passthrough);
        assert_eq!(tray.icon_name(), "hushmic-tray-mute");
    }

    fn submenu<'a>(menu: &'a [MenuItem<HushMicTray>], label: &str) -> &'a RadioGroup<HushMicTray> {
        let sub = menu
            .iter()
            .find_map(|i| match i {
                MenuItem::SubMenu(s) if s.label == label => Some(s),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{label} submenu"));
        match sub.submenu.first() {
            Some(MenuItem::RadioGroup(g)) => g,
            _ => panic!("{label} submenu must hold a radio group"),
        }
    }

    fn labels(g: &RadioGroup<HushMicTray>) -> Vec<String> {
        g.options.iter().map(|o| o.label.clone()).collect()
    }

    /// The reported bug: the defaults say 24 dB, the RODE profile in
    /// effect says maximum — the radio must show the profile.
    #[test]
    fn strength_shows_the_active_profile_weakest_first() {
        let mut tray = test_tray();
        tray.cfg.attn_limit = 24.0;
        tray.profile = ActiveProfile {
            device: Some("alsa_input.test".into()),
            saved: true,
            model: "dpdfnet8_48khz_hr".into(),
            attn_limit: 100.0,
        };
        let menu = tray.menu();
        let g = submenu(&menu, "Strength");
        assert_eq!(labels(g), ["Gentle", "Medium", "Strong", "Maximum"]);
        assert_eq!(g.selected, 3);
        tray.profile.attn_limit = 6.0;
        assert_eq!(submenu(&tray.menu(), "Strength").selected, 0);
        // A strength set elsewhere matches no entry: none is checked.
        tray.profile.attn_limit = 18.0;
        assert_eq!(submenu(&tray.menu(), "Strength").selected, STRENGTHS.len());
    }

    #[test]
    fn a_strength_click_updates_the_profile_shown_not_the_defaults() {
        let (mut tray, rx) = tray_with_rx();
        let defaults = tray.cfg.attn_limit;
        let menu = tray.menu();
        (submenu(&menu, "Strength").select)(&mut tray, 2);
        assert_eq!(tray.profile.attn_limit, 24.0);
        assert_eq!(tray.cfg.attn_limit, defaults);
        assert!(matches!(rx.try_recv(), Ok(TrayCmd::SetAttn(v)) if v == 24.0));
        assert!(rx.try_recv().is_err(), "exactly one command per activation");
    }

    fn mode_radio(menu: &[MenuItem<HushMicTray>]) -> &RadioGroup<HushMicTray> {
        menu.iter()
            .find_map(|i| match i {
                MenuItem::RadioGroup(g) => Some(g),
                _ => None,
            })
            .expect("the mode radio sits in the top level")
    }

    #[test]
    fn mode_radio_labels_and_selection() {
        let mut tray = test_tray(); // Config::default() is enabled
        let menu = tray.menu();
        let g = mode_radio(&menu);
        assert_eq!(
            labels(g),
            [
                "Noise suppression",
                "Noise suppression off",
                "Mute",
                "Turn off HushMic"
            ]
        );
        assert_eq!(g.selected, 0, "enabled + Suppress selects the first entry");

        tray.mode = RunMode::Bypass;
        assert_eq!(mode_radio(&tray.menu()).selected, 1);
        tray.mode = RunMode::Mute;
        assert_eq!(mode_radio(&tray.menu()).selected, 2);

        // Disabled wins over whatever mode is remembered.
        tray.cfg.enabled = false;
        assert_eq!(mode_radio(&tray.menu()).selected, 3);
    }

    #[test]
    fn mode_radio_select_sends_commands() {
        let (mut tray, rx) = tray_with_rx();
        tray.status = TrayStatus::Active;
        let menu = tray.menu();
        let g = mode_radio(&menu);

        (g.select)(&mut tray, 2);
        assert!(matches!(
            rx.try_recv(),
            Ok(TrayCmd::SetMode(Some(RunMode::Mute)))
        ));
        assert!(tray.cfg.enabled, "mute is a chain-alive state");
        assert_eq!(tray.mode, RunMode::Mute, "optimistic local state update");

        (g.select)(&mut tray, 3);
        assert!(matches!(rx.try_recv(), Ok(TrayCmd::AskOff)));
        // the daemon may ask first (an app uses HushMic): Off shows once
        // it happens
        assert!(tray.cfg.enabled, "no optimistic Off");
        tray.cfg.enabled = false;

        // From Off, picking a chain-alive state re-enables with that mode.
        (g.select)(&mut tray, 1);
        assert!(matches!(
            rx.try_recv(),
            Ok(TrayCmd::SetMode(Some(RunMode::Bypass)))
        ));
        assert!(tray.cfg.enabled);
        assert!(rx.try_recv().is_err(), "exactly one command per activation");
    }

    #[test]
    fn icon_theme_path_follows_the_env_var() {
        // Set + unset probed inside ONE test: the var is process-global and
        // the test harness runs tests on parallel threads.
        let tray = test_tray();
        std::env::remove_var("HUSHMIC_TRAY_THEME_DIR");
        assert_eq!(tray.icon_theme_path(), "");
        std::env::set_var("HUSHMIC_TRAY_THEME_DIR", "/opt/hushmic/icons");
        assert_eq!(tray.icon_theme_path(), "/opt/hushmic/icons");
        std::env::remove_var("HUSHMIC_TRAY_THEME_DIR");
        assert_eq!(tray.icon_theme_path(), "");
    }

    fn test_tray() -> HushMicTray {
        tray_with_rx().0
    }

    fn tray_with_rx() -> (HushMicTray, std::sync::mpsc::Receiver<TrayCmd>) {
        // Labels are asserted in English; pin the catalog before the
        // process-global loader initializes lazily.
        crate::i18n::pin_english();
        let (tx, rx) = std::sync::mpsc::channel();
        let tray = HushMicTray {
            cfg: Config::default(),
            profile: Config::default().profile_for(None),
            mics: vec![Source {
                name: "alsa_input.test".into(),
                description: "Test Mic".into(),
            }],
            cmd_tx: tx,
            status: TrayStatus::Off,
            mode: RunMode::Suppress,
            engine: None,
            engine_light_configured: false,
            icon_style: IconStyle::Color,
            extras: TrayExtras::default(),
        };
        (tray, rx)
    }

    #[test]
    fn the_microphone_submenu_names_the_default_and_an_absent_choice() {
        let mut tray = test_tray();
        assert_eq!(
            labels(submenu(&tray.menu(), "Microphone")),
            ["System default", "Test Mic"]
        );
        tray.extras.default_name = Some("Built-in Audio".into());
        assert_eq!(
            labels(submenu(&tray.menu(), "Microphone"))[0],
            "System default (Built-in Audio)"
        );
        // The chosen mic is unplugged: an extra entry, selected, by its
        // display name once the snapshot knows it, else by node name.
        tray.cfg.mic = Some("alsa_input.rode".into());
        let menu = tray.menu();
        let g = submenu(&menu, "Microphone");
        assert_eq!(labels(g).last().unwrap(), "alsa_input.rode · unplugged");
        assert_eq!(g.selected, 2);
        tray.extras.absent_name = Some("RODE NT-USB".into());
        assert_eq!(
            labels(submenu(&tray.menu(), "Microphone")).last().unwrap(),
            "RODE NT-USB · unplugged"
        );
        // Seen first in the list last time: it keeps that place.
        tray.extras.absent_slot = Some(0);
        let menu = tray.menu();
        let g = submenu(&menu, "Microphone");
        assert_eq!(
            labels(g),
            [
                "System default (Built-in Audio)",
                "RODE NT-USB · unplugged",
                "Test Mic"
            ]
        );
        assert_eq!(g.selected, 1);
        tray.extras.absent_slot = None;
        // A present mic gets no extra entry.
        tray.cfg.mic = Some("alsa_input.test".into());
        let menu = tray.menu();
        let g = submenu(&menu, "Microphone");
        assert_eq!(g.options.len(), 2);
        assert_eq!(g.selected, 1);
    }

    #[test]
    fn picking_a_microphone_sends_it_and_the_absent_entry_does_nothing() {
        let (mut tray, rx) = tray_with_rx();
        tray.cfg.mic = Some("alsa_input.rode".into());
        let menu = tray.menu();
        let g = submenu(&menu, "Microphone");
        (g.select)(&mut tray, 2);
        assert!(
            rx.try_recv().is_err(),
            "the unplugged entry is already chosen"
        );
        (g.select)(&mut tray, 1);
        assert!(matches!(rx.try_recv(), Ok(TrayCmd::SelectMic(Some(m))) if m == "alsa_input.test"));
        (g.select)(&mut tray, 0);
        assert!(matches!(rx.try_recv(), Ok(TrayCmd::SelectMic(None))));
        assert_eq!(tray.cfg.mic, None);
        // The unplugged entry in its own place, before "Test Mic".
        tray.cfg.mic = Some("alsa_input.rode".into());
        tray.extras.absent_slot = Some(0);
        let menu = tray.menu();
        let g = submenu(&menu, "Microphone");
        (g.select)(&mut tray, 1);
        assert!(rx.try_recv().is_err(), "already chosen");
        (g.select)(&mut tray, 2);
        assert!(matches!(rx.try_recv(), Ok(TrayCmd::SelectMic(Some(m))) if m == "alsa_input.test"));
    }

    /// The menu's top level as the board has it: (status line,) Open
    /// HushMic, the modes, the two submenus, Quit.
    fn layout(menu: &[MenuItem<HushMicTray>]) -> Vec<Vec<String>> {
        let mut groups: Vec<Vec<String>> = vec![vec![]];
        for item in menu {
            let g = groups.last_mut().unwrap();
            match item {
                MenuItem::Separator => groups.push(vec![]),
                MenuItem::Standard(s) => g.push(s.label.clone()),
                MenuItem::Checkmark(c) => g.push(c.label.clone()),
                MenuItem::SubMenu(m) => g.push(format!("{} ▸", m.label)),
                MenuItem::RadioGroup(r) => g.extend(labels(r)),
            }
        }
        groups
    }

    #[test]
    fn the_menu_is_tray_a() {
        let menu = test_tray().menu();
        assert_eq!(
            layout(&menu),
            [
                vec!["Open HushMic"],
                vec![
                    "Noise suppression",
                    "Noise suppression off",
                    "Mute",
                    "Turn off HushMic"
                ],
                vec!["Microphone ▸", "Strength ▸"],
                vec!["Quit HushMic"],
            ]
        );
    }

    #[test]
    fn the_status_line_shows_only_when_something_is_unusual() {
        let mut tray = test_tray();
        for (notice, text) in [
            (TrayNotice::Attention, "Needs attention"),
            (TrayNotice::Reconnecting, "Reconnecting…"),
            (
                TrayNotice::Unplugged("RODE NT-USB".into()),
                "RODE NT-USB unplugged",
            ),
            (TrayNotice::Unfiltered, "Noise suppression paused for now"),
            (TrayNotice::Light, "Using Light for now"),
        ] {
            tray.extras.notice = Some(notice);
            let menu = tray.menu();
            let MenuItem::Standard(line) = &menu[0] else {
                panic!("the status line comes first")
            };
            assert_eq!(line.label, text);
            assert!(!line.enabled, "a line to read, not a command");
            assert!(matches!(menu[1], MenuItem::Separator));
            assert_eq!(layout(&menu)[1], ["Open HushMic"]);
        }
        tray.extras.notice = None;
        assert_eq!(layout(&tray.menu())[0], ["Open HushMic"]);
        // Files cleaning: a line of its own, under any other.
        tray.extras.cleaning = 2;
        assert_eq!(layout(&tray.menu())[0], ["Cleaning 2 files…"]);
        tray.extras.cleaning = 1;
        tray.extras.notice = Some(TrayNotice::Reconnecting);
        let menu = tray.menu();
        assert_eq!(layout(&menu)[0], ["Reconnecting…", "Cleaning 1 file…"]);
        assert_eq!(layout(&menu)[1], ["Open HushMic"]);
    }

    #[test]
    fn open_and_a_left_click_ask_for_the_window_and_quit_quits() {
        let (mut tray, rx) = tray_with_rx();
        let menu = tray.menu();
        let item = |label: &str| {
            menu.iter()
                .find_map(|i| match i {
                    MenuItem::Standard(s) if s.label == label => Some(s),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{label}"))
        };
        (item("Open HushMic").activate)(&mut tray);
        assert!(matches!(rx.try_recv(), Ok(TrayCmd::Open)));
        tray.activate(0, 0);
        assert!(matches!(rx.try_recv(), Ok(TrayCmd::Open)));
        (item("Quit HushMic").activate)(&mut tray);
        assert!(matches!(rx.try_recv(), Ok(TrayCmd::AskQuit)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn the_status_line_follows_the_snapshot() {
        use crate::appstate::mock::{state, Scenario};
        for (scenario, want) in [
            (Scenario::OnInCall, None),
            (Scenario::Bypass, None),
            (Scenario::Muted, None),
            (Scenario::Starting, None),
            (Scenario::Off, None),
            (Scenario::LightForNow, Some(TrayNotice::Light)),
            (Scenario::UnfilteredForNow, Some(TrayNotice::Unfiltered)),
            (Scenario::Reconnecting, Some(TrayNotice::Reconnecting)),
            (Scenario::NeedsAttention, Some(TrayNotice::Attention)),
            (Scenario::PipewireUnreachable, Some(TrayNotice::Attention)),
            (
                Scenario::MicUnplugged,
                Some(TrayNotice::StandIn(
                    "RODE NT-USB".into(),
                    "Built-in Audio".into(),
                )),
            ),
        ] {
            assert_eq!(notice_for(&state(scenario)), want, "{scenario:?}");
        }
        // The daemon heard 10 s of silence from a present mic.
        let mut s = state(Scenario::OnInCall);
        s.mic.silent = true;
        let mic = s.description_of(s.mic.active.as_deref().unwrap()).unwrap();
        assert_eq!(notice_for(&s), Some(TrayNotice::Silent(mic.to_string())));
        s.mode = Mode::Off;
        assert_eq!(notice_for(&s), None, "off: nothing to say");
        // An unplugged mic says that instead.
        let mut s = state(Scenario::MicUnplugged);
        s.mic.silent = true;
        assert!(matches!(notice_for(&s), Some(TrayNotice::StandIn(..))));
        // With none standing in (the daemon's fallback names nobody, or
        // the chosen one), apps hear nothing (round 2, A4).
        for active in [None, s.mic.selected.clone()] {
            let mut s = state(Scenario::MicUnplugged);
            s.mic.active = active;
            assert_eq!(
                notice_for(&s),
                Some(TrayNotice::Unplugged("RODE NT-USB".into()))
            );
        }
        // A fallback in bypass is not audible as one: no line.
        let mut s = state(Scenario::UnfilteredForNow);
        s.mode = Mode::Bypass;
        assert_eq!(notice_for(&s), None);
        // Light by choice is not a fallback.
        let mut s = state(Scenario::LightForNow);
        s.engine.configured_light = true;
        assert_eq!(notice_for(&s), None);
    }

    #[test]
    fn the_microphone_submenu_uses_the_snapshot_names() {
        let mut tray = test_tray();
        tray.mics.push(Source {
            name: "alsa_input.other".into(),
            description: "Test Mic".into(),
        });
        // Before the first snapshot: PipeWire's names.
        assert_eq!(
            labels(submenu(&tray.menu(), "Microphone")),
            ["System default", "Test Mic", "Test Mic"]
        );
        tray.extras.mic_names = vec![
            ("alsa_input.other".into(), "Test Mic (2)".into()),
            ("alsa_input.test".into(), "Test Mic".into()),
        ];
        assert_eq!(
            labels(submenu(&tray.menu(), "Microphone")),
            ["System default", "Test Mic", "Test Mic (2)"]
        );
    }

    #[test]
    fn the_extras_name_the_default_and_the_absent_mic() {
        use crate::appstate::mock::{state, Scenario};
        let x = TrayExtras::of(&state(Scenario::OnInCall));
        assert_eq!(x.notice, None);
        assert_eq!(x.default_name.as_deref(), Some("Built-in Audio"));
        assert_eq!(x.absent_name, None);
        let x = TrayExtras::of(&state(Scenario::MicUnplugged));
        assert_eq!(x.absent_name.as_deref(), Some("RODE NT-USB"));
        assert_eq!(x.absent_slot, Some(0), "first, where it was");
        // Without PipeWire nothing is known by name.
        let x = TrayExtras::of(&state(Scenario::PipewireUnreachable));
        assert_eq!(x.default_name, None);
    }
}
