//! Desktop notifications, as they are sent, for the pill to show.
//!
//! GNOME Shell draws its own banners; Nook only watches. A connection of
//! its own becomes a bus monitor (`org.freedesktop.DBus.Monitoring.
//! BecomeMonitor`, which the session bus allows for the user's own
//! session) for the three ways apps send notifications on GNOME:
//! `org.freedesktop.Notifications.Notify` (libnotify, Chromium, Electron),
//! `org.gtk.Notifications.AddNotification` (GApplication: GNOME's own
//! apps) and `org.freedesktop.portal.Notification.AddNotification`
//! (Flatpak apps; the portal passes them on to GNOME Shell, which would
//! show twice, so a repeat within a moment is dropped). Nothing is answered or changed, and nothing leaves
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
    let rules = vec![
        "type='method_call',interface='org.freedesktop.Notifications',member='Notify'",
        "type='method_call',interface='org.gtk.Notifications',member='AddNotification'",
        "type='method_call',interface='org.freedesktop.portal.Notification',member='AddNotification'",
    ];
    conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus.Monitoring"),
        "BecomeMonitor",
        &(rules, 0u32),
    )
    .map_err(|e| PlatformError::other("watching notifications", e))?;
    tracing::info!("notifications: watching the session bus");
    let mut last: Option<(Notification, std::time::Instant)> = None;
    let mut seen = 0u64;
    for msg in zbus::blocking::MessageIterator::from(&conn) {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("notifications: bad message on the monitor: {e}");
                continue;
            }
        };
        let header = msg.header();
        let iface = header
            .interface()
            .map(|i| i.to_string())
            .unwrap_or_default();
        let member = header.member().map(|m| m.to_string()).unwrap_or_default();
        // The first few messages, by kind only, to tell "nothing arrives"
        // from "arrives but isn't understood" in a report.
        if seen < 5 {
            seen += 1;
            tracing::info!(
                "notifications: monitor got {:?} {iface}.{member}",
                header.message_type()
            );
        }
        if header.message_type() != zbus::message::Type::MethodCall {
            continue;
        }
        let body = msg.body();
        let n = match (iface.as_str(), member.as_str()) {
            ("org.freedesktop.Notifications", "Notify") => {
                let (app, _, _, summary, text, _, hints, _) = match body.deserialize::<NotifyArgs>()
                {
                    Ok(args) => args,
                    Err(e) => {
                        tracing::warn!("notifications: unreadable Notify: {e}");
                        continue;
                    }
                };
                // Transient progress pings (volume, brightness OSDs) aren't news.
                if hints.contains_key("x-canonical-private-synchronous") {
                    continue;
                }
                Notification {
                    app,
                    summary: clean(&summary),
                    body: clean(&text),
                }
            }
            ("org.gtk.Notifications", "AddNotification") => {
                let Ok((app_id, _, fields)) =
                    body.deserialize::<(String, String, HashMap<String, OwnedValue>)>()
                else {
                    continue;
                };
                from_fields(&app_name(&app_id), &fields)
            }
            ("org.freedesktop.portal.Notification", "AddNotification") => {
                let Ok((_, fields)) = body.deserialize::<(String, HashMap<String, OwnedValue>)>()
                else {
                    continue;
                };
                from_fields("", &fields)
            }
            _ => continue,
        };
        tracing::info!("notifications: one from {:?}", n.app);
        if n.summary.is_empty() && n.body.is_empty() {
            continue;
        }
        if !banners_shown() {
            tracing::info!("notifications: not shown, Do Not Disturb is on");
            continue;
        }
        // The portal hands a Flatpak app's notification on to GNOME Shell:
        // the same text again, a moment later.
        let repeat = last.as_ref().is_some_and(|(p, at)| {
            p.summary == n.summary && p.body == n.body && at.elapsed().as_secs() < 3
        });
        last = Some((n.clone(), std::time::Instant::now()));
        if repeat {
            continue;
        }
        if !deliver(n) {
            break;
        }
    }
    tracing::warn!("notifications: the bus monitor stopped");
    Ok(())
}

/// A GLib/portal notification dictionary (`title`, `body`).
fn from_fields(app: &str, fields: &HashMap<String, OwnedValue>) -> Notification {
    let text = |key: &str| {
        fields
            .get(key)
            .and_then(|v| String::try_from(v.clone()).ok())
            .map(|t| clean(&t))
            .unwrap_or_default()
    };
    Notification {
        app: app.to_owned(),
        summary: text("title"),
        body: text("body"),
    }
}

/// A readable name from an application id: `org.gnome.Calendar` ->
/// `Calendar`, `org.telegram.desktop` -> `Telegram`.
fn app_name(id: &str) -> String {
    let parts: Vec<&str> = id.split('.').filter(|p| !p.is_empty()).collect();
    let pick = match parts.as_slice() {
        [.., name, last] if last.eq_ignore_ascii_case("desktop") => *name,
        [.., last] => *last,
        [] => "",
    };
    let mut c = pick.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
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

    #[test]
    fn app_ids_read_as_names() {
        assert_eq!(app_name("org.gnome.Calendar"), "Calendar");
        assert_eq!(app_name("org.telegram.desktop"), "Telegram");
        assert_eq!(app_name("firefox"), "Firefox");
        assert_eq!(app_name(""), "");
    }
}
