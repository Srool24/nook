//! Desktop notifications, as they are sent, for the pill to show.
//!
//! GNOME Shell owns `org.freedesktop.Notifications` and draws its own
//! banners; Nook only watches. A connection of its own becomes a bus
//! monitor (`org.freedesktop.DBus.Monitoring.BecomeMonitor`) for the
//! `Notify` method calls apps make, which the session bus allows for the
//! user's own session. Nothing is answered or changed, and nothing leaves
//! the machine. While GNOME's "Do Not Disturb" is on (banners off), the
//! notifications are dropped here as GNOME drops its banners.

use crate::PlatformError;
use crate::Result;
use crate::sys::settings::{self, Setting};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use zbus::zvariant::OwnedValue;

/// One notification an app sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// The sending app's name, e.g. "Firefox"; may be empty.
    pub app: String,
    pub summary: String,
    /// Body text with the simple markup some apps send stripped.
    pub body: String,
}

/// Receiving end of the watcher; the worker stops when it is dropped.
pub struct NotificationWatcher {
    rx: Receiver<Notification>,
}

impl NotificationWatcher {
    /// Starts watching. `on_new` runs (on the worker thread) after each
    /// notification, to wake the UI.
    pub fn spawn(on_new: impl Fn() + Send + 'static) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("nook-notifications".into())
            .spawn(move || {
                if let Err(e) = run(&|n| {
                    let sent = tx.send(n).is_ok();
                    on_new();
                    sent
                }) {
                    tracing::warn!("notifications in the pill unavailable: {e}");
                }
            })
            .map_err(|e| PlatformError::other("spawning notification watcher", e))?;
        Ok(Self { rx })
    }

    /// Notifications that arrived since the last call, oldest first.
    pub fn poll(&self) -> Vec<Notification> {
        self.rx.try_iter().collect()
    }
}

/// The `Notify` arguments: app name, replaces id, icon, summary, body,
/// actions, hints, timeout.
type NotifyArgs = (
    String,
    u32,
    String,
    String,
    String,
    Vec<String>,
    HashMap<String, OwnedValue>,
    i32,
);

fn run(deliver: &dyn Fn(Notification) -> bool) -> Result<()> {
    // A monitor connection can't make calls afterwards, so it is not the
    // shared one.
    let conn = zbus::blocking::Connection::session()
        .map_err(|e| PlatformError::other("connecting to the session bus", e))?;
    let rule = "type='method_call',interface='org.freedesktop.Notifications',member='Notify'";
    conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus.Monitoring"),
        "BecomeMonitor",
        &(vec![rule], 0u32),
    )
    .map_err(|e| PlatformError::other("watching notifications", e))?;
    for msg in zbus::blocking::MessageIterator::from(&conn).flatten() {
        let header = msg.header();
        if header.message_type() != zbus::message::Type::MethodCall
            || header.member().map(|m| m.as_str()) != Some("Notify")
        {
            continue;
        }
        let Ok((app, _, _, summary, body, _, hints, _)) = msg.body().deserialize::<NotifyArgs>()
        else {
            continue;
        };
        // Transient progress pings (volume, brightness OSDs) aren't news.
        if hints.contains_key("x-canonical-private-synchronous") || !banners_shown() {
            continue;
        }
        let n = Notification {
            app,
            summary: clean(&summary),
            body: clean(&body),
        };
        if n.summary.is_empty() && n.body.is_empty() {
            continue;
        }
        if !deliver(n) {
            break;
        }
    }
    Ok(())
}

/// GNOME's Do Not Disturb turns `show-banners` off.
fn banners_shown() -> bool {
    settings::get("org.gnome.desktop.notifications", "show-banners")
        .as_ref()
        .and_then(Setting::as_bool)
        .unwrap_or(true)
}

/// One line of plain text: tags (`<b>`, `<a href>`) and the common entities
/// that notification markup allows, removed or decoded; whitespace folded.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_flattened_to_one_line() {
        assert_eq!(
            clean("<b>Dana</b>: see\n<a href=\"x\">this</a> &amp; that"),
            "Dana: see this & that"
        );
        assert_eq!(clean("2 &lt; 3"), "2 < 3");
        assert_eq!(clean("  שלום   עולם \n"), "שלום עולם");
    }
}
