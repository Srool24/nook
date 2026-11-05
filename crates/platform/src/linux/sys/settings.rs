//! GNOME settings (GSettings) and the portal's appearance settings.
//!
//! Reads go through the Settings portal and are cached: the system watcher
//! samples the theme, accent and wallpaper every second, and the portal's
//! `SettingChanged` signal keeps the cache current, so those samples cost
//! nothing. Without a portal, values come from the `gsettings` tool and are
//! re-read at most every few seconds. Writes always use `gsettings`, which
//! talks to dconf like GNOME Settings does.

use super::{cmd, dbus};
use crate::Result;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a value read without the portal stays fresh.
const POLL_TTL: Duration = Duration::from_secs(5);

/// A setting's value, reduced to what Nook reads.
#[derive(Clone, Debug, PartialEq)]
pub enum Setting {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Strs(Vec<String>),
    /// A tuple of numbers, such as the portal's accent color `(ddd)`.
    Floats(Vec<f64>),
}

impl Setting {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Setting::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Setting::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_floats(&self) -> Option<&[f64]> {
        match self {
            Setting::Floats(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Setting::Str(s) => Some(s),
            _ => None,
        }
    }
}

type Key = (String, String);

struct Cache {
    values: HashMap<Key, (Instant, Option<Setting>)>,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(Cache {
            values: HashMap::new(),
        })
    })
}

/// Whether the portal answers and its change signal is being followed
/// (then cached values never go stale).
fn following() -> bool {
    static FOLLOWING: OnceLock<bool> = OnceLock::new();
    *FOLLOWING.get_or_init(|| {
        dbus::read_setting("org.freedesktop.appearance", "color-scheme").is_some()
            && follow_changes().is_ok()
    })
}

/// Updates cached values from the portal's `SettingChanged` signal.
fn follow_changes() -> Result<()> {
    let conn = dbus::session()?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.freedesktop.portal.Settings")
        .and_then(|b| b.member("SettingChanged"))
        .map_err(|e| crate::PlatformError::other("settings match rule", e))?
        .build();
    let iter = zbus::blocking::MessageIterator::for_match_rule(rule, conn, Some(64))
        .map_err(|e| crate::PlatformError::other("subscribing to setting changes", e))?;
    std::thread::Builder::new()
        .name("nook-settings".into())
        .spawn(move || {
            for msg in iter.flatten() {
                let Ok((ns, key, value)) = msg
                    .body()
                    .deserialize::<(String, String, zbus::zvariant::OwnedValue)>()
                else {
                    continue;
                };
                let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = c.values.get_mut(&(ns, key)) {
                    *entry = (Instant::now(), from_value(&value));
                }
            }
        })
        .map_err(|e| crate::PlatformError::other("spawning the settings listener", e))?;
    Ok(())
}

/// Reads `schema` / `key`, e.g. `("org.gnome.desktop.interface",
/// "color-scheme")` or `("org.freedesktop.appearance", "accent-color")`.
pub fn get(schema: &str, key: &str) -> Option<Setting> {
    let live = following();
    let k = (schema.to_owned(), key.to_owned());
    if let Some((at, v)) = cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values
        .get(&k)
        && (live || at.elapsed() < POLL_TTL)
    {
        return v.clone();
    }
    let v = read(schema, key);
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values
        .insert(k, (Instant::now(), v.clone()));
    v
}

fn read(schema: &str, key: &str) -> Option<Setting> {
    if let Some(v) = dbus::read_setting(schema, key)
        && let Some(s) = from_value(&v)
    {
        return Some(s);
    }
    if schema.starts_with("org.freedesktop.") {
        return None; // portal-only namespaces
    }
    cmd::which("gsettings")?;
    let text = cmd::output("gsettings", &["get", schema, key]).ok()?;
    parse_gvariant(&text)
}

fn from_value(v: &zbus::zvariant::Value<'_>) -> Option<Setting> {
    use zbus::zvariant::Value;
    Some(match dbus::inner(v) {
        Value::Bool(b) => Setting::Bool(*b),
        Value::Str(s) => Setting::Str(s.to_string()),
        Value::F64(f) => Setting::Float(*f),
        Value::Array(_) => Setting::Strs(dbus::as_strs(v)),
        Value::Structure(_) => {
            Setting::Floats(dbus::fields(v).iter().filter_map(dbus::as_f64).collect())
        }
        other => Setting::Int(dbus::as_i64(other)?),
    })
}

/// Writes a value given in GVariant text form (`"'file:///a.jpg'"`, `"true"`).
pub fn set(schema: &str, key: &str, value: &str) -> Result<()> {
    cmd::run("gsettings", &["set", schema, key, value])
}

/// Quotes a string as a GVariant string literal.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Parses what `gsettings get` prints for the simple types.
pub fn parse_gvariant(text: &str) -> Option<Setting> {
    let t = text.trim();
    // Typed literals: "uint32 5", "@as []", "int64 3".
    let t = match t.split_once(' ') {
        Some((ty, rest))
            if ty.starts_with('@')
                || matches!(
                    ty,
                    "uint32"
                        | "int32"
                        | "uint64"
                        | "int64"
                        | "uint16"
                        | "int16"
                        | "byte"
                        | "double"
                ) =>
        {
            rest.trim()
        }
        _ => t,
    };
    match t {
        "true" => return Some(Setting::Bool(true)),
        "false" => return Some(Setting::Bool(false)),
        _ => {}
    }
    if t.starts_with('[') {
        let inner = t.trim_start_matches('[').trim_end_matches(']');
        let items = split_strings(inner);
        return Some(Setting::Strs(items));
    }
    if t.starts_with('\'') || t.starts_with('"') {
        return split_strings(t).into_iter().next().map(Setting::Str);
    }
    if let Ok(n) = t.parse::<i64>() {
        return Some(Setting::Int(n));
    }
    t.parse::<f64>().ok().map(Setting::Float)
}

/// The quoted strings in a list body: `'a', 'b\'c'`.
fn split_strings(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\'' && c != '"' {
            continue;
        }
        let quote = c;
        let mut cur = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
                c if c == quote => break,
                c => cur.push(c),
            }
        }
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gsettings_output() {
        assert_eq!(
            parse_gvariant("'prefer-dark'\n"),
            Some(Setting::Str("prefer-dark".into()))
        );
        assert_eq!(parse_gvariant("true"), Some(Setting::Bool(true)));
        assert_eq!(parse_gvariant("uint32 300"), Some(Setting::Int(300)));
        assert_eq!(parse_gvariant("1.25"), Some(Setting::Float(1.25)));
        assert_eq!(parse_gvariant("@as []"), Some(Setting::Strs(vec![])));
        assert_eq!(
            parse_gvariant("['/org/a/', 'it\\'s']"),
            Some(Setting::Strs(vec!["/org/a/".into(), "it's".into()]))
        );
    }

    #[test]
    fn quotes_strings() {
        assert_eq!(quote("file:///a b.jpg"), "'file:///a b.jpg'");
        assert_eq!(quote("it's"), "'it\\'s'");
    }
}
