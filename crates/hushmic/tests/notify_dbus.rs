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
    /// The ids `CloseNotification` was called with.
    closed: Arc<Mutex<Vec<u32>>>,
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

    fn close_notification(&self, id: u32) {
        self.closed.lock().unwrap().push(id);
    }

    /// Like GNOME Shell, Plasma, dunst and mako: bodies are markup.
    fn get_capabilities(&self) -> Vec<String> {
        ["actions", "body", "body-markup"]
            .map(String::from)
            .to_vec()
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
    let calls_after = mock.calls.clone();
    let closed = mock.closed.clone();
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
    {
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

    // --- a problem the window shows (its page shows it, with focus):
    // held, and sent once the window no longer shows it if still so; a
    // notice about the app has a chain of its own ---

    use hushmic::appstate::Page;
    use notify::Problem;
    notify::set_window_shows(notify::shown_on(Page::Live, notify::LiveShows::Untold));
    notify::send_problem(Problem::Failure, "dialog-error", "held", "");
    notify::send_problem_transient(Problem::MicBack, "audio-input-microphone", "moment", "");
    // to first run: the failure is still shown there, the moment is over
    notify::set_window_shows(notify::shown_on(Page::FirstRun, notify::LiveShows::Untold));
    let dropped = notify::release_held(|p| p == Problem::Failure);
    assert_eq!(dropped, [Problem::MicBack]);
    // first run does not show a re-route: out at once
    notify::send_problem(Problem::Rerouted, "dialog-error", "p1", "");
    // focus goes: the failure, still so, goes out once
    notify::set_window_shows(0);
    assert!(notify::release_held(|_| true).is_empty());
    assert!(notify::release_held(|_| true).is_empty());
    notify::send_transient(
        Slot::Notice,
        "audio-input-microphone",
        "n1",
        "still running",
    );
    notify::send_and_wait(Slot::MicTest, "x", "flush", "", Duration::from_secs(10));
    let calls = calls.lock().unwrap();
    let sent: Vec<&str> = calls[6..].iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(
        sent,
        ["p1", "held", "n1", "flush"],
        "held only what the window shows, sent once when it no longer does"
    );
    // a re-route has a chain of its own: it replaces no status bubble
    // (UX panel round 2, A5: one chain per topic)
    assert_eq!(calls[6].replaces_id, 0);
    assert_eq!(
        calls[7].replaces_id, 0,
        "a failure has a chain of its own: no microphone notice replaces it"
    );
    assert_eq!(calls[8].replaces_id, 0, "the notice chain starts fresh");
    assert!(calls[8].has_transient);
    drop(calls);

    // --- a healthy Live in front (its card told "none"): a problem the
    // card could show waits for the card's word; with none that shows it,
    // it goes out when the grace ends, and a timer wakes the loop for it ---
    let (wake_tx, wake_rx) = std::sync::mpsc::channel();
    let wake_tx = Mutex::new(wake_tx);
    notify::set_wake(move || {
        let _ = wake_tx.lock().unwrap().send(Instant::now());
    });
    notify::set_window_shows(notify::shown_on(Page::Live, notify::LiveShows::Nothing));
    notify::set_window_on_live(true);
    let arrived = Instant::now();
    notify::send_problem(Problem::MicFallback, "audio-input-microphone", "grace", "");
    assert!(notify::release_held(|_| true).is_empty());
    notify::send_and_wait(Slot::MicTest, "x", "flush2", "", Duration::from_secs(10));
    assert!(
        !calls_after
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.summary == "grace"),
        "held for Live's card, not sent on arrival"
    );
    let woke = wake_rx
        .recv_timeout(notify::CARD_GRACE + Duration::from_secs(5))
        .expect("the grace's timer wakes the loop");
    assert!(woke - arrived >= notify::CARD_GRACE);
    assert!(notify::release_held(|_| true).is_empty());
    notify::send_and_wait(Slot::MicTest, "x", "flush3", "", Duration::from_secs(10));
    {
        let calls = calls_after.lock().unwrap();
        let tail: Vec<&str> = calls[calls.len() - 3..]
            .iter()
            .map(|c| c.summary.as_str())
            .collect();
        assert_eq!(
            tail,
            ["flush2", "grace", "flush3"],
            "out once the grace ends"
        );
    }
    notify::set_window_on_live(false);
    notify::set_window_shows(0);

    // --- every body that carries a name (a folder, a microphone) reads as
    // plain text to a server that parses markup: the cleaned-files note
    // straight, the default-released notice through the queue ---
    let esc = |b: &str| {
        b.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let dir = std::path::Path::new("/home/you/A & <B>");
    let n = hushmic::files::note::note(&hushmic::files::jobs::Cleaned {
        count: 2,
        saved: Some(
            ["a", "b"]
                .map(|f| {
                    (
                        std::path::PathBuf::from(format!("/rec/{f}.wav")),
                        dir.join(format!("{f} (cleaned).wav")),
                    )
                })
                .to_vec(),
        ),
        failed: 0,
    });
    assert!(n.body.contains("A & <B>"), "{}", n.body);
    notify::send_blocking(Slot::Files, "x", &n.summary, &n.body, false)
        .expect("Notify with a folder's name");
    let device = hushmic::tr!("notify-default-released-open", device = "A & <B>");
    notify::send_and_wait(
        Slot::Notice,
        "x",
        "released",
        &device,
        Duration::from_secs(10),
    );
    let calls = calls_after.lock().unwrap();
    let bodies: Vec<&str> = calls[calls.len() - 2..]
        .iter()
        .map(|c| c.body.as_str())
        .collect();
    assert_eq!(bodies, [esc(&n.body), esc(&device)]);
    assert!(bodies[0].contains("A &amp; &lt;B&gt;"), "{}", bodies[0]);
    drop(calls);

    // --- "No sound…" goes when sound is back, and the next one is a new
    // bubble; the off notice keeps its own (UX panel round 2, A5) ---
    notify::send_problem(Problem::MicSilent, "x", "silent", "");
    notify::send_and_wait(Slot::Power, "x", "off", "", Duration::from_secs(10));
    let (silent_id, off_id) = {
        let calls = calls_after.lock().unwrap();
        let n = calls.len() as u32;
        (100 + n - 1, 100 + n)
    };
    notify::close(Slot::Silence);
    notify::send_and_wait(Slot::MicTest, "x", "flush4", "", Duration::from_secs(10));
    assert_eq!(*closed.lock().unwrap(), [silent_id]);
    assert_eq!(notify::last_id(Slot::Silence), 0);
    assert_eq!(notify::last_id(Slot::Power), off_id);
    notify::close(Slot::Silence);
    notify::send_problem(Problem::MicSilent, "x", "silent again", "");
    notify::send_and_wait(Slot::MicTest, "x", "flush5", "", Duration::from_secs(10));
    assert_eq!(
        closed.lock().unwrap().len(),
        1,
        "nothing out: nothing to close"
    );
    let calls = calls_after.lock().unwrap();
    let again = calls.iter().find(|c| c.summary == "silent again").unwrap();
    assert_eq!(again.replaces_id, 0, "a new bubble");
}
