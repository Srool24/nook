//! XDG base and user directories.

use std::path::PathBuf;

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn env_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    match std::env::var_os(var) {
        Some(v) if PathBuf::from(&v).is_absolute() => Some(PathBuf::from(v)),
        _ => home().map(|h| h.join(fallback)),
    }
}

/// `~/.config`.
pub fn config_home() -> Option<PathBuf> {
    env_dir("XDG_CONFIG_HOME", ".config")
}

/// `~/.local/share`.
pub fn data_home() -> Option<PathBuf> {
    env_dir("XDG_DATA_HOME", ".local/share")
}

/// `~/.cache`.
pub fn cache_home() -> Option<PathBuf> {
    env_dir("XDG_CACHE_HOME", ".cache")
}

/// `$XDG_RUNTIME_DIR` (per user, `0700`), or a private folder under the
/// cache when the session has none.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
        && dir.is_absolute()
    {
        return dir;
    }
    let dir = cache_home()
        .unwrap_or_else(std::env::temp_dir)
        .join("nook-runtime");
    let _ = std::fs::create_dir_all(&dir);
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    dir
}

/// A user directory from `~/.config/user-dirs.dirs` (`"PICTURES"`,
/// `"DOWNLOAD"`, `"MUSIC"`, `"DESKTOP"`, `"DOCUMENTS"`...), with the usual
/// English default when it is not configured.
pub fn user_dir(name: &str) -> Option<PathBuf> {
    let home = home()?;
    let key = format!("XDG_{name}_DIR");
    let file = config_home().map(|c| c.join("user-dirs.dirs"));
    if let Some(text) = file.and_then(|f| std::fs::read_to_string(f).ok())
        && let Some(dir) = parse_user_dirs(&text, &key, &home)
    {
        return Some(dir);
    }
    let fallback = match name {
        "DESKTOP" => "Desktop",
        "DOWNLOAD" => "Downloads",
        "DOCUMENTS" => "Documents",
        "MUSIC" => "Music",
        "PICTURES" => "Pictures",
        "VIDEOS" => "Videos",
        _ => return None,
    };
    Some(home.join(fallback))
}

/// `XDG_PICTURES_DIR="$HOME/Pictures"` lines from `user-dirs.dirs`.
fn parse_user_dirs(text: &str, key: &str, home: &std::path::Path) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let (k, v) = line.trim().split_once('=')?;
        if k.trim() != key {
            return None;
        }
        let v = v.trim().trim_matches('"');
        let path = match v.strip_prefix("$HOME") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(v),
        };
        // "$HOME/" alone means the directory is disabled.
        (path != home && path.is_absolute()).then_some(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_dirs() {
        let text = "# comment\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\nXDG_MUSIC_DIR=\"/data/music\"\nXDG_DESKTOP_DIR=\"$HOME/\"\n";
        let home = std::path::Path::new("/home/me");
        assert_eq!(
            parse_user_dirs(text, "XDG_PICTURES_DIR", home),
            Some(PathBuf::from("/home/me/Bilder"))
        );
        assert_eq!(
            parse_user_dirs(text, "XDG_MUSIC_DIR", home),
            Some(PathBuf::from("/data/music"))
        );
        assert_eq!(parse_user_dirs(text, "XDG_DESKTOP_DIR", home), None);
        assert_eq!(parse_user_dirs(text, "XDG_VIDEOS_DIR", home), None);
    }
}
