//! Click-through outside the shell shape, without a window region.
//!
//! A small thread polls the cursor and reports when it crosses the shape;
//! the UI thread then flips the overlay's `WS_EX_TRANSPARENT` bit
//! ([`window::set_click_through`]): inside, the window takes mouse input as
//! usual; outside, clicks fall through to the app behind. It polls every
//! 8 ms near the shape and every 40 ms elsewhere (one `GetCursorPos` each),
//! which is far below the 120 ms hover delay and does not register in idle
//! CPU.
//!
//! The thread never touches the window itself: changing a window's style
//! from another thread sends a synchronous message to the window's thread,
//! which deadlocks as soon as that thread waits on anything the poller holds.
//!
//! [`window::set_click_through`]: crate::window::set_click_through
//!
//! A window region would do this with no thread, but changing the region of
//! the OpenGL window flashes the newly included area (see `window`).
//!
//! On Linux the X server does the routing: the shape becomes the window's
//! SHAPE input region ([`window::set_input_shape`]), pointer events outside
//! it go to the window below, and no thread is needed.
//!
//! [`window::set_input_shape`]: crate::window

#[cfg(target_os = "linux")]
pub use linux::HitTester;

#[cfg(windows)]
use crate::{PlatformError, Result};
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use std::thread::JoinHandle;
#[cfg(windows)]
use std::time::Duration;
#[cfg(windows)]
use windows::Win32::Foundation::POINT;
#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

/// Interactive area in screen pixels: closed polygons.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HitShape {
    pub polygons: Vec<Vec<(f32, f32)>>,
}

impl HitShape {
    pub fn contains(&self, (x, y): (f32, f32)) -> bool {
        self.polygons.iter().any(|p| polygon_contains(p, (x, y)))
    }

    /// Whether `p` is within `margin` px of the shape's bounding box.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn near(&self, (x, y): (f32, f32), margin: f32) -> bool {
        type Bounds = Option<(f32, f32, f32, f32)>;
        self.polygons
            .iter()
            .flatten()
            .fold(Bounds::None, |acc, &(px, py)| {
                Some(match acc {
                    None => (px, py, px, py),
                    Some((x0, y0, x1, y1)) => (x0.min(px), y0.min(py), x1.max(px), y1.max(py)),
                })
            })
            .is_some_and(|(x0, y0, x1, y1)| {
                x >= x0 - margin && x <= x1 + margin && y >= y0 - margin && y <= y1 + margin
            })
    }
}

/// Even-odd point-in-polygon test.
pub fn polygon_contains(poly: &[(f32, f32)], (x, y): (f32, f32)) -> bool {
    let mut inside = false;
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(windows)]
fn key_down(vk: u16) -> bool {
    // SAFETY: pure state query. The high bit means "currently down".
    unsafe { GetAsyncKeyState(i32::from(vk)) as u16 & 0x8000 != 0 }
}

#[cfg(windows)]
fn any_button_down() -> bool {
    [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]
        .iter()
        .any(|vk| key_down(vk.0))
}

#[cfg(windows)]
struct Shared {
    shape: Mutex<HitShape>,
    inside: AtomicBool,
    stop: AtomicBool,
}

#[cfg(windows)]
pub struct HitTester {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(windows)]
impl HitTester {
    /// Starts polling. `on_change` runs on the polling thread whenever the
    /// cursor enters or leaves the shape; read [`Self::inside`] on the UI
    /// thread and apply it there.
    pub fn spawn(on_change: impl Fn(bool) + Send + 'static) -> Result<Self> {
        let shared = Arc::new(Shared {
            shape: Mutex::new(HitShape::default()),
            inside: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("nook-hittest".into())
            .spawn(move || run(&s, on_change))
            .map_err(|e| PlatformError::Other(format!("spawning hit-test thread: {e}")))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub fn set_shape(&self, shape: HitShape) {
        let mut cur = self.shared.shape.lock().unwrap_or_else(|p| p.into_inner());
        if *cur != shape {
            *cur = shape;
        }
    }

    /// Whether the window should take mouse input: the cursor is inside the
    /// shape, or a button pressed inside it is still held.
    pub fn inside(&self) -> bool {
        self.shared.inside.load(Ordering::Relaxed)
    }
}

#[cfg(windows)]
fn run(shared: &Shared, on_change: impl Fn(bool)) {
    let mut interactive = false;
    while !shared.stop.load(Ordering::Relaxed) {
        let mut pt = POINT::default();
        // SAFETY: valid out pointer.
        let ok = unsafe { GetCursorPos(&mut pt) }.is_ok();
        let p = (pt.x as f32, pt.y as f32);
        let (inside, near) = {
            let shape = shared.shape.lock().unwrap_or_else(|e| e.into_inner());
            (ok && shape.contains(p), ok && shape.near(p, 48.0))
        };
        // Keep input while a button is held over us (press, drag): the
        // window has mouse capture and expects the matching release.
        let hold = interactive && any_button_down();
        if interactive != inside && !hold {
            interactive = inside;
            shared.inside.store(inside, Ordering::Relaxed);
            on_change(inside);
        }
        let wait = if near { 8 } else { 40 };
        std::thread::sleep(Duration::from_millis(wait));
    }
}

#[cfg(windows)]
impl Drop for HitTester {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::HitShape;
    use crate::Result;

    /// Keeps the overlay's input region on the shape. The window always
    /// takes input inside it, so [`Self::inside`] is always true and no
    /// pointer events need synthesizing.
    pub struct HitTester {
        _on_change: Box<dyn Fn(bool) + Send>,
    }

    impl HitTester {
        pub fn spawn(on_change: impl Fn(bool) + Send + 'static) -> Result<Self> {
            Ok(Self {
                _on_change: Box::new(on_change),
            })
        }

        pub fn set_shape(&self, shape: HitShape) {
            crate::window::set_input_shape(&shape);
        }

        pub fn inside(&self) -> bool {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_contains() {
        let sq = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        assert!(polygon_contains(&sq, (5.0, 5.0)));
        assert!(!polygon_contains(&sq, (15.0, 5.0)));
        assert!(!polygon_contains(&sq, (5.0, -1.0)));
        assert!(!polygon_contains(&sq[..2], (0.5, 0.0)));
    }

    #[test]
    fn shape_near_uses_bounding_box() {
        let shape = HitShape {
            polygons: vec![vec![
                (100.0, 0.0),
                (200.0, 0.0),
                (200.0, 40.0),
                (100.0, 40.0),
            ]],
        };
        assert!(shape.contains((150.0, 20.0)));
        assert!(shape.near((90.0, 20.0), 48.0));
        assert!(!shape.near((30.0, 20.0), 48.0));
        assert!(!HitShape::default().near((0.0, 0.0), 48.0));
    }
}
