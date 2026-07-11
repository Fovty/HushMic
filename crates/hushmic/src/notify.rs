//! Best-effort desktop notifications (`org.freedesktop.Notifications`).
//!
//! Errors that only reach stderr are invisible for .desktop/autostart
//! launches, so user-actionable failures (enable() preflight, a stuck or
//! flapping watchdog respawn loop) are surfaced as desktop notifications
//! too. The DBus call goes through zbus directly — no dependency on a
//! `notify-send` binary being installed. Everything here is best-effort: a
//! missing, broken, or hung session bus must never break or block the mic
//! path.
//!
//! All sends go through ONE queue drained by a dedicated worker thread:
//! that guarantees delivery order (a recovery notice can never be overtaken
//! by the failure it clears), makes the per-slot replace-id chain race-free
//! by construction, and isolates every caller from a stalled bus (the
//! connection also carries a method timeout, so the queue itself cannot
//! wedge forever).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Independent replace-chains: a new notification replaces the previous one
/// of the same slot (one evolving bubble instead of a pile), but a mic-test
/// progress update must never swallow a pending failure alert.
#[derive(Clone, Copy, Debug)]
pub enum Slot {
    /// Enable/watchdog failures and the matching recovery notice.
    Status = 0,
    /// "Test my mic" progress and results.
    MicTest = 1,
}

/// A hung notification server (on GNOME it is the shell itself) must not
/// stall the queue worker indefinitely; real servers reply in milliseconds.
const METHOD_TIMEOUT: Duration = Duration::from_secs(5);

static LAST_ID: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

struct Msg {
    slot: Slot,
    icon: String,
    summary: String,
    body: String,
    transient: bool,
    ack: Option<mpsc::Sender<()>>,
}

fn queue() -> &'static Mutex<mpsc::Sender<Msg>> {
    static QUEUE: OnceLock<Mutex<mpsc::Sender<Msg>>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Msg>();
        std::thread::spawn(move || {
            for m in rx {
                let _ = send_blocking(m.slot, &m.icon, &m.summary, &m.body, m.transient);
                if let Some(ack) = m.ack {
                    let _ = ack.send(());
                }
            }
        });
        Mutex::new(tx)
    })
}

fn enqueue(msg: Msg) {
    if let Ok(tx) = queue().lock() {
        let _ = tx.send(msg);
    }
}

/// Queue a persistent notification (stays in the notification center).
/// Never blocks the caller; delivery is ordered across all sends.
pub fn send(slot: Slot, icon: &str, summary: &str, body: &str) {
    enqueue(Msg {
        slot,
        icon: icon.to_string(),
        summary: summary.to_string(),
        body: body.to_string(),
        transient: false,
        ack: None,
    });
}

/// Queue a transient notification (progress bubbles that should not pile up
/// in the notification center after the moment has passed).
pub fn send_transient(slot: Slot, icon: &str, summary: &str, body: &str) {
    enqueue(Msg {
        slot,
        icon: icon.to_string(),
        summary: summary.to_string(),
        body: body.to_string(),
        transient: true,
        ack: None,
    });
}

/// Like [`send`], but waits (bounded) for the delivery attempt. For paths
/// that exit the process right after — the queue worker dies with the
/// process and the notification would be silently lost.
pub fn send_and_wait(slot: Slot, icon: &str, summary: &str, body: &str, max: Duration) {
    let (tx, rx) = mpsc::channel();
    enqueue(Msg {
        slot,
        icon: icon.to_string(),
        summary: summary.to_string(),
        body: body.to_string(),
        transient: false,
        ack: Some(tx),
    });
    let _ = rx.recv_timeout(max);
}

/// Synchronous `Notify` call; returns the server-assigned notification id.
/// Only the queue worker calls this in the app (which is what makes the
/// LAST_ID replace-chains race-free). Pub for the DBus round-trip
/// integration test.
pub fn send_blocking(
    slot: Slot,
    icon: &str,
    summary: &str,
    body: &str,
    transient: bool,
) -> zbus::Result<u32> {
    let idx = slot as usize;
    let replaces = LAST_ID[idx].load(Ordering::Relaxed);
    let mut hints: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
    // Lets the desktop associate the bubble with hushmic.desktop (app name +
    // icon in notification centers).
    hints.insert("desktop-entry", "hushmic".into());
    if transient {
        hints.insert("transient", true.into());
    }
    let conn = zbus::blocking::connection::Builder::session()?
        .method_timeout(METHOD_TIMEOUT)
        .build()?;
    let reply = conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "HushMic",
            replaces,
            icon,
            summary,
            body,
            Vec::<&str>::new(), // actions
            hints,
            -1i32, // expire: server default
        ),
    )?;
    let id: u32 = reply.body().deserialize()?;
    LAST_ID[idx].store(id, Ordering::Relaxed);
    Ok(id)
}

/// After this many consecutive watchdog attempts where enable() succeeded
/// yet the node still never appeared, the respawn loop is declared stuck
/// and surfaced (≈30 s of downtime under the backoff schedule).
const SILENT_FAILURE_THRESHOLD: u32 = 3;

/// This many successful respawns within [`RESPAWN_WINDOW`] means the child
/// keeps crashing and coming back — each cycle is a mic dropout the user
/// hears, so the pattern deserves one notification even though every single
/// respawn "succeeded".
const RESPAWN_THRESHOLD: usize = 3;
const RESPAWN_WINDOW: Duration = Duration::from_secs(180);

/// A recovery notice is only truthful once the node has stayed up for a
/// while: right after a respawn the next crash of a flapping child may be
/// seconds away.
const STABLE_AFTER: Duration = Duration::from_secs(30);

/// Decides *when* a failure deserves a notification, so the watchdog's
/// backoff-gated retries do not re-pop the same bubble forever:
/// user-initiated attempts always surface, retries only surface a *changed*
/// message, a respawn loop that keeps "succeeding" without producing the
/// node surfaces once at a threshold, rapid crash/respawn cycles surface as
/// instability, and a recovery notice is emitted exactly when a previously
/// surfaced failure clears AND the node has been stable for a while. Pure
/// state machine — the caller does the actual sending.
pub struct FailureGate {
    /// Dedup key of the last failure recorded (enable error message, or an
    /// internal marker).
    last: Option<String>,
    silent_fails: u32,
    /// Whether any failure was actually surfaced since the last reset —
    /// only then is a recovery notice warranted.
    notified: bool,
    /// Timestamps of recent successful watchdog respawns (time-decayed by
    /// [`RESPAWN_WINDOW`]); deliberately survives `on_healthy` so a flap
    /// pattern spanning healthy ticks is still visible.
    respawns: VecDeque<Instant>,
}

/// Internal dedup keys. They start with NUL so no real enable() error
/// message can collide with them.
const SILENT_KEY: &str = "\0silent-failure";
const FLAP_KEY: &str = "\0flapping";

impl FailureGate {
    pub fn new() -> Self {
        FailureGate {
            last: None,
            silent_fails: 0,
            notified: false,
            respawns: VecDeque::new(),
        }
    }

    /// enable() failed with `msg`. True => surface it now. Direct user
    /// actions (tray clicks, launch) always get feedback; watchdog retries
    /// only when the message changed since the last recorded failure.
    pub fn on_enable_error(&mut self, msg: &str, user_initiated: bool) -> bool {
        self.silent_fails = 0;
        let show = user_initiated || self.last.as_deref() != Some(msg);
        self.last = Some(msg.to_string());
        self.notified |= show;
        show
    }

    /// A watchdog attempt where enable() returned Ok but `hushmic_source`
    /// never appeared. True once, at the stuck-loop threshold.
    pub fn on_silent_failure(&mut self) -> bool {
        self.silent_fails = self.silent_fails.saturating_add(1);
        if self.silent_fails < SILENT_FAILURE_THRESHOLD {
            return false;
        }
        let show = self.last.as_deref() != Some(SILENT_KEY);
        self.last = Some(SILENT_KEY.to_string());
        self.notified |= show;
        show
    }

    /// A watchdog respawn attempt SUCCEEDED (node back up). True once when
    /// the recent-respawn rate crosses the flap threshold: individually
    /// successful respawns in quick succession mean the child keeps dying.
    pub fn on_respawn(&mut self) -> bool {
        self.on_respawn_at(Instant::now())
    }

    fn on_respawn_at(&mut self, now: Instant) -> bool {
        self.respawns.push_back(now);
        while let Some(t) = self.respawns.front() {
            if now.duration_since(*t) > RESPAWN_WINDOW {
                self.respawns.pop_front();
            } else {
                break;
            }
        }
        if self.respawns.len() < RESPAWN_THRESHOLD {
            return false;
        }
        let show = self.last.as_deref() != Some(FLAP_KEY);
        self.last = Some(FLAP_KEY.to_string());
        self.notified |= show;
        show
    }

    /// The node is confirmed up. True exactly when a failure was surfaced
    /// earlier AND the node has been stable (no respawn for a while) — the
    /// user saw an error bubble, so close the loop with a recovery notice,
    /// but never claim "running again" in the middle of a flap cycle.
    pub fn on_healthy(&mut self) -> bool {
        self.on_healthy_at(Instant::now())
    }

    fn on_healthy_at(&mut self, now: Instant) -> bool {
        let stable = self
            .respawns
            .back()
            .is_none_or(|t| now.duration_since(*t) >= STABLE_AFTER);
        if !stable {
            return false;
        }
        let recovered = self.notified;
        // The respawn history deliberately survives (time-decayed): it is
        // what distinguishes "stable again" from "between two crashes".
        self.last = None;
        self.silent_fails = 0;
        self.notified = false;
        recovered
    }

    /// Silent full reset (the user turned suppression off): stale failure
    /// state must not produce a "running again" notice later.
    pub fn reset(&mut self) {
        *self = FailureGate::new();
    }
}

impl Default for FailureGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_retries_with_same_error_surface_once() {
        let mut g = FailureGate::new();
        assert!(g.on_enable_error("model missing", false));
        assert!(!g.on_enable_error("model missing", false));
        assert!(!g.on_enable_error("model missing", false));
        // a different failure is news again
        assert!(g.on_enable_error("plugin missing", false));
        assert!(!g.on_enable_error("plugin missing", false));
    }

    #[test]
    fn user_initiated_attempts_always_get_feedback() {
        let mut g = FailureGate::new();
        assert!(g.on_enable_error("model missing", true));
        assert!(g.on_enable_error("model missing", true));
        // and they update the dedup key for subsequent watchdog retries
        assert!(!g.on_enable_error("model missing", false));
    }

    #[test]
    fn healthy_resets_dedup_so_a_relapse_surfaces_again() {
        let mut g = FailureGate::new();
        assert!(g.on_enable_error("model missing", false));
        assert!(g.on_healthy());
        assert!(g.on_enable_error("model missing", false));
    }

    #[test]
    fn silent_failures_surface_once_at_threshold() {
        let mut g = FailureGate::new();
        assert!(!g.on_silent_failure());
        assert!(!g.on_silent_failure());
        assert!(g.on_silent_failure()); // threshold = 3
        assert!(!g.on_silent_failure()); // no re-pop while stuck
        assert!(g.on_healthy());
        // relapse after recovery: counts from zero again
        assert!(!g.on_silent_failure());
        assert!(!g.on_silent_failure());
        assert!(g.on_silent_failure());
    }

    #[test]
    fn enable_error_resets_the_silent_streak() {
        let mut g = FailureGate::new();
        assert!(!g.on_silent_failure());
        assert!(!g.on_silent_failure());
        assert!(g.on_enable_error("model missing", false));
        // the streak restarts: two more silent failures are below threshold
        assert!(!g.on_silent_failure());
        assert!(!g.on_silent_failure());
        assert!(g.on_silent_failure());
    }

    #[test]
    fn recovery_notice_only_after_a_surfaced_failure() {
        let mut g = FailureGate::new();
        assert!(!g.on_healthy()); // nothing ever surfaced -> no notice
        assert!(!g.on_silent_failure());
        assert!(!g.on_healthy()); // failure was never SURFACED -> no notice
        assert!(g.on_enable_error("x", false));
        assert!(g.on_healthy());
        assert!(!g.on_healthy()); // one notice per outage, not per tick
    }

    #[test]
    fn reset_is_silent() {
        let mut g = FailureGate::new();
        assert!(g.on_enable_error("x", false));
        g.reset(); // user turned the feature off
        assert!(!g.on_healthy()); // no "running again" for a manual off
    }

    #[test]
    fn rapid_respawns_surface_as_flapping_exactly_once() {
        let mut g = FailureGate::new();
        let t0 = Instant::now();
        assert!(!g.on_respawn_at(t0));
        assert!(!g.on_respawn_at(t0 + Duration::from_secs(30)));
        assert!(g.on_respawn_at(t0 + Duration::from_secs(60))); // 3 within the window
        assert!(!g.on_respawn_at(t0 + Duration::from_secs(90))); // no re-pop while flapping
    }

    #[test]
    fn slow_respawns_are_not_flapping() {
        let mut g = FailureGate::new();
        let t0 = Instant::now();
        // one respawn every 100 s: only 2 ever fall inside the 180 s window
        assert!(!g.on_respawn_at(t0));
        assert!(!g.on_respawn_at(t0 + Duration::from_secs(100)));
        assert!(!g.on_respawn_at(t0 + Duration::from_secs(200)));
        assert!(!g.on_respawn_at(t0 + Duration::from_secs(300)));
    }

    #[test]
    fn no_recovery_notice_in_the_middle_of_a_flap_cycle() {
        let mut g = FailureGate::new();
        let t0 = Instant::now();
        assert!(!g.on_respawn_at(t0));
        assert!(!g.on_respawn_at(t0 + Duration::from_secs(30)));
        assert!(g.on_respawn_at(t0 + Duration::from_secs(60))); // flap surfaced
                                                                // healthy ticks right after a respawn: NOT stable yet -> hold
        assert!(!g.on_healthy_at(t0 + Duration::from_secs(65)));
        assert!(!g.on_healthy_at(t0 + Duration::from_secs(80)));
        // 30 s without a respawn: stable -> recovery notice fires once
        assert!(g.on_healthy_at(t0 + Duration::from_secs(90)));
        assert!(!g.on_healthy_at(t0 + Duration::from_secs(95)));
    }

    #[test]
    fn flapping_can_surface_again_after_a_stable_recovery() {
        let mut g = FailureGate::new();
        let t0 = Instant::now();
        for i in 0..3 {
            g.on_respawn_at(t0 + Duration::from_secs(i * 10));
        }
        assert!(g.on_healthy_at(t0 + Duration::from_secs(300))); // stable recovery
                                                                 // a fresh burst of respawns is news again
        let t1 = t0 + Duration::from_secs(1000);
        assert!(!g.on_respawn_at(t1));
        assert!(!g.on_respawn_at(t1 + Duration::from_secs(10)));
        assert!(g.on_respawn_at(t1 + Duration::from_secs(20)));
    }
}
