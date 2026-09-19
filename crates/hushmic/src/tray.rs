// ksni menu literals already set every field; the trailing `..Default::default()`
// is kept intentionally as forward-compat across ksni versions.
#![allow(clippy::needless_update)]

use crate::config::{Config, TrayIcon};
use crate::controller::RunMode;
use crate::diagnostics::EngineTier;
use crate::pipewire::Source;
use crate::tr;
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, Tray};
use std::sync::mpsc::Sender;

#[derive(Debug)]
pub enum TrayCmd {
    /// `Some(mode)` = a chain-alive state (suppress / bypass / mute);
    /// `None` = Off, the existing tear-the-chain-down disable path.
    SetMode(Option<RunMode>),
    SelectMic(Option<String>),
    SelectModel(String),
    SetAttn(f32),
    SetDefaultToggle(bool),
    SetAutostart(bool),
    TestMic,
    /// Open the compositor's shortcut-binding dialog (portal BindShortcuts).
    SetupShortcuts,
    About,
    Quit,
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
    pub mics: Vec<Source>,
    pub cmd_tx: Sender<TrayCmd>,
    pub status: TrayStatus,
    /// A mic test is currently recording/playing (the menu item is disabled
    /// while it runs; the main loop flips this via handle.update).
    pub testing: bool,
    /// The recovery fallback is engaged: the chain runs on the system
    /// default while the preferred mic is unplugged. Only affects how the
    /// missing-mic entry is labelled.
    pub fallback_active: bool,
    /// The chain-alive processing mode (mirrors the Controller's). Only
    /// meaningful while `cfg.enabled`; the mode radio shows Off otherwise.
    pub mode: RunMode,
    /// The GlobalShortcuts portal answered our probe (session up, events
    /// flowing). Flipped by the main loop via handle.update; gates the
    /// "Set up shortcuts…" entry the way can_set_default gates the
    /// default-mic checkbox — hidden, not explained.
    pub shortcuts_available: bool,
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
}

// The option tables hold ids/values only; labels come from the catalog via
// the match functions below (the message lookup needs literal keys).
const MODELS: &[&str] = &["dpdfnet8_48khz_hr", "dpdfnet2_48khz_hr"];
const ATTN_PRESETS: &[f32] = &[100.0, 24.0, 12.0, 6.0];

impl HushMicTray {
    /// The model radio shows the CONFIGURED model as selected; when the
    /// chain is running a different tier right now (issue #14), the tier in
    /// use says so on its own line, and passthrough on the selected one.
    fn model_item_label(&self, id: &str) -> String {
        let base = model_label(id);
        if self.status != TrayStatus::Active {
            return base;
        }
        let light_in_use = self.engine == Some(EngineTier::Light) && !self.engine_light_configured;
        let is_light = id.starts_with("dpdfnet2");
        // Which entry describes what the chain actually loaded. Not
        // `cfg.model == id`: a per-mic profile can run a different model
        // than the global setting, and then the global one would be
        // annotated while the running one looked idle. `engine_light_
        // configured` is the running chain's own model, so it decides.
        let running = is_light == self.engine_light_configured;
        match self.engine {
            Some(EngineTier::Passthrough) if running => {
                format!("{base} {}", tr!("tray-model-now-passthrough"))
            }
            Some(EngineTier::Light) if light_in_use && is_light => {
                format!("{base} {}", tr!("tray-model-now-light"))
            }
            _ => base,
        }
    }
}

fn model_label(id: &str) -> String {
    match id {
        "dpdfnet2_48khz_hr" => tr!("tray-model-dpdfnet2"),
        _ => tr!("tray-model-dpdfnet8"),
    }
}

fn attn_label(v: f32) -> String {
    if (v - 24.0).abs() < 0.5 {
        tr!("tray-attn-strong")
    } else if (v - 12.0).abs() < 0.5 {
        tr!("tray-attn-medium")
    } else if (v - 6.0).abs() < 0.5 {
        tr!("tray-attn-light")
    } else {
        tr!("tray-attn-maximum")
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
            TrayStatus::Error => tr!("tray-title-error"),
            TrayStatus::Active => match self.engine {
                Some(EngineTier::Passthrough) => tr!("tray-title-passthrough"),
                Some(EngineTier::Light) if !self.engine_light_configured => {
                    tr!("tray-title-light")
                }
                _ => tr!("tray-title"),
            },
            TrayStatus::Off => tr!("tray-title"),
        }
    }
    fn icon_name(&self) -> String {
        sni_icon_name(
            self.status,
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
        crate::branding::tray_icon_rgba(self.status.icon_name())
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
            .collect()
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        // mic radio: index 0 = "System default", then each real source
        let mut mic_opts = vec![RadioItem {
            label: tr!("tray-system-default"),
            ..Default::default()
        }];
        mic_opts.extend(self.mics.iter().map(|m| RadioItem {
            label: m.description.clone(),
            ..Default::default()
        }));
        // A configured mic that is currently absent (unplugged USB device) must
        // not be displayed as "System default": the rendered conf still pins
        // target.object to it. Show it truthfully as an extra, selected entry.
        let missing_mic = match &self.cfg.mic {
            Some(name) if !self.mics.iter().any(|m| &m.name == name) => Some(name.clone()),
            _ => None,
        };
        if let Some(name) = &missing_mic {
            // Once recovery has switched the chain, say what it is actually
            // doing; before that (or when recovery can't run), plain truth.
            let suffix = if self.fallback_active {
                tr!("tray-mic-unplugged")
            } else {
                tr!("tray-mic-unavailable")
            };
            mic_opts.push(RadioItem {
                label: format!("{name} {suffix}"),
                ..Default::default()
            });
        }
        let mic_selected = match &self.cfg.mic {
            None => 0,
            Some(name) => self
                .mics
                .iter()
                .position(|m| &m.name == name)
                .map(|i| i + 1)
                .unwrap_or(self.mics.len() + 1), // the "(unavailable)" entry
        };
        let mics_for_select = self.mics.clone();

        let model_selected = MODELS
            .iter()
            .position(|id| *id == self.cfg.model)
            .unwrap_or(0);
        let attn_selected = ATTN_PRESETS
            .iter()
            .position(|v| (*v - self.cfg.attn_limit).abs() < 0.5)
            .unwrap_or(0);

        let mode_selected = if !self.cfg.enabled {
            3
        } else {
            match self.mode {
                RunMode::Suppress => 0,
                RunMode::Bypass => 1,
                RunMode::Mute => 2,
            }
        };

        vec![
            StandardItem {
                label: if self.testing {
                    tr!("tray-test-running")
                } else {
                    tr!("tray-test-mic")
                },
                icon_name: "audio-input-microphone".into(),
                enabled: !self.testing,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.cmd_tx.send(TrayCmd::TestMic);
                }),
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: tr!("tray-mode"),
                submenu: vec![RadioGroup {
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
                        match sel {
                            Some(m) => {
                                t.cfg.enabled = true;
                                t.mode = m;
                            }
                            None => t.cfg.enabled = false,
                        }
                        let _ = t.cmd_tx.send(TrayCmd::SetMode(sel));
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
                .into()],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            SubMenu {
                label: tr!("tray-microphone"),
                submenu: vec![RadioGroup {
                    selected: mic_selected,
                    select: Box::new(move |t: &mut Self, idx| {
                        let pick = if idx == 0 {
                            None
                        } else {
                            match mics_for_select.get(idx - 1) {
                                Some(m) => Some(m.name.clone()),
                                // the trailing "(unavailable)" entry: already
                                // selected, nothing to change
                                None => return,
                            }
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
                label: tr!("tray-model"),
                submenu: vec![RadioGroup {
                    selected: model_selected,
                    select: Box::new(|t: &mut Self, idx| {
                        let id = MODELS[idx].to_string();
                        t.cfg.model = id.clone();
                        let _ = t.cmd_tx.send(TrayCmd::SelectModel(id));
                    }),
                    options: MODELS
                        .iter()
                        .map(|id| RadioItem {
                            label: self.model_item_label(id),
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                }
                .into()],
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: tr!("tray-strength"),
                submenu: vec![RadioGroup {
                    selected: attn_selected,
                    select: Box::new(|t: &mut Self, idx| {
                        let v = ATTN_PRESETS[idx];
                        t.cfg.attn_limit = v;
                        let _ = t.cmd_tx.send(TrayCmd::SetAttn(v));
                    }),
                    options: ATTN_PRESETS
                        .iter()
                        .map(|v| RadioItem {
                            label: attn_label(*v),
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                }
                .into()],
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: tr!("tray-set-default"),
                checked: self.cfg.set_default,
                // Hidden where changing the system default is impossible (a
                // sandbox without Manager access — see
                // pipewire::can_set_default): a checkbox whose click
                // silently does nothing is worse than no checkbox.
                visible: crate::pipewire::can_set_default(),
                activate: Box::new(|t: &mut Self| {
                    t.cfg.set_default = !t.cfg.set_default;
                    let _ = t.cmd_tx.send(TrayCmd::SetDefaultToggle(t.cfg.set_default));
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                // Before setup: the compositor's bind dialog. After: its
                // shortcut editor (ConfigureShortcuts) — the bind dialog
                // only ever shows for unconfigured shortcuts, so the label
                // must promise what the click actually does.
                label: if self.cfg.shortcuts_setup {
                    tr!("tray-shortcuts-change")
                } else {
                    tr!("tray-shortcuts-setup")
                },
                // The compositor owns the keys and the dialog; this entry
                // only opens it. Hidden while the portal has not answered
                // (no GlobalShortcuts on this desktop, or it is down).
                visible: self.shortcuts_available,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.cmd_tx.send(TrayCmd::SetupShortcuts);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: tr!("tray-autostart"),
                checked: self.cfg.autostart,
                activate: Box::new(|t: &mut Self| {
                    t.cfg.autostart = !t.cfg.autostart;
                    let _ = t.cmd_tx.send(TrayCmd::SetAutostart(t.cfg.autostart));
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: tr!("tray-about"),
                icon_name: "help-about".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.cmd_tx.send(TrayCmd::About);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: tr!("tray-quit"),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.cmd_tx.send(TrayCmd::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
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
                // GTK keys the badge's red on the `error` class.
                assert_eq!(
                    svg.contains(r#"class="ColorScheme-NegativeText error""#),
                    matches!(s, TrayStatus::Error),
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
        let mut tray = test_tray(false);
        tray.status = TrayStatus::Active;
        assert_eq!(tray.icon_name(), "hushmic-tray");
        tray.icon_style = IconStyle::Symbolic;
        assert_eq!(tray.icon_name(), "hushmic-mono-symbolic");
        // The embedded pixmap fallback stays the coloured set either way.
        assert!(!tray.icon_pixmap().is_empty());
    }

    #[test]
    fn title_reflects_status() {
        let mut tray = test_tray(false);
        for (status, want) in [
            (TrayStatus::Active, "HushMic"),
            (TrayStatus::Off, "HushMic"),
            (TrayStatus::Bypass, "HushMic (bypass)"),
            (TrayStatus::Mute, "HushMic (muted)"),
            (TrayStatus::Error, "HushMic (error)"),
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
        assert_eq!(tray.title(), "HushMic (passthrough)");
        tray.status = TrayStatus::Mute;
        assert_eq!(tray.title(), "HushMic (muted)");
        tray.status = TrayStatus::Active;
        tray.engine = Some(EngineTier::Light);
        tray.engine_light_configured = true;
        assert_eq!(tray.title(), "HushMic");
    }

    fn model_labels(tray: &HushMicTray) -> Vec<String> {
        tray.menu()
            .iter()
            .find_map(|i| match i {
                MenuItem::SubMenu(s) if s.label == "Model" => Some(s),
                _ => None,
            })
            .expect("Model submenu")
            .submenu
            .iter()
            .find_map(|i| match i {
                MenuItem::RadioGroup(g) => {
                    Some(g.options.iter().map(|o| o.label.clone()).collect())
                }
                _ => None,
            })
            .expect("model radio group")
    }

    #[test]
    fn model_menu_says_which_tier_runs_now() {
        // The selection stays on the configured model; the entry in use
        // says so while the chain is on another tier (issue #14).
        let mut tray = test_tray(false);
        tray.cfg.model = "dpdfnet8_48khz_hr".into();
        tray.status = TrayStatus::Active;
        let plain = vec![
            "High quality (dpdfnet8)".to_string(),
            "Light / low-CPU (dpdfnet2)".to_string(),
        ];
        assert_eq!(model_labels(&tray), plain);
        tray.engine = Some(EngineTier::Light);
        assert_eq!(
            model_labels(&tray),
            vec![
                "High quality (dpdfnet8)".to_string(),
                "Light / low-CPU (dpdfnet2) (running now)".to_string(),
            ]
        );
        tray.engine = Some(EngineTier::Passthrough);
        assert_eq!(
            model_labels(&tray),
            vec![
                "High quality (dpdfnet8) (paused, mic on without filtering)".to_string(),
                "Light / low-CPU (dpdfnet2)".to_string(),
            ]
        );
        // Off or bypassed: nothing to annotate. Light configured: plain too.
        tray.status = TrayStatus::Bypass;
        assert_eq!(model_labels(&tray), plain);
        tray.status = TrayStatus::Active;
        tray.cfg.model = "dpdfnet2_48khz_hr".into();
        tray.engine = Some(EngineTier::Light);
        tray.engine_light_configured = true;
        assert_eq!(model_labels(&tray), plain);
        // Per-mic profile: the global setting is the quality model, the
        // chain runs the light one for this microphone. Passthrough pauses
        // the model the chain actually loaded, so the light entry is the
        // one that says so.
        tray.cfg.model = "dpdfnet8_48khz_hr".into();
        tray.engine = Some(EngineTier::Passthrough);
        tray.engine_light_configured = true;
        assert_eq!(
            model_labels(&tray),
            vec![
                "High quality (dpdfnet8)".to_string(),
                "Light / low-CPU (dpdfnet2) (paused, mic on without filtering)".to_string(),
            ]
        );
    }

    fn mode_radio(menu: &[MenuItem<HushMicTray>]) -> &RadioGroup<HushMicTray> {
        // The mode radio lives in a "Mode" submenu right below "Test my
        // mic…", matching the Microphone/Model/strength submenu pattern.
        let MenuItem::SubMenu(s) = &menu[1] else {
            panic!("second menu item must be the Mode submenu");
        };
        assert_eq!(s.label, "Mode");
        match s.submenu.first() {
            Some(MenuItem::RadioGroup(g)) => g,
            _ => panic!("Mode submenu must hold the mode radio group"),
        }
    }

    #[test]
    fn mode_radio_labels_and_selection() {
        let mut tray = test_tray(false); // Config::default() is enabled
        let menu = tray.menu();
        let g = mode_radio(&menu);
        let labels: Vec<&str> = g.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Noise suppression", "Bypass", "Mute", "Off"]);
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
        crate::i18n::pin_english();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut tray = HushMicTray {
            cfg: Config::default(),
            mics: vec![],
            cmd_tx: tx,
            status: TrayStatus::Active,
            testing: false,
            fallback_active: false,
            mode: RunMode::Suppress,
            shortcuts_available: false,
            engine: None,
            engine_light_configured: false,
            icon_style: IconStyle::Color,
        };
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
        assert!(matches!(rx.try_recv(), Ok(TrayCmd::SetMode(None))));
        assert!(!tray.cfg.enabled, "Off maps onto the disable path");

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
        let tray = test_tray(false);
        std::env::remove_var("HUSHMIC_TRAY_THEME_DIR");
        assert_eq!(tray.icon_theme_path(), "");
        std::env::set_var("HUSHMIC_TRAY_THEME_DIR", "/opt/hushmic/icons");
        assert_eq!(tray.icon_theme_path(), "/opt/hushmic/icons");
        std::env::remove_var("HUSHMIC_TRAY_THEME_DIR");
        assert_eq!(tray.icon_theme_path(), "");
    }

    fn test_tray(testing: bool) -> HushMicTray {
        // Labels are asserted in English; pin the catalog before the
        // process-global loader initializes lazily.
        crate::i18n::pin_english();
        let (tx, _rx) = std::sync::mpsc::channel();
        HushMicTray {
            cfg: Config::default(),
            mics: vec![Source {
                name: "alsa_input.test".into(),
                description: "Test Mic".into(),
            }],
            cmd_tx: tx,
            status: TrayStatus::Off,
            testing,
            fallback_active: false,
            mode: RunMode::Suppress,
            shortcuts_available: false,
            engine: None,
            engine_light_configured: false,
            icon_style: IconStyle::Color,
        }
    }

    fn mic_labels(tray: &HushMicTray) -> Vec<String> {
        tray.menu()
            .iter()
            .find_map(|i| match i {
                MenuItem::SubMenu(s) if s.label == "Microphone" => Some(s),
                _ => None,
            })
            .expect("Microphone submenu")
            .submenu
            .iter()
            .find_map(|i| match i {
                MenuItem::RadioGroup(g) => {
                    Some(g.options.iter().map(|o| o.label.clone()).collect())
                }
                _ => None,
            })
            .expect("mic radio group")
    }

    #[test]
    fn missing_mic_label_reflects_fallback_state() {
        let mut tray = test_tray(false);
        tray.cfg.mic = Some("alsa_input.rode".into());
        // Not yet fallen back (or recovery can't run): plain truth.
        let labels = mic_labels(&tray);
        assert_eq!(
            labels.last().map(String::as_str),
            Some("alsa_input.rode (unavailable)")
        );
        // Fallback engaged: say what the chain is actually doing.
        tray.fallback_active = true;
        let labels = mic_labels(&tray);
        assert_eq!(
            labels.last().map(String::as_str),
            Some("alsa_input.rode (unplugged — using system default)")
        );
        // A present mic never gets either suffix.
        tray.cfg.mic = Some("alsa_input.test".into());
        let labels = mic_labels(&tray);
        assert!(
            !labels
                .iter()
                .any(|l| l.contains("unavailable") || l.contains("unplugged")),
            "{labels:?}"
        );
    }

    fn mic_test_item(menu: &[MenuItem<HushMicTray>]) -> &StandardItem<HushMicTray> {
        menu.iter()
            .find_map(|i| match i {
                MenuItem::Standard(s)
                    if s.label.starts_with("Test my mic") || s.label.starts_with("Mic test") =>
                {
                    Some(s)
                }
                _ => None,
            })
            .expect("mic test item present")
    }

    #[test]
    fn menu_builds_non_empty() {
        let menu = test_tray(false).menu();
        assert!(!menu.is_empty(), "tray menu should not be empty");
    }

    #[test]
    fn mic_test_item_disables_while_running() {
        let idle = test_tray(false).menu();
        let item = mic_test_item(&idle);
        assert_eq!(item.label, "Test my mic…");
        assert!(item.enabled);

        let busy = test_tray(true).menu();
        let item = mic_test_item(&busy);
        assert_eq!(item.label, "Mic test running…");
        assert!(!item.enabled);
    }

    #[test]
    fn mic_test_item_activate_sends_the_command() {
        crate::i18n::pin_english();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut tray = HushMicTray {
            cfg: Config::default(),
            mics: vec![],
            cmd_tx: tx,
            status: TrayStatus::Off,
            testing: false,
            fallback_active: false,
            mode: RunMode::Suppress,
            shortcuts_available: false,
            engine: None,
            engine_light_configured: false,
            icon_style: IconStyle::Color,
        };
        let menu = tray.menu();
        let item = mic_test_item(&menu);
        (item.activate)(&mut tray);
        assert!(
            matches!(rx.try_recv(), Ok(TrayCmd::TestMic)),
            "activating the item must send TrayCmd::TestMic"
        );
        assert!(rx.try_recv().is_err(), "exactly one command per activation");
    }

    #[test]
    fn about_item_sits_above_quit_and_sends_the_command() {
        crate::i18n::pin_english();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut tray = HushMicTray {
            cfg: Config::default(),
            mics: vec![],
            cmd_tx: tx,
            status: TrayStatus::Off,
            testing: false,
            fallback_active: false,
            mode: RunMode::Suppress,
            shortcuts_available: false,
            engine: None,
            engine_light_configured: false,
            icon_style: IconStyle::Color,
        };
        let menu = tray.menu();
        let pos = |label: &str| {
            menu.iter()
                .position(|i| matches!(i, MenuItem::Standard(s) if s.label == label))
        };
        let about = pos("About HushMic…").expect("About item present");
        let quit = pos("Quit").expect("Quit item present");
        assert!(about < quit, "About must sit above Quit");
        let MenuItem::Standard(item) = &menu[about] else {
            unreachable!()
        };
        (item.activate)(&mut tray);
        assert!(
            matches!(rx.try_recv(), Ok(TrayCmd::About)),
            "activating the item must send TrayCmd::About"
        );
        assert!(rx.try_recv().is_err(), "exactly one command per activation");
    }

    fn shortcuts_item(menu: &[MenuItem<HushMicTray>]) -> &StandardItem<HushMicTray> {
        menu.iter()
            .find_map(|i| match i {
                MenuItem::Standard(s) if s.label.ends_with("shortcuts…") => Some(s),
                _ => None,
            })
            .expect("shortcuts item present in the layout")
    }

    #[test]
    fn shortcuts_label_flips_once_set_up() {
        // Fresh install: first-time wording. After the bind dialog has
        // succeeded once, the click opens the compositor's editor and the
        // label must promise that instead.
        let mut tray = test_tray(false);
        assert_eq!(shortcuts_item(&tray.menu()).label, "Set up shortcuts…");
        tray.cfg.shortcuts_setup = true;
        assert_eq!(shortcuts_item(&tray.menu()).label, "Change shortcuts…");
    }

    #[test]
    fn shortcuts_item_is_portal_gated_and_sends_the_command() {
        // No portal answered (the default): hidden, not explained — the
        // can_set_default pattern. The item still exists in the layout so
        // visibility can flip without a menu rebuild.
        let tray = test_tray(false);
        assert!(!shortcuts_item(&tray.menu()).visible);

        let (tx, rx) = std::sync::mpsc::channel();
        let mut tray = HushMicTray {
            cfg: Config::default(),
            mics: vec![],
            cmd_tx: tx,
            status: TrayStatus::Off,
            testing: false,
            fallback_active: false,
            mode: RunMode::Suppress,
            shortcuts_available: true,
            engine: None,
            engine_light_configured: false,
            icon_style: IconStyle::Color,
        };
        let menu = tray.menu();
        let item = shortcuts_item(&menu);
        assert!(item.visible, "portal present => entry visible");
        (item.activate)(&mut tray);
        assert!(
            matches!(rx.try_recv(), Ok(TrayCmd::SetupShortcuts)),
            "activating the item must send TrayCmd::SetupShortcuts"
        );
        assert!(rx.try_recv().is_err(), "exactly one command per activation");
    }

    #[test]
    fn menu_groups_are_separated_as_designed() {
        let menu = test_tray(false).menu();
        let mut groups: Vec<Vec<&str>> = vec![vec![]];
        for item in &menu {
            match item {
                MenuItem::Separator => groups.push(vec![]),
                MenuItem::Standard(s) => groups.last_mut().unwrap().push(&s.label),
                MenuItem::Checkmark(c) => groups.last_mut().unwrap().push(&c.label),
                MenuItem::SubMenu(m) => groups.last_mut().unwrap().push(&m.label),
                MenuItem::RadioGroup(_) => {}
            }
        }
        assert_eq!(
            groups,
            vec![
                vec!["Test my mic…", "Mode"],
                vec![
                    "Microphone",
                    "Model",
                    "Suppression strength",
                    "Set as default microphone",
                    "Set up shortcuts…",
                ],
                vec!["Start on login", "About HushMic…", "Quit"],
            ]
        );
    }
}
