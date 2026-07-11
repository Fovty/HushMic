// ksni menu literals already set every field; the trailing `..Default::default()`
// is kept intentionally as forward-compat across ksni versions.
#![allow(clippy::needless_update)]

use crate::config::Config;
use crate::pipewire::Source;
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, Tray};
use std::sync::mpsc::Sender;

#[derive(Debug)]
pub enum TrayCmd {
    SetEnabled(bool),
    SelectMic(Option<String>),
    SelectModel(String),
    SetAttn(f32),
    SetDefaultToggle(bool),
    SetAutostart(bool),
    TestMic,
    About,
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayStatus {
    Off,
    Active,
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
            TrayStatus::Error => "hushmic-tray-error",
        }
    }
    pub fn title_suffix(&self) -> &'static str {
        match self {
            TrayStatus::Error => " (error)",
            _ => "",
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
}

const MODELS: &[(&str, &str)] = &[
    ("dpdfnet8_48khz_hr", "High quality (dpdfnet8)"),
    ("dpdfnet2_48khz_hr", "Light / low-CPU (dpdfnet2)"),
];
const ATTN_PRESETS: &[(f32, &str)] = &[
    (100.0, "Maximum"),
    (24.0, "Strong (24 dB)"),
    (12.0, "Medium (12 dB)"),
    (6.0, "Light (6 dB)"),
];

impl Tray for HushMicTray {
    fn id(&self) -> String {
        "hushmic".into()
    }
    fn title(&self) -> String {
        format!("HushMic{}", self.status.title_suffix())
    }
    fn icon_name(&self) -> String {
        self.status.icon_name().into()
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
                for px in data.chunks_exact_mut(4) {
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
            label: "System default".into(),
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
            mic_opts.push(RadioItem {
                label: format!("{name} (unavailable)"),
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
            .position(|(id, _)| *id == self.cfg.model)
            .unwrap_or(0);
        let attn_selected = ATTN_PRESETS
            .iter()
            .position(|(v, _)| (*v - self.cfg.attn_limit).abs() < 0.5)
            .unwrap_or(0);

        vec![
            CheckmarkItem {
                label: "Enable noise suppression".into(),
                checked: self.cfg.enabled,
                activate: Box::new(|t: &mut Self| {
                    t.cfg.enabled = !t.cfg.enabled;
                    let _ = t.cmd_tx.send(TrayCmd::SetEnabled(t.cfg.enabled));
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if self.testing {
                    "Mic test running…".into()
                } else {
                    "Test my mic…".into()
                },
                icon_name: "audio-input-microphone".into(),
                enabled: !self.testing,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.cmd_tx.send(TrayCmd::TestMic);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            SubMenu {
                label: "Microphone".into(),
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
                label: "Model".into(),
                submenu: vec![RadioGroup {
                    selected: model_selected,
                    select: Box::new(|t: &mut Self, idx| {
                        let id = MODELS[idx].0.to_string();
                        t.cfg.model = id.clone();
                        let _ = t.cmd_tx.send(TrayCmd::SelectModel(id));
                    }),
                    options: MODELS
                        .iter()
                        .map(|(_, label)| RadioItem {
                            label: (*label).into(),
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
                label: "Suppression strength".into(),
                submenu: vec![RadioGroup {
                    selected: attn_selected,
                    select: Box::new(|t: &mut Self, idx| {
                        let v = ATTN_PRESETS[idx].0;
                        t.cfg.attn_limit = v;
                        let _ = t.cmd_tx.send(TrayCmd::SetAttn(v));
                    }),
                    options: ATTN_PRESETS
                        .iter()
                        .map(|(_, label)| RadioItem {
                            label: (*label).into(),
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
                label: "Set as default microphone".into(),
                checked: self.cfg.set_default,
                activate: Box::new(|t: &mut Self| {
                    t.cfg.set_default = !t.cfg.set_default;
                    let _ = t.cmd_tx.send(TrayCmd::SetDefaultToggle(t.cfg.set_default));
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: "Start on login".into(),
                checked: self.cfg.autostart,
                activate: Box::new(|t: &mut Self| {
                    t.cfg.autostart = !t.cfg.autostart;
                    let _ = t.cmd_tx.send(TrayCmd::SetAutostart(t.cfg.autostart));
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "About HushMic…".into(),
                icon_name: "help-about".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.cmd_tx.send(TrayCmd::About);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
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
        assert_ne!(Off.icon_name(), Active.icon_name());
        assert_ne!(Active.icon_name(), Error.icon_name());
        assert_ne!(Off.icon_name(), Error.icon_name());
        for s in [Off, Active, Error] {
            assert!(
                s.icon_name().starts_with("hushmic-tray"),
                "{s:?} icon must come from the shipped hushmic-tray set"
            );
        }
        assert_eq!(Error.title_suffix(), " (error)");
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
        }
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
        let (tx, rx) = std::sync::mpsc::channel();
        let mut tray = HushMicTray {
            cfg: Config::default(),
            mics: vec![],
            cmd_tx: tx,
            status: TrayStatus::Off,
            testing: false,
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
        let (tx, rx) = std::sync::mpsc::channel();
        let mut tray = HushMicTray {
            cfg: Config::default(),
            mics: vec![],
            cmd_tx: tx,
            status: TrayStatus::Off,
            testing: false,
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
                vec!["Enable noise suppression", "Test my mic…"],
                vec![
                    "Microphone",
                    "Model",
                    "Suppression strength",
                    "Set as default microphone",
                ],
                vec!["Start on login", "About HushMic…", "Quit"],
            ]
        );
    }
}
