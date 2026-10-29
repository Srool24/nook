//! Launch at sign-in via an XDG autostart entry
//! (`~/.config/autostart/nook.desktop`), which GNOME reads at login.

use crate::sys::xdg;
use crate::{PlatformError, Result};
use std::path::PathBuf;

fn entry_path() -> Option<PathBuf> {
    xdg::config_home().map(|c| c.join("autostart").join("nook.desktop"))
}

pub fn is_enabled() -> bool {
    let Some(text) = entry_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return false;
    };
    !text.lines().any(|l| {
        let l = l.trim().replace(' ', "");
        l.eq_ignore_ascii_case("Hidden=true")
            || l.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
    })
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    let path = entry_path().ok_or_else(|| PlatformError::Other("no home directory".into()))?;
    if !enabled {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(PlatformError::other("removing the autostart entry", e))
            }
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| PlatformError::other("current_exe", e))?;
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=Nook\nComment=A floating notch utility hub\nExec={}\nIcon=nook\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
        crate::url_scheme::exec_quote(&exe)
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| PlatformError::other("creating autostart", e))?;
    }
    std::fs::write(&path, text).map_err(|e| PlatformError::other("writing the autostart entry", e))
}
