//! zbus must stay in async-io mode. AccessKit's AT-SPI adapter connects to
//! the session bus from a thread of its own with a plain executor; if any
//! dependency turns on zbus's `tokio` feature (ashpd does by default),
//! Cargo unifies it and that connect panics ("there is no reactor running"):
//! screen readers silently lose the window. This connects the same way, on
//! a private bus. Self-skips when `dbus-daemon` is unavailable.

use std::future::Future;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

struct BusGuard(Child);
impl Drop for BusGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The smallest executor: no tokio runtime anywhere on this thread.
fn block_on<F: Future>(f: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park();
    }
}

#[test]
fn zbus_connects_without_a_tokio_runtime() {
    let Ok(mut daemon) = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        eprintln!("SKIP zbus_connects_without_a_tokio_runtime: dbus-daemon not available");
        return;
    };
    let addr = {
        let out = daemon.stdout.take().expect("daemon stdout");
        let mut line = String::new();
        let _ = BufReader::new(out).read_line(&mut line);
        line.trim().to_string()
    };
    let _guard = BusGuard(daemon);
    if !addr.starts_with("unix:") {
        eprintln!("SKIP zbus_connects_without_a_tokio_runtime: no session bus ({addr:?})");
        return;
    }
    let joined = std::thread::spawn(move || {
        block_on(async {
            zbus::connection::Builder::address(addr.as_str())?
                .build()
                .await
        })
    })
    .join();
    let conn = joined.unwrap_or_else(|_| {
        panic!("zbus panicked outside a tokio runtime: is its `tokio` feature on again?")
    });
    let _conn = conn.expect("connect to the private bus");
}
