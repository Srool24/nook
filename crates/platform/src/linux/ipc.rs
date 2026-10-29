//! Messages from a second launch to the running Nook, over a Unix socket.
//!
//! The browser starts `nook "nook://install?..."` for every install link,
//! and keyboard shortcuts start `nook --hotkey=...`. Only one Nook runs per
//! session, so that second process hands its message to the first one here
//! and exits. The socket lives in the user's runtime directory (`0700`) and
//! is itself `0600`, so only the same user can write to it.

use crate::sys::xdg;
use crate::{PlatformError, Result};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Longest message accepted; anything past it is dropped.
const MAX_MESSAGE: u64 = 16 * 1024;

fn socket_path(name: &str) -> PathBuf {
    let name: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    xdg::runtime_dir().join(format!("{name}.sock"))
}

/// Listens on a thread for the life of the process and calls `on_message`
/// with each message received.
pub fn serve(name: &str, on_message: impl Fn(String) + Send + 'static) -> Result<()> {
    let path = socket_path(name);
    // A socket left by a Nook that crashed: nobody answers on it.
    if path.exists() && UnixStream::connect(&path).is_err() {
        let _ = std::fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)
        .map_err(|e| PlatformError::other(&format!("binding {}", path.display()), e))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    std::thread::Builder::new()
        .name("nook-ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("ipc accept: {e}");
                        continue;
                    }
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut text = String::new();
                match (&mut stream).take(MAX_MESSAGE).read_to_string(&mut text) {
                    Ok(_) if !text.trim().is_empty() => on_message(text.trim().to_owned()),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("ipc read: {e}"),
                }
            }
        })
        .map_err(|e| PlatformError::other("spawning ipc thread", e))?;
    Ok(())
}

/// Sends `message` to the process serving `name`. Retries for a moment, in
/// case that process is still starting.
pub fn send(name: &str, message: &str) -> Result<()> {
    let path = socket_path(name);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match UnixStream::connect(&path) {
            Ok(mut stream) => {
                stream
                    .write_all(message.as_bytes())
                    .map_err(|e| PlatformError::other("ipc write", e))?;
                let _ = stream.shutdown(std::net::Shutdown::Write);
                return Ok(());
            }
            Err(e) if Instant::now() >= deadline => {
                return Err(PlatformError::other(
                    &format!("ipc connect {}", path.display()),
                    e,
                ));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivers_messages_in_order() {
        let name = format!("Nook.Test.{}", std::process::id());
        let (tx, rx) = std::sync::mpsc::channel();
        serve(&name, move |m| {
            let _ = tx.send(m);
        })
        .unwrap();
        send(&name, "nook://install?id=one").unwrap();
        send(&name, "nook://install?id=two").unwrap();
        let got: Vec<String> = (0..2)
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(got, ["nook://install?id=one", "nook://install?id=two"]);
        let _ = std::fs::remove_file(socket_path(&name));
    }
}
