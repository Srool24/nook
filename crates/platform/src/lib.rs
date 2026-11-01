//! Thin wrappers over the OS APIs Nook uses: Win32, DWM and the shell on
//! Windows; X11 (XWayland on GNOME), the desktop portal, D-Bus and `/proc`
//! on Linux.
//!
//! Every OS call goes through a function here that returns a [`Result`]
//! naming the API that failed, so the rest of the app never sees raw
//! `BOOL`s, `HRESULT`s, X11 errors or handles. Each module keeps the same
//! public API on both systems; on Linux most of them live in `linux/`.

pub mod audio;
#[cfg_attr(target_os = "linux", path = "linux/backdrop.rs")]
pub mod backdrop;
pub mod capture;
#[cfg_attr(target_os = "linux", path = "linux/clipboard.rs")]
pub mod clipboard;
#[cfg_attr(target_os = "linux", path = "linux/embed.rs")]
pub mod embed;
#[cfg_attr(target_os = "linux", path = "linux/files.rs")]
pub mod files;
pub mod fontmatch;
pub mod fontnet;
#[cfg_attr(target_os = "linux", path = "linux/fullscreen.rs")]
pub mod fullscreen;
pub mod hittest;
#[cfg_attr(target_os = "linux", path = "linux/ink.rs")]
pub mod ink;
#[cfg_attr(target_os = "linux", path = "linux/ipc.rs")]
pub mod ipc;
#[cfg_attr(target_os = "linux", path = "linux/material.rs")]
pub mod material;
#[cfg_attr(target_os = "linux", path = "linux/media.rs")]
pub mod media;
#[cfg_attr(target_os = "linux", path = "linux/monitor.rs")]
pub mod monitor;
#[cfg_attr(target_os = "linux", path = "linux/notifications.rs")]
pub mod notifications;
pub mod ocr;
#[cfg_attr(target_os = "linux", path = "linux/perf.rs")]
pub mod perf;
#[cfg_attr(target_os = "linux", path = "linux/power.rs")]
pub mod power;
#[cfg_attr(target_os = "linux", path = "linux/process.rs")]
pub mod process;
pub mod raster;
#[cfg(windows)]
pub mod registry;
#[cfg_attr(target_os = "linux", path = "linux/shell.rs")]
pub mod shell;
#[cfg_attr(target_os = "linux", path = "linux/single_instance.rs")]
pub mod single_instance;
#[cfg_attr(target_os = "linux", path = "linux/snip.rs")]
pub mod snip;
#[cfg_attr(target_os = "linux", path = "linux/startup.rs")]
pub mod startup;
#[cfg_attr(target_os = "linux", path = "linux/system.rs")]
pub mod system;
#[cfg_attr(target_os = "linux", path = "linux/url_scheme.rs")]
pub mod url_scheme;
pub mod wallpaper;
#[cfg_attr(target_os = "linux", path = "linux/window.rs")]
pub mod window;

/// Shared Linux plumbing: the X11 connection, D-Bus and the portal,
/// settings, fonts and helper processes.
#[cfg(target_os = "linux")]
#[path = "linux/sys/mod.rs"]
pub(crate) mod sys;

pub use window::Hwnd;

/// A failed OS API call.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[cfg(windows)]
    #[error("{call} failed: {source}")]
    Win32 {
        call: &'static str,
        #[source]
        source: windows::core::Error,
    },
    #[cfg(windows)]
    #[error("{call} failed with Win32 error {code}")]
    Code { call: &'static str, code: u32 },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, PlatformError>;

/// Attaches the API name to a `windows::core::Result`.
#[cfg(windows)]
pub(crate) trait Win32Context<T> {
    fn api(self, call: &'static str) -> Result<T>;
}

#[cfg(windows)]
impl<T> Win32Context<T> for windows::core::Result<T> {
    fn api(self, call: &'static str) -> Result<T> {
        self.map_err(|source| PlatformError::Win32 { call, source })
    }
}

/// Converts a `WIN32_ERROR` return code into a `Result`.
#[cfg(windows)]
pub(crate) fn check(
    call: &'static str,
    err: windows::Win32::Foundation::WIN32_ERROR,
) -> Result<()> {
    if err.is_ok() {
        Ok(())
    } else {
        Err(PlatformError::Code { call, code: err.0 })
    }
}

/// Null-terminated UTF-16 for `PCWSTR` parameters.
#[cfg(windows)]
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl PlatformError {
    /// An error with a message naming what failed.
    pub fn other(call: &str, e: impl std::fmt::Display) -> Self {
        PlatformError::Other(format!("{call}: {e}"))
    }
}
