//! "Is a fullscreen app or game in front?"
//!
//! Answered for X11 windows (XWayland apps and games, everything in an X11
//! session) from the EWMH state of the active window. Wayland-native
//! windows are invisible to an X11 client; Mutter stacks a focused
//! fullscreen window above Nook's always-on-top window anyway, so the notch
//! does not show over them.

use crate::monitor;
use crate::window::Hwnd;

pub fn foreground_is_fullscreen(own: Hwnd) -> bool {
    let Ok(x) = crate::sys::x11::x() else {
        return false;
    };
    let Some(&active) = x.prop32(x.root, x.atoms._NET_ACTIVE_WINDOW).first() else {
        return false;
    };
    if active == 0 || active == own.xid() || x.pid_of(active) == Some(std::process::id()) {
        return false;
    }
    let state = x.prop32(active, x.atoms._NET_WM_STATE);
    if !state.contains(&x.atoms._NET_WM_STATE_FULLSCREEN) {
        return false;
    }
    let types = x.prop32(active, x.atoms._NET_WM_WINDOW_TYPE);
    if types.contains(&x.atoms._NET_WM_WINDOW_TYPE_DESKTOP) {
        return false;
    }
    let (Ok(theirs), Ok(ours)) = (
        monitor::of_window(Hwnd::from_raw(active as isize)),
        monitor::of_window(own),
    ) else {
        return false;
    };
    theirs.bounds == ours.bounds
}
