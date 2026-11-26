//! Recordings out of what was handed over: a drop, the chooser, or a
//! command line (`open-files`, `clean`). A folder opens into the
//! recordings in it, by their names and then their first bytes (a `.wav`
//! that is text stays out); HushMic's own cleaned copies next to their
//! originals stay out (cleaning them again would write "(cleaned)
//! (cleaned)"). The page's drop ([`crate::pages::files::intake`]) and the
//! command lines sort the same way.

use std::path::{Path, PathBuf};

use super::kinds::{self, Named, Sniffed};

/// A file is a recording, as a drop takes one: a recording's name unless
/// its bytes surely say otherwise; another name only when its bytes say it
/// is one.
pub fn is_recording(p: &Path) -> bool {
    let sniffed = kinds::sniff(p);
    match kinds::named(p) {
        Named::Audio | Named::Video => !matches!(sniffed, Sniffed::Text | Sniffed::NotMedia),
        Named::Other => sniffed == Sniffed::Media,
    }
}

/// The recordings in `dir` by their names, `depth` folders down (not into
/// linked folders), in name order, hidden ones left out; at most `most` (one more is found, so the
/// caller can tell it was cut).
pub fn walk(dir: &Path, depth: usize, most: usize, found: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if found.len() > most {
            return;
        }
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() && depth > 0 => walk(&p, depth - 1, most, found),
            Ok(t) if t.is_dir() => {}
            _ if kinds::named(&p) != Named::Other => found.push(p),
            _ => {}
        }
    }
}

/// "interview (cleaned).m4a", "talk (cleaned 2).wav", "talk (cleaned,
/// Gentle).wav": a copy HushMic wrote.
pub fn looks_cleaned(p: &Path) -> bool {
    cleaned_at(p).is_some()
}

/// Where the mark starts in a cleaned copy's stem.
fn cleaned_at(p: &Path) -> Option<usize> {
    let stem = p.file_stem()?.to_string_lossy().into_owned();
    let at = stem.rfind("(cleaned")?;
    let rest = &stem[at + "(cleaned".len()..];
    let digits = |n: &str| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit());
    let ok = rest == ")"
        || rest
            .strip_prefix(' ')
            .and_then(|r| r.strip_suffix(')'))
            .is_some_and(digits)
        // a re-save at another strength: "(cleaned, Gentle)", "(cleaned,
        // Gentle 2)", "(cleaned, 18 dB)"
        || rest
            .strip_prefix(", ")
            .and_then(|r| r.strip_suffix(')'))
            .is_some_and(|r| !r.is_empty() && !r.contains(['(', ')']));
    ok.then_some(at)
}

/// The original's stem of a cleaned copy's name, None for any other name.
pub fn cleaned_stem(p: &Path) -> Option<String> {
    let at = cleaned_at(p)?;
    let stem = p.file_stem()?.to_string_lossy().into_owned();
    Some(stem[..at].trim_end().to_string())
}

/// `p` is a cleaned copy whose original (the same name without the mark,
/// any recording's extension, the same folder) is among `among`.
pub fn original_in(p: &Path, among: &[PathBuf]) -> bool {
    let Some(stem) = cleaned_stem(p) else {
        return false;
    };
    among.iter().any(|o| {
        o.parent() == p.parent()
            && o.file_stem().is_some_and(|s| s.to_string_lossy() == stem)
            && kinds::named(o) != Named::Other
    })
}

/// How deep `-r` looks. A linked folder is never gone into (the file
/// type isn't followed: no loop, nothing outside the tree); a linked file
/// is taken.
pub const RECURSIVE_DEPTH: usize = 16;
/// At most this many recordings from the command line's folders.
pub const MOST: usize = 10_000;

/// What a command line's paths come to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gathered {
    /// The files given, as given, and the recordings in the folders given.
    pub files: Vec<PathBuf>,
    /// Files in the folders whose names say recording and whose bytes say
    /// otherwise.
    pub not_media: Vec<PathBuf>,
    /// HushMic's own cleaned copies in the folders, next to their originals.
    pub skipped: Vec<PathBuf>,
    /// Folders with no recording in them.
    pub empty: Vec<PathBuf>,
    /// More than [`MOST`] recordings in the folders: the first are taken.
    pub capped: bool,
}

/// The files to work on out of `paths`: a file stays as it is (it says
/// for itself what is wrong with it), a folder opens into its recordings,
/// its top level only unless `recursive`.
pub fn gather(paths: &[PathBuf], recursive: bool) -> Gathered {
    let mut g = Gathered::default();
    let depth = if recursive { RECURSIVE_DEPTH } else { 0 };
    let mut from_folders: Vec<PathBuf> = Vec::new();
    for p in paths {
        if !p.is_dir() {
            if !g.files.contains(p) {
                g.files.push(p.clone());
            }
            continue;
        }
        let mut found = Vec::new();
        walk(p, depth, MOST, &mut found);
        let (ok, not): (Vec<PathBuf>, Vec<PathBuf>) =
            found.into_iter().partition(|f| is_recording(f));
        if ok.is_empty() {
            g.empty.push(p.clone());
        }
        g.not_media.extend(not);
        from_folders.extend(ok);
    }
    let all: Vec<PathBuf> = g.files.iter().chain(&from_folders).cloned().collect();
    for f in from_folders {
        if original_in(&f, &all) {
            g.skipped.push(f);
        } else if g.files.len() >= MOST {
            g.capped = true;
        } else if !g.files.contains(&f) {
            g.files.push(f);
        }
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleaned_copies_are_known_by_their_mark() {
        for n in [
            "a (cleaned).wav",
            "a (cleaned 2).wav",
            "a (cleaned, Gentle).m4a",
            "a (cleaned, Gentle 2).m4a",
            "a (cleaned, 18 dB).flac",
        ] {
            assert!(looks_cleaned(Path::new(n)), "{n}");
            assert_eq!(cleaned_stem(Path::new(n)).as_deref(), Some("a"), "{n}");
        }
        for n in [
            "a.wav",
            "cleaned.wav",
            "a (cleaned x).wav",
            "a (cleaned, ).wav",
        ] {
            assert!(!looks_cleaned(Path::new(n)), "{n}");
        }
    }

    /// F6: a folder opens into its recordings (its top level, or all the
    /// way down with `recursive`), not text named .wav, not HushMic's
    /// own copies of files beside them.
    #[test]
    fn a_folder_gives_its_recordings() {
        let dir = std::env::temp_dir().join(format!("hushmic-gather-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sub = dir.join("Week/Day 2");
        std::fs::create_dir_all(&sub).unwrap();
        let wav = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/files/master_48k_mono.wav"),
        )
        .unwrap();
        let week = dir.join("Week");
        for n in ["b.wav", "a.wav", "a (cleaned).wav", "lone (cleaned).wav"] {
            std::fs::write(week.join(n), &wav).unwrap();
        }
        std::fs::write(week.join("notes.txt"), "notes").unwrap();
        std::fs::write(week.join("fake.wav"), "just some text, not sound").unwrap();
        std::fs::write(sub.join("c.wav"), &wav).unwrap();
        std::fs::create_dir_all(dir.join("Empty")).unwrap();
        let names = |v: &[PathBuf]| -> Vec<String> {
            v.iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        let g = gather(&[week.clone(), dir.join("Empty")], false);
        assert_eq!(names(&g.files), ["a.wav", "b.wav", "lone (cleaned).wav"]);
        assert_eq!(names(&g.skipped), ["a (cleaned).wav"]);
        assert_eq!(names(&g.not_media), ["fake.wav"]);
        assert_eq!(g.empty, [dir.join("Empty")]);
        let g = gather(std::slice::from_ref(&week), true);
        assert_eq!(
            names(&g.files),
            ["c.wav", "a.wav", "b.wav", "lone (cleaned).wav"],
            "Day 2 sorts before the files"
        );
        // a file given by name stays, as given
        let given = week.join("a (cleaned).wav");
        let g = gather(&[given.clone(), week.join("notes.txt")], false);
        assert_eq!(g.files, [given, week.join("notes.txt")]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
