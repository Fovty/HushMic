//! Real-socket round trips: listener thread on one end, the CLI client
//! path on the other, a stub handler standing in for the main loop.

use hushmic::control::{self, ControlReq};
use hushmic::lock;
use std::sync::mpsc;
use std::time::Duration;

fn temp_sock(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("hushmic-ctl-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

/// Stub main loop: answers `mode` with ok/suppress, `mode mute` with ok,
/// everything else with err.
fn stub_handler(req: ControlReq) {
    let reply = match req.line.as_str() {
        "mode" => control::encode_ok("suppress"),
        "mode mute" => control::encode_ok("mute"),
        "quit" => control::encode_ok("stopping"),
        "config get mic" => control::encode_ok("default\n"),
        "config set attn_limit 24" => control::encode_ok("attn_limit = 24"),
        other => control::encode_err(&format!("stub: {other}")),
    };
    let _ = req.reply.send(reply);
}

#[test]
fn round_trip_ok_and_err_over_a_real_socket() {
    let path = temp_sock("rt.sock");
    let listener = lock::bind_control_socket(&path).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener(listener, tx);
    std::thread::spawn(move || {
        for req in rx {
            stub_handler(req);
        }
    });

    let (code, out) = control::client_run_at(&path, &["mode".into()]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), "suppress");

    let (code, out) = control::client_run_at(&path, &["mode".into(), "mute".into()]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), "mute");

    // server-side err -> exit 1 with the message
    let (code, out) = control::client_run_at(&path, &["status".into()]);
    assert_eq!(code, 1);
    assert!(out.contains("stub"), "{out}");
}

#[test]
fn connect_failure_is_exit_2() {
    let path = temp_sock("nobody-home.sock");
    let _ = std::fs::remove_file(&path);
    let (code, out) = control::client_run_at(&path, &["mode".into()]);
    assert_eq!(code, 2);
    assert!(out.contains("not running"), "hint the tray is down: {out}");
}

#[test]
fn usage_errors_never_touch_the_socket() {
    // invalid subcommand exits 1 locally even with no socket at all
    let path = temp_sock("never-bound.sock");
    let _ = std::fs::remove_file(&path);
    let (code, out) = control::client_run_at(&path, &["frobnicate".into()]);
    assert_eq!(code, 1);
    assert!(out.contains("usage:"), "{out}");
}

#[test]
fn stale_socket_file_is_rebound_over() {
    let path = temp_sock("stale.sock");
    // a dead previous instance left its socket file behind
    drop(lock::bind_control_socket(&path).expect("first bind"));
    let listener = lock::bind_control_socket(&path).expect("rebind over stale file");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener(listener, tx);
    std::thread::spawn(move || {
        for req in rx {
            stub_handler(req);
        }
    });
    let (code, _) = control::client_run_at(&path, &["mode".into()]);
    assert_eq!(code, 0);
}

#[test]
fn a_stalled_client_does_not_wedge_the_listener() {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let path = temp_sock("stall.sock");
    let listener = lock::bind_control_socket(&path).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener(listener, tx);
    std::thread::spawn(move || {
        for req in rx {
            stub_handler(req);
        }
    });
    // connect and send NOTHING: the listener must time the connection out
    let mut mute_peer = UnixStream::connect(&path).expect("connect");
    // a real client right after must still get served
    let t0 = std::time::Instant::now();
    let (code, out) = control::client_run_at(&path, &["mode".into()]);
    assert_eq!(code, 0, "{out}");
    assert!(
        t0.elapsed() < Duration::from_secs(10),
        "served within the stall timeout budget"
    );
    let _ = mute_peer.write_all(b"late\n");
}

#[test]
fn quit_and_config_words_round_trip() {
    let path = temp_sock("quit-config.sock");
    let listener = lock::bind_control_socket(&path).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener(listener, tx);
    std::thread::spawn(move || {
        for req in rx {
            stub_handler(req);
        }
    });
    let (code, out) = control::client_run_at(&path, &["quit".into()]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), "stopping");
    let (code, out) = control::client_run_at(&path, &["config".into(), "get".into(), "mic".into()]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), "default");
    let (code, out) = control::client_run_at(
        &path,
        &[
            "config".into(),
            "set".into(),
            "attn_limit".into(),
            "24".into(),
        ],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), "attn_limit = 24");
}

/// A loop that takes each request after `delay`; whether it could.
fn slow_loop(rx: mpsc::Receiver<ControlReq>, delay: Duration, claimed: mpsc::Sender<bool>) {
    std::thread::spawn(move || {
        for req in rx {
            std::thread::sleep(delay);
            let ok = req.claim();
            let _ = claimed.send(ok);
            let _ = req.reply.send(control::encode_ok("mute"));
        }
    });
}

#[test]
fn a_request_not_taken_in_time_never_runs() {
    let path = temp_sock("late.sock");
    let listener = lock::bind_control_socket(&path).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener_timed(listener, tx, Duration::from_millis(200));
    let (ctx, crx) = mpsc::channel();
    slow_loop(rx, Duration::from_millis(600), ctx);
    // a toggle (a mode set outright runs late instead: see
    // a_late_release_still_runs_after_two_stalls)
    let (code, out) = control::client_run_at(&path, &["toggle".into(), "mute".into()]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("did not answer in time"), "{out}");
    assert!(
        !crx.recv_timeout(Duration::from_secs(5)).unwrap(),
        "the loop must not run a request its client was told timed out"
    );
}

#[test]
fn a_taken_request_is_waited_for() {
    let path = temp_sock("taken.sock");
    let listener = lock::bind_control_socket(&path).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener_timed(listener, tx, Duration::from_millis(200));
    std::thread::spawn(move || {
        for req in rx {
            assert!(req.claim());
            // Longer than the wait for taking it: the answer still counts.
            std::thread::sleep(Duration::from_millis(600));
            let _ = req.reply.send(control::encode_ok("mute"));
        }
    });
    let (code, out) = control::client_run_at(&path, &["mode".into(), "mute".into()]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), "mute");
}

#[test]
fn a_client_that_hung_up_is_not_run() {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let path = temp_sock("gone.sock");
    let listener = lock::bind_control_socket(&path).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener(listener, tx);
    let (ctx, crx) = mpsc::channel();
    slow_loop(rx, Duration::from_millis(300), ctx);
    let mut s = UnixStream::connect(&path).expect("connect");
    s.write_all(b"toggle mute\n").unwrap();
    // Gone before the loop gets to it.
    drop(s);
    assert!(!crx.recv_timeout(Duration::from_secs(5)).unwrap());
}

/// A request answered late (a `config set`, answered once its write is on
/// a disk that takes seconds) does not hold up the next one: a mute or a
/// push-to-talk release is answered at once meanwhile.
#[test]
fn a_late_answer_does_not_hold_up_the_next_request() {
    let path = temp_sock("late-answer.sock");
    let listener = lock::bind_control_socket(&path).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener(listener, tx);
    std::thread::spawn(move || {
        for req in rx {
            if req.line.starts_with("config set") {
                // the loop moves on; the writer answers later
                let reply = req.reply.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(2));
                    let _ = reply.send(control::encode_ok("attn_limit = 24"));
                });
            } else {
                stub_handler(req);
            }
        }
    });
    let slow_path = path.clone();
    let slow = std::thread::spawn(move || {
        let args: Vec<String> = ["config", "set", "attn_limit", "24"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        control::client_run_at(&slow_path, &args)
    });
    std::thread::sleep(Duration::from_millis(200));
    let t = std::time::Instant::now();
    let (code, out) = control::client_run_at(&path, &["mode".into(), "mute".into()]);
    assert_eq!((code, out.trim()), (0, "mute"));
    assert!(
        t.elapsed() < Duration::from_millis(1000),
        "{:?}",
        t.elapsed()
    );
    let (code, out) = slow.join().unwrap();
    assert_eq!(
        (code, out.trim()),
        (0, "attn_limit = 24"),
        "the late answer still arrives"
    );
}

/// Push to talk from two quick clients: a press and its release reach the
/// loop in the order they connected, though each answer is waited for on
/// its own thread and the press's answer comes last. A release that
/// overtook its press would leave the mic live.
#[test]
fn a_release_never_overtakes_its_press() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};
    let path = temp_sock("ptt-order.sock");
    let listener = lock::bind_control_socket(&path).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener(listener, tx);
    let mode = Arc::new(Mutex::new(String::from("mute")));
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let (m, s) = (mode.clone(), seen.clone());
    std::thread::spawn(move || {
        for req in rx {
            let want = req.line.trim_start_matches("mode ").to_string();
            *m.lock().unwrap() = want.clone();
            s.lock().unwrap().push(want.clone());
            let reply = req.reply.clone();
            if want == "suppress" {
                // the press's answer comes after the release's
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(3));
                    let _ = reply.send(control::encode_ok(&want));
                });
            } else {
                let _ = reply.send(control::encode_ok(&want));
            }
        }
    });
    let send = |line: &str| {
        let mut c = UnixStream::connect(&path).unwrap();
        c.write_all(format!("{line}\n").as_bytes()).unwrap();
        c
    };
    let rounds = 150;
    for _ in 0..rounds {
        // neither waits for its answer before the other goes
        let mut press = send("mode suppress");
        let mut release = send("mode mute");
        let mut out = String::new();
        release.read_to_string(&mut out).unwrap();
        press.read_to_string(&mut out).unwrap();
        assert_eq!(*mode.lock().unwrap(), "mute", "the mic was left live");
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2 * rounds);
    for pair in seen.chunks(2) {
        assert_eq!(pair, ["suppress", "mute"]);
    }
}

/// Review SF-O1: the loop stalls inside the press's wait, takes the press
/// (live), then stalls again past the release's wait. The release, a mode
/// set outright, is still run, in order; its client hears it was late.
/// Before, it was dropped and the mic stayed live with the key up.
#[test]
fn a_late_release_still_runs_after_two_stalls() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let path = temp_sock("two-stalls.sock");
    let listener = lock::bind_control_socket(&path).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener_timed(listener, tx, Duration::from_millis(1000));
    let send = |line: &str| {
        let mut c = UnixStream::connect(&path).unwrap();
        c.write_all(format!("{line}\n").as_bytes()).unwrap();
        std::thread::spawn(move || {
            let mut o = String::new();
            let _ = c.read_to_string(&mut o);
            o
        })
    };
    let press = send("mode suppress");
    std::thread::sleep(Duration::from_millis(50));
    let release = send("mode mute");
    // muted before; the press takes it live
    let p = rx.recv().unwrap();
    std::thread::sleep(Duration::from_millis(900)); // inside the press's 1 s
    assert!(p.claim());
    let mut mode = "suppress";
    let _ = p.reply.send(control::encode_ok("suppress"));
    drop(p);
    std::thread::sleep(Duration::from_millis(500)); // past the release's 1 s
    let r = rx.recv().unwrap();
    let ran = r.claim();
    if ran {
        mode = "mute";
        let _ = r.reply.send(control::encode_ok("mute"));
    }
    drop(r);
    let pressed = press.join().unwrap();
    assert!(
        pressed.starts_with("ok") && pressed.contains("suppress"),
        "{pressed}"
    );
    let told = release.join().unwrap();
    assert!(ran, "the release was dropped");
    assert_eq!(mode, "mute", "the mic was left live");
    assert!(told.contains("applies this as soon as it can"), "{told}");
    // a toggle may still be dropped once its client stopped waiting
    let toggle = send("toggle mute");
    let t = rx.recv().unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    assert!(!t.claim(), "a late toggle is not run");
    drop(t);
    assert!(toggle.join().unwrap().contains("did not answer in time"));
}

/// Review SF-V1: after an upgrade the old daemon still runs; its parser
/// takes `--written` for the key. The new command line asks again without
/// it, as it would have been asked then.
#[test]
fn a_config_set_reaches_a_daemon_from_before_written() {
    let path = temp_sock("old-daemon.sock");
    let listener = lock::bind_control_socket(&path).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener(listener, tx);
    let (seen_tx, seen) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for req in rx {
            let _ = seen_tx.send(req.line.clone());
            // the old parser: the word after `set` is the key
            let w: Vec<&str> = req.line.split_whitespace().collect();
            let reply = match w.as_slice() {
                ["config", "set", "--written", ..] => control::encode_err(
                    "unknown key '--written' (keys: enabled mic model attn_limit)",
                ),
                ["config", "set", "attn_limit", "35"] => control::encode_ok("attn_limit = 35"),
                _ => control::encode_err("stub"),
            };
            let _ = req.reply.send(reply);
        }
    });
    let words: Vec<String> = ["config", "set", "attn_limit", "35"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (code, out) = control::cli_run_at(&path, &words);
    assert_eq!((code, out.trim()), (0, "attn_limit = 35"));
    let asked: Vec<String> = seen.try_iter().collect();
    assert_eq!(
        asked,
        [
            "config set --written attn_limit 35",
            "config set attn_limit 35"
        ]
    );
    // the old daemon refusing the value itself: that answer stands (one
    // plain ask after the flagged one, no more)
    let words: Vec<String> = ["config", "set", "attn_limit", "999"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (code, _) = control::cli_run_at(&path, &words);
    assert_eq!(code, 1);
    assert_eq!(
        seen.try_iter().count(),
        2,
        "the --written ask, then the plain one"
    );
}

/// Review N-O2: a set mode whose client is gone before the loop takes it
/// (killed, or its read gave up) still runs: the key is up either way.
#[test]
fn a_set_mode_runs_though_its_client_is_gone() {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let path = temp_sock("gone-mode.sock");
    let listener = lock::bind_control_socket(&path).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener(listener, tx);
    let (ctx, crx) = mpsc::channel();
    slow_loop(rx, Duration::from_millis(300), ctx);
    let mut s = UnixStream::connect(&path).expect("connect");
    s.write_all(b"mode mute\n").unwrap();
    drop(s);
    assert!(crx.recv_timeout(Duration::from_secs(5)).unwrap());
}
