//! The session bus, the desktop portal and reading D-Bus values.
//!
//! Portal calls that show UI or take a moment (screenshots, file choosers)
//! answer with a `Request` object whose `Response` signal carries the result;
//! [`portal`] subscribes to that signal before calling, so the answer cannot
//! be missed, and waits for it.

use crate::{PlatformError, Result};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{OwnedValue, Value};

pub const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

static SESSION: OnceLock<std::result::Result<Connection, String>> = OnceLock::new();

/// The session bus, connected on first use.
pub fn session() -> Result<&'static Connection> {
    SESSION
        .get_or_init(|| Connection::session().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| PlatformError::other("connecting to the session bus", e))
}

/// A method call whose reply body deserializes into `R`.
pub fn call<B, R>(dest: &str, path: &str, iface: &str, method: &str, body: &B) -> Result<R>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> serde::Deserialize<'d> + zbus::zvariant::Type,
{
    let what = format!("{iface}.{method}");
    let reply = session()?
        .call_method(Some(dest), path, Some(iface), method, body)
        .map_err(|e| PlatformError::other(&what, e))?;
    reply
        .body()
        .deserialize::<R>()
        .map_err(|e| PlatformError::other(&what, e))
}

/// A method call whose reply is ignored.
pub fn call_void<B>(dest: &str, path: &str, iface: &str, method: &str, body: &B) -> Result<()>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let what = format!("{iface}.{method}");
    session()?
        .call_method(Some(dest), path, Some(iface), method, body)
        .map(|_| ())
        .map_err(|e| PlatformError::other(&what, e))
}

/// Results of a portal request (`a{sv}`).
pub type Results = HashMap<String, OwnedValue>;

/// Outcome of a portal request.
pub enum Response {
    Ok(Results),
    /// The user dismissed the dialog.
    Cancelled,
}

/// Calls a portal method that answers through a `Request` object and waits
/// up to `timeout` (forever with `None`) for its `Response`. `body` gets the
/// handle token to put in the method's options as `handle_token`.
pub fn portal<B>(
    iface: &str,
    method: &str,
    timeout: Option<Duration>,
    body: impl FnOnce(&str) -> B,
) -> Result<Response>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let conn = session()?;
    let token = format!(
        "nook{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let sender = conn
        .unique_name()
        .map(|n| n.trim_start_matches(':').replace('.', "_"))
        .ok_or_else(|| PlatformError::Other("session bus has no unique name".into()))?;
    let path = format!("{PORTAL_PATH}/request/{sender}/{token}");
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.portal.Request")
        .and_then(|b| b.member("Response"))
        .and_then(|b| b.path(path.clone()))
        .map_err(|e| PlatformError::other("portal match rule", e))?
        .build();
    let mut signals = MessageIterator::for_match_rule(rule, conn, Some(8))
        .map_err(|e| PlatformError::other("subscribing to the portal response", e))?;
    let what = format!("{iface}.{method}");
    conn.call_method(
        Some(PORTAL_DEST),
        PORTAL_PATH,
        Some(iface),
        method,
        &body(&token),
    )
    .map_err(|e| PlatformError::other(&what, e))?;

    let (tx, rx) = std::sync::mpsc::channel();
    let wait_path = path.clone();
    std::thread::Builder::new()
        .name("nook-portal".into())
        .spawn(move || {
            for msg in signals.by_ref().flatten() {
                if msg.header().path().is_some_and(|p| p.as_str() == wait_path) {
                    let parsed = msg.body().deserialize::<(u32, Results)>();
                    let _ = tx.send(parsed.map_err(|e| e.to_string()));
                    return;
                }
            }
        })
        .map_err(|e| PlatformError::other("spawning portal wait", e))?;
    let got = match timeout {
        Some(t) => rx.recv_timeout(t).map_err(|_| {
            PlatformError::Other(format!("{what}: no answer from the desktop portal"))
        })?,
        None => rx
            .recv()
            .map_err(|_| PlatformError::Other(format!("{what}: portal wait ended")))?,
    };
    let (code, results) = got.map_err(|e| PlatformError::other(&what, e))?;
    match code {
        0 => Ok(Response::Ok(results)),
        1 => Ok(Response::Cancelled),
        _ => Err(PlatformError::Other(format!("{what}: the portal refused"))),
    }
}

/// Reads one key through the Settings portal (GNOME exposes the
/// `org.gnome.desktop.*` schemas and `org.freedesktop.appearance` there).
pub fn read_setting(namespace: &str, key: &str) -> Option<OwnedValue> {
    const IFACE: &str = "org.freedesktop.portal.Settings";
    if let Ok(v) = call::<_, OwnedValue>(
        PORTAL_DEST,
        PORTAL_PATH,
        IFACE,
        "ReadOne",
        &(namespace, key),
    ) {
        return Some(v);
    }
    // Portals older than version 2 only have `Read`, which wraps the value
    // in one more variant.
    call::<_, OwnedValue>(PORTAL_DEST, PORTAL_PATH, IFACE, "Read", &(namespace, key)).ok()
}

/// Strips variant wrappers.
pub fn inner<'a>(v: &'a Value<'a>) -> &'a Value<'a> {
    match v {
        Value::Value(b) => inner(b),
        other => other,
    }
}

pub fn as_str(v: &Value<'_>) -> Option<String> {
    match inner(v) {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        _ => None,
    }
}

pub fn as_bool(v: &Value<'_>) -> Option<bool> {
    match inner(v) {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

pub fn as_i64(v: &Value<'_>) -> Option<i64> {
    Some(match inner(v) {
        Value::U8(n) => i64::from(*n),
        Value::I16(n) => i64::from(*n),
        Value::U16(n) => i64::from(*n),
        Value::I32(n) => i64::from(*n),
        Value::U32(n) => i64::from(*n),
        Value::I64(n) => *n,
        Value::U64(n) => i64::try_from(*n).ok()?,
        Value::F64(f) => *f as i64,
        _ => return None,
    })
}

pub fn as_f64(v: &Value<'_>) -> Option<f64> {
    match inner(v) {
        Value::F64(f) => Some(*f),
        other => as_i64(other).map(|n| n as f64),
    }
}

/// An array of strings (`as`), or a lone string as a one-item list.
pub fn as_strs(v: &Value<'_>) -> Vec<String> {
    match inner(v) {
        Value::Array(a) => a.inner().iter().filter_map(as_str).collect(),
        other => as_str(other).into_iter().collect(),
    }
}

/// The fields of a struct value (`(ddd)` and the like).
pub fn fields<'a>(v: &'a Value<'a>) -> &'a [Value<'a>] {
    match inner(v) {
        Value::Structure(s) => s.fields(),
        _ => &[],
    }
}

/// Looks `key` up in an `a{sv}` dictionary value.
pub fn dict_get<'a>(v: &'a Value<'a>, key: &str) -> Option<&'a Value<'a>> {
    match inner(v) {
        Value::Dict(d) => d
            .iter()
            .find(|(k, _)| as_str(k).as_deref() == Some(key))
            .map(|(_, v)| v),
        _ => None,
    }
}

/// Decodes `file://` URIs (percent-encoded) into paths.
pub fn uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Skip an authority ("file://localhost/...").
    let rest = &rest[rest.find('/')?..];
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    use std::os::unix::ffi::OsStringExt;
    Some(std::ffi::OsString::from_vec(out).into())
}

/// `file://` URI for a path, percent-encoding everything outside the
/// unreserved set and `/`.
pub fn path_to_uri(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_round_trip() {
        let p = std::path::Path::new("/home/me/My Pictures/צילום #1.png");
        let uri = path_to_uri(p);
        assert!(uri.starts_with("file:///home/me/My%20Pictures/"));
        assert_eq!(uri_to_path(&uri).as_deref(), Some(p));
        assert_eq!(
            uri_to_path("file://localhost/tmp/a%20b").as_deref(),
            Some(std::path::Path::new("/tmp/a b"))
        );
        assert!(uri_to_path("https://example.com").is_none());
    }
}
