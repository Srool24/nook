//! The compositor backdrop (blur behind the shape) of the Windows build.
//!
//! GNOME's compositor offers clients no blur of what lies behind them, so
//! [`Backdrop::new`] always fails here and the app paints the glass in
//! OpenGL instead (the same fallback Windows uses when Composition is
//! unavailable).

use crate::window::Hwnd;
use crate::{PlatformError, Result};

/// The glass layers for one frame (see the Windows module).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tint {
    pub top: [u8; 4],
    pub bottom: [u8; 4],
    pub y0: f32,
    pub y1: f32,
    pub blur: bool,
    pub rim: [u8; 4],
    pub rim_width: f32,
    pub highlight: [u8; 4],
    pub highlight_x0: f32,
    pub highlight_x1: f32,
    pub highlight_y: f32,
}

/// Never constructed on Linux.
pub struct Backdrop {
    never: std::convert::Infallible,
}

impl Backdrop {
    pub fn new(_main: Hwnd) -> Result<Self> {
        Err(PlatformError::Other(
            "no compositor backdrop on Linux".into(),
        ))
    }

    pub fn host(&self) -> Hwnd {
        match self.never {}
    }

    pub fn follow(&mut self, _main: Hwnd) -> Result<()> {
        match self.never {}
    }

    pub fn set_shape(&mut self, _polygon: &[(f32, f32)]) -> Result<()> {
        match self.never {}
    }

    pub fn set_tint(&mut self, _tint: Tint) -> Result<()> {
        match self.never {}
    }

    pub fn set_visible(&mut self, _visible: bool) -> Result<()> {
        match self.never {}
    }
}
