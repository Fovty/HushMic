//! The window as a portal dialog's parent: the `parent_window` every portal
//! call that opens a dialog passes, so the dialog opens over the window
//! (and stays there) instead of as a window of its own.
//!
//! Without one, KDE's file chooser opened behind the HushMic window now and
//! then (VM 107, Plasma X11): the dialog had the focus, the window stayed on
//! top, and only a taskbar entry showed that anything had opened.
//!
//! The identifier is the portals' own: `x11:<XID in hex>` on X11, and on
//! Wayland `wayland:<handle>`, the handle the compositor gives for an
//! xdg-foreign export of the window's surface. It is taken once, when the
//! window is created; until then, and where neither works, it is "" (no
//! parent, as before).

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::sync::OnceLock;

static PARENT: OnceLock<String> = OnceLock::new();

/// The window's identifier for a portal dialog's `parent_window`, or ""
/// when there is none.
pub fn get() -> String {
    PARENT.get().cloned().unwrap_or_default()
}

/// Take the identifier from the window just created (once per process).
pub fn set_from(window: &(impl HasWindowHandle + HasDisplayHandle)) {
    let (Ok(w), Ok(d)) = (window.window_handle(), window.display_handle()) else {
        return;
    };
    let parent = match (w.as_raw(), d.as_raw()) {
        (w @ (RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_)), _) => x11(w),
        (RawWindowHandle::Wayland(w), RawDisplayHandle::Wayland(d)) => {
            // SAFETY: both pointers are the live window's, from winit, and
            // the window outlives this call.
            match unsafe { wayland::export(d.display.as_ptr(), w.surface.as_ptr()) } {
                Ok(handle) => Some(format!("wayland:{handle}")),
                Err(e) => {
                    eprintln!("[hushmic] no parent for portal dialogs: {e}");
                    None
                }
            }
        }
        _ => None,
    };
    if let Some(p) = parent {
        let _ = PARENT.set(p);
    }
}

/// `x11:<XID in hex>` for an X11 window handle.
// The XID is a c_ulong: u64 here, u32 on 32-bit targets.
#[allow(clippy::useless_conversion)]
fn x11(w: RawWindowHandle) -> Option<String> {
    let xid = match w {
        RawWindowHandle::Xlib(h) => u64::from(h.window),
        RawWindowHandle::Xcb(h) => u64::from(h.window.get()),
        _ => return None,
    };
    (xid != 0).then(|| format!("x11:{xid:x}"))
}

mod wayland {
    use std::ffi::c_void;
    use wayland_client::backend::{Backend, ObjectId};
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols::xdg::foreign::zv2::client::{
        zxdg_exported_v2::{self, ZxdgExportedV2},
        zxdg_exporter_v2::ZxdgExporterV2,
    };

    #[derive(Default)]
    struct State {
        handle: Option<String>,
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZxdgExporterV2, ()> for State {
        fn event(
            _: &mut Self,
            _: &ZxdgExporterV2,
            _: <ZxdgExporterV2 as Proxy>::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZxdgExportedV2, ()> for State {
        fn event(
            state: &mut Self,
            _: &ZxdgExportedV2,
            event: zxdg_exported_v2::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let zxdg_exported_v2::Event::Handle { handle } = event {
                state.handle = Some(handle);
            }
        }
    }

    /// Export the toplevel `surface` on `display` (winit's), on a queue of
    /// our own, and wait for its handle. The export lives as long as the
    /// process: the handle stays good for every dialog the window opens.
    ///
    /// # Safety
    /// `display` and `surface` are a live `wl_display` and the window's
    /// live `wl_surface`.
    pub unsafe fn export(display: *mut c_void, surface: *mut c_void) -> Result<String, String> {
        let backend = Backend::from_foreign_display(display.cast());
        let conn = Connection::from_backend(backend);
        let id = ObjectId::from_ptr(WlSurface::interface(), surface.cast())
            .map_err(|e| format!("the window's surface: {e}"))?;
        let surface = WlSurface::from_id(&conn, id).map_err(|e| e.to_string())?;
        let (globals, mut queue) =
            registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let exporter: ZxdgExporterV2 = globals
            .bind(&queue.handle(), 1..=1, ())
            .map_err(|e| format!("the compositor has no xdg-foreign v2 ({e})"))?;
        let exported = exporter.export_toplevel(&surface, &queue.handle(), ());
        let mut state = State::default();
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        let handle = state.handle.ok_or("the compositor sent no handle")?;
        // Unexported only when destroyed: keep it (and its queue) for good.
        std::mem::forget((exported, exporter, queue, conn));
        Ok(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raw_window_handle::{XcbWindowHandle, XlibWindowHandle};

    #[test]
    fn an_x11_window_is_its_xid_in_hex() {
        assert_eq!(
            x11(RawWindowHandle::Xlib(XlibWindowHandle::new(0x400003))),
            Some("x11:400003".to_string())
        );
        assert_eq!(
            x11(RawWindowHandle::Xcb(XcbWindowHandle::new(
                std::num::NonZeroU32::new(0x3e00003).unwrap()
            ))),
            Some("x11:3e00003".to_string())
        );
        assert_eq!(x11(RawWindowHandle::Xlib(XlibWindowHandle::new(0))), None);
    }

    #[test]
    fn without_a_window_there_is_no_parent() {
        assert_eq!(get(), "");
    }
}
