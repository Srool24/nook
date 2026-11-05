//! Helper programs: finding them on `PATH` and running them.

use crate::{PlatformError, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The first `name` on `PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_executable(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Runs a program to completion and returns its stdout (trimmed) if it
/// exited successfully.
pub fn output(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| PlatformError::other(&format!("running {program}"), e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(PlatformError::Other(format!(
            "{program} failed ({}): {}",
            out.status,
            err.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Runs a program to completion, ignoring its output.
pub fn run(program: &str, args: &[&str]) -> Result<()> {
    output(program, args).map(|_| ())
}

/// Starts a program and lets it run on its own (no waiting, no console).
/// The child is reaped by a short-lived thread so it never lingers as a
/// zombie.
pub fn spawn_detached(mut cmd: Command) -> Result<()> {
    let program = format!("{:?}", cmd.get_program());
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| PlatformError::other(&format!("starting {program}"), e))?;
    let _ = std::thread::Builder::new()
        .name("nook-reap".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}
