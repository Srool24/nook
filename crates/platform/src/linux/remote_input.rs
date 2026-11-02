//! Moving the pointer, clicking, scrolling and typing on the user's behalf
//! (the phone as a touchpad), through the RemoteDesktop portal: on a
//! Wayland session it is the only way a client may drive input, and GNOME
//! asks the user first ("Allow remote interaction?").
//!
//! The session is persistent (`persist_mode` 2): the portal hands back a
//! restore token, and starting again with it skips the question until the
//! user revokes it in Settings > Privacy.
//!
//! Calls block (the first one waits for the user's answer), so the caller
//! runs them on a thread of its own.

use crate::sys::dbus;
use crate::{PlatformError, Result};
use std::collections::HashMap;
use std::time::Duration;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, Value};

const IFACE: &str = "org.freedesktop.portal.RemoteDesktop";
/// Device types: keyboard (1) and pointer (2).
const DEVICES: u32 = 1 | 2;

/// Pointer buttons, as Linux input event codes (`BTN_LEFT`...).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    fn code(self) -> i32 {
        match self {
            Button::Left => 0x110,
            Button::Right => 0x111,
            Button::Middle => 0x112,
        }
    }
}

/// An approved remote-input session; closed when dropped.
pub struct RemoteInput {
    session: OwnedObjectPath,
}

type Options<'a> = HashMap<&'a str, Value<'a>>;

impl RemoteInput {
    /// Opens a session, asking the user unless `restore_token` (from an
    /// earlier session) still holds. Returns the session and the token to
    /// keep for next time. `Ok(None)` when the user said no.
    pub fn start(restore_token: Option<&str>) -> Result<Option<(Self, Option<String>)>> {
        let created = dbus::portal(IFACE, "CreateSession", Some(Duration::from_secs(10)), |t| {
            let mut o: Options<'_> = HashMap::new();
            o.insert("handle_token", Value::from(t.to_owned()));
            o.insert("session_handle_token", Value::from(t.to_owned()));
            (o,)
        })?;
        let dbus::Response::Ok(results) = created else {
            return Ok(None);
        };
        let session = results
            .get("session_handle")
            .and_then(|v| dbus::as_str(v))
            .and_then(|s| OwnedObjectPath::try_from(s).ok())
            .ok_or_else(|| PlatformError::Other("RemoteDesktop: no session handle".into()))?;
        let me = Self { session };

        let token = restore_token.map(str::to_owned);
        let selected = dbus::portal(IFACE, "SelectDevices", Some(Duration::from_secs(10)), |t| {
            let mut o: Options<'_> = HashMap::new();
            o.insert("handle_token", Value::from(t.to_owned()));
            o.insert("types", Value::from(DEVICES));
            o.insert("persist_mode", Value::from(2u32));
            if let Some(tok) = &token {
                o.insert("restore_token", Value::from(tok.clone()));
            }
            (me.path(), o)
        })?;
        if matches!(selected, dbus::Response::Cancelled) {
            return Ok(None);
        }
        // The dialog, if any, shows now: wait as long as the user takes.
        let started = dbus::portal(IFACE, "Start", None, |t| {
            let mut o: Options<'_> = HashMap::new();
            o.insert("handle_token", Value::from(t.to_owned()));
            (me.path(), "", o)
        })?;
        let dbus::Response::Ok(results) = started else {
            return Ok(None);
        };
        let restore = results.get("restore_token").and_then(|v| dbus::as_str(v));
        Ok(Some((me, restore)))
    }

    fn path(&self) -> ObjectPath<'_> {
        self.session.as_ref()
    }

    fn notify<B>(&self, method: &str, body: &B) -> Result<()>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        dbus::call_void(dbus::PORTAL_DEST, dbus::PORTAL_PATH, IFACE, method, body)
    }

    /// Moves the pointer by (`dx`, `dy`) logical pixels.
    pub fn motion(&self, dx: f64, dy: f64) -> Result<()> {
        let o: Options<'_> = HashMap::new();
        self.notify("NotifyPointerMotion", &(self.path(), o, dx, dy))
    }

    pub fn button(&self, button: Button, pressed: bool) -> Result<()> {
        let o: Options<'_> = HashMap::new();
        self.notify(
            "NotifyPointerButton",
            &(self.path(), o, button.code(), u32::from(pressed)),
        )
    }

    /// Smooth scrolling by (`dx`, `dy`) pixels; `finish` ends the gesture
    /// (kinetic scrolling may follow).
    pub fn scroll(&self, dx: f64, dy: f64, finish: bool) -> Result<()> {
        let mut o: Options<'_> = HashMap::new();
        if finish {
            o.insert("finish", Value::from(true));
        }
        self.notify("NotifyPointerAxis", &(self.path(), o, dx, dy))
    }

    /// Presses or releases the key that types X11 `keysym`.
    pub fn key(&self, keysym: u32, pressed: bool) -> Result<()> {
        let o: Options<'_> = HashMap::new();
        self.notify(
            "NotifyKeyboardKeysym",
            &(self.path(), o, keysym as i32, u32::from(pressed)),
        )
    }
}

impl Drop for RemoteInput {
    fn drop(&mut self) {
        let _ = dbus::call_void(
            dbus::PORTAL_DEST,
            self.session.as_str(),
            "org.freedesktop.portal.Session",
            "Close",
            &(),
        );
    }
}

/// The X11 keysym that types `c` (Latin-1 directly, the rest through the
/// Unicode keysym range).
pub fn keysym_for_char(c: char) -> u32 {
    let u = u32::from(c);
    match c {
        '\n' => 0xff0d,
        '\t' => 0xff09,
        _ if (0x20..=0x7e).contains(&u) || (0xa0..=0xff).contains(&u) => u,
        _ => 0x0100_0000 | u,
    }
}

/// Keysyms for the named keys the phone sends.
pub fn keysym_for_name(name: &str) -> Option<u32> {
    Some(match name {
        "Enter" => 0xff0d,
        "Backspace" => 0xff08,
        "Escape" => 0xff1b,
        "Tab" => 0xff09,
        "Left" => 0xff51,
        "Up" => 0xff52,
        "Right" => 0xff53,
        "Down" => 0xff54,
        "PageUp" => 0xff55,
        "PageDown" => 0xff56,
        "Home" => 0xff50,
        "End" => 0xff57,
        "Super" => 0xffeb,
        "VolumeUp" => 0x1008ff13,
        "VolumeDown" => 0x1008ff11,
        "Mute" => 0x1008ff12,
        "PlayPause" => 0x1008ff14,
        "Next" => 0x1008ff17,
        "Previous" => 0x1008ff16,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_map_to_keysyms() {
        assert_eq!(keysym_for_char('a'), 0x61);
        assert_eq!(keysym_for_char('é'), 0xe9);
        assert_eq!(keysym_for_char('ש'), 0x0100_05e9);
        assert_eq!(keysym_for_char('\n'), 0xff0d);
        assert_eq!(keysym_for_name("PlayPause"), Some(0x1008ff14));
        assert_eq!(keysym_for_name("nope"), None);
    }
}
