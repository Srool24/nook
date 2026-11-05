//! Global shortcuts on GNOME (Linux).
//!
//! An X11 client cannot grab keys system-wide on a Wayland session, so on
//! GNOME the shortcuts become GNOME custom keyboard shortcuts (Settings >
//! Keyboard > Custom Shortcuts) that run `nook --hotkey=<id>`. That second
//! launch hands the id to the running Nook over IPC and exits; with no Nook
//! running it starts one, which then acts on the key.
//!
//! Nook only touches entries under `custom-keybindings/nook-*`, writes them
//! with one `dconf load`, and never changes a binding the user edited: only
//! missing entries are added and the command is kept pointing at this
//! binary. Elsewhere (an X11 session) `global-hotkey` grabs the keys as on
//! Windows.

use crossbeam_channel::{Receiver, unbounded};
use std::collections::BTreeMap;
use std::process::{Command, Stdio};

/// IPC name the second launch sends shortcut ids to.
const IPC: &str = "Nook.Hotkeys";
const ROOT: &str = "/org/gnome/settings-daemon/plugins/media-keys/";
/// The id of the shortcut that opens and pins the panel.
pub const TOGGLE: &str = "toggle";

/// Whether shortcuts should go through GNOME (a Wayland session on GNOME
/// with `dconf` available).
pub fn use_desktop() -> bool {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
    let gnome = std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("GNOME")));
    wayland && gnome && on_path("dconf")
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

/// The id a `--hotkey=<id>` argument asks for, if any.
pub fn requested() -> Option<String> {
    std::env::args().find_map(|a| a.strip_prefix("--hotkey=").map(str::to_owned))
}

/// Hands `id` to the running Nook. False if none answered.
pub fn forward(id: &str) -> bool {
    nook_platform::ipc::send(IPC, id).is_ok()
}

/// Listens for ids from `nook --hotkey=<id>` launches; each arrival wakes
/// the UI.
pub fn serve(ctx: &egui::Context) -> Option<Receiver<String>> {
    let (tx, rx) = unbounded();
    let ctx = ctx.clone();
    match nook_platform::ipc::serve(IPC, move |id| {
        if tx.send(id).is_ok() {
            ctx.request_repaint();
        }
    }) {
        Ok(()) => Some(rx),
        Err(e) => {
            tracing::warn!("shortcut listener: {e}");
            None
        }
    }
}

/// One shortcut: Nook's id, a name for GNOME Settings, and the accelerator
/// in `global-hotkey` form (`ctrl+alt+KeyC`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    pub id: String,
    pub name: String,
    pub accel: String,
}

/// Makes GNOME's custom shortcuts match `wanted` (on a background thread):
/// adds the missing ones, points existing ones at this binary, and removes
/// `nook-*` entries Nook no longer has (an uninstalled plugin's).
pub fn sync(wanted: Vec<Shortcut>) {
    let spawned = std::thread::Builder::new()
        .name("nook-shortcuts".into())
        .spawn(move || {
            if let Err(e) = apply(&wanted) {
                tracing::warn!("GNOME shortcuts: {e}");
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("spawning shortcut sync: {e}");
    }
}

fn apply(wanted: &[Shortcut]) -> std::result::Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dump = run("dconf", &["dump", ROOT], None)?;
    let current = parse_dump(&dump);
    let plan = plan(&current, wanted, &exe.display().to_string());
    if let Some(ini) = plan.load {
        run("dconf", &["load", ROOT], Some(&ini))?;
    }
    for path in plan.reset {
        run("dconf", &["reset", "-f", &format!("{ROOT}{path}/")], None)?;
    }
    Ok(())
}

fn run(program: &str, args: &[&str], stdin: Option<&str>) -> std::result::Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    if let (Some(text), Some(mut input)) = (stdin, child.stdin.take()) {
        use std::io::Write;
        input
            .write_all(text.as_bytes())
            .map_err(|e| format!("{program}: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `dconf dump` output: section -> key -> raw value.
type Dump = BTreeMap<String, BTreeMap<String, String>>;

fn parse_dump(text: &str) -> Dump {
    let mut out = Dump::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.to_owned();
            out.entry(section.clone()).or_default();
        } else if let Some((k, v)) = line.split_once('=') {
            out.entry(section.clone())
                .or_default()
                .insert(k.trim().to_owned(), v.trim().to_owned());
        }
    }
    out
}

struct Plan {
    /// An ini fragment for `dconf load`, when something must be written.
    load: Option<String>,
    /// Relative paths (`custom-keybindings/nook-x`) to delete.
    reset: Vec<String>,
}

fn plan(current: &Dump, wanted: &[Shortcut], exe: &str) -> Plan {
    let list_raw = current
        .get("/")
        .and_then(|s| s.get("custom-keybindings"))
        .cloned()
        .unwrap_or_else(|| "@as []".into());
    let mut list = parse_list(&list_raw);
    let before = list.clone();
    let path_of = |id: &str| format!("{ROOT}custom-keybindings/nook-{}/", slug(id));
    let mut sections = String::new();
    let mut reset = Vec::new();
    for s in wanted {
        let Some(binding) = gnome_accel(&s.accel) else {
            continue;
        };
        let rel = format!("custom-keybindings/nook-{}", slug(&s.id));
        let command = format!("{} --hotkey={}", quote_exec(exe), s.id);
        let existing = current.get(&rel);
        let mut keys = Vec::new();
        match existing {
            // Keep the user's binding and name; only fix the command.
            Some(e) => {
                if e.get("command").map(String::as_str) != Some(&quote(&command)) {
                    keys.push(format!("command={}", quote(&command)));
                }
            }
            None => {
                keys.push(format!("binding={}", quote(&binding)));
                keys.push(format!("command={}", quote(&command)));
                keys.push(format!("name={}", quote(&format!("Nook: {}", s.name))));
            }
        }
        if !keys.is_empty() {
            sections.push_str(&format!("\n[{rel}]\n{}\n", keys.join("\n")));
        }
        let path = path_of(&s.id);
        if !list.contains(&path) {
            list.push(path);
        }
    }
    // Entries of shortcuts Nook no longer has.
    let keep: Vec<String> = wanted.iter().map(|s| path_of(&s.id)).collect();
    list.retain(|p| {
        let ours = p.starts_with(&format!("{ROOT}custom-keybindings/nook-"));
        if ours && !keep.contains(p) {
            reset.push(p.trim_start_matches(ROOT).trim_end_matches('/').to_owned());
            return false;
        }
        true
    });
    let mut ini = String::new();
    if list != before {
        let items: Vec<String> = list.iter().map(|p| quote(p)).collect();
        ini.push_str(&format!("[/]\ncustom-keybindings=[{}]\n", items.join(", ")));
    }
    ini.push_str(&sections);
    Plan {
        load: (!ini.trim().is_empty()).then_some(ini),
        reset,
    }
}

/// `['/a/', '/b/']` or `@as []` into its strings.
fn parse_list(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\'' {
            continue;
        }
        let mut s = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        s.push(n);
                    }
                }
                '\'' => break,
                c => s.push(c),
            }
        }
        out.push(s);
    }
    out
}

/// A GVariant string literal.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Quotes a path for the command GNOME runs (it splits it like a shell).
fn quote_exec(path: &str) -> String {
    if path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c))
    {
        path.to_owned()
    } else {
        format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// A dconf-safe path segment from an id (plugin ids carry dots).
fn slug(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// `ctrl+alt+KeyC` -> `<Control><Alt>c`, `shift+super+F5` -> `<Shift><Super>F5`.
pub fn gnome_accel(accel: &str) -> Option<String> {
    let mut mods = String::new();
    let mut key = None;
    for part in accel.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "controlleft" | "controlright" => mods.push_str("<Control>"),
            "alt" | "option" | "altleft" | "altright" => mods.push_str("<Alt>"),
            "shift" | "shiftleft" | "shiftright" => mods.push_str("<Shift>"),
            "super" | "meta" | "win" | "cmd" | "command" | "cmdorctrl" => mods.push_str("<Super>"),
            _ => key = Some(part),
        }
    }
    let key = key?;
    let name = if let Some(c) = key.strip_prefix("Key").filter(|c| c.len() == 1) {
        c.to_ascii_lowercase()
    } else if let Some(d) = key.strip_prefix("Digit").filter(|d| d.len() == 1) {
        d.to_owned()
    } else if key.len() == 1 {
        key.to_ascii_lowercase()
    } else {
        match key.to_ascii_lowercase().as_str() {
            "space" => "space".into(),
            "enter" | "return" => "Return".into(),
            "tab" => "Tab".into(),
            "escape" | "esc" => "Escape".into(),
            "backspace" => "BackSpace".into(),
            "delete" => "Delete".into(),
            "insert" => "Insert".into(),
            "home" => "Home".into(),
            "end" => "End".into(),
            "pageup" => "Page_Up".into(),
            "pagedown" => "Page_Down".into(),
            "arrowup" | "up" => "Up".into(),
            "arrowdown" | "down" => "Down".into(),
            "arrowleft" | "left" => "Left".into(),
            "arrowright" | "right" => "Right".into(),
            "printscreen" | "print" => "Print".into(),
            "minus" => "minus".into(),
            "equal" => "equal".into(),
            "comma" => "comma".into(),
            "period" => "period".into(),
            "slash" => "slash".into(),
            "backquote" => "grave".into(),
            f if f.starts_with('f') && f[1..].parse::<u8>().is_ok() => f.to_ascii_uppercase(),
            _ => return None,
        }
    };
    Some(format!("{mods}{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_accelerators() {
        assert_eq!(
            gnome_accel("ctrl+alt+KeyC").as_deref(),
            Some("<Control><Alt>c")
        );
        assert_eq!(
            gnome_accel("ctrl+alt+N").as_deref(),
            Some("<Control><Alt>n")
        );
        assert_eq!(
            gnome_accel("shift+super+F5").as_deref(),
            Some("<Shift><Super>F5")
        );
        assert_eq!(gnome_accel("ctrl+Digit1").as_deref(), Some("<Control>1"));
        assert_eq!(gnome_accel("ctrl+alt"), None);
    }

    #[test]
    fn plans_only_missing_entries() {
        let exe = "/usr/bin/nook";
        let wanted = vec![
            Shortcut {
                id: "toggle".into(),
                name: "Open the panel".into(),
                accel: "ctrl+alt+N".into(),
            },
            Shortcut {
                id: "coffee".into(),
                name: "Coffee".into(),
                accel: "ctrl+alt+KeyC".into(),
            },
        ];
        let dump = format!(
            "[/]\ncustom-keybindings=['{ROOT}custom-keybindings/custom0/', '{ROOT}custom-keybindings/nook-coffee/', '{ROOT}custom-keybindings/nook-old-plugin/']\n\n[custom-keybindings/nook-coffee]\nbinding='<Super>c'\ncommand='/usr/bin/nook --hotkey=coffee'\nname='Nook: Coffee'\n"
        );
        let p = plan(&parse_dump(&dump), &wanted, exe);
        let ini = p.load.expect("writes");
        // The user's own shortcut and binding stay.
        assert!(ini.contains("custom-keybindings/custom0/"));
        assert!(!ini.contains("<Super>c"));
        assert!(!ini.contains("[custom-keybindings/nook-coffee]"));
        // The missing one is added, the stale one removed.
        assert!(ini.contains("[custom-keybindings/nook-toggle]"));
        assert!(ini.contains("binding='<Control><Alt>n'"));
        assert!(ini.contains("command='/usr/bin/nook --hotkey=toggle'"));
        assert!(!ini.contains("nook-old-plugin"));
        assert_eq!(p.reset, ["custom-keybindings/nook-old-plugin"]);
        // Applied again: nothing to do.
        let applied = format!(
            "[/]\ncustom-keybindings=['{ROOT}custom-keybindings/custom0/', '{ROOT}custom-keybindings/nook-coffee/', '{ROOT}custom-keybindings/nook-toggle/']\n\n[custom-keybindings/nook-coffee]\nbinding='<Super>c'\ncommand='/usr/bin/nook --hotkey=coffee'\n\n[custom-keybindings/nook-toggle]\nbinding='<Control><Alt>n'\ncommand='/usr/bin/nook --hotkey=toggle'\nname='Nook: Open the panel'\n"
        );
        let again = plan(&parse_dump(&applied), &wanted, exe);
        assert!(again.load.is_none() && again.reset.is_empty());
    }
}
