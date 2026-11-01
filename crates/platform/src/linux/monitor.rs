//! Monitor lookup (X11 screen pixels).
//!
//! Monitors come from RandR (`GetMonitors`, which XWayland answers with
//! GNOME's logical monitors). Work areas come from `_GTK_WORKAREAS_D0`,
//! which Mutter keeps per monitor (the top bar takes the top of the primary
//! one), with `_NET_WORKAREA` as the fallback.

use crate::sys::x11::{Ctx, x};
use crate::window::Hwnd;
use crate::{PlatformError, Result};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::ConnectionExt as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    /// `self` fully covers `other`.
    pub fn covers(&self, other: &Rect) -> bool {
        self.left <= other.left
            && self.top <= other.top
            && self.right >= other.right
            && self.bottom >= other.bottom
    }

    fn contains(&self, (x, y): (i32, i32)) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    fn intersect(&self, o: &Rect) -> Option<Rect> {
        let r = Rect {
            left: self.left.max(o.left),
            top: self.top.max(o.top),
            right: self.right.min(o.right),
            bottom: self.bottom.min(o.bottom),
        };
        (r.right > r.left && r.bottom > r.top).then_some(r)
    }

    fn from_xywh(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Monitor {
    /// Opaque id, stable while the monitor stays connected.
    pub id: isize,
    pub bounds: Rect,
    pub work: Rect,
    /// Effective DPI (96 = 100%).
    pub dpi: u32,
}

/// Every connected monitor, primary first.
pub fn all() -> Vec<Monitor> {
    let Ok(x) = x() else {
        return Vec::new();
    };
    let dpi = x.xft_dpi();
    let mut list: Vec<(bool, Monitor)> = match x
        .conn
        .randr_get_monitors(x.root, true)
        .ctx("RRGetMonitors")
        .and_then(|c| c.reply().ctx("RRGetMonitors"))
    {
        Ok(reply) => reply
            .monitors
            .iter()
            .map(|m| {
                let bounds =
                    Rect::from_xywh(m.x.into(), m.y.into(), m.width.into(), m.height.into());
                (
                    m.primary,
                    Monitor {
                        id: m.name as isize,
                        bounds,
                        work: bounds,
                        dpi,
                    },
                )
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    if list.is_empty() {
        // No RandR: the whole screen is one monitor.
        let s = &x11rb::connection::Connection::setup(&x.conn).roots[x.screen];
        let bounds = Rect::from_xywh(0, 0, s.width_in_pixels.into(), s.height_in_pixels.into());
        list.push((
            true,
            Monitor {
                id: 0,
                bounds,
                work: bounds,
                dpi,
            },
        ));
    }
    if !list.iter().any(|(p, _)| *p) {
        // No primary flag: the one at the origin, as on Windows.
        if let Some(i) = list
            .iter()
            .position(|(_, m)| m.bounds.left == 0 && m.bounds.top == 0)
        {
            list[i].0 = true;
        }
    }
    let mut monitors: Vec<(bool, Monitor)> = list;
    apply_work_areas(x, &mut monitors);
    monitors.sort_by_key(|(primary, m)| (!primary, m.bounds.left, m.bounds.top));
    monitors.into_iter().map(|(_, m)| m).collect()
}

fn apply_work_areas(x: &crate::sys::x11::X, monitors: &mut [(bool, Monitor)]) {
    let per_monitor = x.prop32(x.root, x.atoms._GTK_WORKAREAS_D0);
    let areas: Vec<Rect> = per_monitor
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| Rect::from_xywh(c[0] as i32, c[1] as i32, c[2] as i32, c[3] as i32))
        .collect();
    if !areas.is_empty() {
        for (_, m) in monitors.iter_mut() {
            if let Some(a) = areas.iter().find(|a| {
                let c = ((a.left + a.right) / 2, (a.top + a.bottom) / 2);
                m.bounds.contains(c)
            }) && let Some(w) = a.intersect(&m.bounds)
            {
                m.work = w;
            }
        }
        return;
    }
    let desktop = x.prop32(x.root, x.atoms._NET_WORKAREA);
    if let &[wx, wy, ww, wh, ..] = desktop.as_slice() {
        let area = Rect::from_xywh(wx as i32, wy as i32, ww as i32, wh as i32);
        for (_, m) in monitors.iter_mut() {
            if let Some(w) = area.intersect(&m.bounds) {
                m.work = w;
            }
        }
    }
}

/// The rectangle around all monitors (the virtual screen).
pub fn virtual_bounds() -> Result<Rect> {
    union(all().iter().map(|m| m.bounds)).ok_or_else(|| PlatformError::Other("no monitors".into()))
}

fn union(rects: impl Iterator<Item = Rect>) -> Option<Rect> {
    rects.reduce(|a, b| Rect {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    })
}

/// The pointer position. Under XWayland it is current while the pointer is
/// over an X11 window (Nook's own included) and the last known position
/// otherwise.
pub fn cursor_pos() -> Result<(i32, i32)> {
    let x = x()?;
    let p = x
        .conn
        .query_pointer(x.root)
        .ctx("QueryPointer")?
        .reply()
        .ctx("QueryPointer")?;
    Ok((p.root_x.into(), p.root_y.into()))
}

pub fn primary() -> Result<Monitor> {
    all()
        .into_iter()
        .next()
        .ok_or_else(|| PlatformError::Other("no monitors".into()))
}

fn nearest(p: (i32, i32)) -> Result<Monitor> {
    let monitors = all();
    if let Some(m) = monitors.iter().find(|m| m.bounds.contains(p)) {
        return Ok(*m);
    }
    let dist = |m: &Monitor| {
        let dx = (m.bounds.left - p.0).max(0).max(p.0 - m.bounds.right + 1);
        let dy = (m.bounds.top - p.1).max(0).max(p.1 - m.bounds.bottom + 1);
        i64::from(dx).pow(2) + i64::from(dy).pow(2)
    };
    monitors
        .into_iter()
        .min_by_key(dist)
        .ok_or_else(|| PlatformError::Other("no monitors".into()))
}

pub fn under_cursor() -> Result<Monitor> {
    nearest(cursor_pos()?)
}

pub fn of_window(hwnd: Hwnd) -> Result<Monitor> {
    let (x, y, w, h) = crate::window::window_rect(hwnd)?;
    nearest((x + w / 2, y + h / 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unions_side_by_side_monitors() {
        let a = Rect::from_xywh(0, 0, 1920, 1080);
        let b = Rect {
            left: -1600,
            top: 200,
            right: 0,
            bottom: 1100,
        };
        let u = union([a, b].into_iter()).expect("two rects");
        assert_eq!((u.left, u.top, u.right, u.bottom), (-1600, 0, 1920, 1100));
        assert!(union(std::iter::empty()).is_none());
    }

    #[test]
    fn intersects_work_areas() {
        let screen = Rect::from_xywh(0, 0, 1920, 1080);
        let work = Rect::from_xywh(0, 32, 1920, 1048);
        assert_eq!(work.intersect(&screen), Some(work));
        assert_eq!(Rect::from_xywh(2000, 0, 10, 10).intersect(&screen), None);
    }
}
