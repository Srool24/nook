//! Files for the shelf: dragging them out to other apps (XDND), the
//! thumbnails Files would show for them, and the file chooser.
//!
//! Dragging out speaks XDND from a connection of its own. The button press
//! that started the drag gave Nook's window an implicit pointer grab, so the
//! X server keeps reporting the pointer while it moves over other windows;
//! the drag follows it with `QueryPointer`, talks XDND to the window under
//! it, and serves the files as `text/uri-list`. Mutter bridges XDND to
//! Wayland apps, so Files, browsers and chat apps all take the drop.

use crate::sys::x11::{Ctx, X};
use crate::sys::{dbus, icons, md5, pixbuf, xdg};
use crate::{PlatformError, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, KeyButMask,
    PropMode, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, Window, WindowClass,
};
use zbus::zvariant::Value;

/// What the drop target did with the files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropResult {
    Copied,
    Moved,
    Linked,
    Cancelled,
}

/// Drags `paths` out of Nook. Call from the UI thread while the mouse
/// button is held; blocks until the drop.
pub fn drag_out(paths: &[PathBuf]) -> Result<DropResult> {
    let paths: Vec<&PathBuf> = paths.iter().filter(|p| p.exists()).collect();
    if paths.is_empty() {
        return Ok(DropResult::Cancelled);
    }
    let uris: String = paths
        .iter()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .map(|p| format!("{}\r\n", dbus::path_to_uri(&p)))
        .collect();
    let x = crate::sys::x11::open()?;
    Drag::new(&x, uris)?.run()
}

struct Drag<'a> {
    x: &'a X,
    source: Window,
    uris: String,
    target: Option<(Window, u32)>,
    accepted: bool,
    action: u32,
    status_pending: bool,
}

impl<'a> Drag<'a> {
    fn new(x: &'a X, uris: String) -> Result<Self> {
        let source = x.conn.generate_id().ctx("generate_id")?;
        x.conn
            .create_window(
                0,
                source,
                x.root,
                -10,
                -10,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new().override_redirect(1),
            )
            .ctx("CreateWindow(drag source)")?;
        x.conn
            .set_selection_owner(source, x.atoms.XdndSelection, x11rb::CURRENT_TIME)
            .ctx("SetSelectionOwner(XdndSelection)")?;
        x.flush();
        Ok(Self {
            x,
            source,
            uris,
            target: None,
            accepted: false,
            action: 0,
            status_pending: false,
        })
    }

    fn run(mut self) -> Result<DropResult> {
        let result = self.track();
        let _ = self.x.conn.destroy_window(self.source);
        self.x.flush();
        result
    }

    /// Follows the pointer until the button is released (or Esc), then
    /// drops on the last target that accepted.
    fn track(&mut self) -> Result<DropResult> {
        let started = Instant::now();
        let mut last = (i16::MIN, i16::MIN);
        loop {
            self.pump_events()?;
            let p = self
                .x
                .conn
                .query_pointer(self.x.root)
                .ctx("QueryPointer")?
                .reply()
                .ctx("QueryPointer")?;
            let held = p.mask.contains(KeyButMask::BUTTON1);
            if !held {
                // A release right at the start: the press came through
                // another path (touch); give it a moment.
                if started.elapsed() < Duration::from_millis(150) {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                return self.drop_here();
            }
            if (p.root_x, p.root_y) != last {
                last = (p.root_x, p.root_y);
                self.moved(p.root_x, p.root_y)?;
            }
            if started.elapsed() > Duration::from_secs(120) {
                self.leave();
                return Ok(DropResult::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(12));
        }
    }

    fn moved(&mut self, rx: i16, ry: i16) -> Result<()> {
        let found = self.find_target(rx, ry);
        if found.map(|t| t.0) != self.target.map(|t| t.0) {
            self.leave();
            if let Some((w, version)) = found {
                self.target = Some((w, version));
                self.accepted = false;
                // Version, no more than three types, then the types.
                let data = [
                    self.source,
                    version.min(5) << 24,
                    self.x.atoms.TEXT_URI_LIST,
                    0,
                    0,
                ];
                self.send(w, self.x.atoms.XdndEnter, data);
            }
        }
        if let Some((w, _)) = self.target {
            let data = [
                self.source,
                0,
                ((rx as u16 as u32) << 16) | ry as u16 as u32,
                x11rb::CURRENT_TIME,
                self.x.atoms.XdndActionCopy,
            ];
            self.send(w, self.x.atoms.XdndPosition, data);
            self.status_pending = true;
        }
        Ok(())
    }

    fn leave(&mut self) {
        if let Some((w, _)) = self.target.take() {
            self.send(w, self.x.atoms.XdndLeave, [self.source, 0, 0, 0, 0]);
        }
        self.accepted = false;
    }

    fn drop_here(&mut self) -> Result<DropResult> {
        // Wait briefly for the answer to the last position.
        let deadline = Instant::now() + Duration::from_millis(300);
        while self.status_pending && Instant::now() < deadline {
            self.pump_events()?;
            std::thread::sleep(Duration::from_millis(5));
        }
        let Some((w, _)) = self.target else {
            return Ok(DropResult::Cancelled);
        };
        if !self.accepted {
            self.leave();
            return Ok(DropResult::Cancelled);
        }
        self.send(
            w,
            self.x.atoms.XdndDrop,
            [self.source, 0, x11rb::CURRENT_TIME, 0, 0],
        );
        // The target now asks for the data and reports when it is done.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(result) = self.pump_events()? {
                return Ok(result);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(DropResult::Copied)
    }

    /// Handles queued events; returns the drop's outcome once the target
    /// reported it.
    fn pump_events(&mut self) -> Result<Option<DropResult>> {
        while let Some(ev) = self.x.conn.poll_for_event().ctx("poll_for_event")? {
            match ev {
                Event::ClientMessage(m) if m.type_ == self.x.atoms.XdndStatus => {
                    let d = m.data.as_data32();
                    self.status_pending = false;
                    self.accepted = d[1] & 1 != 0;
                    self.action = d[4];
                }
                Event::ClientMessage(m) if m.type_ == self.x.atoms.XdndFinished => {
                    let d = m.data.as_data32();
                    let action = if d[2] != 0 { d[2] } else { self.action };
                    return Ok(Some(if d[1] & 1 == 0 {
                        DropResult::Cancelled
                    } else if action == self.x.atoms.XdndActionMove {
                        DropResult::Moved
                    } else {
                        DropResult::Copied
                    }));
                }
                Event::SelectionRequest(r) => {
                    let mut property = r.property;
                    if r.target == self.x.atoms.TARGETS {
                        let targets = [self.x.atoms.TARGETS, self.x.atoms.TEXT_URI_LIST];
                        self.x
                            .set_prop32(r.requestor, r.property, AtomEnum::ATOM, &targets);
                    } else if r.target == self.x.atoms.TEXT_URI_LIST
                        || r.target == self.x.atoms.UTF8_STRING
                    {
                        let _ = self.x.conn.change_property(
                            PropMode::REPLACE,
                            r.requestor,
                            r.property,
                            r.target,
                            8,
                            self.uris.len() as u32,
                            self.uris.as_bytes(),
                        );
                    } else {
                        property = x11rb::NONE;
                    }
                    let notify = SelectionNotifyEvent {
                        response_type: SELECTION_NOTIFY_EVENT,
                        sequence: 0,
                        time: r.time,
                        requestor: r.requestor,
                        selection: r.selection,
                        target: r.target,
                        property,
                    };
                    let _ = self
                        .x
                        .conn
                        .send_event(false, r.requestor, EventMask::NO_EVENT, notify);
                    self.x.flush();
                }
                _ => {}
            }
        }
        Ok(None)
    }

    fn send(&self, window: Window, ty: u32, data: [u32; 5]) {
        let ev = ClientMessageEvent::new(32, window, ty, data);
        let _ = self
            .x
            .conn
            .send_event(false, window, EventMask::NO_EVENT, ev);
        self.x.flush();
    }

    /// The XDND-aware window under the pointer and its protocol version,
    /// never one of Nook's own windows.
    fn find_target(&self, rx: i16, ry: i16) -> Option<(Window, u32)> {
        let mut parent = self.x.root;
        for _ in 0..12 {
            let t = self
                .x
                .conn
                .translate_coordinates(self.x.root, parent, rx, ry)
                .ok()?
                .reply()
                .ok()?;
            let child = t.child;
            if child == x11rb::NONE {
                return None;
            }
            if let Some(&version) = self.x.prop32(child, self.x.atoms.XdndAware).first() {
                if self.x.pid_of(child) == Some(std::process::id()) {
                    return None;
                }
                return Some((child, version));
            }
            parent = child;
        }
        None
    }
}

/// Nothing to set up on Linux.
pub fn init_thumbnail_thread() {}

/// A thumbnail at most `size` pixels on each side: the picture for images,
/// the thumbnail Files made for videos and documents, the app's icon for a
/// desktop entry, the file type's icon otherwise. Returns `(width, height,
/// premultiplied RGBA)`.
pub fn thumbnail(path: &Path, size: i32) -> Result<(u32, u32, Vec<u8>)> {
    let size = size.max(1) as u32;
    let img = thumbnail_image(path, size)
        .ok_or_else(|| PlatformError::Other(format!("no thumbnail for {}", path.display())))?;
    let img = if img.width() > size || img.height() > size {
        img.thumbnail(size, size)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let premultiplied = rgba
        .pixels()
        .flat_map(|p| {
            let [r, g, b, a] = p.0;
            let m = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
            [m(r), m(g), m(b), a]
        })
        .collect();
    Ok((w, h, premultiplied))
}

fn thumbnail_image(path: &Path, size: u32) -> Option<image::DynamicImage> {
    if path.extension().is_some_and(|e| e == "desktop")
        && let Some(entry) = icons::desktop_entry(path)
    {
        let icon = icons::lookup(entry.icon.as_deref()?, size)?;
        return pixbuf::open_any(&icon, size);
    }
    if path.is_file() && is_picture(path) {
        return pixbuf::open_any(path, size);
    }
    if let Some(img) = cached_thumbnail(path, size) {
        return Some(img);
    }
    let icon = file_icon(path)?;
    pixbuf::open_any(&icon, size)
}

fn is_picture(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|e| {
            matches!(
                e.as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "bmp"
                    | "gif"
                    | "webp"
                    | "jxl"
                    | "avif"
                    | "heic"
                    | "heif"
                    | "tif"
                    | "tiff"
                    | "svg"
                    | "ico"
            )
        })
}

/// The freedesktop.org thumbnail cache entry for `path`, if Files (or any
/// thumbnailer) made one.
fn cached_thumbnail(path: &Path, size: u32) -> Option<image::DynamicImage> {
    let abs = std::fs::canonicalize(path).ok()?;
    let name = format!("{}.png", md5::hex(dbus::path_to_uri(&abs).as_bytes()));
    let cache = xdg::cache_home()?.join("thumbnails");
    let dirs = if size > 256 {
        ["xx-large", "x-large", "large", "normal"]
    } else if size > 128 {
        ["large", "x-large", "xx-large", "normal"]
    } else {
        ["normal", "large", "x-large", "xx-large"]
    };
    dirs.iter()
        .map(|d| cache.join(d).join(&name))
        .find(|p| p.exists())
        .and_then(|p| image::open(p).ok())
}

/// The theme icon for the file's type (folder, text, video...).
fn file_icon(path: &Path) -> Option<PathBuf> {
    let names: Vec<String> = if path.is_dir() {
        vec!["folder".into()]
    } else {
        let info = crate::sys::cmd::output(
            "gio",
            &["info", "-a", "standard::icon", &path.display().to_string()],
        )
        .ok()?;
        info.lines()
            .find_map(|l| l.trim().strip_prefix("standard::icon:"))
            .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect())
            .unwrap_or_default()
    };
    names
        .iter()
        .filter(|n| !n.ends_with("-symbolic"))
        .chain(names.iter())
        .find_map(|n| icons::lookup(n, 128))
        .or_else(|| icons::lookup("text-x-generic", 128))
}

/// Shows the system file picker with multi-select. Returns no paths if it
/// was cancelled. Blocks until the user answers; call it on its own thread.
pub fn pick_files(title: &str) -> Result<Vec<PathBuf>> {
    choose(title, false, true, None)
}

/// The file chooser portal: files or a folder, starting in `start`.
pub(crate) fn choose(
    title: &str,
    directory: bool,
    multiple: bool,
    start: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    let start = start.filter(|d| d.is_dir()).map(|d| {
        use std::os::unix::ffi::OsStrExt;
        let mut bytes = d.as_os_str().as_bytes().to_vec();
        bytes.push(0);
        bytes
    });
    let response = dbus::portal(
        "org.freedesktop.portal.FileChooser",
        "OpenFile",
        None,
        |token| {
            let mut options: HashMap<&str, Value<'_>> = HashMap::new();
            options.insert("handle_token", Value::from(token.to_owned()));
            options.insert("modal", Value::from(true));
            options.insert("multiple", Value::from(multiple));
            options.insert("directory", Value::from(directory));
            if let Some(bytes) = &start {
                options.insert("current_folder", Value::from(bytes.clone()));
            }
            ("", title.to_owned(), options)
        },
    )?;
    let dbus::Response::Ok(results) = response else {
        return Ok(Vec::new());
    };
    Ok(results
        .get("uris")
        .map(|v| dbus::as_strs(v))
        .unwrap_or_default()
        .iter()
        .filter_map(|u| dbus::uri_to_path(u))
        .collect())
}
