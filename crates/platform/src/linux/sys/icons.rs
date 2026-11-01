//! Finding icons in the freedesktop.org icon themes, and the icons and
//! names of installed apps (`.desktop` entries).

use super::xdg;
use std::path::{Path, PathBuf};

/// Folders that hold icon themes, most specific first.
fn icon_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = xdg::home() {
        roots.push(home.join(".icons"));
    }
    if let Some(data) = xdg::data_home() {
        roots.push(data.join("icons"));
        roots.push(data.join("flatpak/exports/share/icons"));
    }
    let dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    for d in dirs.split(':').filter(|d| !d.is_empty()) {
        roots.push(Path::new(d).join("icons"));
    }
    roots.push("/var/lib/flatpak/exports/share/icons".into());
    roots
}

/// The best file for icon `name` near `size` pixels: a PNG of at least that
/// size if there is one, then an SVG, then a smaller PNG. Absolute names are
/// taken as is.
pub fn lookup(name: &str, size: u32) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() {
        return p.exists().then(|| p.to_path_buf());
    }
    let themes = ["hicolor", "Adwaita", "AdwaitaLegacy", "breeze", "gnome"];
    let sizes = [256u32, 192, 128, 96, 72, 64, 48, 32, 24, 16];
    let mut bigger: Vec<u32> = sizes.iter().copied().filter(|&s| s >= size).collect();
    bigger.reverse();
    let smaller: Vec<u32> = sizes.iter().copied().filter(|&s| s < size).collect();
    let kinds = [
        "apps",
        "mimetypes",
        "places",
        "devices",
        "categories",
        "actions",
        "status",
    ];
    let roots = icon_roots();
    for root in &roots {
        for theme in themes {
            let base = root.join(theme);
            if !base.is_dir() {
                continue;
            }
            let png = |sizes: &[u32]| {
                sizes.iter().find_map(|s| {
                    kinds
                        .iter()
                        .map(|kind| {
                            base.join(format!("{s}x{s}"))
                                .join(kind)
                                .join(format!("{name}.png"))
                        })
                        .find(|f| f.exists())
                })
            };
            let svg = || {
                ["scalable", "symbolic"].iter().find_map(|dir| {
                    kinds
                        .iter()
                        .map(|kind| base.join(dir).join(kind).join(format!("{name}.svg")))
                        .find(|f| f.exists())
                })
            };
            if let Some(f) = png(&bigger).or_else(svg).or_else(|| png(&smaller)) {
                return Some(f);
            }
        }
    }
    for ext in ["png", "svg", "xpm"] {
        let f = Path::new("/usr/share/pixmaps").join(format!("{name}.{ext}"));
        if f.exists() {
            return Some(f);
        }
    }
    None
}

/// The parts of a desktop entry Nook uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesktopEntry {
    pub name: String,
    pub icon: Option<String>,
    pub exec: Option<String>,
    pub hidden: bool,
}

/// Reads the `[Desktop Entry]` group, with the name in the user's language
/// when the entry has it.
pub fn desktop_entry(path: &Path) -> Option<DesktopEntry> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(parse_desktop_entry(&text, &languages()))
}

fn languages() -> Vec<String> {
    let lang = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let base = lang.split(['.', '@']).next().unwrap_or("").to_owned();
    let short = base.split('_').next().unwrap_or("").to_owned();
    [base, short]
        .into_iter()
        .filter(|l| !l.is_empty() && l != "C")
        .collect()
}

fn parse_desktop_entry(text: &str, langs: &[String]) -> DesktopEntry {
    let mut entry = DesktopEntry::default();
    let mut in_group = false;
    let mut localized: Option<(usize, String)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim().to_owned());
        match key {
            "Name" => entry.name = value,
            "Icon" => entry.icon = Some(value),
            "Exec" => entry.exec = Some(value),
            "NoDisplay" | "Hidden" if value == "true" => entry.hidden = true,
            k if k.starts_with("Name[") => {
                let lang = &k[5..k.len().saturating_sub(1)];
                if let Some(rank) = langs.iter().position(|l| l == lang)
                    && localized.as_ref().is_none_or(|(r, _)| rank < *r)
                {
                    localized = Some((rank, value));
                }
            }
            _ => {}
        }
    }
    if let Some((_, name)) = localized {
        entry.name = name;
    }
    entry
}

/// Where desktop entries are installed, most specific first.
pub fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(data) = xdg::data_home() {
        dirs.push(data.join("applications"));
        dirs.push(data.join("flatpak/exports/share/applications"));
    }
    let sys =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    for d in sys.split(':').filter(|d| !d.is_empty()) {
        dirs.push(Path::new(d).join("applications"));
    }
    dirs.push("/var/lib/flatpak/exports/share/applications".into());
    dirs
}

/// The installed desktop entry with this id (`org.gnome.Nautilus.desktop`).
pub fn find_application(id: &str) -> Option<PathBuf> {
    application_dirs()
        .into_iter()
        .map(|d| d.join(id))
        .find(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_desktop_entries() {
        let text = "[Desktop Entry]\nName=Files\nName[de]=Dateien\nName[he]=קבצים\nIcon=org.gnome.Nautilus\nExec=nautilus --new-window %U\n\n[Desktop Action new-window]\nName=New Window\n";
        let e = parse_desktop_entry(text, &["he_IL".into(), "he".into()]);
        assert_eq!(e.name, "קבצים");
        assert_eq!(e.icon.as_deref(), Some("org.gnome.Nautilus"));
        assert_eq!(e.exec.as_deref(), Some("nautilus --new-window %U"));
        assert!(!e.hidden);
        let plain = parse_desktop_entry(text, &[]);
        assert_eq!(plain.name, "Files");
    }
}
