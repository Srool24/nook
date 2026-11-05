//! The notch inside GNOME's top bar. GNOME Shell draws the bar above every
//! app window, so a small Shell extension (in `top_bar/`, embedded here)
//! lifts Nook's window above it. Nook writes the extension to the user's
//! extensions folder and enables it over D-Bus; a newly written extension
//! is only loaded at the next login on Wayland.

use crate::sys::dbus;
use crate::{PlatformError, Result};
use std::collections::HashMap;
use zbus::zvariant::OwnedValue;

pub const UUID: &str = "nook-top-bar@nook.linux";
const METADATA: &str = include_str!("top_bar/metadata.json");
const EXTENSION: &str = include_str!("top_bar/extension.js");

const DEST: &str = "org.gnome.Shell.Extensions";
const PATH: &str = "/org/gnome/Shell/Extensions";

/// Where the extension stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Running: the notch can sit in the bar.
    Active,
    /// GNOME Shell hasn't loaded it yet: log out and back in once.
    NeedsLogin,
    /// Loaded but failed or turned off by GNOME (too new a Shell, errors,
    /// extensions disabled).
    Off,
}

/// Writes the extension (when missing or outdated) and enables it.
pub fn install_and_enable() -> Result<State> {
    let dir = crate::sys::xdg::data_home()
        .ok_or_else(|| PlatformError::Other("no home directory".into()))?
        .join("gnome-shell/extensions")
        .join(UUID);
    std::fs::create_dir_all(&dir).map_err(|e| PlatformError::other("extension folder", e))?;
    for (name, text) in [("metadata.json", METADATA), ("extension.js", EXTENSION)] {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
            std::fs::write(&path, text)
                .map_err(|e| PlatformError::other("writing the extension", e))?;
        }
    }
    // Extensions as a whole may be switched off (Extensions app).
    let _ = dbus::call_void(
        DEST,
        PATH,
        "org.freedesktop.DBus.Properties",
        "Set",
        &(
            DEST,
            "UserExtensionsEnabled",
            zbus::zvariant::Value::from(true),
        ),
    );
    let _: Result<(bool,)> = dbus::call(DEST, PATH, DEST, "EnableExtension", &(UUID,));
    Ok(state())
}

/// Turns the extension off (the files stay, for next time).
pub fn disable() {
    let _: Result<(bool,)> = dbus::call(DEST, PATH, DEST, "DisableExtension", &(UUID,));
}

pub fn state() -> State {
    let info: Result<(HashMap<String, OwnedValue>,)> =
        dbus::call(DEST, PATH, DEST, "GetExtensionInfo", &(UUID,));
    let Ok((info,)) = info else {
        return State::NeedsLogin;
    };
    if info.is_empty() {
        return State::NeedsLogin;
    }
    // ExtensionState: 1 enabled/active (GNOME 45+ also calls it ACTIVE).
    match info
        .get("state")
        .and_then(|v| f64::try_from(v.clone()).ok())
    {
        Some(s) if (s - 1.0).abs() < 0.01 => State::Active,
        _ => State::Off,
    }
}
