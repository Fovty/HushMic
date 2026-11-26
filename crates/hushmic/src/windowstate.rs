//! The small state file the daemon keeps for the window: the page last
//! shown, first run's step, and whether the first-start beam is used up.
//! Not the user's config (nothing here is a setting), and only the daemon
//! writes it; the window asks through `ui …` requests. Lives in the state
//! dir (`$XDG_STATE_HOME/hushmic/window.json`).
//!
//! The page is restored only for a window that is re-created (a relaunch
//! while one is open). A fresh open lands on Live, or on first run while it
//! is under way: a launch never reopens the page the last window showed.

use crate::appstate::{Page, UiState};
use std::path::PathBuf;

/// First run's steps.
pub const FIRST_RUN_STEPS: u8 = 3;

pub fn path() -> PathBuf {
    let dirs = directories::ProjectDirs::from("io", "hushmic", "hushmic").expect("home dir");
    dirs.state_dir()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| dirs.data_local_dir().to_path_buf())
        .join("window.json")
}

/// The saved state; None when there is no (readable) file.
pub fn load() -> Option<UiState> {
    let text = std::fs::read_to_string(path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write the state. No fsync: a write lost to a crash costs at most a
/// page or a step, never the file (it is replaced by a rename).
pub fn save(ui: &UiState) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(ui).expect("UiState serializes");
    crate::fsutil::replace(&path(), text.as_bytes())
}

/// HushMic ran here before: its config, the chain conf every start
/// writes (`chain_conf`, in the state folder; an earlier version wrote it
/// next to the config, and an upgrader who never changed a setting has
/// only that), or its login entry. Only a machine with none of them is a
/// fresh install that gets first run.
pub fn ran_before(
    config: &std::path::Path,
    chain_conf: &std::path::Path,
    autostart_entry: &std::path::Path,
) -> bool {
    config.exists()
        || chain_conf.exists()
        || config
            .parent()
            .is_some_and(|d| d.join("filter-chain.conf").exists())
        || autostart_entry.exists()
}

/// The state a daemon starts with. A fresh install (no config file and no
/// state file yet) starts first run at step 1; an install that predates
/// the window state never sees it. A first run that was left half way
/// resumes where it was (a step the file cannot mean starts it over). No
/// window is open yet, so the page is a fresh open's.
pub fn at_start(saved: Option<UiState>, config_existed: bool) -> UiState {
    let mut ui = match saved {
        Some(ui) => ui,
        None if !config_existed => UiState {
            first_run_step: Some(1),
            ..UiState::default()
        },
        None => UiState::default(),
    };
    if ui
        .first_run_step
        .is_some_and(|n| !(1..=FIRST_RUN_STEPS).contains(&n))
    {
        ui.first_run_step = Some(1);
    }
    ui.page = fresh_page(&ui, false);
    // The file jobs do not outlive a run.
    ui.file = None;
    ui
}

/// The page a window opens on when none was open: first run while it is
/// under way, else Files while files there clean or wait to be saved
/// (`files`), else Live. Never where the last window was left.
pub fn fresh_page(ui: &UiState, files: bool) -> Page {
    if ui.first_run_step.is_some() {
        Page::FirstRun
    } else if files {
        Page::Files
    } else {
        Page::Live
    }
}

/// The page a re-created window opens on (a relaunch, or Open HushMic,
/// while one is open): the one it showed, unless first run is under way
/// (or over).
pub fn reopen_page(ui: &UiState) -> Page {
    match (ui.first_run_step, ui.page) {
        (Some(_), _) => Page::FirstRun,
        (None, Page::FirstRun) => Page::Live,
        (None, p) => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_install_starts_first_run_and_an_upgrade_does_not() {
        let fresh = at_start(None, false);
        assert_eq!(fresh.first_run_step, Some(1));
        assert_eq!(fresh.page, Page::FirstRun);
        assert_eq!(fresh_page(&fresh, false), Page::FirstRun);
        assert_eq!(fresh_page(&fresh, true), Page::FirstRun, "first run first");
        assert!(!fresh.beam_used);
        let upgrade = at_start(None, true);
        assert_eq!(upgrade.first_run_step, None);
        assert_eq!(fresh_page(&upgrade, false), Page::Live);
        // files clean or wait to be saved: back on Files (B11)
        assert_eq!(fresh_page(&upgrade, true), Page::Files);
    }

    #[test]
    fn a_saved_state_wins_and_a_half_done_first_run_resumes() {
        let saved = UiState {
            page: Page::FirstRun,
            first_run_step: Some(2),
            beam_used: false,
            size: Some([1000, 700]),
            file: Some((3, 48_000)),
        };
        let ui = at_start(Some(saved), true);
        assert_eq!(ui.file, None, "the file jobs were the last run's");
        assert_eq!(ui.first_run_step, Some(2));
        assert_eq!(ui.size, Some([1000, 700]));
        assert_eq!(fresh_page(&ui, false), Page::FirstRun);
        // Done, and the config was deleted since: no first run again.
        let done = UiState {
            page: Page::Settings,
            first_run_step: None,
            beam_used: true,
            ..UiState::default()
        };
        let ui = at_start(Some(done), false);
        assert_eq!(ui.first_run_step, None);
        assert!(ui.beam_used);
    }

    /// A launch opens Live, never the page the window closed on. Only a
    /// re-created window goes back to it.
    #[test]
    fn a_launch_opens_live_and_only_a_re_creation_restores_the_page() {
        for left_on in [Page::Files, Page::Settings, Page::Shortcuts, Page::About] {
            let saved = UiState {
                page: left_on,
                ..UiState::default()
            };
            let ui = at_start(Some(saved), true);
            assert_eq!(ui.page, Page::Live, "{left_on:?}");
            assert_eq!(fresh_page(&ui, false), Page::Live);
            let open = UiState {
                page: left_on,
                ..ui
            };
            assert_eq!(fresh_page(&open, false), Page::Live, "no window open: Live");
            assert_eq!(reopen_page(&open), left_on, "re-created: where it was");
        }
    }

    #[test]
    fn a_finished_first_run_never_reopens_on_its_page() {
        let ui = UiState {
            page: Page::FirstRun,
            first_run_step: None,
            ..UiState::default()
        };
        assert_eq!(reopen_page(&ui), Page::Live);
        assert_eq!(fresh_page(&ui, false), Page::Live);
        let under_way = UiState {
            page: Page::Settings,
            first_run_step: Some(3),
            ..UiState::default()
        };
        assert_eq!(reopen_page(&under_way), Page::FirstRun);
    }

    #[test]
    fn a_hand_edited_or_corrupt_file_is_read_safely() {
        let ui: UiState =
            serde_json::from_str(r#"{"first_run_step": 9, "page": "nowhere"}"#).unwrap_or_default();
        let ui = at_start(Some(ui), true);
        assert!(matches!(
            ui.first_run_step,
            None | Some(1..=FIRST_RUN_STEPS)
        ));
        let ui: UiState = serde_json::from_str(r#"{"first_run_step": 9}"#).unwrap();
        let ui = at_start(Some(ui), true);
        assert_eq!(ui.first_run_step, Some(1), "an impossible step starts over");
        assert_eq!(ui.page, Page::FirstRun);
        let ui: UiState = serde_json::from_str(r#"{"first_run_step": 0}"#).unwrap();
        assert_eq!(at_start(Some(ui), true).first_run_step, Some(1));
    }

    #[test]
    fn an_upgrader_who_never_changed_a_setting_is_no_fresh_install() {
        let dir = std::env::temp_dir().join(format!("hushmic-ran-before-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config = dir.join("hushmic/config.toml");
        let conf = dir.join("state/hushmic/filter-chain.conf");
        let entry = dir.join("autostart/hushmic.desktop");
        assert!(!ran_before(&config, &conf, &entry), "nothing here: fresh");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(config.with_file_name("filter-chain.conf"), "").unwrap();
        assert!(
            ran_before(&config, &conf, &entry),
            "an earlier version's start wrote the chain conf"
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
        std::fs::write(&conf, "").unwrap();
        assert!(
            ran_before(&config, &conf, &entry),
            "a start wrote it (or moved it) to the state folder"
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(&entry, "").unwrap();
        assert!(ran_before(&config, &conf, &entry), "starts on login");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file another version wrote (keys this one does not know, a key
    /// it no longer has) still loads.
    #[test]
    fn a_file_with_unknown_keys_loads() {
        let ui = UiState {
            beam_used: true,
            ..UiState::default()
        };
        let mut v = serde_json::to_value(&ui).unwrap();
        v["take"] = "hushmic/miccheck-take".into();
        v["later"] = 1.into();
        let back: UiState = serde_json::from_value(v).unwrap();
        assert_eq!(back, ui);
    }
}
