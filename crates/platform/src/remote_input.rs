//! Driving the pointer and keyboard from the phone. Linux only for now
//! (`linux/remote_input.rs`, through the RemoteDesktop portal); on Windows
//! `start` reports that it isn't available.

use crate::{PlatformError, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

pub struct RemoteInput;

impl RemoteInput {
    pub fn start(_restore_token: Option<&str>) -> Result<Option<(Self, Option<String>)>> {
        Err(PlatformError::Other(
            "the phone as a mouse is not available on Windows yet".into(),
        ))
    }
    pub fn motion(&self, _dx: f64, _dy: f64) -> Result<()> {
        Ok(())
    }
    pub fn button(&self, _button: Button, _pressed: bool) -> Result<()> {
        Ok(())
    }
    pub fn scroll(&self, _dx: f64, _dy: f64, _finish: bool) -> Result<()> {
        Ok(())
    }
    pub fn key(&self, _keysym: u32, _pressed: bool) -> Result<()> {
        Ok(())
    }
}

pub fn keysym_for_char(c: char) -> u32 {
    u32::from(c)
}

pub fn keysym_for_name(_name: &str) -> Option<u32> {
    None
}
