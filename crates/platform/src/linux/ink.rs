//! The draw-on-screen canvas: a transparent, full-screen X11 window with a
//! 32-bit (ARGB) visual over every monitor. Strokes are rasterized
//! (anti-aliased capsules) into a premultiplied pixel buffer, and only the
//! pixels a pen move touched are sent to the window, which the compositor
//! blends over the desktop.
//!
//! The window lives on its own thread with its own X connection and event
//! loop. Esc closes it, Ctrl+Z undoes; the toolbar (an egui window made
//! transient for the canvas) changes the style through [`InkCanvas`].

use crate::sys::surface::{Keymap, Spec, Surface, keysym};
use crate::sys::x11::X;
use crate::{PlatformError, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt as _, EventMask, KeyButMask};

/// Untouched pixels: fully transparent.
const BACKGROUND: u32 = 0;
/// Eraser radius in pixels.
const ERASER: f32 = 12.0;

/// Commands sent to the canvas thread (`data[0]` of a client message).
const CMD_UNDO: u32 = 1;
const CMD_CLEAR: u32 = 2;
const CMD_CLOSE: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InkTool {
    /// Draws with `color` (unpremultiplied RGBA) and `width` (pixels).
    Pen {
        color: [u8; 4],
        width: f32,
    },
    Eraser,
}

struct Shared {
    tool: Mutex<InkTool>,
    open: AtomicBool,
}

/// Handle to an open canvas. Dropping it closes the canvas.
pub struct InkCanvas {
    window: u32,
    shared: Arc<Shared>,
}

impl InkCanvas {
    /// Opens a canvas over `bounds` (screen pixels: x, y, w, h).
    /// `on_close` runs on the canvas thread when it closes (Esc or drop).
    pub fn open(
        bounds: (i32, i32, i32, i32),
        tool: InkTool,
        on_close: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            tool: Mutex::new(tool),
            open: AtomicBool::new(true),
        });
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("nook-ink".into())
            .spawn(move || {
                let made = crate::sys::x11::open().and_then(|x| {
                    let s = Surface::create(
                        &x,
                        &Spec {
                            bounds,
                            title: "Nook Ink",
                            argb: true,
                            cursor_glyph: 34,
                        },
                    )?;
                    Ok((x, s))
                });
                let (x, surface) = match made {
                    Ok(v) => v,
                    Err(e) => {
                        thread_shared.open.store(false, Ordering::Relaxed);
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let mut canvas = Canvas::new(&x, surface, thread_shared.clone());
                canvas.surface.show(&x);
                canvas.surface.present_all(&x);
                let _ = ready_tx.send(Ok(canvas.surface.window));
                canvas.run();
                canvas.surface.destroy(&x);
                thread_shared.open.store(false, Ordering::Relaxed);
                on_close();
            })
            .map_err(|e| PlatformError::other("spawning ink thread", e))?;
        let window = ready_rx
            .recv()
            .map_err(|_| PlatformError::Other("ink thread exited".into()))??;
        Ok(Self { window, shared })
    }

    pub fn hwnd(&self) -> crate::Hwnd {
        crate::Hwnd::from_raw(self.window as isize)
    }

    pub fn is_open(&self) -> bool {
        self.shared.open.load(Ordering::Relaxed)
    }

    pub fn set_tool(&self, tool: InkTool) {
        *self.shared.tool.lock().unwrap_or_else(|p| p.into_inner()) = tool;
    }

    pub fn undo(&self) {
        self.post(CMD_UNDO);
    }

    pub fn clear(&self) {
        self.post(CMD_CLEAR);
    }

    /// Brings the canvas to the front with the keyboard (so Esc reaches it).
    pub fn focus(&self) {
        if let Ok(x) = crate::sys::x11::x() {
            x.client_message(self.window, x.atoms._NET_ACTIVE_WINDOW, [2, 0, 0, 0, 0]);
            x.flush();
        }
    }

    fn post(&self, cmd: u32) {
        if let Ok(x) = crate::sys::x11::x() {
            let ev =
                ClientMessageEvent::new(32, self.window, x.atoms.NOOK_SELECTION, [cmd, 0, 0, 0, 0]);
            let _ = x
                .conn
                .send_event(false, self.window, EventMask::NO_EVENT, ev);
            x.flush();
        }
    }
}

impl Drop for InkCanvas {
    fn drop(&mut self) {
        self.post(CMD_CLOSE);
    }
}

struct Stroke {
    points: Vec<(f32, f32)>,
    /// Premultiplied `0xAARRGGBB`.
    color: u32,
    width: f32,
}

impl Stroke {
    fn hits(&self, p: (f32, f32), radius: f32) -> bool {
        let r = radius + self.width * 0.5;
        if self.points.len() == 1 {
            return dist(p, self.points[0]) <= r;
        }
        self.points
            .windows(2)
            .any(|w| segment_distance(p, w[0], w[1]) <= r)
    }
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let len2 = abx * abx + aby * aby;
    if len2 <= f32::EPSILON {
        return dist(p, a);
    }
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0);
    dist(p, (a.0 + abx * t, a.1 + aby * t))
}

/// Unpremultiplied RGBA -> premultiplied `0xAARRGGBB`.
fn premultiply([r, g, b, a]: [u8; 4]) -> u32 {
    let m = |c: u8| (u32::from(c) * u32::from(a) + 127) / 255;
    (u32::from(a) << 24) | (m(r) << 16) | (m(g) << 8) | m(b)
}

/// `src` scaled by `cov` (0..=255), premultiplied, composited over `dst`.
fn over(dst: u32, src: u32, cov: u8) -> u32 {
    let k = u32::from(cov);
    let s = |shift: u32| ((src >> shift) & 0xFF) * k / 255;
    let sa = s(24);
    let inv = 255 - sa;
    let d = |shift: u32| (dst >> shift) & 0xFF;
    let ch = |shift: u32| (s(shift) + d(shift) * inv / 255).min(255);
    (ch(24) << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// The stroke in progress: the canvas before it began and its coverage.
struct Scratch {
    base: Vec<u32>,
    cov: Vec<u8>,
}

/// Adds one capsule from `a` to `b` to the current stroke. Returns the
/// rectangle it touched.
fn segment(
    pixels: &mut [u32],
    scratch: &mut Scratch,
    (w, h): (i32, i32),
    a: (f32, f32),
    b: (f32, f32),
    width: f32,
    color: u32,
) -> (i32, i32, i32, i32) {
    let r = width * 0.5;
    let pad = r + 1.0;
    let x0 = ((a.0.min(b.0) - pad).floor() as i32).clamp(0, w);
    let y0 = ((a.1.min(b.1) - pad).floor() as i32).clamp(0, h);
    let x1 = ((a.0.max(b.0) + pad).ceil() as i32).clamp(0, w);
    let y1 = ((a.1.max(b.1) + pad).ceil() as i32).clamp(0, h);
    for y in y0..y1 {
        for x in x0..x1 {
            let d = segment_distance((x as f32 + 0.5, y as f32 + 0.5), a, b);
            let c = ((r + 0.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
            if c == 0 {
                continue;
            }
            let i = (y * w + x) as usize;
            if c > scratch.cov[i] {
                scratch.cov[i] = c;
                pixels[i] = over(scratch.base[i], color, c);
            }
        }
    }
    (x0, y0, x1, y1)
}

struct Canvas<'a> {
    x: &'a X,
    surface: Surface,
    scratch: Scratch,
    keys: Option<Keymap>,
    strokes: Vec<Stroke>,
    current: Option<Stroke>,
    erasing: bool,
    shared: Arc<Shared>,
}

impl<'a> Canvas<'a> {
    fn new(x: &'a X, surface: Surface, shared: Arc<Shared>) -> Self {
        let n = surface.pixels.len();
        Self {
            x,
            keys: Keymap::load(x),
            surface,
            scratch: Scratch {
                base: vec![BACKGROUND; n],
                cov: vec![0; n],
            },
            strokes: Vec::new(),
            current: None,
            erasing: false,
            shared,
        }
    }

    fn tool(&self) -> InkTool {
        *self.shared.tool.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn begin_stroke(&mut self) {
        self.scratch.base.copy_from_slice(&self.surface.pixels);
        self.scratch.cov.iter_mut().for_each(|c| *c = 0);
    }

    fn draw_segment(
        &mut self,
        a: (f32, f32),
        b: (f32, f32),
        width: f32,
        color: u32,
    ) -> (i32, i32, i32, i32) {
        let size = (self.surface.w, self.surface.h);
        segment(
            &mut self.surface.pixels,
            &mut self.scratch,
            size,
            a,
            b,
            width,
            color,
        )
    }

    /// Repaints every stroke from scratch (after undo, erase or clear).
    fn redraw(&mut self) {
        self.surface.pixels.fill(BACKGROUND);
        let strokes = std::mem::take(&mut self.strokes);
        for s in &strokes {
            self.begin_stroke();
            match s.points.as_slice() {
                [] => {}
                [p] => {
                    self.draw_segment(*p, *p, s.width, s.color);
                }
                pts => {
                    for w in pts.windows(2) {
                        self.draw_segment(w[0], w[1], s.width, s.color);
                    }
                }
            }
        }
        self.strokes = strokes;
        self.surface.present_all(self.x);
    }

    fn erase_at(&mut self, p: (f32, f32)) {
        let before = self.strokes.len();
        self.strokes.retain(|s| !s.hits(p, ERASER));
        if self.strokes.len() != before {
            self.redraw();
        }
    }

    fn down(&mut self, p: (f32, f32)) {
        match self.tool() {
            InkTool::Eraser => {
                self.erasing = true;
                self.erase_at(p);
            }
            InkTool::Pen { color, width } => {
                let color = premultiply(color);
                self.begin_stroke();
                let dirty = self.draw_segment(p, p, width, color);
                self.current = Some(Stroke {
                    points: vec![p],
                    color,
                    width,
                });
                self.surface.present(self.x, dirty);
            }
        }
    }

    fn moved(&mut self, p: (f32, f32)) {
        if self.erasing {
            self.erase_at(p);
            return;
        }
        let Some(s) = &mut self.current else {
            return;
        };
        let Some(&last) = s.points.last() else {
            return;
        };
        if dist(last, p) < 1.0 {
            return;
        }
        s.points.push(p);
        let (width, color) = (s.width, s.color);
        let dirty = self.draw_segment(last, p, width, color);
        self.surface.present(self.x, dirty);
    }

    fn up(&mut self) {
        self.erasing = false;
        if let Some(s) = self.current.take() {
            self.strokes.push(s);
        }
    }

    fn undo(&mut self) {
        self.strokes.pop();
        self.redraw();
    }

    /// Handles events until the canvas is closed.
    fn run(&mut self) {
        loop {
            let ev = match self.x.conn.wait_for_event() {
                Ok(ev) => ev,
                Err(e) => {
                    tracing::warn!("ink canvas: {e}");
                    return;
                }
            };
            match ev {
                Event::Expose(e) => {
                    let (x0, y0) = (i32::from(e.x), i32::from(e.y));
                    self.surface.present(
                        self.x,
                        (x0, y0, x0 + i32::from(e.width), y0 + i32::from(e.height)),
                    );
                }
                Event::ButtonPress(b) if b.detail == 1 => {
                    self.down((f32::from(b.event_x), f32::from(b.event_y)));
                }
                // Right click ends the stroke too; the eraser stays a tool.
                Event::ButtonRelease(b) if b.detail == 1 => self.up(),
                Event::MotionNotify(m) if m.state.contains(KeyButMask::BUTTON1) => {
                    self.moved((f32::from(m.event_x), f32::from(m.event_y)));
                }
                Event::KeyPress(k) => {
                    let sym = self.keys.as_ref().map_or(0, |km| km.keysym(k.detail));
                    if sym == keysym::ESCAPE {
                        return;
                    }
                    if sym == keysym::Z && k.state.contains(KeyButMask::CONTROL) {
                        self.undo();
                    }
                }
                Event::ClientMessage(m) => {
                    let d = m.data.as_data32();
                    if m.type_ == self.x.atoms.WM_PROTOCOLS && d[0] == self.x.atoms.WM_DELETE_WINDOW
                    {
                        return;
                    }
                    if m.type_ == self.x.atoms.NOOK_SELECTION {
                        match d[0] {
                            CMD_UNDO => self.undo(),
                            CMD_CLEAR => {
                                self.strokes.clear();
                                self.redraw();
                            }
                            CMD_CLOSE => return,
                            _ => {}
                        }
                    }
                }
                Event::DestroyNotify(_) => return,
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplies_and_composites() {
        assert_eq!(premultiply([255, 0, 0, 255]), 0xFFFF_0000);
        assert_eq!(premultiply([255, 255, 255, 0]), 0);
        assert_eq!(over(BACKGROUND, 0xFFFF_0000, 255), 0xFFFF_0000);
        let half = over(0xFF00_00FF, 0xFFFF_0000, 128);
        assert_eq!(half >> 24, 0xFF);
        assert!((half >> 16) & 0xFF > 100 && half & 0xFF > 100);
    }

    #[test]
    fn segment_coverage_is_antialiased() {
        let n = 20 * 20;
        let mut px = vec![BACKGROUND; n];
        let mut s = Scratch {
            base: vec![BACKGROUND; n],
            cov: vec![0; n],
        };
        segment(
            &mut px,
            &mut s,
            (20, 20),
            (2.0, 10.3),
            (18.0, 10.3),
            4.0,
            0xFFFF_FFFF,
        );
        assert_eq!(px[10 * 20 + 10], 0xFFFF_FFFF);
        let edge = px[12 * 20 + 10];
        assert!(edge >> 24 > 0 && edge >> 24 < 255, "edge {edge:08X}");
        assert_eq!(px[16 * 20 + 10], BACKGROUND);
    }

    #[test]
    fn eraser_hits_segments() {
        let s = Stroke {
            points: vec![(0.0, 0.0), (100.0, 0.0)],
            color: 0,
            width: 4.0,
        };
        assert!(s.hits((50.0, 8.0), 10.0));
        assert!(!s.hits((50.0, 30.0), 10.0));
    }
}
