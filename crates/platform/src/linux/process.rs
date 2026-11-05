//! Process-level housekeeping.

use crate::Result;

/// Hands freed heap memory back to the OS (`malloc_trim`).
///
/// Used once the shell has sat collapsed for a while, like the working-set
/// trim on Windows: an idle notch keeps only what it uses.
pub fn trim_working_set() -> Result<()> {
    #[cfg(target_env = "gnu")]
    // SAFETY: glibc call with no preconditions.
    unsafe {
        libc::malloc_trim(0);
    }
    Ok(())
}
