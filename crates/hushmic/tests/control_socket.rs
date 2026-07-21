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
