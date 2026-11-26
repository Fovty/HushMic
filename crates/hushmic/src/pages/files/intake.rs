//! What a drop or the chooser hands the page, sorted before the daemon
//! sees it: folders open into the recordings in them, what surely isn't a
//! recording stays out (with a short note), a file already on the page
//! isn't added twice, and a copy that looks cleaned already is added with
//! a gentle word. Off the window's thread: a folder on a slow share may
//! take a while to list.

use std::path::{Path, PathBuf};

use crate::files::gather::{is_recording, looks_cleaned, original_in, walk};
use crate::tr;

/// A folder is looked into this deep (its subfolders' subfolders).
const DEPTH: usize = 3;
/// At most this many recordings from one drop.
const MOST: usize = 500;

/// What the page adds, and what it says about the rest.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Intake {
    pub add: Vec<PathBuf>,
    /// Already on the page: the first of them is selected.
    pub already: Vec<PathBuf>,
    pub notes: Vec<Note>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    /// Not audio or video (its name and its first bytes say so).
    NotMedia(Vec<String>),
    /// Already on the page.
    Already(Vec<String>),
    /// Its name says it was cleaned already (added all the same).
    Cleaned(String),
    /// A folder with no recording in it.
    EmptyFolder(String),
    /// HushMic's own cleaned copies of files in the same drop, left out
    /// (cleaning them again would write "(cleaned) (cleaned)").
    SkippedCleaned(Vec<String>),
    /// The drop held more than [`MOST`] recordings: the first are added.
    Capped,
}

impl Note {
    /// The note's sentence.
    pub fn words(&self) -> String {
        match self {
            // named, so a file that went is not missed (F4)
            Note::NotMedia(n) => match n.as_slice() {
                [one] => tr!("files-note-not-media", name = one.clone()),
                [a, b] => tr!("files-note-not-media-two", a = a.clone(), b = b.clone()),
                [a, b, rest @ ..] => tr!(
                    "files-note-not-media-many",
                    names = format!("{a}, {b}"),
                    count = rest.len()
                ),
                [] => String::new(),
            },
            Note::Already(n) if n.len() == 1 => tr!("files-note-already", name = n[0].clone()),
            Note::Already(n) => tr!("files-note-already-many", count = n.len()),
            Note::Cleaned(n) => tr!("files-note-cleaned", name = n.clone()),
            Note::EmptyFolder(f) => tr!("files-note-empty-folder", folder = f.clone()),
            Note::SkippedCleaned(n) if n.len() == 1 => {
                tr!("files-note-skipped-cleaned", name = n[0].clone())
            }
            Note::SkippedCleaned(n) => tr!("files-note-skipped-cleaned-many", count = n.len()),
            Note::Capped => tr!("files-note-capped", count = MOST),
        }
    }
}

impl Intake {
    /// What the page says: its notes, the first two (a toast is short).
    pub fn words(&self) -> Option<String> {
        let w: Vec<String> = self.notes.iter().take(2).map(Note::words).collect();
        (!w.is_empty()).then(|| w.join(" "))
    }
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Sort `paths` (dropped or chosen) against the files on the page now
/// (`have`).
pub fn sort(paths: Vec<PathBuf>, have: &[PathBuf]) -> Intake {
    let mut out = Intake::default();
    let mut not_media = Vec::new();
    let mut already = Vec::new();
    let mut cleaned = None;
    let mut capped = false;
    // the recordings of the drop, folders opened, in order
    let mut found_all: Vec<PathBuf> = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut found = Vec::new();
            walk(&p, DEPTH, MOST, &mut found);
            found.sort();
            if found.is_empty() {
                out.notes.push(Note::EmptyFolder(name(&p)));
            }
            found_all.extend(found);
            continue;
        }
        // a recording's name unless its bytes say it surely isn't one;
        // another name only when its bytes say it is one
        if is_recording(&p) {
            found_all.push(p);
        } else {
            not_media.push(name(&p));
        }
    }
    // our own outputs next to their originals: left out
    let (ours, found_all): (Vec<PathBuf>, Vec<PathBuf>) = found_all
        .iter()
        .cloned()
        .partition(|p| original_in(p, &found_all));
    let skipped: Vec<String> = ours.iter().map(|p| name(p)).collect();
    for p in found_all {
        // twice in one drop: once
        if out.add.contains(&p) || out.already.contains(&p) {
            continue;
        }
        if have.contains(&p) {
            already.push(name(&p));
            out.already.push(p);
            continue;
        }
        if out.add.len() >= MOST {
            capped = true;
            continue;
        }
        if cleaned.is_none() && looks_cleaned(&p) {
            cleaned = Some(name(&p));
        }
        out.add.push(p);
    }
    if !not_media.is_empty() {
        out.notes.insert(0, Note::NotMedia(not_media));
    }
    if !already.is_empty() {
        out.notes.push(Note::Already(already));
    }
    if !skipped.is_empty() {
        out.notes.push(Note::SkippedCleaned(skipped));
    }
    if let Some(c) = cleaned {
        out.notes.push(Note::Cleaned(c));
    }
    if capped {
        out.notes.insert(0, Note::Capped);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::gather::cleaned_stem;

    #[test]
    fn a_drop_is_sorted_before_it_is_added() {
        crate::i18n::pin_english();
        let dir = std::env::temp_dir().join(format!("hushmic-intake-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("Takes/Day 2");
        std::fs::create_dir_all(&sub).unwrap();
        let wav = |p: &Path| {
            let mut b = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
            b.resize(64, 0);
            std::fs::write(p, b).unwrap();
        };
        let a = dir.join("a.wav");
        wav(&a);
        wav(&dir.join("Takes/b.wav"));
        wav(&sub.join("c.wav"));
        std::fs::write(dir.join("Takes/notes.txt"), "hello\n").unwrap();
        wav(&dir.join("Takes/.hidden.wav"));
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, "the plan\n").unwrap();
        let fake = dir.join("fake.mp3");
        std::fs::write(&fake, "not sound at all\n").unwrap();
        // a recording without a recording's name: by its bytes
        let raw = dir.join("rec-0001");
        wav(&raw);
        let again = dir.join("talk (cleaned 2).wav");
        wav(&again);
        let empty = dir.join("Empty");
        std::fs::create_dir_all(&empty).unwrap();
        let i = sort(
            vec![
                a.clone(),
                txt,
                dir.join("Takes"),
                fake,
                raw.clone(),
                again.clone(),
                empty,
            ],
            std::slice::from_ref(&a),
        );
        assert_eq!(
            i.add,
            [
                dir.join("Takes/Day 2/c.wav"),
                dir.join("Takes/b.wav"),
                raw,
                again
            ]
        );
        assert_eq!(i.already, [a]);
        assert_eq!(
            i.notes,
            [
                Note::NotMedia(vec!["notes.txt".into(), "fake.mp3".into()]),
                Note::EmptyFolder("Empty".into()),
                Note::Already(vec!["a.wav".into()]),
                Note::Cleaned("talk (cleaned 2).wav".into()),
            ]
        );
        assert_eq!(
            i.words().as_deref(),
            Some(
                "notes.txt and fake.mp3 aren’t audio or video files. \
                 Empty has no audio or video files."
            )
        );
        let many = Note::NotMedia(vec!["a.txt".into(), "b.pdf".into(), "c.doc".into()]);
        assert_eq!(
            many.words(),
            "a.txt, b.pdf and 1 more aren’t audio or video files."
        );
        let one = sort(vec![dir.join("notes.txt")], &[]);
        assert_eq!(
            one.words().as_deref(),
            Some("notes.txt isn’t an audio or video file.")
        );
        assert!(looks_cleaned(Path::new("/a/x (cleaned).m4a")));
        assert_eq!(
            cleaned_stem(Path::new("/a/ep 1 (cleaned 2).m4a")).as_deref(),
            Some("ep 1")
        );
        assert!(!looks_cleaned(Path::new("/a/x (cleaned up).m4a")));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn wav(p: &Path) {
        let mut b = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        b.resize(64, 0);
        std::fs::write(p, b).unwrap();
    }

    /// A podcaster's folder from an earlier run: the cleaned copies next to
    /// their originals are HushMic's own and stay out; a cleaned copy
    /// added on its own only gets the gentle word.
    #[test]
    fn our_own_cleaned_copies_stay_out_of_a_drop() {
        crate::i18n::pin_english();
        let dir = std::env::temp_dir().join(format!("hushmic-intake-ours-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let show = dir.join("Show");
        std::fs::create_dir_all(&show).unwrap();
        for n in [
            "ep1.m4a",
            "ep1 (cleaned).m4a",
            "ep2.wav",
            "ep2 (cleaned 2).wav",
            "ep3 (cleaned).m4a",
        ] {
            wav(&show.join(n));
        }
        let i = sort(vec![show.clone()], &[]);
        assert_eq!(
            i.add,
            [
                show.join("ep1.m4a"),
                show.join("ep2.wav"),
                show.join("ep3 (cleaned).m4a")
            ]
        );
        assert_eq!(
            i.notes,
            [
                Note::SkippedCleaned(vec![
                    "ep1 (cleaned).m4a".into(),
                    "ep2 (cleaned 2).wav".into()
                ]),
                Note::Cleaned("ep3 (cleaned).m4a".into()),
            ]
        );
        assert!(i
            .words()
            .unwrap()
            .starts_with("Left out 2 cleaned copies of files in the drop."));
        // dropped as files, side by side: the same
        let both = sort(
            vec![show.join("ep1.m4a"), show.join("ep1 (cleaned).m4a")],
            &[],
        );
        assert_eq!(both.add, [show.join("ep1.m4a")]);
        // on its own (its original not in the drop): added, with the word
        let alone = sort(vec![show.join("ep1 (cleaned).m4a")], &[]);
        assert_eq!(alone.add, [show.join("ep1 (cleaned).m4a")]);
        assert_eq!(alone.notes, [Note::Cleaned("ep1 (cleaned).m4a".into())]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A drop of more than 500 recordings adds the first 500 and says so.
    #[test]
    fn a_cut_drop_says_so() {
        crate::i18n::pin_english();
        let dir = std::env::temp_dir().join(format!("hushmic-intake-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for k in 0..MOST + 3 {
            wav(&dir.join(format!("take {k:04}.wav")));
        }
        let i = sort(vec![dir.clone()], &[]);
        assert_eq!(i.add.len(), MOST);
        assert_eq!(i.notes, [Note::Capped]);
        assert_eq!(i.words().as_deref(), Some("Added the first 500 files."));
        // exactly 500: nothing to say
        let all: Vec<PathBuf> = i.add.clone();
        assert!(sort(all, &[]).notes.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
