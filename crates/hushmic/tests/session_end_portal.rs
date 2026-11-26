//! The session-end monitor against a fake `org.freedesktop.portal.Inhibit`
//! on a PRIVATE session bus: the monitor is created, query-end and ending
//! say the session is ending (query-end answered at once), running ends
//! it, another monitor's state is not ours, the close-notice watch takes
//! the portal's word, and a portal that goes away stops the watching but
//! not what it said. A fake KDE session manager: logging out, or going
//! quiet after it answered, is a logout for the close-notice watch. With
//! nothing to watch, the watch says the session stays.
//! Self-skips without `dbus-daemon`. Single #[test]: the bus address and
//! the monitor's state are process-global.

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use hushmic::session_end;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";

type Calls = Arc<Mutex<Vec<String>>>;

struct FakeInhibit {
    calls: Calls,
    session: Arc<Mutex<Option<String>>>,
}

#[zbus::interface(name = "org.freedesktop.portal.Inhibit")]
impl FakeInhibit {
    async fn create_monitor(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
        _window: String,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let token = |k: &str| -> String {
            match options.get(k).map(|v| &**v) {
                Some(Value::Str(s)) => s.to_string(),
                _ => "none".into(),
            }
        };
        let sender = hdr
            .sender()
            .map(|s| s.as_str().trim_start_matches(':').replace('.', "_"))
            .unwrap_or_default();
        let request = format!("{PATH}/request/{sender}/{}", token("handle_token"));
        let session = format!("{PATH}/session/{sender}/{}", token("session_handle_token"));
        self.calls
            .lock()
            .unwrap()
            .push(format!("CreateMonitor {session}"));
        *self.session.lock().unwrap() = Some(session.clone());
        // The client subscribed on the request path before calling: the
        // response may come before the method's reply.
        let mut results: HashMap<&str, Value> = HashMap::new();
        results.insert("session_handle", Value::from(session));
        conn.emit_signal(
            Option::<&str>::None,
            request.as_str(),
            "org.freedesktop.portal.Request",
            "Response",
            &(0u32, results),
        )
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        OwnedObjectPath::try_from(request).map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    fn query_end_response(&self, session_handle: OwnedObjectPath) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("QueryEndResponse {}", session_handle.as_str()));
    }
}

struct FakeKsm {
    shutting_down: Arc<AtomicBool>,
}

#[zbus::interface(name = "org.kde.KSMServerInterface")]
impl FakeKsm {
    #[zbus(name = "isShuttingDown")]
    fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Relaxed)
    }
}

fn ksmserver(
    rt: &tokio::runtime::Runtime,
    addr: &str,
    shutting_down: Arc<AtomicBool>,
) -> zbus::Connection {
    rt.block_on(async {
        zbus::connection::Builder::address(addr)
            .unwrap()
            .name("org.kde.ksmserver")
            .unwrap()
            .serve_at("/KSMServer", FakeKsm { shutting_down })
            .unwrap()
            .build()
            .await
            .expect("fake ksmserver on the private bus")
    })
}

fn state_changed(rt: &tokio::runtime::Runtime, conn: &zbus::Connection, session: &str, state: u32) {
    let mut s: HashMap<&str, Value> = HashMap::new();
    s.insert("screensaver-active", Value::from(false));
    s.insert("session-state", Value::from(state));
    let path = OwnedObjectPath::try_from(session.to_string()).unwrap();
    rt.block_on(async {
        conn.emit_signal(
            Option::<&str>::None,
            PATH,
            "org.freedesktop.portal.Inhibit",
            "StateChanged",
            &(path, s),
        )
        .await
        .expect("emit StateChanged")
    });
}

fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_session_end_monitor_follows_the_inhibit_portal() {
    let Some(bus) = common::private_bus("the_session_end_monitor_follows_the_inhibit_portal")
    else {
        return;
    };
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus.addr);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    // nothing to watch (no portal, no session manager): the session stays
    assert!(hushmic::notify::session_stays(None));

    let calls: Calls = Arc::default();
    let session: Arc<Mutex<Option<String>>> = Arc::default();
    let conn = rt.block_on(async {
        zbus::connection::Builder::address(bus.addr.as_str())
            .unwrap()
            .name(PORTAL)
            .unwrap()
            .serve_at(
                PATH,
                FakeInhibit {
                    calls: calls.clone(),
                    session: session.clone(),
                },
            )
            .unwrap()
            .build()
            .await
            .expect("fake portal on the private bus")
    });

    session_end::spawn();
    until("the monitor", session_end::watching);
    let ours = session.lock().unwrap().clone().unwrap();
    assert!(!session_end::ending());

    // query-end: ending, and answered at once
    state_changed(&rt, &conn, &ours, 2);
    until("query-end", session_end::ending);
    until("the answer", || {
        calls
            .lock()
            .unwrap()
            .contains(&format!("QueryEndResponse {ours}"))
    });
    // the close-notice watch takes the portal's word (no logind session)
    let t = Instant::now();
    assert!(!hushmic::notify::session_stays(None));
    assert!(t.elapsed() < Duration::from_secs(2), "told at once");

    // running again (a cancelled logout)
    state_changed(&rt, &conn, &ours, 1);
    until("running", || !session_end::ending());
    // another monitor's state is not ours
    state_changed(
        &rt,
        &conn,
        "/org/freedesktop/portal/desktop/session/x/other",
        3,
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(!session_end::ending());

    // KDE's session manager logging out: told at once
    let shutting_down = Arc::new(AtomicBool::new(true));
    let ksm = ksmserver(&rt, &bus.addr, shutting_down.clone());
    let t = Instant::now();
    assert!(!hushmic::notify::session_stays(None));
    assert!(t.elapsed() < Duration::from_secs(2), "told at once");
    // not logging out, then gone mid-watch (it ends with the session)
    shutting_down.store(false, Ordering::Relaxed);
    let watch = std::thread::spawn(|| hushmic::notify::session_stays(None));
    std::thread::sleep(Duration::from_secs(1));
    drop(ksm);
    let t = Instant::now();
    assert!(!watch.join().unwrap());
    assert!(
        t.elapsed() < Duration::from_secs(4),
        "told within a step or two"
    );

    // ending, and then the portal goes away (it ends with the session):
    // no monitor any more, but what it said stands
    state_changed(&rt, &conn, &ours, 3);
    until("ending", session_end::ending);
    drop(conn);
    until("the monitor gone", || !session_end::watching());
    assert!(session_end::ending());
    assert!(!hushmic::notify::session_stays(None), "still a logout");
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("CreateMonitor"))
            .count(),
        1
    );

    drop(rt);
    drop(bus);
}
