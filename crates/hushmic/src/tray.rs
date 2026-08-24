// ksni menu literals already set every field; the trailing `..Default::default()`
// is kept intentionally as forward-compat across ksni versions.
#![allow(clippy::needless_update)]

use crate::config::Config;
use crate::controller::RunMode;
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
}

// The option tables hold ids/values only; labels come from the catalog via
// the match functions below (the message lookup needs literal keys).
const MODELS: &[&str] = &["dpdfnet8_48khz_hr", "dpdfnet2_48khz_hr"];
const ATTN_PRESETS: &[f32] = &[100.0, 24.0, 12.0, 6.0];

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
            _ => tr!("tray-title"),
        }
    }
    fn icon_name(&self) -> String {
        // Inside a Flatpak only app-ID-prefixed icon names are exported to
        // the host theme (~/.local/share/flatpak/exports/share/icons on the
        // host XDG_DATA_DIRS), so the SNI host can resolve
        // `<app-id>-tray[-off|-error]` but never the bare `hushmic-tray`
        // set — the manifest installs the ladder under the prefixed names
        // only. The pixmap fallback below stays the safety net for hosts
        // that resolve nothing.
        match crate::sandbox::flatpak_app_id() {
            Some(id) => {
                let suffix = self
                    .status
                    .icon_name()
                    .strip_prefix("hushmic")
                    .expect("tray icon names start with 'hushmic'");
                format!("{id}{suffix}")
            }
            None => self.status.icon_name().into(),
        }
    }
    fn icon_theme_path(&self) -> String {
        // Read per call, not cached: ksni re-queries on every property fetch
        // and the var is set once by the AppImage wrapper before launch.
        // Unset => empty string => the host falls back to the system theme.
        std::env::var("HUSHMIC_TRAY_THEME_DIR").unwrap_or_default()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        // Embedded copies of the same icons, for setups where the named
        // lookup cannot resolve (raw cargo-build runs without the installed
        // ladder, SNI hosts that ignore IconThemePath). Hosts prefer
        // IconName whenever it resolves.
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
                            label: model_label(id),
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
