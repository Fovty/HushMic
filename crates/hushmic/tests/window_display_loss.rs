//! The window under a private Xvfb whose X server then goes away (as at a
//! logout): it leaves quietly with EXIT_DISPLAY_GONE, no panic report, so
//! the daemon logs it instead of telling the user the window could not
//! open. Self-skips without Xvfb, or when the window cannot come up there
//! (no OpenGL), or without xdotool to see its first frame.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn a_window_whose_display_goes_away_leaves_quietly() {
    let Ok(mut xvfb) = Command::new("Xvfb")
        .args([
            "-displayfd",
            "1",
            "-screen",
            "0",
            "1280x800x24",
            "-nolisten",
            "tcp",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        eprintln!("SKIP: Xvfb not available");
        return;
    };
    let mut number = String::new();
    let mut out = xvfb.stdout.take().unwrap();
    let mut byte = [0u8; 1];
    while out.read(&mut byte).unwrap_or(0) == 1 && byte[0] != b'\n' {
        number.push(byte[0] as char);
    }
    let display = format!(":{}", number.trim());
    let dir = std::env::temp_dir().join(format!("hushmic-display-loss-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut window = Command::new(env!("CARGO_BIN_EXE_hushmic"))
        .args(["--window", "--mock", "on-in-call", "--reduced-motion"])
        .env("DISPLAY", &display)
        .env_remove("WAYLAND_DISPLAY")
        .env("XDG_RUNTIME_DIR", &dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("HOME", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the window binary");
    // up: its window is mapped, which eframe does only once it painted
    // its first frame (it opens hidden): from then on the display going
    // away is not a window that could not open. The cap only bounds a
    // window that never comes up (GL setup on a loaded runner is slow).
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut up = false;
    while Instant::now() < deadline {
        if window.try_wait().unwrap().is_some() {
            break;
        }
        let found = Command::new("xdotool")
            .args(["search", "--onlyvisible", "--name", "^HushMic$"])
            .env("DISPLAY", &display)
            .output()
            .map(|o| !o.stdout.is_empty());
        match found {
            Ok(true) => {
                up = true;
                break;
            }
            Ok(false) => {}
            // no xdotool: nothing tells when its first frame is in
            Err(_) => break,
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if !up {
        let _ = window.kill();
        let _ = xvfb.kill();
        let _ = window.wait();
        let _ = xvfb.wait();
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!("SKIP: the window did not come up under Xvfb (or no xdotool)");
        return;
    }
    // the X server goes away
    let _ = xvfb.kill();
    let _ = xvfb.wait();
    // the cap only bounds a window that never notices
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(s) = window.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = window.kill();
            panic!("the window outlived its display by 60 s");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut err = String::new();
    window
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!err.contains("panicked"), "a panic report: {err}");
    assert_eq!(
        status.code(),
        Some(hushmic::window::EXIT_DISPLAY_GONE),
        "exit {status}; stderr: {err}"
    );
}
