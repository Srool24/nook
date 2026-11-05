//! Registers a `name://` link type for the current user, so a browser can
//! start Nook with a link: a hidden desktop entry that handles
//! `x-scheme-handler/<name>`, made the default in `~/.config/mimeapps.list`.

use crate::sys::xdg;
use crate::{PlatformError, Result};
use std::path::Path;

/// Points `scheme://` links at this executable. Writes only what differs,
/// so calling it at every start costs two file reads.
pub fn register(scheme: &str, description: &str) -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| PlatformError::other("current_exe", e))?;
    let apps = xdg::data_home()
        .ok_or_else(|| PlatformError::Other("no home directory".into()))?
        .join("applications");
    let file = format!("nook-{scheme}-handler.desktop");
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName={description}\nExec={} %u\nIcon=nook\nTerminal=false\nNoDisplay=true\nMimeType=x-scheme-handler/{scheme};\n",
        exec_quote(&exe)
    );
    let path = apps.join(&file);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(&entry) {
        std::fs::create_dir_all(&apps)
            .map_err(|e| PlatformError::other("creating applications", e))?;
        std::fs::write(&path, &entry)
            .map_err(|e| PlatformError::other(&format!("writing {}", path.display()), e))?;
        if crate::sys::cmd::which("update-desktop-database").is_some() {
            let _ = crate::sys::cmd::run("update-desktop-database", &[&apps.display().to_string()]);
        }
    }
    let mimeapps = xdg::config_home()
        .ok_or_else(|| PlatformError::Other("no home directory".into()))?
        .join("mimeapps.list");
    let current = std::fs::read_to_string(&mimeapps).unwrap_or_default();
    let updated = set_default(&current, &format!("x-scheme-handler/{scheme}"), &file);
    if updated != current {
        std::fs::write(&mimeapps, updated)
            .map_err(|e| PlatformError::other("writing mimeapps.list", e))?;
    }
    Ok(())
}

/// Quotes a path for a desktop entry's `Exec` key.
pub fn exec_quote(path: &Path) -> String {
    let s = path.display().to_string();
    if !s.chars().any(|c| " \t\"'\\`$;&|<>(){}*?#~".contains(c)) {
        return s;
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// `mimeapps.list` with `mime=desktop` in `[Default Applications]`.
fn set_default(text: &str, mime: &str, desktop: &str) -> String {
    const SECTION: &str = "[Default Applications]";
    let want = format!("{mime}={desktop};");
    let mut out: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut seen_section = false;
    let mut done = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_section && !done {
                out.push(want.clone());
                done = true;
            }
            in_section = trimmed == SECTION;
            seen_section |= in_section;
        } else if in_section
            && let Some((k, _)) = trimmed.split_once('=')
            && k.trim() == mime
        {
            if !done {
                out.push(want.clone());
                done = true;
            }
            continue;
        }
        out.push(line.to_owned());
    }
    // Not written yet: the section is missing, or it is the last one.
    if !done {
        if !seen_section {
            if out.last().is_some_and(|l| !l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(SECTION.to_owned());
        }
        out.push(want);
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_and_replaces_defaults() {
        let m = "x-scheme-handler/nook";
        let d = "nook-nook-handler.desktop";
        assert_eq!(
            set_default("", m, d),
            "[Default Applications]\nx-scheme-handler/nook=nook-nook-handler.desktop;\n"
        );
        let before = "[Default Applications]\ntext/html=firefox.desktop\nx-scheme-handler/nook=old.desktop;\n\n[Added Associations]\na=b\n";
        let after = set_default(before, m, d);
        assert!(after.contains("x-scheme-handler/nook=nook-nook-handler.desktop;"));
        assert!(!after.contains("old.desktop"));
        assert!(after.contains("text/html=firefox.desktop"));
        assert!(after.contains("[Added Associations]\na=b"));
        // Already right: unchanged.
        assert_eq!(set_default(&after, m, d), after);
        let other = "[Added Associations]\na=b\n";
        assert!(set_default(other, m, d).ends_with(
            "[Default Applications]\nx-scheme-handler/nook=nook-nook-handler.desktop;\n"
        ));
    }

    #[test]
    fn quotes_exec_paths() {
        assert_eq!(exec_quote(Path::new("/usr/bin/nook")), "/usr/bin/nook");
        assert_eq!(
            exec_quote(Path::new("/home/me/My Apps/nook")),
            "\"/home/me/My Apps/nook\""
        );
    }
}
