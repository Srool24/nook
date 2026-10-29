//! The X11 connection.
//!
//! Nook runs as an X11 client on Linux, through XWayland on a GNOME Wayland
//! session: a Wayland client cannot place its own window, keep it above
//! others or let clicks through outside a shape, and an X11 client can do all
//! three (`_NET_WM_STATE_ABOVE` and the SHAPE input region are honoured by
//! Mutter for XWayland windows).
//!
//! One shared connection serves the requests made from the UI thread. It
//! never selects events, so nothing piles up in its queue; threads that need
//! events (the snip overlay, the ink canvas, the clipboard watcher, drags)
//! open a connection of their own with [`connect`].

use crate::{PlatformError, Result};
use std::sync::OnceLock;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, PropMode, Window,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

x11rb::atom_manager! {
    pub Atoms: AtomsCookie {
        UTF8_STRING,
        WM_STATE,
        WM_PROTOCOLS,
        WM_DELETE_WINDOW,
        WM_TAKE_FOCUS,
        _NET_WM_NAME,
        _NET_WM_PID,
        _NET_WM_STATE,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_BELOW,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_SKIP_PAGER,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_WINDOW_OPACITY,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_NORMAL,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        _NET_WM_WINDOW_TYPE_UTILITY,
        _NET_WM_BYPASS_COMPOSITOR,
        _NET_WM_FULLSCREEN_MONITORS,
        _NET_WM_USER_TIME,
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST,
        _NET_CLIENT_LIST_STACKING,
        _NET_WORKAREA,
        _NET_FRAME_EXTENTS,
        _GTK_FRAME_EXTENTS,
        _GTK_WORKAREAS_D0,
        _MOTIF_WM_HINTS,
        RESOURCE_MANAGER,
        CLIPBOARD,
        TARGETS,
        NOOK_SELECTION,
        PASSWORD_HINT: b"x-kde-passwordManagerHint",
        XdndAware,
        XdndEnter,
        XdndPosition,
        XdndStatus,
        XdndLeave,
        XdndDrop,
        XdndFinished,
        XdndSelection,
        XdndTypeList,
        XdndActionCopy,
        XdndActionMove,
        TEXT_URI_LIST: b"text/uri-list",
        GNOME_COPIED_FILES: b"x-special/gnome-copied-files",
    }
}

/// The shared connection and what every caller needs from it.
pub struct X {
    pub conn: RustConnection,
    pub root: Window,
    pub screen: usize,
    pub atoms: Atoms,
}

static SHARED: OnceLock<std::result::Result<X, String>> = OnceLock::new();

/// The shared connection, opened on first use.
pub fn x() -> Result<&'static X> {
    SHARED
        .get_or_init(|| open().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| PlatformError::Other(e.clone()))
}

/// Whether an X display is reachable (XWayland or an X11 session).
pub fn available() -> bool {
    x().is_ok()
}

/// A new connection for a thread that reads events.
pub fn open() -> Result<X> {
    let (conn, screen) = RustConnection::connect(None).ctx("connecting to the X server")?;
    let root = conn.setup().roots[screen].root;
    let atoms = Atoms::new(&conn)
        .ctx("InternAtom")?
        .reply()
        .ctx("InternAtom")?;
    Ok(X {
        conn,
        root,
        screen,
        atoms,
    })
}

/// Maps any X11 error into a [`PlatformError`] naming the request.
pub trait Ctx<T> {
    fn ctx(self, call: &str) -> Result<T>;
}

impl<T, E: std::fmt::Display> Ctx<T> for std::result::Result<T, E> {
    fn ctx(self, call: &str) -> Result<T> {
        self.map_err(|e| PlatformError::other(call, e))
    }
}

impl X {
    pub fn flush(&self) {
        let _ = self.conn.flush();
    }

    /// A 32-bit property as a list (empty if missing).
    pub fn prop32(&self, window: Window, prop: u32) -> Vec<u32> {
        self.conn
            .get_property(false, window, prop, AtomEnum::ANY, 0, 1 << 20)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(|v| v.collect()))
            .unwrap_or_default()
    }

    /// A text property (UTF-8 or Latin-1) as a string.
    pub fn prop_string(&self, window: Window, prop: u32) -> Option<String> {
        let reply = self
            .conn
            .get_property(false, window, prop, AtomEnum::ANY, 0, 1 << 16)
            .ok()?
            .reply()
            .ok()?;
        if reply.value.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&reply.value).into_owned())
    }

    pub fn set_prop32(&self, window: Window, prop: u32, ty: impl Into<u32>, data: &[u32]) {
        let _ = self
            .conn
            .change_property32(PropMode::REPLACE, window, prop, ty.into(), data);
    }

    pub fn set_utf8(&self, window: Window, prop: u32, text: &str) {
        let _ = self.conn.change_property8(
            PropMode::REPLACE,
            window,
            prop,
            self.atoms.UTF8_STRING,
            text.as_bytes(),
        );
    }

    pub fn delete_prop(&self, window: Window, prop: u32) {
        let _ = self.conn.delete_property(window, prop);
    }

    /// Sends an EWMH client message for `window` to the root window, the way
    /// a client asks the window manager for a state change.
    pub fn client_message(&self, window: Window, ty: u32, data: [u32; 5]) {
        let ev = ClientMessageEvent::new(32, window, ty, data);
        let _ = self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ev,
        );
    }

    /// Adds (`on`) or removes `_NET_WM_STATE` atoms of a mapped window, and
    /// keeps the property in step for a window that is not mapped yet.
    pub fn wm_state(&self, window: Window, on: bool, a: u32, b: u32) {
        const REMOVE: u32 = 0;
        const ADD: u32 = 1;
        let action = if on { ADD } else { REMOVE };
        self.client_message(window, self.atoms._NET_WM_STATE, [action, a, b, 2, 0]);
        let mut state = self.prop32(window, self.atoms._NET_WM_STATE);
        let before = state.clone();
        for atom in [a, b].into_iter().filter(|&x| x != 0) {
            state.retain(|&s| s != atom);
            if on {
                state.push(atom);
            }
        }
        if state != before {
            self.set_prop32(window, self.atoms._NET_WM_STATE, AtomEnum::ATOM, &state);
        }
    }

    /// The window manager's frame window (the root's child that holds
    /// `window`), or `window` itself when it is unmanaged.
    pub fn toplevel_of(&self, window: Window) -> Window {
        let mut cur = window;
        for _ in 0..16 {
            let Some(tree) = self.conn.query_tree(cur).ok().and_then(|c| c.reply().ok()) else {
                return cur;
            };
            if tree.parent == self.root || tree.parent == 0 {
                return cur;
            }
            cur = tree.parent;
        }
        cur
    }

    /// Absolute position and size of `window`: (x, y, w, h).
    pub fn rect_of(&self, window: Window) -> Result<(i32, i32, i32, i32)> {
        let geo = self
            .conn
            .get_geometry(window)
            .ctx("GetGeometry")?
            .reply()
            .ctx("GetGeometry")?;
        let t = self
            .conn
            .translate_coordinates(window, self.root, 0, 0)
            .ctx("TranslateCoordinates")?
            .reply()
            .ctx("TranslateCoordinates")?;
        Ok((
            i32::from(t.dst_x),
            i32::from(t.dst_y),
            i32::from(geo.width),
            i32::from(geo.height),
        ))
    }

    /// The X server's DPI setting (`Xft.dpi`), which GNOME sets from the
    /// display scale; 96 when unset.
    pub fn xft_dpi(&self) -> u32 {
        let Some(db) = self.prop_string(self.root, self.atoms.RESOURCE_MANAGER) else {
            return 96;
        };
        parse_xft_dpi(&db).unwrap_or(96)
    }

    /// Managed top-level client windows, bottom to top.
    pub fn clients(&self) -> Vec<Window> {
        let stacking = self.prop32(self.root, self.atoms._NET_CLIENT_LIST_STACKING);
        if stacking.is_empty() {
            self.prop32(self.root, self.atoms._NET_CLIENT_LIST)
        } else {
            stacking
        }
    }

    pub fn pid_of(&self, window: Window) -> Option<u32> {
        self.prop32(window, self.atoms._NET_WM_PID).first().copied()
    }

    pub fn title_of(&self, window: Window) -> Option<String> {
        self.prop_string(window, self.atoms._NET_WM_NAME)
            .or_else(|| self.prop_string(window, u32::from(AtomEnum::WM_NAME)))
    }

    /// Whether the window is mapped (shown).
    pub fn is_viewable(&self, window: Window) -> bool {
        use x11rb::protocol::xproto::MapState;
        self.conn
            .get_window_attributes(window)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|a| a.map_state == MapState::VIEWABLE)
    }
}

/// `Xft.dpi` from an X resource database string.
pub fn parse_xft_dpi(db: &str) -> Option<u32> {
    db.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "Xft.dpi")
            .then(|| value.trim().parse::<f32>().ok())
            .flatten()
            .filter(|d| *d >= 48.0)
            .map(|d| d.round() as u32)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_xft_dpi() {
        assert_eq!(
            parse_xft_dpi("Xft.antialias:\t1\nXft.dpi:\t144\n"),
            Some(144)
        );
        assert_eq!(parse_xft_dpi("Xft.dpi: 96.5"), Some(97));
        assert_eq!(parse_xft_dpi("Xcursor.size: 24"), None);
    }
}
