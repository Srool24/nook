//! Desktop notifications for the pill. Not watched on Windows yet (that
//! needs `UserNotificationListener` and the user's consent): the watcher
//! starts and never reports anything. See `linux/notifications.rs`.

use crate::Result;

/// One notification an app sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// The sending app's name; may be empty.
    pub app: String,
    pub summary: String,
    pub body: String,
}

/// Receiving end of the watcher.
pub struct NotificationWatcher;

impl NotificationWatcher {
    pub fn spawn(_on_new: impl Fn() + Send + 'static) -> Result<Self> {
        Ok(Self)
    }

    /// Notifications that arrived since the last call, oldest first.
    pub fn poll(&self) -> Vec<Notification> {
        Vec::new()
    }
}
