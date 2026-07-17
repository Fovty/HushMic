//! Minimal XDG Desktop Portal client: the Background portal's autostart
//! request — the only host-visible autostart channel from inside a Flatpak
//! (a `.desktop` written to the sandbox's own `~/.config/autostart` is
//! invisible to the host session manager, and Flathub forbids punching that
//! directory open precisely because this portal exists).
//!
//! Same direct-zbus approach as notify.rs — no `ashpd` dependency for one
//! method call. And the same worker-queue shape: portal round-trips must
//! never block the tray's event loop, requests execute strictly in toggle
//! order (last toggle wins by construction), and a hung portal stalls only
//! the worker — every phase (connect, method call, signal wait) is bounded
//! by a timeout.
//!
//! Protocol (`org.freedesktop.portal.Background.RequestBackground`):
//! the method returns an `org.freedesktop.portal.Request` object path and
//! the actual verdict arrives as that object's `Response` signal. With a
//! `handle_token` the path is predictable BEFORE the call, so the signal
//! subscription can be set up first — subscribing after would race the
//! reply. `autostart=true` writes `~/.config/autostart/<app-id>.desktop` on
//! the HOST (`Exec=flatpak run --command=… <app-id> …`); `autostart=false`
//! deletes it — exactly the toggle semantics autostart.rs needs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::StreamExt;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// Both desktop backends answer autostart requests without prompting (GNOME
/// unconditionally, KDE silently for the autostart half), so a response
/// normally arrives in milliseconds; the bound only guards a wedged portal.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Queue an autostart request to the portal worker; never blocks. The
/// worker logs failures and surfaces a definitive denial as a notification
/// (the tray checkbox already reflects the user's intent from config — a
/// denial is the one case where intent and reality diverge).
///
/// A request queued moments before the process exits can die with the
/// detached worker — accepted: the config (the intent) is already saved,
/// and the launch-time reconcile() re-requests the persisted state, so any
/// divergence heals on the next start.
pub fn request_autostart(enabled: bool) {
    if let Ok(tx) = queue().lock() {
        let _ = tx.send(enabled);
    }
}

fn queue() -> &'static Mutex<mpsc::Sender<bool>> {
    static QUEUE: OnceLock<Mutex<mpsc::Sender<bool>>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<bool>();
        std::thread::spawn(move || {
            for enabled in rx {
                match request_background_blocking(enabled) {
                    Ok(granted) => {
                        if enabled && !granted {
                            eprintln!(
                                "[hushmic] the desktop denied the autostart request \
                                 (revocable in the system's application permissions)"
                            );
                            crate::notify::send(
                                crate::notify::Slot::Status,
                                "dialog-warning",
                                "Autostart was not allowed",
                                "The desktop denied HushMic's autostart request. You can \
                                 allow it in your system settings under application \
                                 permissions / background apps.",
                            );
                        }
                    }
                    Err(e) => eprintln!("[hushmic] autostart portal request failed: {e}"),
                }
            }
        });
        Mutex::new(tx)
    })
}

/// One synchronous RequestBackground round-trip. Returns whether autostart
/// is granted. Runs on the worker thread only.
fn request_background_blocking(autostart: bool) -> Result<bool, String> {
    // Current-thread tokio runtime: zbus is pinned tokio-mode workspace-wide
    // (see Cargo.toml), and only async zbus can bound a SIGNAL wait — the
    // blocking SignalIterator has no timeout, and a portal that never
    // answers would wedge the worker (and with it every later toggle).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    // The OUTER timeout bounds everything before the signal wait too — the
    // session-bus handshake, proxy setup, and the RequestBackground method
    // call itself all await unboundedly otherwise (zbus applies no default
    // method timeout), and a portal that accepts the connection but never
    // replies would wedge the worker with every later toggle silently
    // queueing behind it. Slightly larger than RESPONSE_TIMEOUT so the
    // signal wait's more specific error message wins in the common case.
    rt.block_on(async {
        tokio::time::timeout(RESPONSE_TIMEOUT + Duration::from_secs(5), async {
            request_background_inner(autostart).await
        })
        .await
        .map_err(|_| "timed out talking to the portal".to_string())?
    })
}

async fn request_background_inner(autostart: bool) -> Result<bool, String> {
    {
        let conn = zbus::Connection::session()
            .await
            .map_err(|e| e.to_string())?;

        // Predictable request path per the portal spec: the sender's unique
        // name with ':' stripped and '.' -> '_', plus our handle_token.
        let sender = conn
            .unique_name()
            .ok_or("session bus connection has no unique name")?
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let token = format!(
            "hushmic_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");

        // Subscribe BEFORE calling: the response fires as soon as the
        // backend handles the request, potentially before the method reply
        // is processed on our side.
        let request_proxy = zbus::Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            request_path.as_str(),
            "org.freedesktop.portal.Request",
        )
        .await
        .map_err(|e| e.to_string())?;
        let mut responses = request_proxy
            .receive_signal("Response")
            .await
            .map_err(|e| e.to_string())?;

        let portal = zbus::Proxy::new(
            &conn,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Background",
        )
        .await
        .map_err(|e| e.to_string())?;

        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("handle_token", token.as_str().into());
        options.insert(
            "reason",
            "Start the HushMic virtual microphone when you log in".into(),
        );
        options.insert("autostart", autostart.into());
        options.insert(
            "commandline",
            vec!["hushmic".to_string(), "--tray".to_string()].into(),
        );
        options.insert("dbus-activatable", false.into());

        let returned: OwnedObjectPath = portal
            .call("RequestBackground", &("", options))
            .await
            .map_err(|e| format!("RequestBackground failed: {e}"))?;

        // Portals since 2017 honor handle_token, so the returned handle is
        // the path we subscribed on. If an exotic backend returns a
        // different one, re-subscribe there — the response may already have
        // fired in that gap, which the timeout below then reports.
        if returned.as_str() != request_path {
            let request_proxy = zbus::Proxy::new(
                &conn,
                "org.freedesktop.portal.Desktop",
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

        let msg = tokio::time::timeout(RESPONSE_TIMEOUT, responses.next())
            .await
            .map_err(|_| "timed out waiting for the portal response")?
            .ok_or("the portal response stream ended without a response")?;
        let (code, results): (u32, HashMap<String, OwnedValue>) =
            msg.body().deserialize().map_err(|e| e.to_string())?;
        // 0 = success, 1 = user cancelled, 2 = other error.
        if code != 0 {
            return Err(format!("the portal request was not granted (code {code})"));
        }
        let granted = results
            .get("autostart")
            .and_then(|v| bool::try_from(v).ok())
            // A success response without the key means the backend applied
            // the request as-is (older portals) — trust the requested state.
            .unwrap_or(autostart);
        Ok(granted)
    }
}
