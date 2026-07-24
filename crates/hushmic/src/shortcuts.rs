//! Global shortcuts via the `org.freedesktop.portal.GlobalShortcuts`
//! desktop portal. The compositor owns the keys and the binding dialog;
//! this module owns the action table, the press/release semantics (pure,
//! tested), and the portal client plumbing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::controller::RunMode;

/// The four bindable actions. Registering all of them is free — anything
/// the user leaves unbound in the compositor dialog is inert. No mode-off
/// hotkey by design (a mic-kill surprise).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    ToggleMute,
    ToggleBypass,
    PushToTalk,
    PushToMute,
}

impl Action {
    pub const ALL: [Action; 4] = [
        Action::ToggleMute,
        Action::ToggleBypass,
        Action::PushToTalk,
        Action::PushToMute,
    ];

    /// Stable portal shortcut id (what the compositor stores bindings by).
    pub fn id(&self) -> &'static str {
        match self {
            Action::ToggleMute => "toggle-mute",
            Action::ToggleBypass => "toggle-bypass",
            Action::PushToTalk => "push-to-talk",
            Action::PushToMute => "push-to-mute",
        }
    }

    /// User-facing string, shown verbatim in the DE's binding dialog.
    pub fn description(&self) -> &'static str {
        match self {
            Action::ToggleMute => "Toggle mute",
            Action::ToggleBypass => "Toggle bypass",
            Action::PushToTalk => "Push to talk",
            Action::PushToMute => "Push to mute",
        }
    }

    /// Reverse of [`Action::id`]; unknown ids are ignored, never a panic
    /// (a newer/foreign compositor could signal anything).
    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.id() == id)
    }
}

/// The `shortcuts` argument for `BindShortcuts` (`a(sa{sv})` on the wire):
/// every action with the description the compositor's binding dialog shows.
/// Registering all four is free — unbound ones are inert.
pub fn bind_payload() -> Vec<(String, HashMap<String, Value<'static>>)> {
    Action::ALL
        .iter()
        .map(|a| {
            let mut props = HashMap::new();
            props.insert("description".to_string(), Value::from(a.description()));
            (a.id().to_string(), props)
        })
        .collect()
}

/// Map an `Activated`/`Deactivated` signal to one of our actions. The
/// portal object is shared bus-wide, so a signal for another app's session
/// (or an id we never registered) must resolve to nothing, never a panic.
pub fn signal_action(our_session: &str, signal_session: &str, id: &str) -> Option<Action> {
    (our_session == signal_session)
        .then(|| Action::from_id(id))
        .flatten()
}

/// What the hold keys are currently doing, owned by the main loop and
/// threaded through every [`shortcut_transition`] call. Two jobs:
/// push-to-mute's release may only restore a mute ITS OWN press performed
/// (a tap while already muted must never un-mute — the hot-mic corner),
/// and a push-to-talk hold left dangling by a dying portal session lets
/// the main loop re-mute (the key's contract is live-only-while-held).
#[derive(Default)]
pub struct Holds {
    /// The push-to-talk key is down (any press, even a no-op one).
    ptt: bool,
    /// The last push-to-mute press actually performed the mute.
    ptm_armed: bool,
}

impl Holds {
    /// A push-to-talk hold is in progress — consulted when the portal
    /// session dies (the release would never arrive; err toward muted).
    pub fn ptt_held(&self) -> bool {
        self.ptt
    }
    /// The session died: any release we were waiting for is lost.
    pub fn reset(&mut self) {
        *self = Holds::default();
    }
}

/// The press/release state machine: what mode (if any) a shortcut event
/// selects, given the current selection, the toggle overlay's return
/// address, and the hold state. `None` = no-op. Guiding principle for
/// every ambiguous corner: err toward MUTED.
pub fn shortcut_transition(
    action: Action,
    activated: bool,
    current: Option<RunMode>,
    prev_alive: RunMode,
    holds: &mut Holds,
) -> Option<Option<RunMode>> {
    match (action, activated) {
        // plain toggles: the CLI overlay machine, press only
        (Action::ToggleMute, true) => Some(crate::control::toggle_next(
            current,
            prev_alive,
            RunMode::Mute,
        )),
        (Action::ToggleBypass, true) => Some(crate::control::toggle_next(
            current,
            prev_alive,
            RunMode::Bypass,
        )),
        (Action::ToggleMute | Action::ToggleBypass, false) => None,

        // push-to-talk: live while held (entering from Mute or Off),
        // muted on release — the key's contract survives mid-hold state
        // changes because the safe direction is muted
        (Action::PushToTalk, true) => {
            holds.ptt = true;
            match current {
                Some(RunMode::Mute) | None => Some(Some(prev_alive)),
                Some(_) => None, // already live
            }
        }
        // release: mute — but only an AUDIBLE state; already muted is a
        // clean no-op, and from Off there is nothing live to silence
        // (spawning a muted chain would fight a mid-hold Off click, or a
        // stray release in a startup race)
        (Action::PushToTalk, false) => {
            holds.ptt = false;
            match current {
                Some(m) if m != RunMode::Mute => Some(Some(RunMode::Mute)),
                _ => None,
            }
        }

        // push-to-mute: silence while held. The press ARMS the release:
        // only a mute this press performed may be restored — tapping the
        // cough button while already muted must never end with a live mic.
        (Action::PushToMute, true) => match current {
            Some(RunMode::Mute) | None => {
                holds.ptm_armed = false;
                None
            }
            Some(_) => {
                holds.ptm_armed = true;
                Some(Some(RunMode::Mute))
            }
        },
        // release restores only if armed AND still muted (a mid-hold
        // change by the user wins; a mid-hold toggle-mute round trip
        // landing back on Mute still restores — acceptable: the user's
        // last action left the mic muted and the key promises "silent
        // only while held")
        (Action::PushToMute, false) => {
            let fire = holds.ptm_armed && current == Some(RunMode::Mute);
            holds.ptm_armed = false;
            fire.then_some(Some(prev_alive))
        }
    }
}

// ---------------------------------------------------------------------------
// Portal client: one worker thread owning the session and the signal stream
// ---------------------------------------------------------------------------

const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS_IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

/// Non-interactive round-trips (CreateSession, silent re-bind) — backends
/// answer these without prompting; the bound only guards a wedged portal.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
/// The interactive BindShortcuts dialog waits on the USER: a timeout here
/// tears the session down and DISMISSES the compositor's dialog mid-use,
/// so it is generous (10 min — first-time assignment of four keys can be
/// slow) and exists only so a wedged portal cannot hold the worker
/// forever. Already-bound shortcuts stay grabbed while a dialog is open —
/// their events queue on the (single, ordered) signal stream and replay
/// in order afterwards.
const DIALOG_TIMEOUT: Duration = Duration::from_secs(600);

/// What the worker reports into the main loop (bridged to `Event::Shortcut`).
#[derive(Debug, PartialEq)]
pub enum PortalEvent {
    /// Session up, signals flowing — show the tray entry.
    Available,
    /// Setup failed or the session died — hide the entry; the main loop
    /// retries via `Cmd::Retry`, throttled through a watchdog Backoff.
    Unavailable,
    Activated(Action),
    Deactivated(Action),
    /// The interactive bind dialog returned successfully — persist
    /// `shortcuts_setup` so future starts re-bind silently.
    BindDone,
    /// ConfigureShortcuts could not be CALLED (v1 portal, or the call
    /// errored) — the main loop points the user at the system's own
    /// keyboard-shortcut settings instead.
    ConfigureUnavailable,
}

/// Main-loop -> worker commands.
#[derive(Debug)]
pub enum Cmd {
    /// First-time interactive BindShortcuts ("Set up shortcuts…").
    Bind,
    /// Open the compositor's configuration UI for the existing binds
    /// ("Change shortcuts…", portal v2's ConfigureShortcuts). Compositors
    /// show the Bind dialog only for UNconfigured shortcuts (KDE re-binds
    /// silently), so this is the only dialog path once set up.
    Configure,
    /// Reconnect attempt after Unavailable (backoff-gated watchdog tick).
    Retry,
}

/// Start the worker thread: probe the portal, create a session, and stream
/// shortcut events. `rebind_on_start` re-registers the actions silently
/// (config.shortcuts_setup — the compositor owns the keys and does not
/// re-prompt for an already-configured app). Never blocks; all failures are
/// reported as `Unavailable` and logged, never surfaced to the user.
pub fn spawn(
    events: std::sync::mpsc::Sender<PortalEvent>,
    rebind_on_start: bool,
) -> tokio::sync::mpsc::UnboundedSender<Cmd> {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        // Current-thread runtime: zbus is pinned tokio-mode workspace-wide,
        // and only async zbus can bound signal waits (see portal.rs).
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(worker(events, rebind_on_start, cmd_rx)),
            Err(e) => {
                eprintln!("[hushmic] global-shortcuts worker could not start: {e}");
                let _ = events.send(PortalEvent::Unavailable);
            }
        }
    });
    cmd_tx
}

async fn worker(
    events: std::sync::mpsc::Sender<PortalEvent>,
    mut rebind: bool,
    mut cmds: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
) {
    loop {
        match serve_session(&events, &mut rebind, &mut cmds).await {
            Ok(()) => return, // main loop gone: exit quietly
            Err(e) => eprintln!("[hushmic] global-shortcuts portal: {e}"),
        }
        if events.send(PortalEvent::Unavailable).is_err() {
            return;
        }
        // Down: wait for the main loop's backoff-gated Retry. Any command
        // wakes a reconnect — a Bind or Configure that races the death
        // just reconnects (the menu entry is hidden while we are down, so
        // the click's intent is gone anyway).
        if cmds.recv().await.is_none() {
            return;
        }
    }
}

/// One session's lifetime: connect, probe, create, (re-)bind, then pump
/// signals and commands until something breaks. `Ok` = the main loop went
/// away (clean shutdown); `Err` = portal trouble worth a retry.
async fn serve_session(
    events: &std::sync::mpsc::Sender<PortalEvent>,
    rebind: &mut bool,
    cmds: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>,
) -> Result<(), String> {
    // The outer timeout bounds the whole non-interactive setup — zbus
    // applies no default method timeout, and a portal that accepts the
    // connection but never replies would wedge the worker silently.
    let (conn, portal, session, version) =
        tokio::time::timeout(RESPONSE_TIMEOUT + Duration::from_secs(5), setup_session())
            .await
            .map_err(|_| "timed out setting up the portal session".to_string())??;

    // Subscribe before announcing: an activation right after a silent
    // re-bind must not fall in a gap. ONE stream for both members —
    // press/release ORDER is semantics (a reordered tap could end with a
    // live mic), and two independent streams lose cross-stream order
    // whenever both hold a backlog (keys tapped while the worker awaits
    // an open bind/configure dialog; tokio::select! polls its branches in
    // random order). A single stream preserves connection arrival order.
    let mut signals = portal
        .receive_all_signals()
        .await
        .map_err(|e| e.to_string())?;
    // A dying portal must not leave a zombie "Available" (tray entry up,
    // hotkeys silently dead until hushmic restarts): watch the session's
    // Closed signal (orderly close of OUR session) and the portal name's
    // owner (a crashed/restarted portal emits no Closed). Either path
    // lands in the ordinary Unavailable -> backoff-retry flow.
    let session_proxy = zbus::Proxy::new(
        &conn,
        PORTAL_NAME,
        session.clone(),
        "org.freedesktop.portal.Session",
    )
    .await
    .map_err(|e| e.to_string())?;
    let mut closed = session_proxy
        .receive_signal("Closed")
        .await
        .map_err(|e| e.to_string())?;
    let dbus = zbus::fdo::DBusProxy::new(&conn)
        .await
        .map_err(|e| e.to_string())?;
    let mut owner_changes = dbus
        .receive_name_owner_changed()
        .await
        .map_err(|e| e.to_string())?;
    if *rebind {
        // Silent re-registration so events flow; failure is a session
        // failure (retried gently) — Available without events would lie.
        bind(&conn, &portal, &session).await?;
    }
    // The ONE positive line of a healthy worker: success must not be
    // silent, or external health checks have nothing to grep for.
    eprintln!("[hushmic] global-shortcuts: session up (portal v{version})");
    if events.send(PortalEvent::Available).is_err() {
        return Ok(());
    }
    loop {
        let sent = tokio::select! {
            m = signals.next() => {
                let msg = m.ok_or("the portal signal stream ended")?;
                let member = msg.header().member().map(|n| n.as_str().to_owned());
                match member.as_deref() {
                    Some(dir @ ("Activated" | "Deactivated")) => {
                        match parse_signal(&session, &msg) {
                            Some(a) if dir == "Activated" =>
                                events.send(PortalEvent::Activated(a)),
                            Some(a) => events.send(PortalEvent::Deactivated(a)),
                            None => Ok(()),
                        }
                    }
                    // ShortcutsChanged etc.: nothing we act on.
                    _ => Ok(()),
                }
            },
            m = closed.next() => {
                let _ = m;
                return Err("the portal closed the session".to_string());
            },
            c = owner_changes.next() => match c {
                None => return Err("the bus connection dropped".to_string()),
                Some(sig) => {
                    let portal_died = sig
                        .args()
                        .map(|a| a.name.as_str() == PORTAL_NAME)
                        .unwrap_or(false);
                    if portal_died {
                        return Err("the portal restarted (name owner changed)".to_string());
                    }
                    Ok(())
                }
            },
            c = cmds.recv() => match c {
                None => return Ok(()),
                Some(Cmd::Retry) => Ok(()), // already up
                Some(Cmd::Bind) => match bind(&conn, &portal, &session).await {
                    Ok(()) => {
                        *rebind = true;
                        events.send(PortalEvent::BindDone)
                    }
                    // A failed or CANCELLED dialog burns this session's
                    // single BindShortcuts attempt (the portal spec allows
                    // one per session): tear the session down so the retry
                    // path hands the next click a fresh one.
                    Err(e) => return Err(format!("interactive bind did not complete: {e}")),
                },
                Some(Cmd::Configure) => {
                    if version >= 2 {
                        match configure(&portal, &session).await {
                            Ok(()) => Ok(()),
                            // Only a failed CALL means the editor cannot
                            // open from here — tell the main loop so the
                            // user gets the settings hint instead of
                            // silence. (A slow dialog timing out is not
                            // that; it already opened.)
                            Err(e) if e.starts_with("ConfigureShortcuts failed") => {
                                eprintln!("[hushmic] {e}");
                                events.send(PortalEvent::ConfigureUnavailable)
                            }
                            Err(e) => {
                                eprintln!("[hushmic] shortcut configure: {e}");
                                Ok(())
                            }
                        }
                    } else {
                        events.send(PortalEvent::ConfigureUnavailable)
                    }
                }
            },
        };
        if sent.is_err() {
            return Ok(());
        }
    }
}

/// Connect, probe the interface, create our session. The probe (a readable
/// `version` property) is what decides tray-entry visibility; the version
/// gates ConfigureShortcuts (v2+).
async fn setup_session() -> Result<(zbus::Connection, zbus::Proxy<'static>, String, u32), String> {
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| e.to_string())?;
    // Terminal/script launches run in the plain session scope, where the
    // portal cannot derive an app id — and KDE then refuses CreateSession
    // outright ("An app id is required"), which used to hide the menu
    // entry depending on HOW hushmic was started. Non-sandboxed apps
    // register their id explicitly (host Registry, xdg-desktop-portal
    // 1.18+); it must be the FIRST portal call on this connection.
    // Best-effort: a sandboxed app's id comes from the sandbox (the host
    // registry would refuse it), older portals lack the interface, and an
    // app-scope launch works unregistered.
    if crate::sandbox::flatpak_app_id().is_none() {
        let options: HashMap<String, Value> = HashMap::new();
        if let Err(e) = conn
            .call_method(
                Some(PORTAL_NAME),
                PORTAL_PATH,
                Some("org.freedesktop.host.portal.Registry"),
                "Register",
                &("hushmic", options),
            )
            .await
        {
            eprintln!("[hushmic] portal app-id registration unavailable: {e}");
        }
    }
    let portal = zbus::Proxy::new(&conn, PORTAL_NAME, PORTAL_PATH, SHORTCUTS_IFACE)
        .await
        .map_err(|e| e.to_string())?;
    let version: u32 = portal
        .get_property("version")
        .await
        .map_err(|e| format!("no GlobalShortcuts portal: {e}"))?;
    let session = create_session(&conn, &portal).await?;
    Ok((conn, portal, session, version))
}

async fn create_session(
    conn: &zbus::Connection,
    portal: &zbus::Proxy<'static>,
) -> Result<String, String> {
    let session_token = fresh_token("hushmic_s");
    let (code, results) = portal_request(conn, portal, "CreateSession", RESPONSE_TIMEOUT, |t| {
        let mut options: HashMap<String, Value> = HashMap::new();
        options.insert("handle_token".into(), t.to_string().into());
        options.insert("session_handle_token".into(), session_token.clone().into());
        (options,)
    })
    .await?;
    if code != 0 {
        return Err(format!("the session request was not granted (code {code})"));
    }
    // The path is predictable per the portal spec; prefer the backend's
    // answer when present (string on KDE/GNOME, but accept a path too).
    let predicted = format!(
        "/org/freedesktop/portal/desktop/session/{}/{session_token}",
        sender_id(conn)?
    );
    Ok(results
        .get("session_handle")
        .and_then(|v| match &**v {
            Value::Str(s) => Some(s.to_string()),
            Value::ObjectPath(p) => Some(p.to_string()),
            _ => None,
        })
        .unwrap_or(predicted))
}

/// BindShortcuts: registers all four actions on the session. Interactive
/// use may show the compositor's dialog; an already-configured app id is
/// answered silently. A non-zero response (incl. the user cancelling) is
/// an Err for the caller to log.
async fn bind(
    conn: &zbus::Connection,
    portal: &zbus::Proxy<'static>,
    session: &str,
) -> Result<(), String> {
    let session_path =
        ObjectPath::try_from(session.to_string()).map_err(|e| format!("bad session path: {e}"))?;
    let (code, _results) = portal_request(conn, portal, "BindShortcuts", DIALOG_TIMEOUT, |t| {
        let mut options: HashMap<String, Value> = HashMap::new();
        options.insert("handle_token".into(), t.to_string().into());
        (session_path.clone(), bind_payload(), String::new(), options)
    })
    .await?;
    if code != 0 {
        return Err(format!("the binding request was not granted (code {code})"));
    }
    Ok(())
}

/// ConfigureShortcuts (portal v2): opens the compositor's shortcut editor
/// for this session's binds — the change-keys path once shortcuts are set
/// up. Unlike every other portal method here it is a PLAIN method per the
/// interface XML: no request handle out-argument, no Response signal, and
/// the only supported option is an activation_token (which a tray has no
/// window to mint). The reply just acknowledges that the editor was
/// requested; only a failed call means it could not open.
async fn configure(portal: &zbus::Proxy<'static>, session: &str) -> Result<(), String> {
    let session_path =
        ObjectPath::try_from(session.to_string()).map_err(|e| format!("bad session path: {e}"))?;
    let options: HashMap<String, Value> = HashMap::new();
    let reply = tokio::time::timeout(
        RESPONSE_TIMEOUT,
        portal.call::<_, _, ()>(
            "ConfigureShortcuts",
            &(session_path, String::new(), options),
        ),
    )
    .await
    .map_err(|_| "timed out calling ConfigureShortcuts".to_string())?;
    reply.map_err(|e| format!("ConfigureShortcuts failed: {e}"))
}

/// The portal Request/Response dance (portal.rs's shape, generalized):
/// subscribe on the predictable request path BEFORE calling — the response
/// can fire before the method reply is processed on our side. `build`
/// receives the handle_token and returns the method's full argument tuple.
async fn portal_request<B>(
    conn: &zbus::Connection,
    portal: &zbus::Proxy<'static>,
    method: &str,
    timeout: Duration,
    build: impl FnOnce(&str) -> B,
) -> Result<(u32, HashMap<String, OwnedValue>), String>
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
{
    let token = fresh_token("hushmic");
    let request_path = format!(
        "/org/freedesktop/portal/desktop/request/{}/{token}",
        sender_id(conn)?
    );
    let request_proxy = zbus::Proxy::new(
        conn,
        PORTAL_NAME,
        request_path.clone(),
        "org.freedesktop.portal.Request",
    )
    .await
    .map_err(|e| e.to_string())?;
    let mut responses = request_proxy
        .receive_signal("Response")
        .await
        .map_err(|e| e.to_string())?;
    let body = build(&token);
    let returned: OwnedObjectPath = portal
        .call(method, &body)
        .await
        .map_err(|e| format!("{method} failed: {e}"))?;
    // Portals since 2017 honor handle_token, so the returned handle is the
    // path we subscribed on. If an exotic backend returns a different one,
    // re-subscribe there — a response lost in that gap surfaces as the
    // timeout below.
    if returned.as_str() != request_path {
        let request_proxy = zbus::Proxy::new(
            conn,
            PORTAL_NAME,
            returned.as_str().to_string(),
            "org.freedesktop.portal.Request",
        )
        .await
        .map_err(|e| e.to_string())?;
        responses = request_proxy
            .receive_signal("Response")
            .await
            .map_err(|e| e.to_string())?;
    }
    let msg = tokio::time::timeout(timeout, responses.next())
        .await
        .map_err(|_| format!("timed out waiting for the {method} response"))?
        .ok_or("the portal response stream ended without a response")?;
    msg.body().deserialize().map_err(|e| e.to_string())
}

/// A payload that does not deserialize or does not concern our session
/// resolves to no action.
fn parse_signal(session: &str, msg: &zbus::Message) -> Option<Action> {
    type Body = (OwnedObjectPath, String, u64, HashMap<String, OwnedValue>);
    let (path, id, _timestamp, _options) = msg.body().deserialize::<Body>().ok()?;
    signal_action(session, path.as_str(), &id)
}

fn sender_id(conn: &zbus::Connection) -> Result<String, String> {
    Ok(conn
        .unique_name()
        .ok_or("session bus connection has no unique name")?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_"))
}

fn fresh_token(prefix: &str) -> String {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    format!(
        "{prefix}_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use RunMode::*;

    #[test]
    fn bind_payload_registers_every_action_with_its_description() {
        // The BindShortcuts argument: all four actions, each carrying the
        // user-facing description the compositor shows in its dialog.
        let payload = bind_payload();
        assert_eq!(payload.len(), Action::ALL.len());
        for (action, (id, props)) in Action::ALL.iter().zip(&payload) {
            assert_eq!(id, action.id());
            let desc = props.get("description").expect("description property");
            assert_eq!(
                String::try_from(desc.clone()).expect("description is a string"),
                action.description()
            );
        }
    }

    #[test]
    fn signal_action_filters_foreign_sessions_and_unknown_ids() {
        let ours = "/org/freedesktop/portal/desktop/session/x/hushmic_s_1";
        assert_eq!(
            signal_action(ours, ours, "toggle-mute"),
            Some(Action::ToggleMute)
        );
        // Another app's session signaling on the shared portal object must
        // never fire our actions, even with a colliding shortcut id.
        assert_eq!(
            signal_action(
                ours,
                "/org/freedesktop/portal/desktop/session/x/other",
                "toggle-mute"
            ),
            None
        );
        // A foreign/newer id on our session is ignored, never a panic.
        assert_eq!(signal_action(ours, ours, "frobnicate"), None);
    }

    #[test]
    fn ids_and_descriptions_are_the_spec_strings() {
        let table = [
            (Action::ToggleMute, "toggle-mute", "Toggle mute"),
            (Action::ToggleBypass, "toggle-bypass", "Toggle bypass"),
            (Action::PushToTalk, "push-to-talk", "Push to talk"),
            (Action::PushToMute, "push-to-mute", "Push to mute"),
        ];
        for (a, id, desc) in table {
            assert_eq!(a.id(), id);
            assert_eq!(a.description(), desc);
            assert_eq!(Action::from_id(id), Some(a), "{id} round-trips");
        }
        assert_eq!(Action::from_id("frobnicate"), None);
        assert_eq!(Action::from_id(""), None);
    }

    #[test]
    fn toggles_mirror_the_cli_overlay() {
        // Same machine as `hushmic toggle mute|bypass`.
        let mut h = Holds::default();
        let t =
            |cur, prev, h: &mut Holds| shortcut_transition(Action::ToggleMute, true, cur, prev, h);
        assert_eq!(t(Some(Suppress), Suppress, &mut h), Some(Some(Mute)));
        assert_eq!(
            t(Some(Mute), Bypass, &mut h),
            Some(Some(Bypass)),
            "overlay returns"
        );
        assert_eq!(
            t(None, Suppress, &mut h),
            Some(Some(Mute)),
            "from Off: born muted"
        );
        // release of a plain toggle is nothing
        assert_eq!(
            shortcut_transition(Action::ToggleMute, false, Some(Mute), Suppress, &mut h),
            None
        );
        assert_eq!(
            shortcut_transition(Action::ToggleBypass, true, Some(Bypass), Suppress, &mut h),
            Some(Some(Suppress)),
            "toggle-bypass leaves bypass for the previous state"
        );
        assert!(!h.ptt_held(), "toggles never touch the hold state");
    }

    #[test]
    fn push_to_talk_is_live_while_held_muted_after() {
        let mut h = Holds::default();
        let press =
            |cur, prev, h: &mut Holds| shortcut_transition(Action::PushToTalk, true, cur, prev, h);
        // held: leave mute for the previous chain-alive state
        assert_eq!(press(Some(Mute), Suppress, &mut h), Some(Some(Suppress)));
        assert_eq!(
            press(Some(Mute), Bypass, &mut h),
            Some(Some(Bypass)),
            "bypass user talks in bypass"
        );
        // from Off: ENABLE into the previous state (born-muted spawn makes
        // the window leak-free; the user bound the key to be heard)
        assert_eq!(press(None, Suppress, &mut h), Some(Some(Suppress)));
        // already live: pressing does nothing (but the hold is tracked)
        assert_eq!(press(Some(Suppress), Suppress, &mut h), None);
        assert_eq!(press(Some(Bypass), Suppress, &mut h), None);
        assert!(h.ptt_held(), "a press means the key is down");
        // release mutes from every AUDIBLE state — even if the mode changed
        // mid-hold, the key's contract is "live only while held" (err
        // toward muted); already muted, it is a clean no-op (no dead-click
        // config/tray churn)
        for cur in [Some(Suppress), Some(Bypass)] {
            assert_eq!(
                shortcut_transition(Action::PushToTalk, false, cur, Suppress, &mut h),
                Some(Some(Mute)),
                "release from {cur:?}"
            );
        }
        assert_eq!(
            shortcut_transition(Action::PushToTalk, false, Some(Mute), Suppress, &mut h),
            None,
            "release while already muted changes nothing"
        );
        assert!(!h.ptt_held(), "release ends the hold");
        // ...and NOT from Off: Off is already silent, and re-enabling a
        // muted chain would fight a mid-hold Off click (or a stray
        // release during startup races) — same rule as push-to-mute's
        // release guard
        assert_eq!(
            shortcut_transition(Action::PushToTalk, false, None, Suppress, &mut h),
            None,
            "release from Off must not spawn a chain"
        );
    }

    #[test]
    fn push_to_mute_is_the_inverse_cough_button() {
        let mut h = Holds::default();
        let press =
            |cur, prev, h: &mut Holds| shortcut_transition(Action::PushToMute, true, cur, prev, h);
        assert_eq!(press(Some(Suppress), Suppress, &mut h), Some(Some(Mute)));
        // release restores the previous state — this press DID the muting
        assert_eq!(
            shortcut_transition(Action::PushToMute, false, Some(Mute), Bypass, &mut h),
            Some(Some(Bypass))
        );
        assert_eq!(press(Some(Bypass), Suppress, &mut h), Some(Some(Mute)));
        // a mid-hold change by the user wins over the armed restore (and
        // staying muted is the safe direction, so no forced un-mute)
        for cur in [Some(Suppress), Some(Bypass), None] {
            let mut armed = Holds::default();
            assert_eq!(
                shortcut_transition(
                    Action::PushToMute,
                    true,
                    Some(Suppress),
                    Suppress,
                    &mut armed
                ),
                Some(Some(Mute))
            );
            assert_eq!(
                shortcut_transition(Action::PushToMute, false, cur, Suppress, &mut armed),
                None,
                "release from {cur:?} must not fight a mid-hold change"
            );
        }
        // Off: nothing to silence, no chain spawn just to mute it
        let mut h = Holds::default();
        assert_eq!(press(None, Suppress, &mut h), None);
    }

    #[test]
    fn push_to_mute_tap_while_muted_never_unmutes() {
        // THE hot-mic corner: the user muted deliberately (toggle/tray),
        // then taps the cough button. The press has nothing to silence, so
        // the release must not "restore" a state the press never left —
        // otherwise a mute + reflexive tap ends with a live mic.
        let mut h = Holds::default();
        assert_eq!(
            shortcut_transition(Action::PushToMute, true, Some(Mute), Suppress, &mut h),
            None,
            "press while already muted is a no-op"
        );
        assert_eq!(
            shortcut_transition(Action::PushToMute, false, Some(Mute), Suppress, &mut h),
            None,
            "…and its release must stay muted"
        );
        // A stale release (press consumed by a dead session, key held
        // through a reconnect) is equally inert.
        let mut h = Holds::default();
        assert_eq!(
            shortcut_transition(Action::PushToMute, false, Some(Mute), Bypass, &mut h),
            None,
            "release without a live armed press never un-mutes"
        );
    }
}
