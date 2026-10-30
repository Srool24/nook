//! Window styling, placement, click-through and z-order on X11 (XWayland
//! on a GNOME Wayland session).
//!
//! The overlay is a borderless X11 window kept above others with
//! `_NET_WM_STATE_ABOVE`, out of the taskbar and the window switcher with
//! `_NET_WM_STATE_SKIP_TASKBAR`/`SKIP_PAGER`. Clicks outside the drawn shape
//! fall through because the window's SHAPE *input* region is set to the
//! shape ([`set_input_shape`], fed by `hittest`); the X server then routes
//! pointer events outside it to whatever is below, with no polling thread.
//! Taking focus is controlled with the ICCCM input hint.

use crate::hittest::HitShape;
use crate::sys::x11::{Ctx, x};
use crate::{PlatformError, Result};
use std::collections::HashMap;
use std::sync::Mutex;
use x11rb::connection::Connection as _;
use x11rb::properties::WmHints;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{
    AtomEnum, ClipOrdering, ConfigureWindowAux, ConnectionExt as _, Rectangle,
};

/// A window handle that can cross threads: the X11 window id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hwnd(isize);

impl Hwnd {
    pub fn from_raw(raw: isize) -> Self {
        Self(raw)
    }

    pub fn raw(self) -> isize {
        self.0
    }

    pub(crate) fn xid(self) -> u32 {
        self.0 as u32
    }
}

/// What has been applied to each of our windows, so per-frame calls only
/// talk to the X server when something changes.
#[derive(Default)]
struct State {
    styled: bool,
    input_hint: Option<bool>,
    on_top: Option<bool>,
    rect: Option<(i32, i32, i32, i32)>,
    opacity: Option<u8>,
    click_through: bool,
    /// Input region in window pixels (`None`: the whole window).
    input: Option<Vec<Rectangle>>,
    /// What the X server has (`Rectangle` has no `PartialEq`).
    applied_input: Option<Option<RectKey>>,
}

/// Rectangles as comparable tuples: (x, y, width, height).
type RectKey = Vec<(i16, i16, u16, u16)>;

fn rect_key(rects: &Option<Vec<Rectangle>>) -> Option<RectKey> {
    rects
        .as_ref()
        .map(|v| v.iter().map(|r| (r.x, r.y, r.width, r.height)).collect())
}

static STATE: Mutex<Option<HashMap<u32, State>>> = Mutex::new(None);
/// The overlay window (`ensure_overlay`), whose input region the hit
/// tester shapes.
static MAIN: Mutex<Option<u32>> = Mutex::new(None);

fn with_state<R>(hwnd: Hwnd, f: impl FnOnce(&mut State) -> R) -> R {
    let mut map = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(map
        .get_or_insert_with(HashMap::new)
        .entry(hwnd.xid())
        .or_default())
}

/// The client area already covers the whole window on X11.
pub fn fill_client(_hwnd: Hwnd) -> Result<()> {
    Ok(())
}

fn skip_taskbar(hwnd: Hwnd) -> Result<()> {
    let x = x()?;
    x.wm_state(
        hwnd.xid(),
        true,
        x.atoms._NET_WM_STATE_SKIP_TASKBAR,
        x.atoms._NET_WM_STATE_SKIP_PAGER,
    );
    x.flush();
    Ok(())
}

/// Frameless, no-taskbar window that may take focus (full-screen overlays
/// such as draw-on-screen). Cheap to call every frame.
pub fn ensure_popup(hwnd: Hwnd) -> Result<bool> {
    if with_state(hwnd, |s| std::mem::replace(&mut s.styled, true)) {
        return Ok(false);
    }
    skip_taskbar(hwnd)?;
    set_input_hint(hwnd, true)?;
    Ok(true)
}

/// Per-pixel alpha comes from the 32-bit visual winit picks for a
/// transparent window; the compositor blends it.
pub fn enable_alpha(_hwnd: Hwnd) -> Result<()> {
    Ok(())
}

/// Makes `hwnd` transient for `owner`, which keeps it above its owner (the
/// draw toolbar above the ink canvas).
pub fn set_owner(hwnd: Hwnd, owner: Hwnd) {
    if let Ok(x) = x() {
        x.set_prop32(
            hwnd.xid(),
            AtomEnum::WM_TRANSIENT_FOR.into(),
            AtomEnum::WINDOW,
            &[owner.xid()],
        );
        x.flush();
    }
}

/// Window corners are the compositor's business on Linux.
pub fn round_corners(_hwnd: Hwnd) {}

/// A top-level window of this process with exactly this title (eframe does
/// not hand out the handles of secondary viewports).
pub fn find_own(title: &str) -> Option<Hwnd> {
    let x = x().ok()?;
    let pid = std::process::id();
    let mut candidates = x.clients();
    if let Ok(tree) = x
        .conn
        .query_tree(x.root)
        .ctx("QueryTree")
        .and_then(|c| c.reply().ctx("QueryTree"))
    {
        candidates.extend(tree.children);
    }
    candidates
        .into_iter()
        .find(|&w| x.pid_of(w) == Some(pid) && x.title_of(w).as_deref() == Some(title))
        .map(|w| Hwnd::from_raw(w as isize))
}

fn set_input_hint(hwnd: Hwnd, input: bool) -> Result<bool> {
    if with_state(hwnd, |s| s.input_hint) == Some(input) {
        return Ok(false);
    }
    let x = x()?;
    let mut hints = WmHints::get(&x.conn, hwnd.xid())
        .ctx("GetProperty(WM_HINTS)")?
        .reply()
        .ok()
        .flatten()
        .unwrap_or_default();
    hints.input = Some(input);
    hints
        .set(&x.conn, hwnd.xid())
        .ctx("ChangeProperty(WM_HINTS)")?;
    x.flush();
    with_state(hwnd, |s| s.input_hint = Some(input));
    Ok(true)
}

/// Makes the window a frameless, no-taskbar overlay that does not take
/// focus unless `activatable` (text input needs focus). Call it every
/// frame: it only talks to the X server when something changed. Returns
/// whether it changed anything.
pub fn ensure_overlay(hwnd: Hwnd, activatable: bool) -> Result<bool> {
    *MAIN.lock().unwrap_or_else(|e| e.into_inner()) = Some(hwnd.xid());
    let first = !with_state(hwnd, |s| std::mem::replace(&mut s.styled, true));
    if first {
        skip_taskbar(hwnd)?;
    }
    let hint = set_input_hint(hwnd, activatable)?;
    Ok(first || hint)
}

/// Shell drags have no modal loop that could swallow the release on X11.
pub fn post_left_button_up(_hwnd: Hwnd) {}

/// Brings the window to the front with keyboard focus. Sent as a pager
/// request, which GNOME honours right after the click that asked for it.
pub fn activate(hwnd: Hwnd) {
    if let Ok(x) = x() {
        let _ = set_input_hint(hwnd, true);
        x.client_message(hwnd.xid(), x.atoms._NET_ACTIVE_WINDOW, [2, 0, 0, 0, 0]);
        x.flush();
    }
}

/// Whole-window opacity (0 hides it, 255 shows the per-pixel alpha content
/// as is), through `_NET_WM_WINDOW_OPACITY`, which Mutter honours.
pub fn set_opacity(hwnd: Hwnd, alpha: u8) -> Result<()> {
    if with_state(hwnd, |s| s.opacity) == Some(alpha) {
        return Ok(());
    }
    let x = x()?;
    let prop = x.atoms._NET_WM_WINDOW_OPACITY;
    if alpha == 255 {
        x.delete_prop(hwnd.xid(), prop);
    } else {
        x.set_prop32(
            hwnd.xid(),
            prop,
            AtomEnum::CARDINAL,
            &[u32::from(alpha) * 0x0101_0101],
        );
    }
    x.flush();
    with_state(hwnd, |s| s.opacity = Some(alpha));
    Ok(())
}

/// Shows the window, if it is hidden.
pub fn ensure_shown(hwnd: Hwnd) {
    if let Ok(x) = x()
        && !x.is_viewable(hwnd.xid())
    {
        let _ = x.conn.map_window(hwnd.xid());
        x.flush();
    }
}

/// Lets mouse input fall through the whole window (an empty input region)
/// or restores the input region the hit tester last set.
pub fn set_click_through(hwnd: Hwnd, through: bool) -> Result<()> {
    with_state(hwnd, |s| s.click_through = through);
    apply_input(hwnd)
}

/// Sets the overlay's input region to `shape` (screen pixels): clicks
/// outside it reach the windows below.
pub fn set_input_shape(shape: &HitShape) {
    let Some(main) = *MAIN.lock().unwrap_or_else(|e| e.into_inner()) else {
        return;
    };
    let hwnd = Hwnd::from_raw(main as isize);
    let origin = with_state(hwnd, |s| s.rect)
        .map(|(x, y, _, _)| (x, y))
        .or_else(|| window_rect(hwnd).ok().map(|(x, y, _, _)| (x, y)))
        .unwrap_or_default();
    let rects = rasterize(shape, origin);
    with_state(hwnd, |s| s.input = Some(rects));
    if let Err(e) = apply_input(hwnd) {
        tracing::warn!("input region: {e}");
    }
}

fn apply_input(hwnd: Hwnd) -> Result<()> {
    let want = with_state(hwnd, |s| {
        if s.click_through {
            Some(Vec::new())
        } else {
            s.input.clone()
        }
    });
    let key = rect_key(&want);
    if with_state(hwnd, |s| s.applied_input.as_ref() == Some(&key)) {
        return Ok(());
    }
    let x = x()?;
    match &want {
        Some(rects) => x
            .conn
            .shape_rectangles(
                SO::SET,
                SK::INPUT,
                ClipOrdering::UNSORTED,
                hwnd.xid(),
                0,
                0,
                rects,
            )
            .ctx("ShapeRectangles")?,
        // No region set yet: the whole window takes input.
        None => x
            .conn
            .shape_mask(SO::SET, SK::INPUT, hwnd.xid(), 0, 0, x11rb::NONE)
            .ctx("ShapeMask")?,
    };
    x.flush();
    with_state(hwnd, |s| s.applied_input = Some(key));
    Ok(())
}

/// Turns polygons (screen pixels) into the X rectangles of the pixels whose
/// centers lie inside, relative to `origin`. Rows with the same spans are
/// merged into one band, so a rounded shape takes a few dozen rectangles.
pub fn rasterize(shape: &HitShape, origin: (i32, i32)) -> Vec<Rectangle> {
    type Span = (i32, i32);
    let (mut y0, mut y1) = (i32::MAX, i32::MIN);
    for &(_, py) in shape.polygons.iter().flatten() {
        y0 = y0.min(py.floor() as i32);
        y1 = y1.max(py.ceil() as i32);
    }
    if y0 >= y1 {
        return Vec::new();
    }
    let spans_at = |y: f32| -> Vec<Span> {
        let mut spans: Vec<Span> = Vec::new();
        for poly in &shape.polygons {
            let n = poly.len();
            if n < 3 {
                continue;
            }
            let mut xs: Vec<f32> = Vec::new();
            for i in 0..n {
                let (ax, ay) = poly[i];
                let (bx, by) = poly[(i + 1) % n];
                if (ay > y) != (by > y) {
                    xs.push(ax + (y - ay) * (bx - ax) / (by - ay));
                }
            }
            xs.sort_by(f32::total_cmp);
            for pair in xs.as_chunks::<2>().0 {
                // Pixels whose centers fall between the crossings.
                let l = (pair[0] - 0.5).ceil() as i32;
                let r = (pair[1] - 0.5).floor() as i32 + 1;
                if r > l {
                    spans.push((l, r));
                }
            }
        }
        spans.sort_unstable();
        let mut merged: Vec<Span> = Vec::new();
        for (l, r) in spans {
            match merged.last_mut() {
                Some(last) if l <= last.1 => last.1 = last.1.max(r),
                _ => merged.push((l, r)),
            }
        }
        merged
    };
    let mut out = Vec::new();
    let mut band: Option<(i32, Vec<Span>)> = None;
    let flush = |out: &mut Vec<Rectangle>, start: i32, end: i32, spans: &[Span]| {
        for &(l, r) in spans {
            let (rx, ry) = (l - origin.0, start - origin.1);
            out.push(Rectangle {
                x: rx.clamp(i16::MIN.into(), i16::MAX.into()) as i16,
                y: ry.clamp(i16::MIN.into(), i16::MAX.into()) as i16,
                width: (r - l).clamp(0, u16::MAX.into()) as u16,
                height: (end - start).clamp(0, u16::MAX.into()) as u16,
            });
        }
    };
    for y in y0..y1 {
        let spans = spans_at(y as f32 + 0.5);
        match &band {
            Some((_, cur)) if *cur == spans => {}
            Some((start, cur)) => {
                flush(&mut out, *start, y, cur);
                band = Some((y, spans));
            }
            None => band = Some((y, spans)),
        }
    }
    if let Some((start, cur)) = band {
        flush(&mut out, start, y1, &cur);
    }
    out
}

/// Whether another window covers the notch. An X11 client cannot see
/// Wayland windows, and Mutter keeps the stacking itself, so: no.
pub fn is_covered(_hwnd: Hwnd) -> bool {
    false
}

pub fn is_topmost(hwnd: Hwnd) -> bool {
    if let Some(on_top) = with_state(hwnd, |s| s.on_top) {
        return on_top;
    }
    x().is_ok_and(|x| {
        x.prop32(hwnd.xid(), x.atoms._NET_WM_STATE)
            .contains(&x.atoms._NET_WM_STATE_ABOVE)
    })
}

/// Puts the overlay above all windows (`on_top`) or behind all normal
/// windows. `owner` (the Windows backdrop window) does not exist here.
pub fn set_layer(main: Hwnd, _owner: Option<Hwnd>, on_top: bool) -> Result<()> {
    let x = x()?;
    x.wm_state(main.xid(), on_top, x.atoms._NET_WM_STATE_ABOVE, 0);
    x.wm_state(main.xid(), !on_top, x.atoms._NET_WM_STATE_BELOW, 0);
    x.flush();
    with_state(main, |s| s.on_top = Some(on_top));
    Ok(())
}

/// Window rectangle in screen pixels: (left, top, width, height).
pub fn window_rect(hwnd: Hwnd) -> Result<(i32, i32, i32, i32)> {
    x()?.rect_of(hwnd.xid())
}

/// Moves and resizes the window without touching the stacking order.
pub fn set_rect(hwnd: Hwnd, x: i32, y: i32, w: i32, h: i32) -> Result<()> {
    if w <= 0 || h <= 0 {
        return Err(PlatformError::Other("empty window rectangle".into()));
    }
    let xc = crate::sys::x11::x()?;
    // winit pins a non-resizable window's minimum and maximum size to its
    // first size, and Mutter holds the window to them: Nook stayed in the
    // top-left corner at that size. Pin them to the new size instead.
    use x11rb::properties::{WmSizeHints, WmSizeHintsSpecification};
    let hints = WmSizeHints {
        position: Some((WmSizeHintsSpecification::ProgramSpecified, x, y)),
        size: Some((WmSizeHintsSpecification::ProgramSpecified, w, h)),
        min_size: Some((w, h)),
        max_size: Some((w, h)),
        ..WmSizeHints::default()
    };
    hints
        .set_normal_hints(&xc.conn, hwnd.xid())
        .ctx("WM_NORMAL_HINTS")?;
    let aux = ConfigureWindowAux::new()
        .x(x)
        .y(y)
        .width(w as u32)
        .height(h as u32);
    xc.conn
        .configure_window(hwnd.xid(), &aux)
        .ctx("ConfigureWindow")?;
    let _ = xc.conn.flush();
    with_state(hwnd, |s| s.rect = Some((x, y, w, h)));
    Ok(())
}

/// The display's DPI (`Xft.dpi`, set from GNOME's scale), 96 = 100%.
pub fn dpi(_hwnd: Hwnd) -> u32 {
    x().map_or(96, |x| x.xft_dpi())
}

/// Forgets what was applied to a window that is gone.
pub fn forget(hwnd: Hwnd) {
    let mut map = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = map.as_mut() {
        m.remove(&hwnd.xid());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterizes_a_square_into_one_rectangle() {
        let shape = HitShape {
            polygons: vec![vec![(10.0, 20.0), (30.0, 20.0), (30.0, 25.0), (10.0, 25.0)]],
        };
        let rects = rasterize(&shape, (10, 20));
        assert_eq!(rects.len(), 1);
        let r = rects[0];
        assert_eq!((r.x, r.y, r.width, r.height), (0, 0, 20, 5));
    }

    #[test]
    fn rasterizes_a_triangle_into_bands() {
        let shape = HitShape {
            polygons: vec![vec![(0.0, 0.0), (8.0, 8.0), (0.0, 8.0)]],
        };
        let rects = rasterize(&shape, (0, 0));
        assert_eq!(rects.len(), 8);
        assert!(rects.windows(2).all(|w| w[0].width < w[1].width));
        assert!(rasterize(&HitShape::default(), (0, 0)).is_empty());
    }
}
