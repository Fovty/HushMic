//! Round-trip test for the desktop-notification sender against a mock
//! `org.freedesktop.Notifications` service on a PRIVATE session bus, so it
//! runs headless (CI) and never touches a real desktop. Self-skips when
//! `dbus-daemon` is unavailable or cannot start a session bus.
//!
//! Single #[test]: the private bus address goes through the process-global
//! `DBUS_SESSION_BUS_ADDRESS`, and the sender's replace-id chain is
//! process-global state — parallel test fns would race both.

use hushmic::notify::{self, Slot};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct BusGuard(Child);
impl Drop for BusGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Debug)]
struct Call {
    app_name: String,
    replaces_id: u32,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    has_desktop_entry: bool,
    has_transient: bool,
}

#[derive(Clone, Default)]
struct MockNotifications {
    calls: Arc<Mutex<Vec<Call>>>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl MockNotifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, zbus::zvariant::OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        let mut calls = self.calls.lock().unwrap();
        calls.push(Call {
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            has_desktop_entry: hints.contains_key("desktop-entry"),
            has_transient: hints.contains_key("transient"),
        });
        // Server-assigned ids, distinct per call.
        100 + calls.len() as u32
    }
}

#[test]
fn notify_round_trip_on_a_private_bus() {
    if Command::new("dbus-daemon")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP notify_round_trip_on_a_private_bus: dbus-daemon not available");
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
    // Binary present but session bus unstartable (e.g. no session.conf
    // installed) is an environment problem, not a code defect: skip, like
    // the missing-binary case.
    if !addr.starts_with("unix:") {
        eprintln!(
            "SKIP notify_round_trip_on_a_private_bus: dbus-daemon reported no usable \
             session bus address (got {addr:?})"
        );
        return;
    }
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &addr);

    let mock = MockNotifications::default();
    let calls = mock.calls.clone();
    let _server = zbus::blocking::connection::Builder::session()
        .expect("mock: session builder")
        .name("org.freedesktop.Notifications")
        .expect("mock: claim well-known name")
        .serve_at("/org/freedesktop/Notifications", mock)
        .expect("mock: serve object")
        .build()
        .expect("mock: build connection");

    // --- the raw blocking call (what the queue worker executes) ---

    // First status notification: fresh chain (replaces nothing).
    let id1 = notify::send_blocking(Slot::Status, "dialog-error", "sum1", "body1", false)
        .expect("first Notify call");
    // Second one must REPLACE the first (same slot).
    let id2 = notify::send_blocking(Slot::Status, "dialog-error", "sum2", "body2", false)
        .expect("second Notify call");
    // A mic-test bubble is an INDEPENDENT chain and transient.
    let id3 = notify::send_blocking(
        Slot::MicTest,
        "audio-input-microphone",
        "test",
        "recording…",
        true,
    )
    .expect("mic-test Notify call");

    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 3, "mock should have seen all three calls");

        assert_eq!(calls[0].app_name, "HushMic");
        assert_eq!(calls[0].replaces_id, 0);
        assert_eq!(calls[0].app_icon, "dialog-error");
        assert_eq!(calls[0].summary, "sum1");
        assert_eq!(calls[0].body, "body1");
        assert!(calls[0].actions.is_empty());
        assert!(calls[0].has_desktop_entry, "desktop-entry hint missing");
        assert!(!calls[0].has_transient, "status bubbles must persist");
        assert_eq!(id1, 101);

        assert_eq!(
            calls[1].replaces_id, id1,
            "same-slot notifications must replace, not stack"
        );
        assert_eq!(id2, 102);

        assert_eq!(
            calls[2].replaces_id, 0,
            "mic-test slot must not swallow the status chain"
        );
        assert!(calls[2].has_transient, "mic-test bubbles are transient");
        assert_eq!(id3, 103);
    }

    // --- the public queued API: ordered delivery through the worker ---

    notify::send(Slot::Status, "dialog-error", "q1", "first");
    notify::send_transient(Slot::MicTest, "audio-input-microphone", "q2", "second");
    notify::send_and_wait(
        Slot::Status,
        "audio-input-microphone",
        "q3",
        "third",
        Duration::from_secs(10),
    );
    // send_and_wait acks after ITS delivery; queue order means q1/q2 landed
    // before it. Poll briefly anyway to be robust against a slow mock.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if calls.lock().unwrap().len() >= 6 || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 6, "queued sends must all be delivered");
    assert_eq!(
        (
            calls[3].summary.as_str(),
            calls[4].summary.as_str(),
            calls[5].summary.as_str()
        ),
        ("q1", "q2", "q3"),
        "queue must preserve send order"
    );
    assert_eq!(
        calls[3].replaces_id, id2,
        "queued status send continues the status replace-chain"
    );
    assert!(!calls[3].has_transient);
    assert!(calls[4].has_transient, "send_transient sets the hint");
    assert_eq!(
        calls[4].replaces_id, id3,
        "queued mic-test send continues the mic-test replace-chain"
    );
}
