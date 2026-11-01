//! An X11 window with a pixel buffer behind it, for the full-screen
//! overlays (snip, ink) that paint in software and push only what changed.

use super::x11::{Ctx, X};
use crate::Result;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    AtomEnum, ColormapAlloc, ConnectionExt as _, CreateGCAux, CreateWindowAux, CursorWrapper,
    EventMask, Gcontext, ImageFormat, PropMode, Visualid, Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;

/// What kind of window to make.
pub struct Spec<'a> {
    pub bounds: (i32, i32, i32, i32),
    pub title: &'a str,
    /// A 32-bit visual with alpha (composited over the desktop).
    pub argb: bool,
    /// X cursor font glyph (34: crosshair).
    pub cursor_glyph: u16,
}

pub struct Surface {
    pub window: Window,
    pub gc: Gcontext,
    pub w: i32,
    pub h: i32,
    pub depth: u8,
    /// Pixels `0xAARRGGBB`, top-down, `w * h` (premultiplied for ARGB).
    pub pixels: Vec<u32>,
}

fn argb_visual(x: &X) -> Option<Visualid> {
    let screen = &x.conn.setup().roots[x.screen];
    screen
        .allowed_depths
        .iter()
        .filter(|d| d.depth == 32)
        .flat_map(|d| d.visuals.iter())
        .find(|v| v.class == x11rb::protocol::xproto::VisualClass::TRUE_COLOR)
        .map(|v| v.visual_id)
}

impl Surface {
    /// Creates (but does not map) a window over `bounds` that covers every
    /// monitor it spans: fullscreen, above other windows, out of the
    /// taskbar, focusable.
    pub fn create(x: &X, spec: &Spec<'_>) -> Result<Self> {
        let (bx, by, w, h) = spec.bounds;
        let screen = &x.conn.setup().roots[x.screen];
        let window = x.conn.generate_id().ctx("generate_id")?;
        let events = EventMask::EXPOSURE
            | EventMask::KEY_PRESS
            | EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::STRUCTURE_NOTIFY
            | EventMask::FOCUS_CHANGE;
        let (depth, visual, mut aux) = match spec.argb.then(|| argb_visual(x)).flatten() {
            Some(visual) => {
                let cmap = x.conn.generate_id().ctx("generate_id")?;
                x.conn
                    .create_colormap(ColormapAlloc::NONE, cmap, x.root, visual)
                    .ctx("CreateColormap")?;
                (
                    32u8,
                    visual,
                    CreateWindowAux::new()
                        .colormap(cmap)
                        .background_pixel(0)
                        .border_pixel(0),
                )
            }
            None => (
                screen.root_depth,
                screen.root_visual,
                CreateWindowAux::new().background_pixel(screen.black_pixel),
            ),
        };
        aux = aux.event_mask(events);
        let font = open_cursor_font(x)?;
        if let Ok(cursor) = CursorWrapper::create_glyph_cursor(
            &x.conn,
            font,
            font,
            spec.cursor_glyph,
            spec.cursor_glyph + 1,
            0,
            0,
            0,
            0xFFFF,
            0xFFFF,
            0xFFFF,
        ) {
            aux = aux.cursor(cursor.into_cursor());
        }
        let _ = x.conn.close_font(font);
        x.conn
            .create_window(
                depth,
                window,
                x.root,
                bx as i16,
                by as i16,
                w as u16,
                h as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                visual,
                &aux,
            )
            .ctx("CreateWindow(overlay)")?;
        x.set_utf8(window, x.atoms._NET_WM_NAME, spec.title);
        let _ = x.conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            spec.title.as_bytes(),
        );
        let _ = x.conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"nook\0Nook\0",
        );
        x.set_prop32(
            window,
            x.atoms._NET_WM_PID,
            AtomEnum::CARDINAL,
            &[std::process::id()],
        );
        x.set_prop32(
            window,
            x.atoms.WM_PROTOCOLS,
            AtomEnum::ATOM,
            &[x.atoms.WM_DELETE_WINDOW],
        );
        x.set_prop32(
            window,
            x.atoms._NET_WM_STATE,
            AtomEnum::ATOM,
            &[
                x.atoms._NET_WM_STATE_FULLSCREEN,
                x.atoms._NET_WM_STATE_ABOVE,
                x.atoms._NET_WM_STATE_SKIP_TASKBAR,
                x.atoms._NET_WM_STATE_SKIP_PAGER,
            ],
        );
        // No window decorations (Motif hints: flags=decorations, none).
        x.set_prop32(
            window,
            x.atoms._MOTIF_WM_HINTS,
            x.atoms._MOTIF_WM_HINTS,
            &[2, 0, 0, 0, 0],
        );
        // Keep the compositor compositing (the ink is see-through).
        x.set_prop32(
            window,
            x.atoms._NET_WM_BYPASS_COMPOSITOR,
            AtomEnum::CARDINAL,
            &[2],
        );
        let gc = x.conn.generate_id().ctx("generate_id")?;
        x.conn
            .create_gc(gc, window, &CreateGCAux::new().graphics_exposures(0))
            .ctx("CreateGC")?;
        Ok(Self {
            window,
            gc,
            w,
            h,
            depth,
            pixels: vec![0; (w.max(0) * h.max(0)) as usize],
        })
    }

    /// Maps the window and asks for it to span every monitor in `bounds`.
    pub fn show(&self, x: &X) {
        let _ = x.conn.map_window(self.window);
        if let Some(edges) = fullscreen_monitors(x) {
            x.client_message(
                self.window,
                x.atoms._NET_WM_FULLSCREEN_MONITORS,
                [edges[0], edges[1], edges[2], edges[3], 1],
            );
        }
        x.client_message(self.window, x.atoms._NET_ACTIVE_WINDOW, [2, 0, 0, 0, 0]);
        x.flush();
    }

    /// Sends the pixels inside `(x0, y0, x1, y1)` to the window.
    pub fn present(&self, x: &X, (x0, y0, x1, y1): (i32, i32, i32, i32)) {
        let (x0, y0) = (x0.clamp(0, self.w), y0.clamp(0, self.h));
        let (x1, y1) = (x1.clamp(0, self.w), y1.clamp(0, self.h));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let row_bytes = ((x1 - x0) * 4) as usize;
        // Keep each request well under the server's limit.
        let max_rows = ((200 * 1024) / row_bytes).max(1) as i32;
        let mut y = y0;
        while y < y1 {
            let rows = max_rows.min(y1 - y);
            let mut data = Vec::with_capacity(row_bytes * rows as usize);
            for yy in y..y + rows {
                let start = (yy * self.w + x0) as usize;
                for &p in &self.pixels[start..start + (x1 - x0) as usize] {
                    data.extend_from_slice(&p.to_le_bytes());
                }
            }
            let _ = x.conn.put_image(
                ImageFormat::Z_PIXMAP,
                self.window,
                self.gc,
                (x1 - x0) as u16,
                rows as u16,
                x0 as i16,
                y as i16,
                0,
                self.depth,
                &data,
            );
            y += rows;
        }
        x.flush();
    }

    pub fn present_all(&self, x: &X) {
        self.present(x, (0, 0, self.w, self.h));
    }

    pub fn destroy(&self, x: &X) {
        let _ = x.conn.destroy_window(self.window);
        x.flush();
    }
}

fn open_cursor_font(x: &X) -> Result<u32> {
    let font = x.conn.generate_id().ctx("generate_id")?;
    x.conn.open_font(font, b"cursor").ctx("OpenFont(cursor)")?;
    Ok(font)
}

/// `_NET_WM_FULLSCREEN_MONITORS` edges (top, bottom, left, right monitor
/// indices in RandR order) for a window covering every monitor.
fn fullscreen_monitors(x: &X) -> Option<[u32; 4]> {
    use x11rb::protocol::randr::ConnectionExt as _;
    let mons = x
        .conn
        .randr_get_monitors(x.root, true)
        .ok()?
        .reply()
        .ok()?
        .monitors;
    if mons.len() < 2 {
        return None;
    }
    let idx = |key: &dyn Fn(&x11rb::protocol::randr::MonitorInfo) -> i32| {
        mons.iter()
            .enumerate()
            .min_by_key(|(_, m)| key(m))
            .map(|(i, _)| i as u32)
            .unwrap_or(0)
    };
    Some([
        idx(&|m| i32::from(m.y)),
        idx(&|m| -(i32::from(m.y) + i32::from(m.height))),
        idx(&|m| i32::from(m.x)),
        idx(&|m| -(i32::from(m.x) + i32::from(m.width))),
    ])
}

/// Keyboard mapping lookups: keysym of a key code.
pub struct Keymap {
    min: u8,
    per: u8,
    syms: Vec<u32>,
}

impl Keymap {
    pub fn load(x: &X) -> Option<Self> {
        let setup = x.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let reply = x
            .conn
            .get_keyboard_mapping(min, max - min + 1)
            .ok()?
            .reply()
            .ok()?;
        Some(Self {
            min,
            per: reply.keysyms_per_keycode,
            syms: reply.keysyms,
        })
    }

    /// The unshifted keysym of `code` (lowercase for letters).
    pub fn keysym(&self, code: u8) -> u32 {
        let i = usize::from(code.saturating_sub(self.min)) * usize::from(self.per);
        self.syms.get(i).copied().unwrap_or(0)
    }
}

pub mod keysym {
    pub const ESCAPE: u32 = 0xff1b;
    pub const RETURN: u32 = 0xff0d;
    pub const KP_ENTER: u32 = 0xff8d;
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const A: u32 = 0x61;
    pub const F: u32 = 0x66;
    pub const Z: u32 = 0x7a;
}
