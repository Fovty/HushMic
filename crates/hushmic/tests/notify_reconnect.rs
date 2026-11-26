//! The notification connection after a failed call, on a PRIVATE session
//! bus: the old connection is closed and its action listener ends, so a
//! busy shell that times calls out leaks neither; one listener hears each
//! click, also from a server that broadcasts `ActionInvoked`; and a
//! handler registered after the first notification still hears it.
//! Every click has a key of its own, so a click sent again (the first
//! may come before the listener's match rule) is never taken for one
//! heard twice. Self-skips without `dbus-daemon`.
//!
//! Its own test binary: the bus address and the connection are
//! process-global.

use hushmic::notify::{self, Slot};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct BusGuard(Child);
impl Drop for BusGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Clone, Default)]
struct MockNotifications {
    /// The next Notify fails (as a timed-out call does for the client).
    fail_next: Arc<AtomicBool>,
    /// The last id given (0: none yet).
    last: Arc<AtomicU32>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl MockNotifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        _app_name: String,
        _replaces_id: u32,
        _app_icon: String,
        _summary: String,
        _body: String,
        _actions: Vec<String>,
        _hints: HashMap<String, zbus::zvariant::OwnedValue>,
        _expire_timeout: i32,
    ) -> zbus::fdo::Result<u32> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed("busy".into()));
        }
        Ok(self.last.fetch_add(1, Ordering::SeqCst) + 301)
    }
}

/// How long anything that should happen gets: wide, for a loaded runner
/// (the bus, the listeners and this test share its CPU). Only a failure
/// waits that long.
const PATIENCE: Duration = Duration::from_secs(45);

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_failed_call_closes_the_connection_and_one_listener_hears_each_click() {
    if Command::new("dbus-daemon")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP: dbus-daemon not available");
        return;
    }
    let mut daemon = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn dbus-daemon");
    let addr = {
        let out = daemon.stdout.take().expect("daemon stdout");
        let mut line = String::new();
        let _ = BufReader::new(out).read_line(&mut line);
        line.trim().to_string()
    };
    let _guard = BusGuard(daemon);
    if !addr.starts_with("unix:") {
        eprintln!("SKIP: no usable session bus address (got {addr:?})");
        return;
    }
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &addr);
    let mock = MockNotifications::default();
    let (fail_next, last) = (mock.fail_next.clone(), mock.last.clone());
    let server = zbus::blocking::connection::Builder::session()
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .serve_at("/org/freedesktop/Notifications", mock)
        .unwrap()
        .build()
        .unwrap();
    // Broadcast, as some servers do: every connection listening hears it.
    let clicks = AtomicU32::new(0);
    let click = |key: &str| {
        let id = 300 + last.load(Ordering::SeqCst);
        let key = format!("{key}-{}", clicks.fetch_add(1, Ordering::SeqCst));
        server
            .emit_signal(
                None::<&str>,
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
                "ActionInvoked",
                &(id, key.as_str()),
            )
            .unwrap();
    };
    // Click (again, every half second) until a click of `key` is heard.
    let heard = Mutex::new(Vec::<(Slot, String)>::new());
    let until_heard = |rx: &std::sync::mpsc::Receiver<(Slot, String)>, key: &str| {
        let deadline = Instant::now() + PATIENCE;
        let prefix = format!("{key}-");
        while !heard
            .lock()
            .unwrap()
            .iter()
            .any(|(_, k)| k.starts_with(&prefix))
        {
            assert!(Instant::now() < deadline, "no {key} click heard");
            click(key);
            if let Ok(got) = rx.recv_timeout(Duration::from_millis(500)) {
                heard.lock().unwrap().push(got);
            }
        }
    };
    let send = || {
        notify::send_blocking_with(
            Slot::Files,
            "x",
            "3 recordings cleaned.",
            "",
            false,
            &[("show", "Show")],
        )
    };

    // A notification before anyone listens: no listener yet.
    send().unwrap();
    assert_eq!(notify::action_listeners(), 0);
    // A handler registered later hears that connection's clicks.
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    notify::on_action(move |slot, key| {
        let _ = tx.lock().unwrap().send((slot, key));
    });
    wait_until("the listener", || {
        notify::action_listeners_subscribed() == 1
    });
    // (Its match rule is in place once a click goes through.)
    until_heard(&rx, "first");

    // A failed call: that connection closes and its listener ends.
    for _ in 0..3 {
        fail_next.store(true, Ordering::SeqCst);
        assert!(send().is_err());
        send().unwrap();
    }
    wait_until("one listener left", || notify::action_listeners() == 1);
    wait_until("it to hear clicks", || {
        notify::action_listeners_subscribed() == 1
    });
    until_heard(&rx, "again");
    // Each once, though every click went to every connection.
    std::thread::sleep(Duration::from_secs(2));
    let mut heard = heard.into_inner().unwrap();
    heard.extend(rx.try_iter());
    assert!(heard.iter().all(|(slot, _)| *slot == Slot::Files));
    let mut keys: Vec<&str> = heard.iter().map(|(_, k)| k.as_str()).collect();
    keys.sort_unstable();
    let n = keys.len();
    keys.dedup();
    assert_eq!(keys.len(), n, "a click heard more than once: {heard:?}");
}
