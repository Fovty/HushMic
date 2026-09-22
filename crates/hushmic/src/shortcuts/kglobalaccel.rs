//! Global shortcuts through KDE's own shortcut service (`kglobalaccel`),
//! the way native KDE apps register theirs. Used on Plasma older than 6.4,
//! where the portal's BindShortcuts opens System Settings on every call
//! (5.27 and 6.0 cannot even register the keys through it), so the silent
//! re-bind on each start that the portal path relies on does not exist.
//!
//! The component is `hushmic` with the portal's action ids: that is the
//! component KDE's portal creates for our app id, so keys bound through
//! the portal on 6.1+ carry over, and a later Plasma upgrade back onto the
//! portal path finds them too.

use std::time::Duration;

use futures_util::StreamExt;
use zbus::zvariant::OwnedObjectPath;

use super::{Action, Cmd, PortalEvent};

const KGA_NAME: &str = "org.kde.kglobalaccel";
const KGA_PATH: &str = "/kglobalaccel";
const KGA_IFACE: &str = "org.kde.KGlobalAccel";
const COMPONENT_IFACE: &str = "org.kde.kglobalaccel.Component";
const COMPONENT: &str = "hushmic";
const COMPONENT_FRIENDLY: &str = "HushMic";
/// KGlobalAccel::SetPresent: the action is live (its keys are grabbed).
/// Without NoAutoloading the daemon keeps the keys the user configured and
/// only takes ours (none) for an action it has never seen.
const SET_PRESENT: u32 = 2;
/// Every call here is answered by the daemon without user interaction.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// The daemon's action id: component, action, and their display names
/// (what System Settings lists under Shortcuts).
pub fn action_id(a: Action) -> Vec<String> {
    vec![
        COMPONENT.to_string(),
        a.id().to_string(),
        COMPONENT_FRIENDLY.to_string(),
        a.description().to_string(),
    ]
}

/// A press or release from the component object. Other components' keys
/// arrive on their own objects, but the body is checked anyway: an
/// unknown component or action resolves to nothing, never a panic.
pub fn signal_action(member: &str, component: &str, action: &str) -> Option<(Action, bool)> {
    let pressed = match member {
        "globalShortcutPressed" => true,
        "globalShortcutReleased" => false,
        _ => return None,
    };
    (component == COMPONENT)
        .then(|| Action::from_id(action))
        .flatten()
        .map(|a| (a, pressed))
}

pub(super) async fn worker(
    events: std::sync::mpsc::Sender<PortalEvent>,
    mut cmds: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
) {
    loop {
        match serve(&events, &mut cmds).await {
            Ok(()) => return,
            Err(e) => eprintln!("[hushmic] global shortcuts (kglobalaccel): {e}"),
        }
        if events.send(PortalEvent::Unavailable).is_err() {
            return;
        }
        // Same retry contract as the portal worker: the main loop's
        // backoff-gated Retry (or any command) reconnects.
        match cmds.recv().await {
            None => return,
            Some(Cmd::Shutdown(ack)) => {
                let _ = ack.send(());
                return;
            }
            Some(_) => {}
        }
    }
}

async fn call<B, R>(proxy: &zbus::Proxy<'_>, method: &str, body: &B) -> Result<R, String>
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    tokio::time::timeout(CALL_TIMEOUT, proxy.call(method, body))
        .await
        .map_err(|_| format!("timed out calling {method}"))?
        .map_err(|e| format!("{method} failed: {e}"))
}

async fn serve(
    events: &std::sync::mpsc::Sender<PortalEvent>,
    cmds: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>,
) -> Result<(), String> {
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| e.to_string())?;
    let kga = zbus::Proxy::new(&conn, KGA_NAME, KGA_PATH, KGA_IFACE)
        .await
        .map_err(|e| e.to_string())?;
    // Watch the daemon's owner before registering: KWin hosts it on
    // Plasma 6, so a KWin restart takes our registration with it.
    let dbus = zbus::fdo::DBusProxy::new(&conn)
        .await
        .map_err(|e| e.to_string())?;
    let mut owner_changes = dbus
        .receive_name_owner_changed()
        .await
        .map_err(|e| e.to_string())?;
    for a in Action::ALL {
        let id = action_id(a);
        call::<_, ()>(&kga, "doRegister", &(&id,)).await?;
        call::<_, Vec<i32>>(&kga, "setShortcut", &(&id, Vec::<i32>::new(), SET_PRESENT)).await?;
    }
    let component: OwnedObjectPath = call(&kga, "getComponent", &(COMPONENT,)).await?;
    let comp = zbus::Proxy::new(&conn, KGA_NAME, component, COMPONENT_IFACE)
        .await
        .map_err(|e| e.to_string())?;
    // ONE stream for press and release, as in the portal worker: their
    // order is the push-to-talk contract.
    let mut signals = comp
        .receive_all_signals()
        .await
        .map_err(|e| e.to_string())?;
    eprintln!("[hushmic] global-shortcuts: registered with kglobalaccel");
    if events.send(PortalEvent::Available).is_err() {
        release(&kga).await;
        return Ok(());
    }
    loop {
        let sent = tokio::select! {
            m = signals.next() => {
                let msg = m.ok_or("the kglobalaccel signal stream ended")?;
                let member = msg.header().member().map(|n| n.as_str().to_owned());
                let body = msg.body().deserialize::<(String, String, i64)>().ok();
                match (member, body) {
                    (Some(member), Some((component, action, _ts))) => {
                        match signal_action(&member, &component, &action) {
                            Some((a, true)) => events.send(PortalEvent::Activated(a)),
                            Some((a, false)) => events.send(PortalEvent::Deactivated(a)),
                            None => Ok(()),
                        }
                    }
                    _ => Ok(()),
                }
            },
            c = owner_changes.next() => match c {
                None => return Err("the bus connection dropped".to_string()),
                Some(sig) => {
                    let ours = sig.args().map(|a| a.name.as_str() == KGA_NAME).unwrap_or(false);
                    if ours {
                        return Err("the shortcut service restarted".to_string());
                    }
                    Ok(())
                }
            },
            c = cmds.recv() => match c {
                None => {
                    release(&kga).await;
                    return Ok(());
                }
                Some(Cmd::Shutdown(ack)) => {
                    release(&kga).await;
                    let _ = ack.send(());
                    return Ok(());
                }
                Some(Cmd::Retry) => Ok(()),
                // Keys are assigned in System Settings, on HushMic's own
                // page; opening it is the whole setup.
                Some(cmd @ (Cmd::Bind | Cmd::Configure)) => match open_settings() {
                    Ok(()) if matches!(cmd, Cmd::Bind) => events.send(PortalEvent::BindDone),
                    Ok(()) => Ok(()),
                    Err(e) => {
                        eprintln!("[hushmic] could not open the shortcut settings: {e}");
                        events.send(PortalEvent::ConfigureUnavailable)
                    }
                },
            },
        };
        if sent.is_err() {
            release(&kga).await;
            return Ok(());
        }
    }
}

/// On quit: mark the actions inactive so their keys are free again while
/// HushMic is not running (what KGlobalAccel does for a closing KDE app).
/// Best-effort; the daemon keeps the bindings themselves.
async fn release(kga: &zbus::Proxy<'_>) {
    // all four at once and bounded as a whole: main waits 2 s for this, and
    // one slow reply must not keep the other keys grabbed
    let calls = Action::ALL.map(|a| async move {
        let _ = kga.call::<_, _, ()>("setInactive", &(action_id(a),)).await;
    });
    let _ = tokio::time::timeout(
        Duration::from_millis(1500),
        futures_util::future::join_all(calls),
    )
    .await;
}

/// System Settings on HushMic's page of the Shortcuts module, the same
/// command KDE's portal runs for its bind dialog on these versions.
fn open_settings() -> std::io::Result<()> {
    let attempts: [(&str, &[&str]); 3] = [
        ("systemsettings", &["kcm_keys", "--args", COMPONENT]),
        ("kcmshell6", &["kcm_keys"]),
        ("kcmshell5", &["kcm_keys"]),
    ];
    let mut last = std::io::Error::from(std::io::ErrorKind::NotFound);
    for (prog, args) in attempts {
        match std::process::Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                // Reap it whenever the user closes the window.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_ids_use_the_portal_component_and_ids() {
        // The KDE portal names our component after the app id and keeps our
        // shortcut ids: binds made there must land on these same actions.
        for a in Action::ALL {
            let id = action_id(a);
            assert_eq!(id.len(), 4, "kglobalaccel ignores shorter action ids");
            assert_eq!(id[0], "hushmic");
            assert_eq!(id[1], a.id());
            assert_eq!(id[2], "HushMic");
            assert_eq!(id[3], a.description());
        }
    }

    #[test]
    fn signals_map_press_and_release_and_ignore_the_rest() {
        assert_eq!(
            signal_action("globalShortcutPressed", "hushmic", "push-to-talk"),
            Some((Action::PushToTalk, true))
        );
        assert_eq!(
            signal_action("globalShortcutReleased", "hushmic", "push-to-talk"),
            Some((Action::PushToTalk, false))
        );
        assert_eq!(
            signal_action("globalShortcutPressed", "kwin", "toggle-mute"),
            None
        );
        assert_eq!(
            signal_action("globalShortcutPressed", "hushmic", "frobnicate"),
            None
        );
        assert_eq!(signal_action("cleanUp", "hushmic", "toggle-mute"), None);
    }
}
