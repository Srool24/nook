//! Handing files, URLs and apps to the desktop (GIO's `gio` tool, which
//! picks the same handlers Files and GNOME do).

use crate::sys::{cmd, dbus, xdg};
use crate::{PlatformError, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Nook's own folder for settings and data: `~/.config/nook` (the
/// counterpart of `%APPDATA%\Nook`).
pub fn app_dir() -> Option<PathBuf> {
    xdg::config_home().map(|c| c.join("nook"))
}

/// The user's Screenshots folder (Pictures/Screenshots, where GNOME saves
/// them).
pub fn screenshots_dir() -> Option<PathBuf> {
    pictures_dir().map(|p| p.join("Screenshots"))
}

/// The installed app with this desktop entry id
/// (`org.gnome.Nautilus.desktop`), as the path of its entry.
pub fn find_app(id: &str) -> Option<PathBuf> {
    crate::sys::icons::find_application(id)
}

/// An app's name from its desktop entry, in the user's language.
pub fn app_name(entry: &Path) -> Option<String> {
    crate::sys::icons::desktop_entry(entry)
        .map(|e| e.name)
        .filter(|n| !n.is_empty())
}

/// The user's Downloads folder.
pub fn downloads_dir() -> Option<PathBuf> {
    xdg::user_dir("DOWNLOAD")
}

/// The user's Pictures folder.
pub fn pictures_dir() -> Option<PathBuf> {
    xdg::user_dir("PICTURES")
}

/// The user's Music folder.
pub fn music_dir() -> Option<PathBuf> {
    xdg::user_dir("MUSIC")
}

/// Moves a file or folder to the trash, silently.
pub fn recycle(path: &Path) -> Result<()> {
    if cmd::which("gio").is_some() {
        let p = path.display().to_string();
        return cmd::run("gio", &["trash", "--", &p]);
    }
    trash_by_hand(path)
}

/// The freedesktop.org trash for files on the home volume, for systems
/// without GIO.
fn trash_by_hand(path: &Path) -> Result<()> {
    let trash = xdg::data_home()
        .ok_or_else(|| PlatformError::Other("no home directory".into()))?
        .join("Trash");
    let (files, info) = (trash.join("files"), trash.join("info"));
    std::fs::create_dir_all(&files).map_err(|e| PlatformError::other("creating the trash", e))?;
    std::fs::create_dir_all(&info).map_err(|e| PlatformError::other("creating the trash", e))?;
    let name = path
        .file_name()
        .ok_or_else(|| PlatformError::Other("nothing to trash".into()))?
        .to_string_lossy()
        .into_owned();
    let mut target = name.clone();
    let mut n = 1;
    while files.join(&target).exists() || info.join(format!("{target}.trashinfo")).exists() {
        n += 1;
        target = format!("{name}.{n}");
    }
    let abs = std::fs::canonicalize(path).map_err(|e| PlatformError::other("trash", e))?;
    let t = crate::system::local_time();
    let stamp = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    );
    let uri = dbus::path_to_uri(&abs);
    let entry = format!(
        "[Trash Info]\nPath={}\nDeletionDate={stamp}\n",
        uri.trim_start_matches("file://")
    );
    std::fs::write(info.join(format!("{target}.trashinfo")), entry)
        .map_err(|e| PlatformError::other("writing trash info", e))?;
    std::fs::rename(&abs, files.join(&target))
        .map_err(|e| PlatformError::other("moving to trash", e))
}

/// Shows `path` selected in a Files window (the `FileManager1` D-Bus
/// interface Nautilus implements), or opens its folder.
pub fn reveal(path: &Path) -> Result<()> {
    let uri = dbus::path_to_uri(path);
    let shown = dbus::call_void(
        "org.freedesktop.FileManager1",
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1",
        "ShowItems",
        &(vec![uri.as_str()], ""),
    );
    if shown.is_ok() {
        return Ok(());
    }
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    open(&dir.display().to_string())
}

/// Opens `target` with its default handler, like a double click in Files:
/// a URL, a file or folder, an app's `.desktop` entry, or a program on
/// `PATH` (with arguments).
pub fn open(target: &str) -> Result<()> {
    let t = target.trim();
    if t.is_empty() {
        return Err(PlatformError::Other("nothing to open".into()));
    }
    let is_url = t.contains("://") || t.starts_with("mailto:") || t.starts_with("tel:");
    let path = Path::new(t);
    if !is_url && t.ends_with(".desktop") && path.is_absolute() {
        return launch_desktop(path);
    }
    if is_url || path.exists() {
        return open_with_default(t);
    }
    // A program, as Windows' ShellExecute runs "taskmgr.exe".
    let mut words = t.split_whitespace();
    if let Some(program) = words.next()
        && cmd::which(program).is_some()
    {
        let mut c = Command::new(program);
        c.args(words);
        return cmd::spawn_detached(c);
    }
    Err(PlatformError::Other(format!("don't know how to open {t}")))
}

fn open_with_default(target: &str) -> Result<()> {
    let mut c = if cmd::which("gio").is_some() {
        let mut c = Command::new("gio");
        c.args(["open", target]);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(target);
        c
    };
    // Nook may run with XWayland-only settings; the app it opens should not.
    c.env_remove("GDK_BACKEND");
    cmd::spawn_detached(c)
}

/// Starts the app a desktop entry describes.
fn launch_desktop(path: &Path) -> Result<()> {
    if cmd::which("gio").is_some() {
        let mut c = Command::new("gio");
        c.arg("launch").arg(path);
        c.env_remove("GDK_BACKEND");
        return cmd::spawn_detached(c);
    }
    let id = path
        .file_stem()
        .ok_or_else(|| PlatformError::Other("bad desktop entry".into()))?;
    let mut c = Command::new("gtk-launch");
    c.arg(id);
    cmd::spawn_detached(c)
}
