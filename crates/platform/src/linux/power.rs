//! Keep-awake through GNOME's session manager (`org.gnome.SessionManager`
//! `Inhibit`), falling back to the desktop portal's `Inhibit` elsewhere.
//!
//! Both drop the inhibitor when Nook's bus connection goes away, so even a
//! crash cannot leave the machine stuck awake (as `SetThreadExecutionState`
//! does on Windows).

use crate::sys::dbus;
use crate::{PlatformError, Result};
use std::collections::HashMap;
use std::sync::Mutex;
use zbus::zvariant::{OwnedObjectPath, Value};

/// gnome-session inhibit flags.
const INHIBIT_SUSPEND: u32 = 4;
const INHIBIT_IDLE: u32 = 8;

enum Held {
    Gnome(u32),
    Portal(OwnedObjectPath),
}

static HELD: Mutex<Option<Held>> = Mutex::new(None);

pub fn keep_awake(display: bool) -> Result<()> {
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(h) = held.take() {
        drop_inhibitor(h);
    }
    let flags = INHIBIT_SUSPEND | if display { INHIBIT_IDLE } else { 0 };
    let reason = "Coffee is keeping the computer awake";
    let gnome = dbus::call::<_, u32>(
        "org.gnome.SessionManager",
        "/org/gnome/SessionManager",
        "org.gnome.SessionManager",
        "Inhibit",
        &("nook", 0u32, reason, flags),
    );
    match gnome {
        Ok(cookie) => {
            *held = Some(Held::Gnome(cookie));
            Ok(())
        }
        Err(gnome_err) => {
            let mut options: HashMap<&str, Value<'_>> = HashMap::new();
            options.insert("reason", Value::from(reason));
            let handle = dbus::call::<_, OwnedObjectPath>(
                dbus::PORTAL_DEST,
                dbus::PORTAL_PATH,
                "org.freedesktop.portal.Inhibit",
                "Inhibit",
                &("", flags, options),
            )
            .map_err(|e| PlatformError::Other(format!("{gnome_err}; {e}")))?;
            *held = Some(Held::Portal(handle));
            Ok(())
        }
    }
}

/// Back to normal power management.
pub fn release() -> Result<()> {
    if let Some(h) = HELD.lock().unwrap_or_else(|e| e.into_inner()).take() {
        drop_inhibitor(h);
    }
    Ok(())
}

fn drop_inhibitor(h: Held) {
    let res = match h {
        Held::Gnome(cookie) => dbus::call_void(
            "org.gnome.SessionManager",
            "/org/gnome/SessionManager",
            "org.gnome.SessionManager",
            "Uninhibit",
            &(cookie,),
        ),
        Held::Portal(path) => dbus::call_void(
            dbus::PORTAL_DEST,
            path.as_str(),
            "org.freedesktop.portal.Request",
            "Close",
            &(),
        ),
    };
    if let Err(e) = res {
        tracing::warn!("releasing keep-awake: {e}");
    }
}
