//! The desktop's look: whether it prefers a light or dark style, and
//! whether it asks for less motion. Read by the daemon from the settings
//! portal (`org.freedesktop.portal.Settings`) and followed through its
//! `SettingChanged` signal, so the window learns both from the snapshot and
//! never opens a bus of its own.
//!
//! Sources, first answer wins:
//! - style: `org.freedesktop.appearance color-scheme` (1 dark, 2 light,
//!   0 no preference);
//! - reduced motion: `org.freedesktop.appearance reduced-motion` where the
//!   portal has it (1 reduced), else the desktop's own setting: on KDE
//!   `AnimationDurationFactor` (0 = no animations) through the portal's
//!   `org.kde.kdeglobals.KDE` namespace or else from `kdeglobals` itself,
//!   then GNOME's `org.gnome.desktop.interface enable-animations`;
//!   elsewhere GNOME's key first (a GTK portal backend on KDE answers
//!   GNOME's key with its default, so KDE's own comes first there).
//!
//! The portal is followed through `SettingChanged`, and asked again once
//! changes settle (its answer wins over what a signal said); the
//! `kdeglobals` file is read again whenever a window opens
//! ([`Reader::refresh`]). Nothing
//! answering is no error: the style is unknown (the window goes dark) and
//! motion stays on.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use futures_util::StreamExt;
use zbus::zvariant::{OwnedValue, Value};

const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";

const APPEARANCE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME: &str = "color-scheme";
const REDUCED_MOTION: &str = "reduced-motion";
const GNOME_INTERFACE: &str = "org.gnome.desktop.interface";
const ENABLE_ANIMATIONS: &str = "enable-animations";
const KDE_GLOBALS: &str = "org.kde.kdeglobals.KDE";
const ANIMATION_FACTOR: &str = "AnimationDurationFactor";

/// Every portal call is bounded: a portal that accepts the call but never
/// answers must not hold the reader.
const CALL_TIMEOUT: Duration = Duration::from_secs(3);
/// After the session bus itself failed, the next try waits this long.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Once changes stop coming for this long, the portal is asked again and
/// what it answers stands: KDE's portal (Plasma 5.27) can announce a colour
/// scheme switch with a value read from a `kdeglobals` that was not fully
/// written yet ("no preference" in the signal, light from `ReadOne` a
/// moment later), and sends nothing after it.
const SETTLE: Duration = Duration::from_secs(1);

/// The style the desktop prefers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    NoPreference,
    Dark,
    Light,
}

/// What the desktop said; None = it did not say (or there is no portal).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Look {
    pub scheme: Option<Scheme>,
    pub reduced_motion: Option<bool>,
}

/// The reduced-motion answers by source, so a change to one recomputes the
/// verdict in the same order as the first read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Motion {
    /// Running on KDE: its own setting before GNOME's.
    kde: bool,
    freedesktop: Option<bool>,
    gnome: Option<bool>,
    kde_portal: Option<bool>,
    kde_file: Option<bool>,
}

impl Motion {
    fn reduced(&self) -> Option<bool> {
        let kde = self.kde_portal.or(self.kde_file);
        if self.kde {
            self.freedesktop.or(kde).or(self.gnome)
        } else {
            self.freedesktop.or(self.gnome).or(kde)
        }
    }
}

/// The value inside however many variant layers the portal wrapped it in
/// (the deprecated `Read` answers a variant in a variant).
fn inner<'a>(mut v: &'a Value<'a>) -> &'a Value<'a> {
    while let Value::Value(b) = v {
        v = b;
    }
    v
}

fn as_u32(v: &Value) -> Option<u32> {
    match inner(v) {
        Value::U32(n) => Some(*n),
        Value::I32(n) => u32::try_from(*n).ok(),
        Value::U8(n) => Some(u32::from(*n)),
        Value::U16(n) => Some(u32::from(*n)),
        Value::I16(n) => u32::try_from(*n).ok(),
        Value::U64(n) => u32::try_from(*n).ok(),
        Value::I64(n) => u32::try_from(*n).ok(),
        _ => None,
    }
}

/// `color-scheme`: 0 no preference, 1 dark, 2 light (other numbers are not
/// defined; read as no preference).
pub fn scheme_of(v: &Value) -> Option<Scheme> {
    Some(match as_u32(v)? {
        1 => Scheme::Dark,
        2 => Scheme::Light,
        _ => Scheme::NoPreference,
    })
}

/// `reduced-motion`: 0 no preference, 1 reduced.
pub fn freedesktop_reduced(v: &Value) -> Option<bool> {
    as_u32(v).map(|n| n == 1)
}

/// GNOME's `enable-animations` (a boolean): off = reduced.
pub fn gnome_reduced(v: &Value) -> Option<bool> {
    match inner(v) {
        Value::Bool(b) => Some(!*b),
        _ => None,
    }
}

/// KDE's `AnimationDurationFactor`: 0 = no animations. The portal relays
/// the config entry, a string ("0", "0.5"), or a number.
pub fn kde_reduced(v: &Value) -> Option<bool> {
    let factor = match inner(v) {
        Value::F64(f) => *f,
        Value::Str(s) => s.trim().parse().ok()?,
        other => f64::from(as_u32(other)?),
    };
    Some(factor <= 0.0)
}

/// `AnimationDurationFactor` from a `kdeglobals` file's `[KDE]` group.
pub fn kdeglobals_reduced(text: &str) -> Option<bool> {
    let mut in_kde = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_kde = line == "[KDE]";
            continue;
        }
        if !in_kde {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == ANIMATION_FACTOR {
                let factor: f64 = v.trim().parse().ok()?;
                return Some(factor <= 0.0);
            }
        }
    }
    None
}

/// What the reader shares with its handle.
#[derive(Default)]
struct Shared {
    /// "Read again now" (a window opens).
    refresh: Notify,
    /// The desktop's name (`XDG_CURRENT_DESKTOP`) as a launch passed it on:
    /// a daemon the systemd unit started before the session has none of
    /// its own.
    desktop: Mutex<Option<String>>,
}

impl Shared {
    fn is_kde(&self) -> bool {
        let passed = self
            .desktop
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        passed
            .or_else(|| std::env::var("XDG_CURRENT_DESKTOP").ok())
            .is_some_and(|d| names_kde(&d))
    }
}

fn names_kde(desktop: &str) -> bool {
    desktop.split(':').any(|e| e.eq_ignore_ascii_case("KDE"))
}

/// The user's `kdeglobals` (KDE only: another desktop's leftover file says
/// nothing about this one).
fn kdeglobals_file(kde: bool) -> Option<bool> {
    if !kde {
        return None;
    }
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    kdeglobals_reduced(&std::fs::read_to_string(dir.join("kdeglobals")).ok()?)
}

/// The running reader: its first answer, and a way to have it read again.
#[derive(Clone, Default)]
pub struct Reader {
    first: Arc<(Mutex<Option<Look>>, Condvar)>,
    shared: Arc<Shared>,
}

impl Reader {
    /// The first answer, waiting at most `budget` for it (a window should
    /// paint its first frame in the desktop's style).
    pub fn first(&self, budget: Duration) -> Option<Look> {
        let (slot, ready) = &*self.first;
        let guard = slot.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = ready
            .wait_timeout_while(guard, budget, |l| l.is_none())
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }

    /// Read everything again now (a window opens: the `kdeglobals` file is
    /// not followed otherwise), or retry a session bus that failed.
    pub fn refresh(&self) {
        self.shared.refresh.notify_one();
    }

    /// The desktop a launch named (its `XDG_CURRENT_DESKTOP`), for a daemon
    /// whose own environment has none; read again when it is new.
    pub fn desktop(&self, name: &str) {
        let mut slot = self
            .shared
            .desktop
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if slot.as_deref() != Some(name) {
            *slot = Some(name.to_string());
            drop(slot);
            self.refresh();
        }
    }

    fn set_first(&self, look: Look) {
        let (slot, ready) = &*self.first;
        let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = Some(look);
            ready.notify_all();
        }
    }
}

/// Watch the desktop's look on a thread of its own; `report` gets the first
/// answer (also when nothing answered) and every change after it. `desktop`
/// is the desktop's name when the caller knows it better than this
/// process's environment.
pub fn spawn(report: impl Fn(Look) + Send + 'static, desktop: Option<&str>) -> Reader {
    let reader = Reader::default();
    *reader
        .shared
        .desktop
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = desktop.map(str::to_string);
    let seen = reader.clone();
    let shared = reader.shared.clone();
    let report = move |look: Look| {
        seen.set_first(look);
        report(look);
    };
    let spawned = std::thread::Builder::new()
        .name("hushmic-desktop".into())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(watch(&report, &shared)),
                Err(e) => {
                    eprintln!("[hushmic] desktop style: no runtime ({e})");
                    report(Look::default());
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("[hushmic] desktop style: no thread ({e})");
        reader.set_first(Look::default());
    }
    reader
}

async fn watch(report: &(impl Fn(Look) + Send), shared: &Shared) {
    let mut last: Option<Look> = None;
    let mut tell = |look: Look| {
        if last != Some(look) {
            last = Some(look);
            report(look);
        }
    };
    // A session bus that stays unusable is said once, not every retry.
    let mut said: Option<String> = None;
    loop {
        let e = serve(&mut tell, shared).await;
        if said.as_ref() != Some(&e) {
            eprintln!("[hushmic] desktop style: {e}");
            said = Some(e);
        }
        tell(Look {
            scheme: None,
            reduced_motion: {
                let kde = shared.is_kde();
                Motion {
                    kde,
                    kde_file: kdeglobals_file(kde),
                    ..Motion::default()
                }
                .reduced()
            },
        });
        tokio::select! {
            _ = tokio::time::sleep(RETRY_AFTER) => {}
            _ = shared.refresh.notified() => {}
        }
    }
}

async fn read(settings: &zbus::Proxy<'_>, version: u32, ns: &str, key: &str) -> Option<OwnedValue> {
    // ReadOne (version 2) answers the value itself; Read, the only one
    // before it, wraps it in one more variant (`inner` unwraps both).
    let method = if version >= 2 { "ReadOne" } else { "Read" };
    match tokio::time::timeout(CALL_TIMEOUT, settings.call(method, &(ns, key))).await {
        Ok(Ok(v)) => Some(v),
        _ => None,
    }
}

/// Everything the portal says now (nothing, when there is no portal).
async fn read_all(settings: &zbus::Proxy<'_>, shared: &Shared) -> (Option<Scheme>, Motion) {
    let version: u32 = tokio::time::timeout(CALL_TIMEOUT, settings.get_property("version"))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(1);
    let scheme = read(settings, version, APPEARANCE, COLOR_SCHEME)
        .await
        .and_then(|v| scheme_of(&v));
    let kde = shared.is_kde();
    let mut motion = Motion {
        kde,
        freedesktop: read(settings, version, APPEARANCE, REDUCED_MOTION)
            .await
            .and_then(|v| freedesktop_reduced(&v)),
        gnome: read(settings, version, GNOME_INTERFACE, ENABLE_ANIMATIONS)
            .await
            .and_then(|v| gnome_reduced(&v)),
        kde_portal: read(settings, version, KDE_GLOBALS, ANIMATION_FACTOR)
            .await
            .and_then(|v| kde_reduced(&v)),
        kde_file: None,
    };
    if motion.kde_portal.is_none() {
        motion.kde_file = kdeglobals_file(kde);
    }
    (scheme, motion)
}

/// Read the portal and follow it for as long as the session bus lasts: a
/// portal that starts, stops or restarts is read again. The signal stream
/// stays subscribed throughout (it follows the name's owner), so a change
/// right after a restart is not lost. Returns why the bus is unusable.
async fn serve(tell: &mut impl FnMut(Look), shared: &Shared) -> String {
    let conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => return format!("no session bus ({e})"),
    };
    let dbus = match zbus::fdo::DBusProxy::new(&conn).await {
        Ok(d) => d,
        Err(e) => return e.to_string(),
    };
    // Subscribed before reading, so a portal that starts or restarts in
    // between is not missed.
    let mut owners = match dbus.receive_name_owner_changed().await {
        Ok(o) => o,
        Err(e) => return e.to_string(),
    };
    let settings = match zbus::proxy::Builder::<zbus::Proxy>::new(&conn)
        .destination(PORTAL_NAME)
        .and_then(|b| b.path(PORTAL_PATH))
        .and_then(|b| b.interface(SETTINGS))
        .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No))
    {
        Ok(b) => match b.build().await {
            Ok(p) => p,
            Err(e) => return e.to_string(),
        },
        Err(e) => return e.to_string(),
    };
    let mut changes = match settings.receive_signal("SettingChanged").await {
        Ok(c) => c,
        Err(e) => return e.to_string(),
    };

    let (scheme, mut motion) = read_all(&settings, shared).await;
    let mut look = Look {
        scheme,
        reduced_motion: motion.reduced(),
    };
    tell(look);

    // When to ask the portal again after a change (see SETTLE).
    let mut settle: Option<tokio::time::Instant> = None;
    loop {
        tokio::select! {
            m = changes.next() => {
                let Some(msg) = m else {
                    return "the bus connection dropped".into();
                };
                let Ok((ns, key, value)) = msg.body().deserialize::<(String, String, OwnedValue)>()
                else {
                    continue;
                };
                // Any change: a scheme switch also rewrites other groups.
                settle = Some(tokio::time::Instant::now() + SETTLE);
                match (ns.as_str(), key.as_str()) {
                    (APPEARANCE, COLOR_SCHEME) => look.scheme = scheme_of(&value),
                    (APPEARANCE, REDUCED_MOTION) => motion.freedesktop = freedesktop_reduced(&value),
                    (GNOME_INTERFACE, ENABLE_ANIMATIONS) => motion.gnome = gnome_reduced(&value),
                    (KDE_GLOBALS, ANIMATION_FACTOR) => motion.kde_portal = kde_reduced(&value),
                    _ => continue,
                }
                look.reduced_motion = motion.reduced();
                tell(look);
            }
            o = owners.next() => {
                let Some(sig) = o else {
                    return "the bus connection dropped".into();
                };
                if sig.args().is_ok_and(|a| a.name.as_str() == PORTAL_NAME) {
                    // started, stopped or restarted: what it says now
                    (look.scheme, motion) = read_all(&settings, shared).await;
                    look.reduced_motion = motion.reduced();
                    tell(look);
                }
            }
            _ = tokio::time::sleep_until(settle.unwrap_or_else(tokio::time::Instant::now)),
                if settle.is_some() =>
            {
                settle = None;
                (look.scheme, motion) = read_all(&settings, shared).await;
                look.reduced_motion = motion.reduced();
                tell(look);
            }
            _ = shared.refresh.notified() => {
                (look.scheme, motion) = read_all(&settings, shared).await;
                look.reduced_motion = motion.reduced();
                tell(look);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_schemes_read_as_the_spec_says() {
        assert_eq!(scheme_of(&Value::U32(0)), Some(Scheme::NoPreference));
        assert_eq!(scheme_of(&Value::U32(1)), Some(Scheme::Dark));
        assert_eq!(scheme_of(&Value::U32(2)), Some(Scheme::Light));
        assert_eq!(scheme_of(&Value::U32(7)), Some(Scheme::NoPreference));
        // the deprecated Read: a variant in a variant
        let nested = Value::Value(Box::new(Value::Value(Box::new(Value::U32(2)))));
        assert_eq!(scheme_of(&nested), Some(Scheme::Light));
        assert_eq!(scheme_of(&Value::from("dark")), None);
    }

    #[test]
    fn motion_answers_from_each_desktop() {
        assert_eq!(freedesktop_reduced(&Value::U32(1)), Some(true));
        assert_eq!(freedesktop_reduced(&Value::U32(0)), Some(false));
        assert_eq!(gnome_reduced(&Value::Bool(false)), Some(true));
        assert_eq!(gnome_reduced(&Value::Bool(true)), Some(false));
        assert_eq!(kde_reduced(&Value::from("0")), Some(true));
        assert_eq!(kde_reduced(&Value::from("0.5")), Some(false));
        assert_eq!(kde_reduced(&Value::F64(0.0)), Some(true));
        assert_eq!(kde_reduced(&Value::I32(1)), Some(false));
        assert_eq!(kde_reduced(&Value::from("fast")), None);
    }

    #[test]
    fn the_first_source_that_answers_wins() {
        let mut m = Motion {
            kde: false,
            freedesktop: None,
            gnome: Some(false),
            kde_portal: Some(true),
            kde_file: Some(true),
        };
        assert_eq!(m.reduced(), Some(false), "GNOME's key first elsewhere");
        // On KDE its own setting comes before GNOME's (a GTK backend
        // answers GNOME's key with its default there).
        m.kde = true;
        assert_eq!(m.reduced(), Some(true));
        m.freedesktop = Some(false);
        assert_eq!(m.reduced(), Some(false), "the portal's own key first");
        assert_eq!(Motion::default().reduced(), None);
        let m = Motion {
            kde: true,
            kde_file: Some(true),
            gnome: Some(false),
            ..Motion::default()
        };
        assert_eq!(
            m.reduced(),
            Some(true),
            "the file stands in for the portal's KDE key"
        );
    }

    #[test]
    fn a_passed_desktop_name_counts_before_the_environment() {
        assert!(names_kde("KDE"));
        assert!(names_kde("ubuntu:KDE"));
        assert!(!names_kde("GNOME"));
        let shared = Shared::default();
        *shared.desktop.lock().unwrap() = Some("KDE".into());
        assert!(shared.is_kde());
        *shared.desktop.lock().unwrap() = Some("GNOME".into());
        assert!(!shared.is_kde());
    }

    #[test]
    fn kdeglobals_is_read_from_its_kde_group() {
        let text = "[General]\nAnimationDurationFactor=1\n\n[KDE]\nLookAndFeelPackage=x\nAnimationDurationFactor=0\n";
        assert_eq!(kdeglobals_reduced(text), Some(true));
        assert_eq!(
            kdeglobals_reduced("[KDE]\nAnimationDurationFactor = 0.25\n"),
            Some(false)
        );
        assert_eq!(kdeglobals_reduced("[KDE]\nSingleClick=false\n"), None);
        assert_eq!(
            kdeglobals_reduced("[General]\nAnimationDurationFactor=0\n"),
            None
        );
    }
}
