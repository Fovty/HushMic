//! `hushmic quit` and `hushmic miccheck` return when the work is done,
//! not when it is asked for: the real binary against a stub instance (its
//! lock and control socket in a private runtime dir).

use hushmic::control::{self, ControlReq};
use hushmic::lock;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn runtime(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hushmic-waits-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A stub instance at `dir` answering with `answer`.
fn serve(dir: &Path, answer: impl Fn(&str) -> String + Send + 'static) {
    let listener = lock::bind_control_socket(&dir.join("hushmic-control.sock")).expect("bind");
    let (tx, rx) = mpsc::channel::<ControlReq>();
    control::spawn_listener(listener, tx);
    std::thread::spawn(move || {
        for req in rx {
            let _ = req.reply.send(answer(&req.line));
        }
    });
}

fn hushmic(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_hushmic"))
        .args(args)
        .env("XDG_RUNTIME_DIR", dir)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn quit_returns_once_the_instance_is_gone() {
    let dir = runtime("quit");
    let held = lock::try_lock(&dir.join("hushmic.lock"))
        .unwrap()
        .expect("free");
    let (gone_tx, gone_rx) = mpsc::channel::<()>();
    serve(&dir, move |line| match line {
        "quit" => {
            let _ = gone_tx.send(());
            control::encode_ok("stopping")
        }
        other => control::encode_err(other),
    });
    // the instance lets go of its lock 600 ms after the quit
    let lets_go = std::thread::spawn(move || {
        gone_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(600));
        drop(held);
    });
    let t0 = Instant::now();
    let (code, out, err) = hushmic(&dir, &["quit"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(out, "stopped\n");
    assert!(
        t0.elapsed() >= Duration::from_millis(550),
        "returned before the instance was gone: {:?}",
        t0.elapsed()
    );
    lets_go.join().unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn miccheck_returns_when_both_takes_are_played() {
    let dir = runtime("miccheck");
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n = asked.clone();
    serve(&dir, move |line| match line {
        "miccheck" => control::encode_ok("recording 5 seconds now"),
        // running for the first three looks, then done
        "miccheck state" => {
            let k = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            control::encode_ok(if k < 3 { "running" } else { "done" })
        }
        other => control::encode_err(other),
    });
    let (code, out, err) = hushmic(&dir, &["miccheck"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(
        out,
        "recording 5 seconds now\ndone: you heard the raw take, then the cleaned one\n"
    );
    assert!(asked.load(std::sync::atomic::Ordering::SeqCst) >= 4);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_failed_miccheck_says_why_and_exits_1() {
    let dir = runtime("micfail");
    serve(&dir, |line| match line {
        "miccheck" => control::encode_ok("recording 5 seconds now"),
        "miccheck state" => control::encode_ok("failed no sound came from the microphone"),
        other => control::encode_err(other),
    });
    let (code, out, err) = hushmic(&dir, &["miccheck"]);
    assert_eq!(code, 1, "{out}{err}");
    assert_eq!(out, "recording 5 seconds now\n");
    assert_eq!(
        err,
        "the mic check failed: no sound came from the microphone\n"
    );
    let _ = std::fs::remove_dir_all(dir);
}
