//! Which backdrop is in use. Linux has no compositor blur a client can ask
//! for behind its own shape, so the glass is always drawn by egui.

use crate::window::Hwnd;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// Shape-clipped blurred backdrop; draw the glass translucent.
    Acrylic,
    /// No backdrop; draw the glass nearly opaque.
    Opaque,
}

impl Applied {
    pub fn translucent(self) -> bool {
        matches!(self, Applied::Acrylic)
    }
}

/// Tells GTK-aware window managers which theme variant the window uses.
pub fn prepare(hwnd: Hwnd, dark: bool) {
    let Ok(x) = crate::sys::x11::x() else {
        return;
    };
    use x11rb::protocol::xproto::ConnectionExt as _;
    let Ok(atom) = x
        .conn
        .intern_atom(false, b"_GTK_THEME_VARIANT")
        .map_err(|_| ())
        .and_then(|c| c.reply().map_err(|_| ()))
    else {
        return;
    };
    x.set_utf8(hwnd.xid(), atom.atom, if dark { "dark" } else { "light" });
    x.flush();
}
