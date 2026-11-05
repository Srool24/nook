//! One Nook per user session: an exclusive lock on a file in the runtime
//! directory, released by the kernel when the process ends, however it ends.

use crate::sys::xdg;
use crate::{PlatformError, Result};
use std::os::fd::AsRawFd;

/// Holds the lock for the life of the process.
pub struct InstanceGuard(#[allow(dead_code)] std::fs::File);

/// `Ok(None)` if another instance already runs.
pub fn acquire(name: &str) -> Result<Option<InstanceGuard>> {
    let path = xdg::runtime_dir().join(format!("{name}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| PlatformError::other(&format!("opening {}", path.display()), e))?;
    // SAFETY: flock on a file descriptor we own.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(InstanceGuard(file)));
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(None)
    } else {
        Err(PlatformError::other("flock", err))
    }
}
