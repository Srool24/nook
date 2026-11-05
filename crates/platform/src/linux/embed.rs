//! Hosting another process's window on top of the notch: phone link shows
//! scrcpy's mirror window over its page as if it were part of the panel.
//!
//! scrcpy is started as an X11 client (`SDL_VIDEODRIVER=x11`), so its
//! window can be managed like Nook's own: undecorated, out of the taskbar,
//! transient for Nook's main window (which keeps it above the panel) and
//! above other windows. A SHAPE bounding region gives it the same rounded
//! corners as the cards around it, an empty SHAPE input region makes it
//! view-only, and `_NET_WM_WINDOW_OPACITY` hides it without unmapping it
//! (an unmapped GL window may stop presenting).
//!
//! While the guest has the keyboard, [`EscHook`] (a passive grab of the Esc
//! key on the guest window) takes Esc for Nook, so the user can always get
//! out of a mirrored phone that would otherwise swallow every key.

use crate::Result;
use crate::sys::surface::{Keymap, keysym};
use crate::sys::x11::{Ctx, x};
use crate::window::Hwnd;
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ClipOrdering, ConfigureWindowAux, ConnectionExt as _,
    CreateWindowAux, EventMask, GrabMode, ModMask, Rectangle, StackMode, WindowClass,
};

/// A visible top-level window of process `pid` whose title is `title`.
pub fn find_process_window(pid: u32, title: &str) -> Option<Hwnd> {
    let x = x().ok()?;
    x.clients()
        .into_iter()
        .find(|&w| x.pid_of(w) == Some(pid) && x.title_of(w).as_deref() == Some(title))
        .map(|w| Hwnd::from_raw(w as isize))
}

/// Whether the handle still names a window (the guest may have exited).
pub fn is_alive(hwnd: Hwnd) -> bool {
    x().is_ok_and(|x| {
        x.conn
            .get_window_attributes(hwnd.xid())
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some()
    })
}

/// Turns `guest` into an undecorated, taskbar-less window kept above
/// `owner`.
pub fn adopt(guest: Hwnd, owner: Hwnd) -> Result<()> {
    let x = x()?;
    let g = guest.xid();
    // Motif hints: no decorations.
    x.set_prop32(
        g,
        x.atoms._MOTIF_WM_HINTS,
        x.atoms._MOTIF_WM_HINTS,
        &[2, 0, 0, 0, 0],
    );
    x.wm_state(
        g,
        true,
        x.atoms._NET_WM_STATE_SKIP_TASKBAR,
        x.atoms._NET_WM_STATE_SKIP_PAGER,
    );
    x.wm_state(g, true, x.atoms._NET_WM_STATE_ABOVE, 0);
    x.set_prop32(
        g,
        AtomEnum::WM_TRANSIENT_FOR.into(),
        AtomEnum::WINDOW,
        &[owner.xid()],
    );
    x.flush();
    Ok(())
}

/// Moves and sizes the guest and rounds its corners with `radius` pixels.
pub fn place(guest: Hwnd, x0: i32, y0: i32, w: i32, h: i32, radius: i32) -> Result<()> {
    let x = x()?;
    let aux = ConfigureWindowAux::new()
        .x(x0)
        .y(y0)
        .width(w.max(1) as u32)
        .height(h.max(1) as u32);
    x.conn
        .configure_window(guest.xid(), &aux)
        .ctx("ConfigureWindow(guest)")?;
    let rects = rounded_rect(w, h, radius);
    x.conn
        .shape_rectangles(
            SO::SET,
            SK::BOUNDING,
            ClipOrdering::YX_BANDED,
            guest.xid(),
            0,
            0,
            &rects,
        )
        .ctx("ShapeRectangles(guest)")?;
    x.flush();
    Ok(())
}

/// The rows of a `w` x `h` rectangle with corners of `radius`, as bands.
fn rounded_rect(w: i32, h: i32, radius: i32) -> Vec<Rectangle> {
    let r = radius.clamp(0, w.min(h) / 2);
    let mut out = Vec::new();
    for y in 0..r {
        // Inset of this row at the corner circle.
        let dy = r as f32 - y as f32 - 0.5;
        let dx = (r as f32 * r as f32 - dy * dy).max(0.0).sqrt();
        let inset = (r as f32 - dx).round() as i32;
        for row in [y, h - 1 - y] {
            out.push(Rectangle {
                x: inset as i16,
                y: row as i16,
                width: (w - 2 * inset).max(0) as u16,
                height: 1,
            });
        }
    }
    if h > 2 * r {
        out.push(Rectangle {
            x: 0,
            y: r as i16,
            width: w as u16,
            height: (h - 2 * r) as u16,
        });
    }
    out.sort_by_key(|r| (r.y, r.x));
    out
}

/// Puts `guest` directly above `owner` (the transient relationship keeps it
/// there; this restacks it once in case it started below).
pub fn raise_above(guest: Hwnd, owner: Hwnd) {
    let Ok(x) = x() else {
        return;
    };
    let aux = ConfigureWindowAux::new()
        .sibling(x.toplevel_of(owner.xid()))
        .stack_mode(StackMode::ABOVE);
    if x.conn.configure_window(guest.xid(), &aux).is_err() {
        let aux = ConfigureWindowAux::new().stack_mode(StackMode::ABOVE);
        let _ = x.conn.configure_window(guest.xid(), &aux);
    }
    x.flush();
}

/// Shows or hides the guest through its opacity; it is never unmapped.
pub fn set_visible(guest: Hwnd, visible: bool) {
    let Ok(x) = x() else {
        return;
    };
    let prop = x.atoms._NET_WM_WINDOW_OPACITY;
    if visible {
        x.delete_prop(guest.xid(), prop);
        if !x.is_viewable(guest.xid()) {
            let _ = x.conn.map_window(guest.xid());
        }
    } else {
        x.set_prop32(guest.xid(), prop, AtomEnum::CARDINAL, &[0]);
    }
    x.flush();
}

/// Click-through (view only) or interactive.
pub fn set_pass_through(guest: Hwnd, through: bool) {
    let Ok(x) = x() else {
        return;
    };
    let _ = if through {
        x.conn.shape_rectangles(
            SO::SET,
            SK::INPUT,
            ClipOrdering::UNSORTED,
            guest.xid(),
            0,
            0,
            &[],
        )
    } else {
        x.conn
            .shape_mask(SO::SET, SK::INPUT, guest.xid(), 0, 0, x11rb::NONE)
    };
    x.flush();
}

/// The active window, as far as an X11 client can tell.
pub fn foreground() -> Option<Hwnd> {
    let x = x().ok()?;
    let w = x
        .prop32(x.root, x.atoms._NET_ACTIVE_WINDOW)
        .first()
        .copied()?;
    (w != 0).then(|| Hwnd::from_raw(w as isize))
}

/// Hands the keyboard to `hwnd` (as a pager request, which GNOME honours).
pub fn give_focus(hwnd: Hwnd) {
    if let Ok(x) = x() {
        x.client_message(hwnd.xid(), x.atoms._NET_ACTIVE_WINDOW, [2, 0, 0, 0, 0]);
        x.flush();
    }
}

/// Catches Esc while the keyboard is in `target`: the key never reaches
/// it, and `on_esc` runs instead (on the hook's thread). Dropping the hook
/// removes it.
pub struct EscHook {
    wake: u32,
    join: Option<std::thread::JoinHandle<()>>,
}

impl EscHook {
    pub fn install(target: Hwnd, on_esc: impl Fn() + Send + 'static) -> Result<Self> {
        let conn = crate::sys::x11::open()?;
        let keys = Keymap::load(&conn);
        let setup = conn.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let esc = (min..=max)
            .find(|&k| {
                keys.as_ref()
                    .is_some_and(|km| km.keysym(k) == keysym::ESCAPE)
            })
            .ok_or_else(|| crate::PlatformError::Other("no Escape key in the keymap".into()))?;
        let wake = conn.conn.generate_id().ctx("generate_id")?;
        conn.conn
            .create_window(
                0,
                wake,
                conn.root,
                -10,
                -10,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new(),
            )
            .ctx("CreateWindow(esc hook)")?;
        conn.conn
            .grab_key(
                true,
                target.xid(),
                ModMask::ANY,
                esc,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .ctx("GrabKey(Escape)")?
            .check()
            .ctx("GrabKey(Escape)")?;
        conn.flush();
        let guest = target.xid();
        let join = std::thread::Builder::new()
            .name("nook-esc-hook".into())
            .spawn(move || {
                while let Ok(ev) = conn.conn.wait_for_event() {
                    match ev {
                        Event::KeyPress(k) if k.detail == esc => on_esc(),
                        Event::ClientMessage(m) if m.window == wake => break,
                        _ => {}
                    }
                }
                let _ = conn.conn.ungrab_key(esc, guest, ModMask::ANY);
                let _ = conn.conn.destroy_window(wake);
                conn.flush();
            })
            .map_err(|e| crate::PlatformError::other("spawning hook thread", e))?;
        Ok(Self {
            wake,
            join: Some(join),
        })
    }
}

impl Drop for EscHook {
    fn drop(&mut self) {
        if let Ok(x) = x() {
            let ev = ClientMessageEvent::new(32, self.wake, x.atoms.NOOK_SELECTION, [0u32; 5]);
            let _ = x.conn.send_event(false, self.wake, EventMask::NO_EVENT, ev);
            x.flush();
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_corners_in_bands() {
        let rects = rounded_rect(100, 60, 10);
        // 10 rows at each end plus the middle band.
        assert_eq!(rects.len(), 21);
        let top = rects.first().unwrap();
        assert!(top.x > 0 && top.width < 100);
        let middle = rects.iter().find(|r| r.height > 1).unwrap();
        assert_eq!(
            (middle.x, middle.y, middle.width, middle.height),
            (0, 10, 100, 40)
        );
        assert_eq!(rounded_rect(50, 50, 0).len(), 1);
    }
}
