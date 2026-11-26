//! "2 recordings are ready.": the note when the files on the Files page
//! are done while the window is closed or in the background, with Save all
//! and Show. When every one of them was saved, "Saved 2 recordings." and
//! where, and Show in folder opens it; after the note's own Save all, the
//! same (or how many weren't saved).

use super::jobs::Cleaned;
use crate::tr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What the note says and what its buttons do.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub summary: String,
    /// Where they were saved, that they aren't yet, or nothing.
    pub body: String,
    /// The buttons, the first the default.
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// "Show": the window, on Files.
    Files,
    /// "Show in folder": one of the saved files, in its folder.
    Folder(PathBuf),
    /// "Save all" ("Save" for one): every one, as the page's Save all.
    SaveAll { one: bool },
}

impl Action {
    /// The notification's action key and label.
    pub fn button(&self) -> (&'static str, String) {
        match self {
            Action::Files => ("show", tr!("notify-files-show")),
            Action::Folder(_) => ("folder", tr!("notify-files-show-in-folder")),
            Action::SaveAll { one: true } => ("save-all", tr!("notify-files-save")),
            Action::SaveAll { one: false } => ("save-all", tr!("notify-files-save-all")),
        }
    }
}

/// The note for the files cleaned (or saved by the note's Save all). Not
/// every one saved: they are ready, with Save all and Show. Every one
/// saved: next to their originals, or all in one folder, it says so; one
/// folder for them all: Show in folder opens it, more than one: Show
/// opens Files. Some of the note's Save all failed: how many, and Show.
pub fn note(c: &Cleaned) -> Note {
    if c.failed > 0 {
        return Note {
            summary: tr!("notify-files-not-saved", count = c.failed),
            body: String::new(),
            actions: vec![Action::Files],
        };
    }
    let Some(saved) = c.saved.as_ref().filter(|s| !s.is_empty()) else {
        return Note {
            summary: tr!("notify-files-ready", count = c.count),
            body: tr!("notify-files-ready-body", count = c.count),
            actions: vec![Action::SaveAll { one: c.count == 1 }, Action::Files],
        };
    };
    let summary = tr!("notify-files-saved", count = saved.len());
    let plain = Note {
        summary: summary.clone(),
        body: String::new(),
        actions: vec![Action::Files],
    };
    let mut folders: Vec<&Path> = saved.iter().filter_map(|(_, s)| s.parent()).collect();
    folders.sort();
    folders.dedup();
    let next_to = saved.iter().all(|(o, s)| o.parent() == s.parent());
    let body = if next_to {
        tr!("notify-files-saved-next", count = saved.len())
    } else if let [one] = folders[..] {
        tr!(
            "notify-files-saved-in",
            folder = crate::pages::files::daemon::folder_shown(one)
        )
    } else {
        return plain;
    };
    let action = match folders[..] {
        [_] => Action::Folder(saved[0].1.clone()),
        _ => Action::Files,
    };
    Note {
        summary,
        body,
        actions: vec![action],
    }
}

/// The saved file the last note's Show in folder shows.
static SHOWN: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Send the note for `c` (a desktop notification).
pub fn send(c: &Cleaned) {
    let n = note(c);
    for a in &n.actions {
        if let Action::Folder(p) = a {
            *SHOWN.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.clone());
        }
    }
    let buttons: Vec<(&str, String)> = n.actions.iter().map(Action::button).collect();
    let buttons: Vec<(&str, &str)> = buttons.iter().map(|(k, l)| (*k, l.as_str())).collect();
    crate::notify::send_with_actions(
        crate::notify::Slot::Files,
        "audio-input-microphone",
        &n.summary,
        &n.body,
        &buttons,
    );
}

/// The note's Show in folder was clicked: the last note's file, in its
/// folder (off the caller's thread). False when there is none.
pub fn show_folder() -> bool {
    let Some(p) = SHOWN.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return false;
    };
    std::thread::spawn(move || {
        if let Err(e) = super::reveal::show_in_folder(&p) {
            eprintln!("[hushmic] show in folder: {e}");
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cleaned(saved: Option<&[(&str, &str)]>) -> Cleaned {
        Cleaned {
            failed: 0,
            count: saved.map_or(3, |s| s.len()),
            saved: saved.map(|s| {
                s.iter()
                    .map(|(o, d)| (PathBuf::from(o), PathBuf::from(d)))
                    .collect()
            }),
        }
    }

    #[test]
    fn a_note_says_where_they_were_saved_only_when_every_one_was() {
        crate::i18n::pin_english();
        // not every one saved: ready, with Save all and Show (B11)
        let n = note(&cleaned(None));
        assert_eq!(
            (n.summary.as_str(), n.body.as_str()),
            ("3 recordings are ready.", "They aren’t saved yet.")
        );
        assert_eq!(n.actions, [Action::SaveAll { one: false }, Action::Files]);
        assert_eq!(n.actions[0].button(), ("save-all", "Save all".to_string()));
        assert_eq!(n.actions[1].button(), ("show", "Show".to_string()));
        let one = Cleaned {
            count: 1,
            ..cleaned(None)
        };
        let n = note(&one);
        assert_eq!(n.summary, "1 recording is ready.");
        assert_eq!(n.actions[0].button(), ("save-all", "Save".to_string()));
        // next to the originals, in one folder: Show in folder opens it
        let n = note(&cleaned(Some(&[
            ("/rec/a.wav", "/rec/a (cleaned).wav"),
            ("/rec/b.m4a", "/rec/b (cleaned).m4a"),
            ("/rec/c.flac", "/rec/c (cleaned).flac"),
        ])));
        assert_eq!(n.summary, "Saved 3 recordings.");
        assert_eq!(n.body, "Saved next to the originals.");
        assert_eq!(n.actions, [Action::Folder("/rec/a (cleaned).wav".into())]);
        assert_eq!(
            n.actions[0].button(),
            ("folder", "Show in folder".to_string())
        );
        let n = note(&cleaned(Some(&[("/rec/a.wav", "/rec/a (cleaned).wav")])));
        assert_eq!(n.summary, "Saved 1 recording.");
        assert_eq!(n.body, "Saved next to the original.");
        // next to the originals, in several folders: Show opens Files
        let n = note(&cleaned(Some(&[
            ("/rec/a.wav", "/rec/a (cleaned).wav"),
            ("/talks/b.wav", "/talks/b (cleaned).wav"),
        ])));
        assert_eq!(n.body, "Saved next to the originals.");
        assert_eq!(n.actions, [Action::Files]);
        // all in the chosen folder
        let n = note(&cleaned(Some(&[
            ("/rec/a.wav", "/home/you/Recordings/Cleaned/a (cleaned).wav"),
            (
                "/talks/b.wav",
                "/home/you/Recordings/Cleaned/b (cleaned).wav",
            ),
        ])));
        assert_eq!(n.body, "Saved in Recordings/Cleaned.");
        assert_eq!(
            n.actions,
            [Action::Folder(
                "/home/you/Recordings/Cleaned/a (cleaned).wav".into()
            )]
        );
        // partly next to the originals, partly elsewhere: as before
        let n = note(&cleaned(Some(&[
            ("/rec/a.wav", "/rec/a (cleaned).wav"),
            ("/talks/b.wav", "/home/you/Cleaned/b (cleaned).wav"),
        ])));
        assert_eq!(
            (n.body.as_str(), &n.actions[..]),
            ("", &[Action::Files][..])
        );
        // the note's Save all, one of which failed
        let n = note(&Cleaned {
            failed: 1,
            ..cleaned(Some(&[("/rec/a.wav", "/rec/a (cleaned).wav")]))
        });
        assert_eq!(n.summary, "1 recording wasn’t saved.");
        assert_eq!(n.actions, [Action::Files]);
    }
}
