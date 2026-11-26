//! The desktop's word that the session is ending, from the Inhibit portal's
//! session monitor (`CreateMonitor`, then `StateChanged`): query-end and
//! ending mean a logout or shutdown is under way. The portal answers from
//! inside a Flatpak, where logind is out of reach; on the host it adds to
//! logind. Backends: GNOME's for long, KDE's since Plasma 6.6; without
//! one, nothing is ever reported.
//!
//! The monitor lives on a thread of its own (connect, create, follow the
//! signals; again after a portal restart, with a pause that grows while
//! the portal keeps failing), and answers every query-end at once: HushMic
//! never holds a logout up.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::shortcuts::{fresh_token, portal_request, PORTAL_NAME, PORTAL_PATH};

/// The portal's `session-state`.
const QUERY_END: u32 = 2;
const ENDING: u32 = 3;

/// A query-end or ending older than this is not the logout under way any
/// more (one the user cancelled, on a backend that never says Running
/// again).
const STALE_AFTER: Duration = Duration::from_secs(60);

/// How long to wait before trying the portal again: the first pause, and
/// the longest (it doubles while the portal keeps failing, e.g. a backend
/// without a session monitor).
const RETRY: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(30 * 60);

/// The pause after `pause` when the portal failed again; a monitor that
/// was up starts over.
fn next_pause(pause: Duration, was_up: bool) -> Duration {
    if was_up {
        RETRY
    } else {
        (pause * 2).min(RETRY_MAX)
    }
}

/// A monitor is in place.
static WATCHING: AtomicBool = AtomicBool::new(false);
/// When the session last said query-end or ending; None while running.
static ENDING_SINCE: Mutex<Option<Instant>> = Mutex::new(None);

/// A monitor is in place: [`ending`] would know.
pub fn watching() -> bool {
    WATCHING.load(Ordering::Relaxed)
}

/// The session is ending (a logout or shutdown under way).
pub fn ending() -> bool {
    ending_at(Instant::now())
}

fn ending_at(now: Instant) -> bool {
    ENDING_SINCE
        .lock()
        .ok()
        .and_then(|s| *s)
        .is_some_and(|t| now.saturating_duration_since(t) < STALE_AFTER)
}

/// Take a `session-state` in: query-end and ending start the ending (or
/// keep its first time), anything else (running) ends it.
fn note_state(state: u32, now: Instant) {
    if let Ok(mut s) = ENDING_SINCE.lock() {
        if state == QUERY_END || state == ENDING {
            s.get_or_insert(now);
        } else {
            *s = None;
        }
    }
}

/// The `session-state` in a `StateChanged` for `session`; None when it is
/// about another monitor or says nothing about it.
pub fn session_state(
    session: &str,
    path: &str,
    state: &HashMap<String, OwnedValue>,
) -> Option<u32> {
    if path != session {
        return None;
    }
    match &**state.get("session-state")? {
        Value::U32(v) => Some(*v),
        Value::Value(v) => match &**v {
            Value::U32(v) => Some(*v),
            _ => None,
        },
        _ => None,
    }
}

/// Start the monitor (the daemon, once).
pub fn spawn() {
    let spawned = std::thread::Builder::new()
        .name("hushmic-session".into())
        .spawn(|| {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[hushmic] session end: no runtime ({e})");
                    return;
                }
            };
            rt.block_on(async {
                // a portal that stays unusable is said once
                let mut said: Option<String> = None;
                let mut pause = RETRY / 2;
                loop {
                    let e = serve().await;
                    let was_up = WATCHING.swap(false, Ordering::Relaxed);
                    if said.as_ref() != Some(&e) {
                        eprintln!("[hushmic] session end: {e}");
                        said = Some(e);
                    }
                    pause = next_pause(pause, was_up);
                    tokio::time::sleep(pause).await;
                }
            });
        });
    if let Err(e) = spawned {
        eprintln!("[hushmic] session end: no thread ({e})");
    }
}

/// Create the monitor and follow it until the portal goes; the error says
/// why it stopped.
async fn serve() -> String {
    match follow().await {
        Ok(()) => "the portal went away".into(),
        Err(e) => e,
    }
}

async fn follow() -> Result<(), String> {
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| format!("no session bus ({e})"))?;
    let portal = zbus::Proxy::new(
        &conn,
        PORTAL_NAME,
        PORTAL_PATH,
        "org.freedesktop.portal.Inhibit",
    )
    .await
    .map_err(|e| e.to_string())?;
    // subscribe before the monitor exists: its first state may come at once
    let mut changes = portal
        .receive_signal("StateChanged")
        .await
        .map_err(|e| e.to_string())?;
    let mut owner = portal
        .receive_owner_changed()
        .await
        .map_err(|e| e.to_string())?;
    let session_token = fresh_token("hushmic_session");
    let (code, results) = portal_request(
        &conn,
        &portal,
        "CreateMonitor",
        Duration::from_secs(10),
        |token| {
            let mut options: HashMap<String, Value> = HashMap::new();
            options.insert("handle_token".into(), token.to_string().into());
            options.insert("session_handle_token".into(), session_token.clone().into());
            ("", options)
        },
    )
    .await?;
    if code != 0 {
        return Err(format!("CreateMonitor answered {code}"));
    }
    let session = match results.get("session_handle").map(|v| &**v) {
        Some(Value::Str(s)) => s.to_string(),
        Some(Value::ObjectPath(p)) => p.to_string(),
        _ => return Err("CreateMonitor gave no session handle".into()),
    };
    WATCHING.store(true, Ordering::Relaxed);
    loop {
        tokio::select! {
            msg = changes.next() => {
                let Some(msg) = msg else { return Ok(()) };
                type Body = (OwnedObjectPath, HashMap<String, OwnedValue>);
                let Ok((path, state)) = msg.body().deserialize::<Body>() else { continue };
                let Some(state) = session_state(&session, path.as_str(), &state) else { continue };
                note_state(state, Instant::now());
                if state == QUERY_END {
                    let handle = OwnedObjectPath::try_from(session.clone())
                        .map_err(|e| e.to_string())?;
                    let _ = portal.call_method("QueryEndResponse", &(handle,)).await;
                }
            }
            _ = owner.next() => {
                // a restarted (or gone) portal forgot the monitor; what it
                // said stands (a portal that ends with the session goes
                // in the middle of a logout)
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_portal_is_tried_less_and_less_often() {
        let mut pause = RETRY / 2;
        let mut seen = Vec::new();
        for _ in 0..10 {
            pause = next_pause(pause, false);
            seen.push(pause.as_secs());
        }
        assert_eq!(seen[..4], [30, 60, 120, 240]);
        assert_eq!(*seen.last().unwrap(), RETRY_MAX.as_secs());
        // a monitor that was up and went (a portal restart): soon again
        assert_eq!(next_pause(RETRY_MAX, true), RETRY);
    }

    #[test]
    fn only_our_monitor_says_anything() {
        let mut state = HashMap::new();
        state.insert("session-state".to_string(), OwnedValue::from(2u32));
        state.insert("screensaver-active".to_string(), OwnedValue::from(false));
        assert_eq!(session_state("/s/ours", "/s/ours", &state), Some(2));
        assert_eq!(session_state("/s/ours", "/s/theirs", &state), None);
        state.remove("session-state");
        assert_eq!(session_state("/s/ours", "/s/ours", &state), None);
    }

    #[test]
    fn query_end_and_ending_end_the_session_until_running_or_stale() {
        let t = Instant::now();
        // (the only test touching the global)
        note_state(1, t);
        assert!(!ending_at(t));
        note_state(QUERY_END, t);
        assert!(ending_at(t + Duration::from_secs(5)));
        // ending keeps the first time
        note_state(ENDING, t + Duration::from_secs(10));
        assert!(!ending_at(t + STALE_AFTER));
        note_state(QUERY_END, t);
        note_state(1, t + Duration::from_secs(1));
        assert!(!ending_at(t + Duration::from_secs(2)), "cancelled: running");
    }
}
