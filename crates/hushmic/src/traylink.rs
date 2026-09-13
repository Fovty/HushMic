//! The optional tray handle. `--headless` / `tray = false` runs with
//! `Disabled`; a missing StatusNotifierWatcher leaves `Pending` and the
//! main loop retries on ticks. Every `update` site in main.rs keeps its
//! closure shape: without a tray the closure is simply never run.

use crate::tray::HushMicTray;
use ksni::blocking::TrayMethods;

pub enum TrayLink {
    Sni(ksni::blocking::Handle<HushMicTray>),
    /// Wanted, not registered yet (no watcher on the bus). Retried.
    Pending,
    /// Not wanted this run.
    Disabled,
}

impl TrayLink {
    pub fn update<R, F: FnOnce(&mut HushMicTray) -> R>(&self, f: F) -> Option<R> {
        match self {
            TrayLink::Sni(h) => h.update(f),
            TrayLink::Pending | TrayLink::Disabled => None,
        }
    }
    pub fn is_sni(&self) -> bool {
        matches!(self, TrayLink::Sni(_))
    }
    pub fn wants_retry(&self) -> bool {
        matches!(self, TrayLink::Pending)
    }
}

/// One registration attempt. Inside a Flatpak the session-bus proxy only
/// lets us own names under our app ID, so the spec's well-known
/// `org.kde.StatusNotifierItem-{pid}-{id}` name is denied and a plain
/// spawn() fails outright; ksni's sanctioned fallback registers by unique
/// connection name only (same solution as Chromium's).
pub fn try_spawn(tray: HushMicTray) -> Result<TrayLink, ksni::Error> {
    let spawned = if crate::sandbox::is_flatpak() {
        tray.disable_dbus_name(true).spawn()
    } else {
        tray.spawn()
    };
    spawned.map(TrayLink::Sni)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tray_means_update_is_a_no_op() {
        let mut ran = false;
        assert_eq!(TrayLink::Disabled.update(|_| ran = true), None);
        assert_eq!(TrayLink::Pending.update(|_| ran = true), None);
        assert!(!ran);
        assert!(TrayLink::Pending.wants_retry());
        assert!(!TrayLink::Disabled.wants_retry());
        assert!(!TrayLink::Disabled.is_sni());
    }
}
