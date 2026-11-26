//! Round-trip test for the global-shortcuts portal worker against a mock
//! `org.freedesktop.portal.GlobalShortcuts` service on a PRIVATE session
//! bus with no service directories (`common::private_bus`), so it runs
//! headless, never touches a real desktop and never auto-starts a portal.
//! Self-skips when `dbus-daemon` is unavailable.
//!
//! Single #[test]: the private bus address goes through the process-global
//! `DBUS_SESSION_BUS_ADDRESS` — parallel test fns would race it. The one
//! scenario walks the worker's whole life: no portal -> Unavailable,
//! Retry -> Available, session-filtered signal delivery, interactive bind,
//! the silent re-bind of a second start, and the keys each step reports.

mod common;

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use hushmic::appstate::Keys;
use hushmic::shortcuts::{self, Action, Cmd, PortalEvent};

#[derive(Default)]
struct FakeState {
    /// Session object paths handed out, in CreateSession order.
    sessions: Vec<String>,
    /// One entry per BindShortcuts call: the (id, description) pairs.
    binds: Vec<Vec<(String, String)>>,
    /// Session paths ConfigureShortcuts was called with.
    configures: Vec<String>,
    /// App ids registered via the host Registry (terminal launches have no
    /// derivable app id — KDE refuses CreateSession without one).
    registers: Vec<String>,
    /// Interleaved call log ("register:<id>" / "create") so ORDERING
    /// claims are real: Register must be the first portal call on the
    /// connection, strictly before CreateSession.
    order: Vec<String>,
    /// Answer the next BindShortcuts with response code 1 (user cancelled).
    cancel_next_bind: bool,
    /// Sessions that bound: ListShortcuts lists nothing for the others.
    bound: Vec<String>,
    /// Hold ListShortcuts answers (a slow or wedged portal): each request
    /// path waits in `held` with whether its session had bound when asked,
    /// until the test answers it.
    hold_lists: bool,
    held: Vec<(String, bool)>,
}

/// The keys "the desktop" has for HushMic: toggle-mute only.
fn desktop_trigger(id: &str) -> &'static str {
    if id == "toggle-mute" {
        "Ctrl+Alt+M"
    } else {
        ""
    }
}

/// `a(sa{sv})` as a portal answers: every action with its keys.
fn shortcut_list() -> Vec<(String, HashMap<String, Value<'static>>)> {
    Action::ALL
        .iter()
        .map(|a| {
            let mut props = HashMap::new();
            props.insert("description".to_string(), Value::from(a.description()));
            props.insert(
                "trigger_description".to_string(),
                Value::from(desktop_trigger(a.id())),
            );
            (a.id().to_string(), props)
        })
        .collect()
}

/// What the worker reports for that list (or for a session with none).
fn keys_event(bound: bool) -> PortalEvent {
    PortalEvent::Keys(
        Action::ALL
            .iter()
            .map(|a| {
                let keys = match desktop_trigger(a.id()) {
                    t if bound && !t.is_empty() => Keys::Text(t.to_string()),
                    _ => Keys::NotSet,
                };
                (*a, keys)
            })
            .collect(),
    )
}

#[derive(Clone, Default)]
struct FakeRegistry {
    state: Arc<Mutex<FakeState>>,
}

#[zbus::interface(name = "org.freedesktop.host.portal.Registry")]
impl FakeRegistry {
    fn register(&self, app_id: String, _options: HashMap<String, OwnedValue>) {
        let mut st = self.state.lock().unwrap();
        st.order.push(format!("register:{app_id}"));
        st.registers.push(app_id);
    }
}

#[derive(Clone, Default)]
struct FakePortal {
    state: Arc<Mutex<FakeState>>,
    /// Interface version to advertise: 2 = ConfigureShortcuts exists.
    version: u32,
}

fn token_of(options: &HashMap<String, OwnedValue>, key: &str) -> String {
    options
        .get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_else(|| panic!("{key} missing from options"))
}

fn sender_id(hdr: &zbus::message::Header<'_>) -> String {
    hdr.sender()
        .expect("caller has a unique name")
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_")
}

async fn respond(
    conn: &zbus::Connection,
    request_path: &str,
    code: u32,
    results: HashMap<&str, Value<'_>>,
) {
    conn.emit_signal(
        Option::<&str>::None,
        request_path,
        "org.freedesktop.portal.Request",
        "Response",
        &(code, results),
    )
    .await
    .expect("emit Response");
}

/// ListShortcuts' Response: the keys for a session that bound, else none.
async fn answer_list(conn: &zbus::Connection, request: &str, bound: bool) {
    let list = if bound { shortcut_list() } else { vec![] };
    let mut results = HashMap::new();
    results.insert("shortcuts", Value::from(list));
    respond(conn, request, 0, results).await;
}

#[zbus::interface(name = "org.freedesktop.portal.GlobalShortcuts")]
impl FakePortal {
    // The real portal exposes lowercase "version" (zbus would default to
    // PascalCase, which the probe must NOT accept).
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        self.version
    }

    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> OwnedObjectPath {
        let sender = sender_id(&hdr);
        let request = format!(
            "/org/freedesktop/portal/desktop/request/{sender}/{}",
            token_of(&options, "handle_token")
        );
        let session = format!(
            "/org/freedesktop/portal/desktop/session/{sender}/{}",
            token_of(&options, "session_handle_token")
        );
        {
            let mut st = self.state.lock().unwrap();
            st.order.push("create".to_string());
            st.sessions.push(session.clone());
        }
        // Real backends answer with the session handle in the results (as a
        // string, matching KDE/GNOME).
        let mut results = HashMap::new();
        results.insert("session_handle", Value::from(session));
        respond(conn, &request, 0, results).await;
        ObjectPath::try_from(request).unwrap().into()
    }

    #[allow(clippy::type_complexity)]
    async fn bind_shortcuts(
        &self,
        session: OwnedObjectPath,
        shortcuts: Vec<(String, HashMap<String, OwnedValue>)>,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> OwnedObjectPath {
        let sender = sender_id(&hdr);
        let request = format!(
            "/org/freedesktop/portal/desktop/request/{sender}/{}",
            token_of(&options, "handle_token")
        );
        let code = {
            let mut st = self.state.lock().unwrap();
            if std::mem::take(&mut st.cancel_next_bind) {
                1 // user cancelled the dialog
            } else {
                let recorded = shortcuts
                    .into_iter()
                    .map(|(id, props)| {
                        let desc = props
                            .get("description")
                            .and_then(|v| String::try_from(v.clone()).ok())
                            .unwrap_or_default();
                        (id, desc)
                    })
                    .collect();
                st.binds.push(recorded);
                st.bound.push(session.to_string());
                0
            }
        };
        let mut results = HashMap::new();
        if code == 0 {
            results.insert("shortcuts", Value::from(shortcut_list()));
        }
        respond(conn, &request, code, results).await;
        ObjectPath::try_from(request).unwrap().into()
    }

    async fn list_shortcuts(
        &self,
        session: OwnedObjectPath,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> OwnedObjectPath {
        let request = format!(
            "/org/freedesktop/portal/desktop/request/{}/{}",
            sender_id(&hdr),
            token_of(&options, "handle_token")
        );
        let bound = {
            let mut st = self.state.lock().unwrap();
            let bound = st.bound.contains(&session.to_string());
            if st.hold_lists {
                st.held.push((request.clone(), bound));
                return ObjectPath::try_from(request).unwrap().into();
            }
            bound
        };
        answer_list(conn, &request, bound).await;
        ObjectPath::try_from(request).unwrap().into()
    }

    // Per the interface XML, ConfigureShortcuts is a PLAIN method: in-args
    // only, no request handle, no Response signal (options support only
    // activation_token). A client doing the Request dance here would hang
    // or misparse against a real portal — the fake must not humor it.
    fn configure_shortcuts(
        &self,
        session: OwnedObjectPath,
        _parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) {
        self.state
            .lock()
            .unwrap()
            .configures
            .push(session.to_string());
    }
}

/// The next event, or a panic with what actually arrived (strict ordering:
/// a mis-filtered foreign signal would surface here as the wrong event).
fn expect_event(rx: &mpsc::Receiver<PortalEvent>, want: PortalEvent) {
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(got) => assert_eq!(got, want),
        Err(e) => panic!("no event within 10 s (wanted {want:?}): {e}"),
    }
}

fn wait_until(mut done: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn portal_worker_round_trip_on_a_private_bus() {
    // No service directories: phase 0's missing portal must stay missing
    // (a stock session bus would start the host's real one).
    let Some(bus) = common::private_bus("portal_worker_round_trip_on_a_private_bus") else {
        return;
    };
    let addr = bus.addr.clone();
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &addr);

    // --- phase 0: a bus with no portal on it -> Unavailable ---
    let (ev_tx, ev) = mpsc::channel();
    let cmds = shortcuts::spawn(ev_tx, false);
    // Every worker first says which client serves it.
    expect_event(&ev, PortalEvent::Backend(shortcuts::Backend::Portal));
    expect_event(&ev, PortalEvent::Unavailable);

    // --- phase 1: the portal appears; a Retry brings the worker up ---
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("test runtime");
    let fake = FakePortal {
        version: 2, // ConfigureShortcuts available
        ..FakePortal::default()
    };
    let state = fake.state.clone();
    let registry = FakeRegistry {
        state: state.clone(),
    };
    let server = rt.block_on(async {
        zbus::connection::Builder::session()
            .expect("mock: session builder")
            .name("org.freedesktop.portal.Desktop")
            .expect("mock: claim portal name")
            .serve_at("/org/freedesktop/portal/desktop", fake)
            .expect("mock: serve object")
            .serve_at("/org/freedesktop/portal/desktop", registry)
            .expect("mock: serve registry")
            .build()
            .await
            .expect("mock: build connection")
    });
    cmds.send(Cmd::Retry).expect("worker alive");
    expect_event(&ev, PortalEvent::Available);
    // Never bound: the listing has no keys.
    expect_event(&ev, keys_event(false));
    wait_until(
        || !state.lock().unwrap().sessions.is_empty(),
        "the worker's CreateSession",
    );
    let session = state.lock().unwrap().sessions[0].clone();
    // Registered strictly BEFORE the session (interleaved call log, not
    // two lists): a terminal launch has no derivable app id, KDE refuses
    // CreateSession without one, and Register must be the first portal
    // call on the connection.
    assert_eq!(
        state.lock().unwrap().order.first().map(String::as_str),
        Some("register:hushmic"),
        "app-id registration must precede every other portal call"
    );

    // --- phase 2: signal delivery, session- and id-filtered ---
    let emit = |member: &'static str, sess: String, id: &'static str| {
        rt.block_on(async {
            server
                .emit_signal(
                    Option::<&str>::None,
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.GlobalShortcuts",
                    member,
                    &(
                        ObjectPath::try_from(sess).unwrap(),
                        id,
                        7u64,
                        HashMap::<String, Value>::new(),
                    ),
                )
                .await
                .expect("emit shortcut signal")
        })
    };
    // Another app's session first: if the filter leaked, the wrong event
    // would arrive before ours and fail the strict-order expectations.
    let foreign = "/org/freedesktop/portal/desktop/session/other/app".to_string();
    emit("Activated", foreign.clone(), "toggle-bypass");
    emit("Activated", session.clone(), "toggle-mute");
    expect_event(&ev, PortalEvent::Activated(Action::ToggleMute));
    // Unknown ids on OUR session are skipped, not fatal.
    emit("Activated", session.clone(), "frobnicate");
    emit("Deactivated", session.clone(), "push-to-talk");
    expect_event(&ev, PortalEvent::Deactivated(Action::PushToTalk));

    // --- phase 3: interactive bind ("Set up shortcuts…") ---
    cmds.send(Cmd::Bind).expect("worker alive");
    // The bind's answer carries the keys, ahead of the done.
    expect_event(&ev, keys_event(true));
    expect_event(&ev, PortalEvent::BindDone);
    {
        let st = state.lock().unwrap();
        assert_eq!(st.binds.len(), 1, "one BindShortcuts call");
        let want: Vec<(String, String)> = Action::ALL
            .iter()
            .map(|a| (a.id().to_string(), a.description().to_string()))
            .collect();
        assert_eq!(st.binds[0], want, "all actions bound with their labels");
    }

    // --- phase 3b: once set up, the click means ConfigureShortcuts ---
    cmds.send(Cmd::Configure).expect("worker alive");
    wait_until(
        || state.lock().unwrap().configures.len() == 1,
        "the ConfigureShortcuts call",
    );
    {
        let st = state.lock().unwrap();
        assert_eq!(st.configures[0], session, "editor opens OUR session");
        assert_eq!(st.binds.len(), 1, "configure must not re-bind");
    }

    // --- phase 3c: the user changed a key in the editor: ShortcutsChanged
    // reports it, for our session only, and only the changed action ---
    let changed = |sess: String, id: &str, keys: &str| {
        let mut props = HashMap::new();
        props.insert("trigger_description".to_string(), Value::from(keys));
        rt.block_on(async {
            server
                .emit_signal(
                    Option::<&str>::None,
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.GlobalShortcuts",
                    "ShortcutsChanged",
                    &(
                        ObjectPath::try_from(sess).unwrap(),
                        vec![(id.to_string(), props)],
                    ),
                )
                .await
                .expect("emit ShortcutsChanged")
        })
    };
    changed(foreign.clone(), "push-to-talk", "F12");
    changed(session.clone(), "push-to-talk", "Meta+Space");
    expect_event(
        &ev,
        PortalEvent::Keys(vec![(Action::PushToTalk, Keys::Text("Meta+Space".into()))]),
    );

    // --- phase 4: a second start with shortcuts_setup re-binds silently ---
    let (ev2_tx, ev2) = mpsc::channel();
    let _cmds2 = shortcuts::spawn(ev2_tx, true);
    // Every worker first says which client serves it.
    expect_event(&ev2, PortalEvent::Backend(shortcuts::Backend::Portal));
    expect_event(&ev2, PortalEvent::Available);
    // The silent re-bind's keys, after Available.
    expect_event(&ev2, keys_event(true));
    wait_until(
        || state.lock().unwrap().binds.len() == 2,
        "the silent re-bind",
    );
    assert_eq!(
        state.lock().unwrap().sessions.len(),
        2,
        "each worker owns its own session"
    );

    // --- phase 5: a cancelled bind dialog burns the session's single
    // BindShortcuts attempt — the worker must tear down and give the next
    // click a fresh session ---
    let (ev3_tx, ev3) = mpsc::channel();
    let cmds3 = shortcuts::spawn(ev3_tx, false);
    // Every worker first says which client serves it.
    expect_event(&ev3, PortalEvent::Backend(shortcuts::Backend::Portal));
    expect_event(&ev3, PortalEvent::Available);
    expect_event(&ev3, keys_event(false));
    state.lock().unwrap().cancel_next_bind = true;
    cmds3.send(Cmd::Bind).expect("worker alive");
    expect_event(&ev3, PortalEvent::Unavailable);
    cmds3.send(Cmd::Retry).expect("worker alive");
    expect_event(&ev3, PortalEvent::Available);
    expect_event(&ev3, keys_event(false));
    cmds3.send(Cmd::Bind).expect("worker alive");
    expect_event(&ev3, keys_event(true));
    expect_event(&ev3, PortalEvent::BindDone);
    assert_eq!(
        state.lock().unwrap().binds.len(),
        3,
        "retry after cancel binds on the fresh session"
    );

    // --- phase 6: the portal dies — a long-lived tray must NOTICE (owner
    // watch), not keep a zombie "Available" with silently dead hotkeys ---
    drop(server);
    expect_event(&ev, PortalEvent::Unavailable);

    // --- phase 7: a v1 portal has no ConfigureShortcuts — the click must
    // surface the settings hint, never silence ---
    let fake_v1 = FakePortal {
        version: 1,
        ..FakePortal::default()
    };
    let state_v1 = fake_v1.state.clone();
    let server_v1 = rt.block_on(async {
        zbus::connection::Builder::session()
            .expect("mock v1: session builder")
            .name("org.freedesktop.portal.Desktop")
            .expect("mock v1: claim portal name")
            .serve_at("/org/freedesktop/portal/desktop", fake_v1)
            .expect("mock v1: serve object")
            .build()
            .await
            .expect("mock v1: build connection")
    });
    let (ev4_tx, ev4) = mpsc::channel();
    let cmds4 = shortcuts::spawn(ev4_tx, false);
    // Every worker first says which client serves it.
    expect_event(&ev4, PortalEvent::Backend(shortcuts::Backend::Portal));
    // The v2->v1 name handoff is asynchronous on the daemon side: tolerate
    // a transient failed attempt (routes to the dead old owner) and drive
    // the ordinary Retry path until the worker is up.
    let mut up = false;
    for _ in 0..20 {
        match ev4
            .recv_timeout(Duration::from_secs(10))
            .expect("worker4 event")
        {
            PortalEvent::Available => {
                up = true;
                break;
            }
            PortalEvent::Unavailable => {
                std::thread::sleep(Duration::from_millis(100));
                cmds4.send(Cmd::Retry).expect("worker alive");
            }
            other => panic!("unexpected event while connecting: {other:?}"),
        }
    }
    assert!(up, "worker never reached the v1 portal");
    expect_event(&ev4, keys_event(false));
    cmds4.send(Cmd::Configure).expect("worker alive");
    expect_event(&ev4, PortalEvent::ConfigureUnavailable);

    // --- phase 8: a portal that never answers ListShortcuts must not hold
    // key presses back (the listing runs beside the event loop) ---
    state_v1.lock().unwrap().hold_lists = true;
    let (ev5_tx, ev5) = mpsc::channel();
    let cmds5 = shortcuts::spawn(ev5_tx, false);
    expect_event(&ev5, PortalEvent::Backend(shortcuts::Backend::Portal));
    let mut up = false;
    for _ in 0..20 {
        match ev5
            .recv_timeout(Duration::from_secs(10))
            .expect("worker5 event")
        {
            PortalEvent::Available => {
                up = true;
                break;
            }
            PortalEvent::Unavailable => {
                std::thread::sleep(Duration::from_millis(100));
                cmds5.send(Cmd::Retry).expect("worker alive");
            }
            other => panic!("unexpected event while connecting: {other:?}"),
        }
    }
    assert!(up, "worker5 never came up");
    let session5 = state_v1.lock().unwrap().sessions.last().unwrap().clone();
    rt.block_on(async {
        server_v1
            .emit_signal(
                Option::<&str>::None,
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.GlobalShortcuts",
                "Activated",
                &(
                    ObjectPath::try_from(session5).unwrap(),
                    "toggle-mute",
                    7u64,
                    HashMap::<String, Value>::new(),
                ),
            )
            .await
            .expect("emit Activated")
    });
    match ev5.recv_timeout(Duration::from_secs(2)) {
        Ok(got) => assert_eq!(got, PortalEvent::Activated(Action::ToggleMute)),
        Err(e) => panic!("a key press waited on the unanswered listing: {e}"),
    }

    // --- phase 9: a listing asked before a bind and answered after it
    // must not overwrite the bind's keys with its older answer ---
    wait_until(
        || {
            state_v1
                .lock()
                .unwrap()
                .held
                .iter()
                .any(|(_, bound)| !bound)
        },
        "the listing to reach the portal",
    );
    cmds5.send(Cmd::Bind).expect("worker alive");
    expect_event(&ev5, keys_event(true));
    expect_event(&ev5, PortalEvent::BindDone);
    let held: Vec<(String, bool)> = std::mem::take(&mut state_v1.lock().unwrap().held);
    assert!(
        held.iter().any(|(_, bound)| !bound),
        "the pre-bind listing is still unanswered"
    );
    rt.block_on(async {
        for (request, bound) in &held {
            answer_list(&server_v1, request, *bound).await;
        }
    });
    match ev5.recv_timeout(Duration::from_millis(1500)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(got) => panic!("the stale listing was reported after the bind: {got:?}"),
        Err(e) => panic!("worker5 gone: {e}"),
    }
}
