//! GNOME settings (GSettings).
//!
//! Reads go through the Settings portal first (one D-Bus round trip, which
//! matters for the watcher that samples the theme every few seconds) and
//! fall back to the `gsettings` tool. Writes always use `gsettings`, which
//! talks to dconf like GNOME Settings does.

use super::{cmd, dbus};
use crate::Result;

/// A setting's value, reduced to what Nook reads.
#[derive(Clone, Debug, PartialEq)]
pub enum Setting {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Strs(Vec<String>),
}

impl Setting {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Setting::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Setting::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Setting::Int(n) => Some(*n as f64),
            Setting::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_strs(&self) -> Option<&[String]> {
        match self {
            Setting::Strs(v) => Some(v),
            _ => None,
        }
    }
}

/// Reads `schema` / `key`, e.g. `("org.gnome.desktop.interface", "color-scheme")`.
pub fn get(schema: &str, key: &str) -> Option<Setting> {
    if let Some(v) = dbus::read_setting(schema, key)
        && let Some(s) = from_value(&v)
    {
        return Some(s);
    }
    if cmd::which("gsettings").is_none() {
        return None;
    }
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
