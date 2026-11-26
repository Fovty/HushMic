//! A hushmic the daemon starts through `/proc/self/exe` is called "exe"
//! by the kernel; it names itself `hushmic` again (`selfexe::restore_name`),
//! so `ps`, `pgrep -x hushmic` and task lists see it as before. Here the
//! binary runs under a link named "exe" (the same comm), as `hushmic
//! watch` against a stand-in socket that never answers, so it stays up
//! to be looked at.

use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn a_hushmic_started_as_exe_calls_itself_hushmic() {
    let dir = std::env::temp_dir().join(format!("hushmic-comm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("exe");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_hushmic"), &exe).unwrap();
    let listener = UnixListener::bind(dir.join("hushmic-control.sock")).unwrap();
    let mut child = Command::new(&exe)
        .arg("watch")
        .env("XDG_RUNTIME_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // It connects and waits for the answer. A child that exits instead
    // fails here, not by hanging the run.
    listener.set_nonblocking(true).unwrap();
    let t = Instant::now();
    let _conn = loop {
        match listener.accept() {
            Ok((conn, _)) => break conn,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("hushmic watch ended ({status}) before it connected");
                }
                if t.elapsed() > Duration::from_secs(10) {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("hushmic watch never connected");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("accept: {e}"),
        }
    };
    let comm = format!("/proc/{}/comm", child.id());
    let t = Instant::now();
    let mut name = String::new();
    while t.elapsed() < Duration::from_secs(10) {
        name = std::fs::read_to_string(&comm).unwrap_or_default();
        if name.trim_end() == "hushmic" {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(name.trim_end(), "hushmic");
    let _ = std::fs::remove_dir_all(dir);
}
