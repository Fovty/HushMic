//! "Open with HushMic" (`hushmic open-files FILE…`) against a stand-in for
//! the running instance's control socket: each file goes on the Files page
//! by its absolute path, then the window opens there; a file the instance
//! refuses is named and the exit code says so. The desktop entry it comes
//! from is valid.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hushmic-open-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The control socket in `run`: every request line is kept; `files add`
/// of a path with "refuse" in it is answered with an error.
fn stand_in(run: &Path) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let listener = UnixListener::bind(run.join("hushmic-control.sock")).unwrap();
    let keep = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let mut line = String::new();
            let _ = BufReader::new(&conn).read_line(&mut line);
            let line = line.trim().to_string();
            let reply = if line.contains("refuse") {
                "err not a file\n".to_string()
            } else {
                "ok\n1".to_string()
            };
            keep.lock().unwrap().push(line);
            let _ = (&conn).write_all(reply.as_bytes());
        }
    });
    seen
}

fn open_files(dir: &Path, run: &Path, files: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hushmic"))
        .arg("open-files")
        .args(files)
        .current_dir(dir)
        .env("XDG_RUNTIME_DIR", run)
        .output()
        .unwrap()
}

#[test]
fn files_go_on_the_page_then_the_window_opens_there() {
    let dir = scratch("page");
    let run = dir.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let seen = stand_in(&run);
    let out = open_files(&dir, &run, &["a b.wav", "/x/c.mp4"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let a = serde_json::to_string(&dir.join("a b.wav").display().to_string()).unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        [
            format!("files add {a}"),
            "files add \"/x/c.mp4\"".to_string(),
            "files open".to_string()
        ]
    );
    // One refused: named, exit 1, the window still opens for the other.
    seen.lock().unwrap().clear();
    let out = open_files(&dir, &run, &["/x/refuse.wav", "/x/d.wav"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("/x/refuse.wav: not a file"));
    assert_eq!(
        seen.lock().unwrap().last().map(String::as_str),
        Some("files open")
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// F6: a folder gives its recordings, its top level, or with -r its
/// subfolders too; not HushMic's own copies, and not text named .wav.
#[test]
fn a_folder_gives_its_recordings() {
    let dir = scratch("folder");
    let run = dir.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let seen = stand_in(&run);
    let week = dir.join("Week");
    std::fs::create_dir_all(week.join("Day 2")).unwrap();
    let wav = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/files/master_48k_mono.wav"),
    )
    .unwrap();
    for n in ["a.wav", "a (cleaned).wav", "Day 2/b.wav"] {
        std::fs::write(week.join(n), &wav).unwrap();
    }
    std::fs::write(week.join("fake.wav"), "only text").unwrap();
    let add = |p: &Path| {
        format!(
            "files add {}",
            serde_json::to_string(&p.display().to_string()).unwrap()
        )
    };
    let out = open_files(&dir, &run, &[week.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        *seen.lock().unwrap(),
        [add(&week.join("a.wav")), "files open".to_string()]
    );
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        err.contains("fake.wav: left out, not an audio or video file"),
        "{err}"
    );
    assert!(err.contains("a (cleaned).wav: left out"), "{err}");
    seen.lock().unwrap().clear();
    let out = open_files(&dir, &run, &["-r", week.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        *seen.lock().unwrap(),
        [
            add(&week.join("Day 2/b.wav")),
            add(&week.join("a.wav")),
            "files open".to_string()
        ]
    );
    // a folder with none: said, exit 1, nothing added
    seen.lock().unwrap().clear();
    std::fs::create_dir_all(dir.join("Empty")).unwrap();
    let out = open_files(&dir, &run, &[dir.join("Empty").to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no audio or video files in it"));
    assert!(seen.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn no_file_is_a_usage_error() {
    let dir = scratch("usage");
    let out = open_files(&dir, &dir, &[]);
    assert_eq!(out.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_desktop_entry_is_valid_and_hidden_from_menus() {
    let entry = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/hushmic-clean.desktop");
    let text = std::fs::read_to_string(&entry).unwrap();
    assert!(text.contains("NoDisplay=true"));
    assert!(text.contains("Exec=hushmic open-files %F"));
    match Command::new("desktop-file-validate").arg(&entry).output() {
        Ok(out) => assert!(
            out.status.success() && out.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        ),
        Err(_) => eprintln!("SKIP desktop-file-validate: not installed"),
    }
}
