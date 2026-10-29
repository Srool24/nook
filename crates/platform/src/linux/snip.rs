//! The freeze-and-select overlay shared by the text extractor, lens search,
//! the screenshot tool, the font detector and the color picker.
//!
//! Every monitor is grabbed first (see `capture`), then shown frozen in one
//! full-screen window that spans all of them, so menus, tooltips and video
//! hold still while you select, and a selection or the loupe can be on any
//! monitor or across them. The hint, labels and loupe follow the monitor
//! under the cursor.
//!
//! Everything is painted in software on the overlay's own thread and X
//! connection: the frozen frame and a dimmed copy live in memory, and each
//! change only recomposes and sends the rectangles that changed, so
//! dragging costs a few small image uploads and never flickers.
//!
//! - [`SnipMode::Region`]: drag a rectangle. Outside it the screen is
//!   dimmed; a label shows its size. Release to finish; Shift at release
//!   asks for a single line of text.
//! - [`SnipMode::Screenshot`]: like region, plus the X11 window under the
//!   cursor is highlighted and a click takes it (Wayland windows are not
//!   visible to X11 clients; the monitor is taken there); F takes the
//!   monitor under the cursor, A every monitor, Enter the highlighted area.
//! - [`SnipMode::Pick`]: a loupe follows the cursor with the pixels under
//!   it magnified and the color's hex code. Scroll to zoom, arrows nudge by
//!   a pixel (Shift: ten), click or Enter picks.
//!
//! Esc or right click cancels. The result is handed to a callback on the
//! overlay thread after its window is gone, so slow work there (OCR) never
//! holds the overlay open.

use crate::capture::{self, Frame};
use crate::sys::fonts;
use crate::sys::surface::{Keymap, Spec, Surface, keysym};
use crate::sys::x11::X;
use crate::{PlatformError, Result, monitor};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt as _, EventMask, KeyButMask};

/// Loupe zoom steps: pixels shown across (odd, so one sits in the middle).
const LOUPE_CELLS: [i32; 6] = [31, 21, 15, 11, 9, 7];
const LOUPE_DEFAULT: usize = 2;
/// Loupe edge in logical pixels.
const LOUPE_SIZE: i32 = 150;
/// Selections smaller than this (pixels) count as a stray click.
const MIN_SELECTION: i32 = 4;
/// Brightness kept outside the selection, out of 256.
const DIM: u32 = 120;
/// `data[0]` of the client message that closes the overlay.
const CMD_CLOSE: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnipMode {
    Region,
    Screenshot,
    Pick,
}

#[derive(Debug)]
pub enum SnipOutcome {
    /// The selected part of the frozen frame. `single_line` is true when
    /// Shift was held as the selection finished.
    Region {
        frame: Frame,
        single_line: bool,
    },
    /// The picked pixel (`[r, g, b]`) and where it was on screen.
    Color {
        rgb: [u8; 3],
        at: (i32, i32),
    },
    Cancelled,
}

/// Handle to an open overlay. Dropping it cancels the overlay.
pub struct Snip {
    window: u32,
    open: Arc<AtomicBool>,
}

impl Snip {
    /// Freezes every monitor and opens the overlay over all of them.
    /// `hint` shows at the top until the pointer starts working.
    /// `on_done` runs on the overlay thread once the overlay has closed.
    pub fn open(
        mode: SnipMode,
        hint: impl Into<String>,
        on_done: impl FnOnce(SnipOutcome) + Send + 'static,
    ) -> Result<Self> {
        let hint = hint.into();
        let open = Arc::new(AtomicBool::new(true));
        let thread_open = open.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("nook-snip".into())
            .spawn(move || {
                let created = monitor::virtual_bounds()
                    .and_then(capture::grab)
                    .and_then(|frame| {
                        let x = crate::sys::x11::open()?;
                        Overlay::create(x, mode, frame, &hint, &monitor::all())
                    });
                let mut overlay = match created {
                    Ok(o) => o,
                    Err(e) => {
                        thread_open.store(false, Ordering::Relaxed);
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                overlay.render_all();
                overlay.surface.show(&overlay.x);
                overlay.surface.present_all(&overlay.x);
                overlay.grab_keyboard();
                let _ = ready_tx.send(Ok(overlay.surface.window));
                overlay.run();
                overlay.surface.destroy(&overlay.x);
                let outcome = overlay.outcome.take().unwrap_or(SnipOutcome::Cancelled);
                drop(overlay);
                thread_open.store(false, Ordering::Relaxed);
                on_done(outcome);
            })
            .map_err(|e| PlatformError::other("spawning snip thread", e))?;
        let window = ready_rx
            .recv()
            .map_err(|_| PlatformError::Other("snip thread exited".into()))??;
        Ok(Self { window, open })
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }
}

impl Drop for Snip {
    fn drop(&mut self) {
        if !self.is_open() {
            return;
        }
        if let Ok(x) = crate::sys::x11::x() {
            let ev = ClientMessageEvent::new(
                32,
                self.window,
                x.atoms.NOOK_SELECTION,
                [CMD_CLOSE, 0, 0, 0, 0],
            );
            let _ = x
                .conn
                .send_event(false, self.window, EventMask::NO_EVENT, ev);
            x.flush();
        }
    }
}

/// `(left, top, right, bottom)` in overlay pixels.
type R = (i32, i32, i32, i32);

fn contains(r: R, (x, y): (i32, i32)) -> bool {
    x >= r.0 && x < r.2 && y >= r.1 && y < r.3
}

fn inflate(r: R, by: i32) -> R {
    (r.0 - by, r.1 - by, r.2 + by, r.3 + by)
}

/// Normalized rectangle between two corners.
fn span(a: (i32, i32), b: (i32, i32)) -> R {
    (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

fn intersect(a: R, b: R) -> Option<R> {
    let r = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    (r.2 > r.0 && r.3 > r.1).then_some(r)
}

/// "640 × 480".
fn size_label(sel: R) -> String {
    format!("{} \u{00D7} {}", sel.2 - sel.0, sel.3 - sel.1)
}

/// Darkens an opaque pixel.
fn dimmed(p: u32) -> u32 {
    let ch = |shift: u32| (((p >> shift) & 0xFF) * DIM / 256) << shift;
    0xFF00_0000 | ch(16) | ch(8) | ch(0)
}

fn rgb(r: u8, g: u8, b: u8) -> u32 {
    0xFF00_0000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

/// Where the loupe goes for a cursor at `c`: below right, flipped to stay
/// inside `bounds` (the cursor's monitor).
fn loupe_rect(c: (i32, i32), size: i32, label: i32, gap: i32, bounds: R) -> R {
    let total_h = size + label;
    let x = if c.0 + gap + size <= bounds.2 {
        c.0 + gap
    } else {
        c.0 - gap - size
    };
    let y = if c.1 + gap + total_h <= bounds.3 {
        c.1 + gap
    } else {
        c.1 - gap - total_h
    };
    (x, y, x + size, y + total_h)
}

/// One monitor under the overlay: where it is (overlay coordinates) and its
/// scale.
struct Screen {
    rect: R,
    scale: f32,
}

/// Drawing into the overlay's pixels, clipped to one dirty rectangle.
struct Painter<'a> {
    px: &'a mut [u32],
    w: i32,
    clip: R,
}

impl Painter<'_> {
    fn put(&mut self, x: i32, y: i32, color: u32, alpha: f32) {
        if !contains(self.clip, (x, y)) || alpha <= 0.0 {
            return;
        }
        let i = (y * self.w + x) as usize;
        if alpha >= 1.0 {
            self.px[i] = color;
            return;
        }
        let d = self.px[i];
        let mix = |shift: u32| {
            let a = ((d >> shift) & 0xFF) as f32;
            let b = ((color >> shift) & 0xFF) as f32;
            ((a + (b - a) * alpha).round() as u32) << shift
        };
        self.px[i] = 0xFF00_0000 | mix(16) | mix(8) | mix(0);
    }

    fn fill(&mut self, r: R, color: u32) {
        let Some((x0, y0, x1, y1)) = intersect(r, self.clip) else {
            return;
        };
        for y in y0..y1 {
            let row = (y * self.w) as usize;
            self.px[row + x0 as usize..row + x1 as usize].fill(color);
        }
    }

    /// A one-pixel outline just inside `r`.
    fn frame(&mut self, r: R, color: u32) {
        let (l, t, rr, b) = r;
        self.fill((l, t, rr, t + 1), color);
        self.fill((l, b - 1, rr, b), color);
        self.fill((l, t, l + 1, b), color);
        self.fill((rr - 1, t, rr, b), color);
    }

    /// A filled rounded rectangle with anti-aliased corners.
    fn round_rect(&mut self, r: R, radius: f32, color: u32) {
        let Some((x0, y0, x1, y1)) = intersect(r, self.clip) else {
            return;
        };
        let (l, t, rr, b) = (r.0 as f32, r.1 as f32, r.2 as f32, r.3 as f32);
        let rad = radius.min((rr - l) / 2.0).min((b - t) / 2.0);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let cx = px.clamp(l + rad, rr - rad);
                let cy = py.clamp(t + rad, b - rad);
                let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                let a = (rad - d + 0.5).clamp(0.0, 1.0);
                self.put(x, y, color, a);
            }
        }
    }

    /// Copies `r` from `src` (a full-size buffer).
    fn copy(&mut self, src: &[u32], r: R) {
        let Some((x0, y0, x1, y1)) = intersect(r, self.clip) else {
            return;
        };
        for y in y0..y1 {
            let row = (y * self.w) as usize;
            let (a, b) = (row + x0 as usize, row + x1 as usize);
            self.px[a..b].copy_from_slice(&src[a..b]);
        }
    }

    /// One line of text centered in `r`.
    fn text_centered(&mut self, r: R, line: &fonts::Line, color: u32) {
        let x = r.0 + ((r.2 - r.0) as f32 - line.width).max(0.0) as i32 / 2;
        let y = r.1 + ((r.3 - r.1) as f32 - line.height).max(0.0) as i32 / 2;
        let mut cover: Vec<(i32, i32, f32)> = Vec::new();
        line.draw(|gx, gy, c| cover.push((x + gx, y + gy, c)));
        for (px, py, c) in cover {
            self.put(px, py, color, c);
        }
    }
}

struct Overlay {
    x: X,
    surface: Surface,
    keys: Option<Keymap>,
    font: Option<Arc<ab_glyph::FontVec>>,
    mode: SnipMode,
    /// Screen position of the frozen frame (its pixels are `bright`).
    origin: (i32, i32),
    size: (i32, i32),
    bright: Vec<u32>,
    /// The frozen frame dimmed (region and screenshot modes).
    dim: Option<Vec<u32>>,
    screens: Vec<Screen>,
    /// Index of the screen under the cursor.
    screen: usize,
    /// Visible X11 windows, front to back, overlay coordinates
    /// (screenshot mode only).
    windows: Vec<R>,
    hint: String,
    /// Drag anchor and current corner, overlay coordinates.
    anchor: Option<(i32, i32)>,
    corner: (i32, i32),
    /// Last known cursor position (overlay coordinates).
    cursor: (i32, i32),
    zoom: usize,
    /// What was last painted besides the frame (to repaint on change).
    decor: Vec<R>,
    outcome: Option<SnipOutcome>,
    done: bool,
}

impl Overlay {
    fn create(
        x: X,
        mode: SnipMode,
        frame: Frame,
        hint: &str,
        monitors: &[monitor::Monitor],
    ) -> Result<Self> {
        let (ox, oy) = frame.origin;
        let (w, h) = (frame.width as i32, frame.height as i32);
        let surface = Surface::create(
            &x,
            &Spec {
                bounds: (ox, oy, w, h),
                title: "Nook Snip",
                argb: false,
                cursor_glyph: 34,
            },
        )?;
        let dim = match mode {
            SnipMode::Region | SnipMode::Screenshot => {
                Some(frame.pixels.iter().map(|&p| dimmed(p)).collect())
            }
            SnipMode::Pick => None,
        };
        let mut screens: Vec<Screen> = monitors
            .iter()
            .map(|m| Screen {
                rect: (
                    m.bounds.left - ox,
                    m.bounds.top - oy,
                    m.bounds.right - ox,
                    m.bounds.bottom - oy,
                ),
                scale: m.dpi as f32 / 96.0,
            })
            .collect();
        if screens.is_empty() {
            screens.push(Screen {
                rect: (0, 0, w, h),
                scale: 1.0,
            });
        }
        let cursor = x
            .conn
            .query_pointer(x.root)
            .ok()
            .and_then(|c| c.reply().ok())
            .map_or((w / 2, h / 2), |p| {
                (i32::from(p.root_x) - ox, i32::from(p.root_y) - oy)
            });
        let windows = if mode == SnipMode::Screenshot {
            visible_windows(&x, (ox, oy))
        } else {
            Vec::new()
        };
        let keys = Keymap::load(&x);
        let mut overlay = Self {
            x,
            surface,
            keys,
            font: fonts::ui_semibold(),
            mode,
            origin: (ox, oy),
            size: (w, h),
            bright: frame.pixels,
            dim,
            screens,
            screen: 0,
            windows,
            hint: hint.to_owned(),
            anchor: None,
            corner: cursor,
            cursor,
            zoom: LOUPE_DEFAULT,
            decor: Vec::new(),
            outcome: None,
            done: false,
        };
        overlay.screen = overlay.screen_at(cursor);
        overlay.decor = overlay.decorations();
        Ok(overlay)
    }

    /// Asks for the keyboard (Esc, F, A, arrows) even if the window
    /// manager did not focus the overlay.
    fn grab_keyboard(&self) {
        use x11rb::protocol::xproto::GrabMode;
        let _ = self.x.conn.grab_keyboard(
            false,
            self.surface.window,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        );
        let _ = self.x.conn.set_input_focus(
            x11rb::protocol::xproto::InputFocus::PARENT,
            self.surface.window,
            x11rb::CURRENT_TIME,
        );
        self.x.flush();
    }

    /// The screen containing `p`, or the nearest one.
    fn screen_at(&self, p: (i32, i32)) -> usize {
        if let Some(i) = self.screens.iter().position(|s| contains(s.rect, p)) {
            return i;
        }
        let dist = |r: R| {
            let dx = (r.0 - p.0).max(p.0 - r.2 + 1).max(0);
            let dy = (r.1 - p.1).max(p.1 - r.3 + 1).max(0);
            i64::from(dx) * i64::from(dx) + i64::from(dy) * i64::from(dy)
        };
        (0..self.screens.len())
            .min_by_key(|&i| dist(self.screens[i].rect))
            .unwrap_or(0)
    }

    fn current(&self) -> &Screen {
        &self.screens[self.screen.min(self.screens.len() - 1)]
    }

    /// Logical pixels to device pixels, at the scale of the cursor's
    /// monitor.
    fn px(&self, logical: f32) -> i32 {
        (logical * self.current().scale).round() as i32
    }

    fn selection(&self) -> Option<R> {
        self.anchor.map(|a| span(a, self.corner))
    }

    fn line(&self, text: &str) -> Option<fonts::Line> {
        let font = self.font.as_ref()?;
        Some(fonts::layout(font, text, 13.0 * self.current().scale))
    }

    /// Rectangles of everything drawn over the frame, for the current state.
    fn decorations(&self) -> Vec<R> {
        let (w, h) = self.size;
        let mut out = Vec::new();
        let pad = self.px(4.0);
        match self.mode {
            SnipMode::Region | SnipMode::Screenshot => {
                if let Some(sel) = self.selection().or_else(|| self.target()) {
                    out.push(inflate(sel, pad));
                    let tw = self.label_width(sel);
                    out.push(inflate(self.size_label_rect(sel, tw), 2));
                }
                if self.anchor.is_none() {
                    out.push(self.hint_rect());
                }
            }
            SnipMode::Pick => {
                out.push(inflate(self.loupe(), pad));
                out.push(self.hint_rect());
            }
        }
        out.into_iter()
            .filter_map(|r| intersect(r, (0, 0, w, h)))
            .collect()
    }

    fn hint_rect(&self) -> R {
        let m = self.current().rect;
        let cx = (m.0 + m.2) / 2;
        let half = self.px(260.0).min((m.2 - m.0) / 2);
        let top = m.1 + self.px(24.0);
        (cx - half, top, cx + half, top + self.px(34.0))
    }

    fn label_width(&self, sel: R) -> Option<i32> {
        self.line(&size_label(sel)).map(|l| l.width.ceil() as i32)
    }

    /// Where the size label sits for `sel`: below it, or above when there
    /// is no room. `text_w` sizes it to its text.
    fn size_label_rect(&self, sel: R, text_w: Option<i32>) -> R {
        let lw = text_w.map_or(self.px(96.0), |t| t + self.px(20.0));
        let lh = self.px(24.0);
        let gap = self.px(8.0);
        let m = self.current().rect;
        let top = if sel.3 + gap + lh <= m.3 {
            sel.3 + gap
        } else {
            (sel.1 - gap - lh).max(m.1)
        };
        (sel.0, top, sel.0 + lw.max(self.px(96.0)), top + lh)
    }

    fn loupe(&self) -> R {
        let cells = LOUPE_CELLS[self.zoom];
        let cell = (self.px(LOUPE_SIZE as f32) / cells).max(1);
        loupe_rect(
            self.cursor,
            cell * cells,
            self.px(30.0),
            self.px(20.0),
            self.current().rect,
        )
    }

    /// Repaints whatever changed since the last call.
    fn refresh(&mut self) {
        let now = self.decorations();
        let dirty: Vec<R> = self.decor.iter().chain(&now).copied().collect();
        self.decor = now;
        for r in dirty {
            self.render(r);
            self.surface.present(&self.x, r);
        }
    }

    fn render_all(&mut self) {
        let (w, h) = self.size;
        self.render((0, 0, w, h));
    }

    /// Recomposes `clip`: the (dimmed) frame, then everything over it.
    fn render(&mut self, clip: R) {
        let mut px = std::mem::take(&mut self.surface.pixels);
        {
            let mut p = Painter {
                px: &mut px,
                w: self.size.0,
                clip,
            };
            p.copy(self.dim.as_deref().unwrap_or(&self.bright), clip);
            match self.mode {
                SnipMode::Region | SnipMode::Screenshot => self.paint_region(&mut p),
                SnipMode::Pick => self.paint_pick(&mut p),
            }
        }
        self.surface.pixels = px;
    }

    /// A dark rounded label with centered white text.
    fn pill(&self, p: &mut Painter<'_>, r: R, text: &str) {
        let radius = (r.3 - r.1) as f32 / 2.0;
        p.round_rect(r, radius, rgb(24, 24, 28));
        if text.is_empty() {
            return;
        }
        if let Some(line) = self.line(text) {
            p.text_centered(r, &line, rgb(255, 255, 255));
        }
    }

    /// The hint, in a pill sized to the text, centered in `hint_rect`.
    fn paint_hint(&self, p: &mut Painter<'_>) {
        let area = self.hint_rect();
        let tw = self.line(&self.hint).map_or(0, |l| l.width.ceil() as i32);
        let half = (tw / 2 + self.px(16.0)).min((area.2 - area.0) / 2);
        let cx = (area.0 + area.2) / 2;
        self.pill(p, (cx - half, area.1, cx + half, area.3), &self.hint);
    }

    fn paint_region(&self, p: &mut Painter<'_>) {
        if let Some(sel) = self.selection().or_else(|| self.target()) {
            self.paint_box(p, sel);
        }
        if self.anchor.is_none() {
            self.paint_hint(p);
        }
    }

    /// The selection (or the area it would take): bright inside, framed,
    /// with its size.
    fn paint_box(&self, p: &mut Painter<'_>, sel: R) {
        let (sw, sh) = (sel.2 - sel.0, sel.3 - sel.1);
        if sw > 0 && sh > 0 {
            p.copy(&self.bright, sel);
        }
        p.frame(inflate(sel, 1), rgb(255, 255, 255));
        let lr = self.size_label_rect(sel, self.label_width(sel));
        self.pill(p, lr, &size_label(sel));
    }

    fn paint_pick(&self, p: &mut Painter<'_>) {
        self.paint_hint(p);
        let cells = LOUPE_CELLS[self.zoom];
        let lr = self.loupe();
        let size = lr.2 - lr.0;
        let cell = (size / cells).max(1);
        let half = cells / 2;
        let (cx, cy) = self.cursor;
        let (w, h) = self.size;
        // Magnified pixels.
        for j in 0..cells {
            for i in 0..cells {
                let sx = (cx - half + i).clamp(0, w - 1);
                let sy = (cy - half + j).clamp(0, h - 1);
                let color = self.bright[(sy * w + sx) as usize];
                let x0 = lr.0 + i * cell;
                let y0 = lr.1 + j * cell;
                p.fill((x0, y0, x0 + cell, y0 + cell), color);
            }
        }
        // Outline: dark outside, white inside, readable on any color.
        let frame_box = (lr.0, lr.1, lr.2, lr.1 + size);
        p.frame(inflate(frame_box, 2), rgb(20, 20, 24));
        p.frame(inflate(frame_box, 1), rgb(255, 255, 255));
        // The picked cell.
        let x = lr.0 + half * cell;
        let y = lr.1 + half * cell;
        p.frame((x - 1, y - 1, x + cell + 1, y + cell + 1), rgb(0, 0, 0));
        p.frame((x, y, x + cell, y + cell), rgb(255, 255, 255));
        // Label: swatch and hex.
        let [r, g, b] = self.picked();
        let text = format!("#{r:02X}{g:02X}{b:02X}");
        let label = (lr.0, lr.1 + size + self.px(6.0), lr.2, lr.3);
        self.pill(p, label, "");
        let sw = self.px(12.0);
        let mid = (label.1 + label.3) / 2;
        let swatch = (
            label.0 + self.px(12.0),
            mid - sw / 2,
            label.0 + self.px(12.0) + sw,
            mid + sw / 2,
        );
        p.fill(swatch, rgb(r, g, b));
        p.frame(swatch, rgb(255, 255, 255));
        if let Some(line) = self.line(&text) {
            p.text_centered(
                (swatch.2, label.1, label.2, label.3),
                &line,
                rgb(255, 255, 255),
            );
        }
    }

    /// Screenshot mode, before dragging: the window under the cursor
    /// (clipped to the screen), or the cursor's monitor.
    fn target(&self) -> Option<R> {
        if self.mode != SnipMode::Screenshot || self.anchor.is_some() {
            return None;
        }
        let (w, h) = self.size;
        let hit = self
            .windows
            .iter()
            .find(|r| contains(**r, self.cursor))
            .and_then(|r| intersect(*r, (0, 0, w, h)))
            .filter(|r| r.2 - r.0 >= MIN_SELECTION && r.3 - r.1 >= MIN_SELECTION);
        Some(hit.unwrap_or(self.current().rect))
    }

    fn picked(&self) -> [u8; 3] {
        let (w, h) = self.size;
        let x = self.cursor.0.clamp(0, w - 1) as usize;
        let y = self.cursor.1.clamp(0, h - 1) as usize;
        let p = self.bright[y * w as usize + x];
        [(p >> 16) as u8, (p >> 8) as u8, p as u8]
    }

    fn finish(&mut self, outcome: SnipOutcome) {
        self.outcome = Some(outcome);
        self.done = true;
    }

    fn moved(&mut self, p: (i32, i32)) {
        self.cursor = p;
        self.screen = self.screen_at(p);
        if self.anchor.is_some() {
            self.corner = p;
        }
        self.refresh();
    }

    fn down(&mut self, p: (i32, i32)) {
        match self.mode {
            SnipMode::Region | SnipMode::Screenshot => {
                self.anchor = Some(p);
                self.corner = p;
                self.refresh();
            }
            SnipMode::Pick => self.pick(),
        }
    }

    fn up(&mut self, p: (i32, i32), shift: bool) {
        let Some(a) = self.anchor else {
            return;
        };
        self.corner = p;
        let sel = span(a, p);
        if sel.2 - sel.0 < MIN_SELECTION || sel.3 - sel.1 < MIN_SELECTION {
            // A click, not a drag: take the highlighted area, or start over.
            self.anchor = None;
            match self.target() {
                Some(t) => self.finish_rect(t, shift),
                None => self.refresh(),
            }
            return;
        }
        self.finish_rect(sel, shift);
    }

    /// Finishes with the part of the frozen frame inside `sel` (overlay
    /// coordinates).
    fn finish_rect(&mut self, sel: R, single_line: bool) {
        let (ox, oy) = self.origin;
        let crop = capture::crop_pixels(
            &self.bright,
            self.size.0 as u32,
            self.origin,
            monitor::Rect {
                left: ox + sel.0,
                top: oy + sel.1,
                right: ox + sel.2,
                bottom: oy + sel.3,
            },
        );
        match crop {
            Some(frame) => self.finish(SnipOutcome::Region { frame, single_line }),
            None => self.finish(SnipOutcome::Cancelled),
        }
    }

    fn pick(&mut self) {
        let (ox, oy) = self.origin;
        let at = (ox + self.cursor.0, oy + self.cursor.1);
        let rgb = self.picked();
        self.finish(SnipOutcome::Color { rgb, at });
    }

    fn wheel(&mut self, up: bool) {
        if self.mode != SnipMode::Pick {
            return;
        }
        self.zoom = if up {
            (self.zoom + 1).min(LOUPE_CELLS.len() - 1)
        } else {
            self.zoom.saturating_sub(1)
        };
        self.refresh();
    }

    fn key(&mut self, sym: u32, shift: bool) {
        if sym == keysym::ESCAPE {
            self.finish(SnipOutcome::Cancelled);
            return;
        }
        let enter = sym == keysym::RETURN || sym == keysym::KP_ENTER;
        if self.mode == SnipMode::Screenshot && self.anchor.is_none() {
            let (w, h) = self.size;
            match sym {
                keysym::F => {
                    let r = self.current().rect;
                    self.finish_rect(r, shift);
                }
                keysym::A => self.finish_rect((0, 0, w, h), shift),
                _ if enter => {
                    if let Some(t) = self.target() {
                        self.finish_rect(t, shift);
                    }
                }
                _ => {}
            }
            return;
        }
        if self.mode != SnipMode::Pick {
            return;
        }
        if enter {
            self.pick();
            return;
        }
        let step = if shift { 10 } else { 1 };
        let (dx, dy) = match sym {
            keysym::LEFT => (-step, 0),
            keysym::RIGHT => (step, 0),
            keysym::UP => (0, -step),
            keysym::DOWN => (0, step),
            _ => return,
        };
        let (w, h) = self.size;
        let x = (self.cursor.0 + dx).clamp(0, w - 1);
        let y = (self.cursor.1 + dy).clamp(0, h - 1);
        // Move the real pointer too where the server allows it.
        let _ = self.x.conn.warp_pointer(
            x11rb::NONE,
            self.surface.window,
            0,
            0,
            0,
            0,
            x as i16,
            y as i16,
        );
        self.x.flush();
        self.moved((x, y));
    }

    fn run(&mut self) {
        while !self.done {
            let ev = match self.x.conn.wait_for_event() {
                Ok(ev) => ev,
                Err(e) => {
                    tracing::warn!("snip overlay: {e}");
                    return;
                }
            };
            match ev {
                Event::Expose(e) => {
                    let (x0, y0) = (i32::from(e.x), i32::from(e.y));
                    self.surface.present(
                        &self.x,
                        (x0, y0, x0 + i32::from(e.width), y0 + i32::from(e.height)),
                    );
                }
                Event::MotionNotify(m) => {
                    // Only the latest position matters.
                    let mut last = (i32::from(m.event_x), i32::from(m.event_y));
                    while let Ok(Some(Event::MotionNotify(n))) = self.x.conn.poll_for_event() {
                        last = (i32::from(n.event_x), i32::from(n.event_y));
                    }
                    self.moved(last);
                }
                Event::ButtonPress(b) => match b.detail {
                    1 => self.down((i32::from(b.event_x), i32::from(b.event_y))),
                    3 => self.finish(SnipOutcome::Cancelled),
                    4 => self.wheel(true),
                    5 => self.wheel(false),
                    _ => {}
                },
                Event::ButtonRelease(b) if b.detail == 1 => {
                    let shift = b.state.contains(KeyButMask::SHIFT);
                    self.up((i32::from(b.event_x), i32::from(b.event_y)), shift);
                }
                Event::KeyPress(k) => {
                    let sym = self.keys.as_ref().map_or(0, |km| km.keysym(k.detail));
                    self.key(sym, k.state.contains(KeyButMask::SHIFT));
                }
                Event::FocusOut(_) => {
                    // Keep the keyboard: the window manager may have focused
                    // the overlay only after mapping it.
                    self.grab_keyboard();
                }
                Event::ClientMessage(m) => {
                    let d = m.data.as_data32();
                    let close = (m.type_ == self.x.atoms.WM_PROTOCOLS
                        && d[0] == self.x.atoms.WM_DELETE_WINDOW)
                        || (m.type_ == self.x.atoms.NOOK_SELECTION && d[0] == CMD_CLOSE);
                    if close {
                        self.finish(SnipOutcome::Cancelled);
                    }
                }
                Event::DestroyNotify(_) => return,
                _ => {}
            }
        }
        let _ = self.x.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    }
}

/// Visible X11 top-level windows of other processes, front to back, as
/// their frames (without client-side shadows), in overlay coordinates.
fn visible_windows(x: &X, origin: (i32, i32)) -> Vec<R> {
    let own = std::process::id();
    let mut out = Vec::new();
    for w in x.clients().into_iter().rev() {
        if x.pid_of(w) == Some(own) || !x.is_viewable(w) {
            continue;
        }
        let state = x.prop32(w, x.atoms._NET_WM_STATE);
        if state.contains(&x.atoms._NET_WM_STATE_HIDDEN) {
            continue;
        }
        let types = x.prop32(w, x.atoms._NET_WM_WINDOW_TYPE);
        if types.contains(&x.atoms._NET_WM_WINDOW_TYPE_DESKTOP)
            || types.contains(&x.atoms._NET_WM_WINDOW_TYPE_DOCK)
        {
            continue;
        }
        let Ok((wx, wy, ww, wh)) = x.rect_of(w) else {
            continue;
        };
        // GTK draws its shadow inside the window; take it off.
        let gtk = x.prop32(w, x.atoms._GTK_FRAME_EXTENTS);
        let [l, r, t, b] = match gtk[..] {
            [l, r, t, b] => [l as i32, r as i32, t as i32, b as i32],
            _ => [0; 4],
        };
        // Server-side decorations: the frame around the client.
        let frame = x.prop32(w, x.atoms._NET_FRAME_EXTENTS);
        let [fl, fr, ft, fb] = match frame[..] {
            [l, r, t, b] => [l as i32, r as i32, t as i32, b as i32],
            _ => [0; 4],
        };
        let rect = (
            wx + l - fl - origin.0,
            wy + t - ft - origin.1,
            wx + ww - r + fr - origin.0,
            wy + wh - b + fb - origin.1,
        );
        if rect.2 - rect.0 < MIN_SELECTION || rect.3 - rect.1 < MIN_SELECTION {
            continue;
        }
        out.push(rect);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_any_drag_direction() {
        assert_eq!(span((10, 20), (5, 30)), (5, 20, 10, 30));
        assert_eq!(span((5, 30), (10, 20)), (5, 20, 10, 30));
    }

    #[test]
    fn dims_each_channel() {
        assert_eq!(dimmed(0xFFFF_FFFF), 0xFF77_7777);
        assert_eq!(dimmed(0xFF00_0000), 0xFF00_0000);
    }

    #[test]
    fn loupe_stays_on_screen() {
        let screen = (0, 0, 1920, 1080);
        assert_eq!(
            loupe_rect((100, 100), 150, 30, 20, screen),
            (120, 120, 270, 300)
        );
        let r = loupe_rect((1900, 1060), 150, 30, 20, screen);
        assert!(r.2 <= 1920 && r.3 <= 1080);
    }

    #[test]
    fn painter_clips() {
        let mut px = vec![0u32; 16];
        let mut p = Painter {
            px: &mut px,
            w: 4,
            clip: (1, 1, 3, 3),
        };
        p.fill((0, 0, 4, 4), 0xFFFF_FFFF);
        assert_eq!(px.iter().filter(|&&v| v != 0).count(), 4);
        assert_eq!(px[0], 0);
        assert_eq!(px[5], 0xFFFF_FFFF);
    }
}
