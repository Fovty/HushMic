//! A notification's actions, round trip on a PRIVATE session bus: the
//! actions go out with the notification, and the server's `ActionInvoked`,
//! sent to the connection that sent the notification (as GNOME does), comes
//! back to the handler with the notification's slot. A click on someone
//! else's notification does not. Self-skips without `dbus-daemon`.
//!
//! Its own test binary: the bus address and the notification chains are
//! process-global.

use hushmic::notify::{self, Slot};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long anything that should happen gets: wide, for a loaded runner
/// (the bus, the listener and this test share its CPU). Only a failure
/// waits that long.
const PATIENCE: Duration = Duration::from_secs(45);

struct BusGuard(Child);
impl Drop for BusGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Each call: its sender, the id given, and the actions.
type Calls = Arc<Mutex<Vec<(String, u32, Vec<String>)>>>;

#[derive(Clone, Default)]
struct MockNotifications {
    calls: Calls,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl MockNotifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        _app_name: String,
        _replaces_id: u32,
        _app_icon: String,
        _summary: String,
        _body: String,
        actions: Vec<String>,
        _hints: HashMap<String, zbus::zvariant::OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        let mut calls = self.calls.lock().unwrap();
        let id = 200 + calls.len() as u32;
        let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
        calls.push((sender, id, actions));
        id
    }
}

#[test]
fn a_click_on_an_action_comes_back_with_its_slot() {
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
    let calls = mock.calls.clone();
    let server = zbus::blocking::connection::Builder::session()
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .serve_at("/org/freedesktop/Notifications", mock)
        .unwrap()
        .build()
        .unwrap();

    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    notify::on_action(move |slot, key| {
        let _ = tx.lock().unwrap().send((slot, key));
    });
    notify::send_with_actions(
        Slot::Files,
        "audio-input-microphone",
        "3 recordings cleaned.",
        "",
        &[("show", "Show")],
    );
    let deadline = Instant::now() + PATIENCE;
    let (sender, id, actions) = loop {
        if let Some(c) = calls.lock().unwrap().first().cloned() {
            break c;
        }
        assert!(Instant::now() < deadline, "the notification never came");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(actions, ["default", "", "show", "Show"]);

    // Clicks reach the listener once its match rule is on the bus (one
    // sent before is lost, on a real desktop too), and are known as ours
    // once the server's answer, the id, is in: the server has the bubble
    // (the mock saw it) a moment before this side reads that answer, a
    // long moment on a busy runner.
    let deadline = Instant::now() + PATIENCE;
    while notify::action_listeners_subscribed() == 0 || notify::last_id(Slot::Files) != id {
        assert!(
            Instant::now() < deadline,
            "the listener never subscribed, or the id never came back"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // A click on someone else's notification: not ours to hear.
    let click = |id: u32, key: &str| {
        server
            .emit_signal(
                Some(sender.as_str()),
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
                "ActionInvoked",
                &(id, key),
            )
            .unwrap();
    };
    click(id + 50, "show");
    click(id, "show");
    let got = rx.recv_timeout(PATIENCE).expect("the click");
    assert_eq!(got, (Slot::Files, "show".to_string()));
    click(id, "default");
    let got = rx.recv_timeout(PATIENCE).expect("the bubble");
    assert_eq!(got, (Slot::Files, "default".to_string()));
    assert!(rx.try_recv().is_err());
}
