//! The window's X11 class, `hushmic` both ways (UX panel round 2, A24).
//!
//! egui hands its app id to winit as the class with an empty instance, so
//! `xprop` reads `WM_CLASS = "", "hushmic"` and window rules that go by
//! the instance (i3's `instance=`, `xdotool search --classname`, KWin's
//! window rules) find nothing. Called while the window is still unmapped
//! (eframe shows it after its first frame), when ICCCM wants WM_CLASS
//! set. Wayland has no instance: the app id stays as egui sets it.

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::ffi::CStr;

const NAME: &CStr = c"hushmic";

pub fn set(window: &(impl HasWindowHandle + HasDisplayHandle)) {
    let (Ok(w), Ok(d)) = (window.window_handle(), window.display_handle()) else {
        return;
    };
    let (RawWindowHandle::Xlib(w), RawDisplayHandle::Xlib(d)) = (w.as_raw(), d.as_raw()) else {
        return;
    };
    let Some(display) = d.display else {
        return;
    };
    // winit has loaded libX11 already: this finds the same library
    let Ok(xlib) = x11_dl::xlib::Xlib::open() else {
        return;
    };
    let mut hint = x11_dl::xlib::XClassHint {
        res_name: NAME.as_ptr().cast_mut(),
        res_class: NAME.as_ptr().cast_mut(),
    };
    // SAFETY: the display and the window are winit's, alive for this call
    // on the event loop's thread; XSetClassHint copies the strings into
    // the property and keeps no pointer.
    unsafe {
        (xlib.XSetClassHint)(display.as_ptr().cast(), w.window, &mut hint);
        (xlib.XFlush)(display.as_ptr().cast());
    }
}
