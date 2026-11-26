//! A stuck loop and a client that keeps asking (a status bar polling
//! `hushmic status`): the answers waited for on threads of their own stay
//! within `MAX_WAITING`, a request past that hears "busy" at once, and the
//! ones waited for still hear "did not answer in time". Its own test
//! binary, so no other test's threads are counted.

use hushmic::control::{self, ControlReq};
use hushmic::lock;
use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

fn answer_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
        .filter(|c| c.trim() == "hushmic-answer")
        .count()
}

/// How long the test waits for something that comes by itself: wide, and
/// only a failure waits that long.
const PATIENCE: Duration = Duration::from_secs(60);

/// Wait until `holds`.
fn until(what: &str, holds: impl Fn() -> bool) {
    let patience = Instant::now() + PATIENCE;
    while !holds() {
        assert!(Instant::now() < patience, "never: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// One request, and the line it is answered with.
fn ask(path: &Path, line: &str) -> String {
    let mut c = UnixStream::connect(path).unwrap();
    c.write_all(line.as_bytes()).unwrap();
    // the held request keeps its own handle on the connection:
    // the answer's line, not the end of the stream
    c.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut out = String::new();
    let _ = std::io::BufReader::new(c).read_line(&mut out);
    out
}

#[test]
fn a_stuck_loop_does_not_pile_up_answer_threads() {
    let d = std::env::temp_dir().join(format!("hushmic-ctl-bp-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("bp.sock");
    let listener = lock::bind_control_socket(&path).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    // The listener's own wait for the loop is an hour, so no wait ends by
    // the clock here: the loop is stuck until the test has seen what it
    // came for, and then lets go of what it kept, which the listener
    // answers as it answers a wait that ran out.
    control::spawn_listener_timed(listener, tx, Duration::from_secs(3600));
    // stuck: takes nothing, answers nothing, keeps every request
    let kept = Arc::new(Mutex::new(Vec::new()));
    let (lines_tx, lines) = mpsc::channel::<String>();
    let keeps = kept.clone();
    std::thread::spawn(move || {
        for req in rx {
            let line = req.line.clone();
            keeps.lock().unwrap().push(req);
            let _ = lines_tx.send(line);
        }
    });
    // As many requests as are waited for at once: a thread each.
    let waited: Vec<_> = (0..control::MAX_WAITING)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || ask(&path, "status\n"))
        })
        .collect();
    until("a thread for each of the first requests", || {
        answer_threads() == control::MAX_WAITING
    });
    // Past the cap: busy at once.
    for _ in 0..2 * control::MAX_WAITING {
        let out = ask(&path, "status\n");
        assert!(out.contains("HushMic is busy; try again"), "{out}");
    }
    // while it is busy, a mode set outright still goes to the loop
    let told = ask(&path, "mode mute\n");
    assert!(told.contains("applies this as soon as it can"), "{told}");
    loop {
        let line = lines
            .recv_timeout(PATIENCE)
            .expect("the mute reached the loop");
        if line == "mode mute" {
            break;
        }
    }
    // None of those got a thread: no answer thread has ended so far, so
    // the ones there are all there ever were.
    assert_eq!(answer_threads(), control::MAX_WAITING);
    // The loop lets go: the ones waited for hear that it did not answer,
    // and their threads end.
    kept.lock().unwrap().clear();
    for c in waited {
        let out = c.join().unwrap();
        assert!(out.contains("did not answer in time"), "{out}");
    }
    until("the answer threads end", || answer_threads() == 0);
    let _ = std::fs::remove_dir_all(&d);
}
