//! Clipboard watching and writing for the clipboard history.
//!
//! A worker thread asks the X server (XFixes) to be told whenever the
//! `CLIPBOARD` selection changes owner, so it sleeps until something is
//! copied. Mutter mirrors the Wayland clipboard into XWayland, so copies in
//! Wayland apps arrive too. The worker reads a PNG or UTF-8 text, skips
//! content that password managers flag as secret
//! (`x-kde-passwordManagerHint`, set by KeePassXC, Bitwarden and others),
//! and sends entries to the UI.
//!
//! Writes go through `arboard`, which keeps serving what Nook copied for as
//! long as Nook runs. Nothing is persisted: the history lives in memory
//! only.

use crate::sys::x11::{Ctx, X};
use crate::{PlatformError, Result};
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, Property, Window, WindowClass,
};

/// Longest text kept, in bytes (about 1 MB).
const MAX_TEXT: usize = 1024 * 1024;
/// Largest thumbnail edge, in pixels.
pub const THUMB_SIZE: u32 = 240;
/// Largest image kept, in bytes of PNG.
const MAX_IMAGE: usize = 48 * 1024 * 1024;
/// Apps that copy in several steps announce first and fill in right after;
/// reading at once can catch them half way.
const READ_DELAY: Duration = Duration::from_millis(120);

/// A copied image: the full picture (for pasting back) and a thumbnail.
#[derive(Debug)]
pub struct ClipImage {
    pub width: u32,
    pub height: u32,
    /// Unpremultiplied RGBA8, at most [`THUMB_SIZE`] on its longest edge.
    pub thumb: Vec<u8>,
    pub thumb_size: [usize; 2],
    rgba: Vec<u8>,
}

impl PartialEq for ClipImage {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width && self.height == other.height && self.rgba == other.rgba
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClipContent {
    Text(String),
    Image(Arc<ClipImage>),
}

/// Handle to the watcher thread. Dropping it stops the thread.
pub struct ClipboardWatcher {
    rx: Receiver<ClipContent>,
    stop: Arc<AtomicBool>,
    window: Window,
}

impl ClipboardWatcher {
    /// Starts watching. `on_change` runs (on the worker thread) after each
    /// new entry.
    pub fn spawn(on_change: impl Fn() + Send + 'static) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        std::thread::Builder::new()
            .name("nook-clipboard".into())
            .spawn(move || {
                let reader = match Reader::new() {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(reader.window));
                reader.watch(&thread_stop, |c| {
                    if tx.send(c).is_ok() {
                        on_change();
                    }
                });
            })
            .map_err(|e| PlatformError::other("spawning clipboard thread", e))?;
        let window = ready_rx
            .recv()
            .map_err(|_| PlatformError::Other("clipboard thread exited".into()))??;
        Ok(Self { rx, stop, window })
    }

    /// Entries copied since the last call, oldest first.
    pub fn poll(&self) -> Vec<ClipContent> {
        self.rx.try_iter().collect()
    }

    /// Puts `content` back on the clipboard.
    pub fn set(&self, content: ClipContent) {
        let res = match &content {
            ClipContent::Text(t) => copy_text(t),
            ClipContent::Image(img) => with_clipboard(|cb| {
                cb.set_image(arboard::ImageData {
                    width: img.width as usize,
                    height: img.height as usize,
                    bytes: Cow::Borrowed(&img.rgba),
                })
            }),
        };
        if let Err(e) = res {
            tracing::warn!("clipboard write: {e}");
        }
    }
}

impl Drop for ClipboardWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the worker out of its wait.
        if let Ok(x) = crate::sys::x11::x() {
            let ev = x11rb::protocol::xproto::ClientMessageEvent::new(
                32,
                self.window,
                x.atoms.WM_PROTOCOLS,
                [0u32; 5],
            );
            let _ = x
                .conn
                .send_event(false, self.window, EventMask::NO_EVENT, ev);
            x.flush();
        }
    }
}

/// The process-wide `arboard` handle. It owns the selections Nook sets and
/// serves them while it lives, so it lives as long as Nook.
fn with_clipboard<T>(
    f: impl FnOnce(&mut arboard::Clipboard) -> std::result::Result<T, arboard::Error>,
) -> Result<T> {
    static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);
    let mut guard = CLIPBOARD.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(arboard::Clipboard::new().map_err(|e| PlatformError::other("clipboard", e))?);
    }
    let cb = guard.as_mut().expect("set above");
    f(cb).map_err(|e| PlatformError::other("clipboard", e))
}

/// Puts `text` on the clipboard, from any thread.
pub fn copy_text(text: &str) -> Result<()> {
    with_clipboard(|cb| cb.set_text(text.to_owned()))
}

/// The text on the clipboard, if any, from any thread.
pub fn read_text() -> Option<String> {
    with_clipboard(|cb| cb.get_text()).ok()
}

/// Puts an image on the clipboard (as PNG, which every Linux app reads),
/// from any thread.
pub fn copy_image(frame: &crate::capture::Frame) -> Result<()> {
    let rgba: Vec<u8> = frame
        .pixels
        .iter()
        .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, 0xFF])
        .collect();
    with_clipboard(|cb| {
        cb.set_image(arboard::ImageData {
            width: frame.width as usize,
            height: frame.height as usize,
            bytes: Cow::Owned(rgba),
        })
    })
}

/// Reads the clipboard on the watcher's own connection.
struct Reader {
    x: X,
    window: Window,
    png: Atom,
    incr: Atom,
    text_plain: Atom,
}

impl Reader {
    fn new() -> Result<Self> {
        let x = crate::sys::x11::open()?;
        let window = x.conn.generate_id().ctx("generate_id")?;
        x.conn
            .create_window(
                0,
                window,
                x.root,
                -10,
                -10,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .ctx("CreateWindow(clipboard)")?;
        x.conn
            .xfixes_query_version(5, 0)
            .ctx("XFixesQueryVersion")?
            .reply()
            .ctx("XFixesQueryVersion")?;
        x.conn
            .xfixes_select_selection_input(
                window,
                x.atoms.CLIPBOARD,
                SelectionEventMask::SET_SELECTION_OWNER,
            )
            .ctx("XFixesSelectSelectionInput")?;
        let intern = |name: &[u8]| -> Result<Atom> {
            Ok(x.conn
                .intern_atom(false, name)
                .ctx("InternAtom")?
                .reply()
                .ctx("InternAtom")?
                .atom)
        };
        let png = intern(b"image/png")?;
        let incr = intern(b"INCR")?;
        let text_plain = intern(b"text/plain;charset=utf-8")?;
        x.flush();
        Ok(Self {
            x,
            window,
            png,
            incr,
            text_plain,
        })
    }

    fn watch(&self, stop: &AtomicBool, mut emit: impl FnMut(ClipContent)) {
        loop {
            let ev = match self.x.conn.wait_for_event() {
                Ok(ev) => ev,
                Err(e) => {
                    tracing::warn!("clipboard watcher: {e}");
                    return;
                }
            };
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Event::XfixesSelectionNotify(n) = ev else {
                continue;
            };
            if n.owner == x11rb::NONE {
                continue;
            }
            std::thread::sleep(READ_DELAY);
            match self.read() {
                Ok(Some(c)) => emit(c),
                Ok(None) => {}
                Err(e) => tracing::debug!("reading the clipboard: {e}"),
            }
        }
    }

    fn read(&self) -> Result<Option<ClipContent>> {
        let targets: Vec<Atom> = self
            .convert(self.x.atoms.TARGETS)?
            .map(|(_, data)| {
                data.chunks_exact(4)
                    .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect()
            })
            .unwrap_or_default();
        if targets.contains(&self.x.atoms.PASSWORD_HINT) {
            tracing::debug!("skipping clipboard content flagged as secret");
            return Ok(None);
        }
        if targets.contains(&self.png)
            && let Some((_, png)) = self.convert(self.png)?
        {
            if png.len() > MAX_IMAGE {
                return Ok(None);
            }
            return Ok(decode_png(&png).map(|img| ClipContent::Image(Arc::new(img))));
        }
        for target in [
            self.x.atoms.UTF8_STRING,
            self.text_plain,
            u32::from(AtomEnum::STRING),
        ] {
            if !targets.is_empty() && !targets.contains(&target) {
                continue;
            }
            if let Some((_, bytes)) = self.convert(target)? {
                let mut text = String::from_utf8_lossy(&bytes).into_owned();
                if text.trim().is_empty() {
                    return Ok(None);
                }
                if text.len() > MAX_TEXT {
                    let mut cut = MAX_TEXT;
                    while !text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    text.truncate(cut);
                }
                return Ok(Some(ClipContent::Text(text)));
            }
        }
        Ok(None)
    }

    /// Asks the clipboard owner for `target` and waits for the data
    /// (following the INCR protocol for large transfers).
    fn convert(&self, target: Atom) -> Result<Option<(Atom, Vec<u8>)>> {
        let prop = self.x.atoms.NOOK_SELECTION;
        self.x
            .conn
            .convert_selection(
                self.window,
                self.x.atoms.CLIPBOARD,
                target,
                prop,
                x11rb::CURRENT_TIME,
            )
            .ctx("ConvertSelection")?;
        self.x.flush();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if Instant::now() > deadline {
                return Ok(None);
            }
            match self.x.conn.poll_for_event().ctx("poll_for_event")? {
                Some(Event::SelectionNotify(n)) if n.requestor == self.window => {
                    if n.property == x11rb::NONE {
                        return Ok(None);
                    }
                    break;
                }
                Some(_) => continue,
                None => std::thread::sleep(Duration::from_millis(4)),
            }
        }
        let reply = self
            .x
            .conn
            .get_property(true, self.window, prop, AtomEnum::ANY, 0, u32::MAX / 4)
            .ctx("GetProperty")?
            .reply()
            .ctx("GetProperty")?;
        self.x.flush();
        if reply.type_ != self.incr {
            return Ok(Some((reply.type_, reply.value)));
        }
        // INCR: deleting the property asked for the first chunk; each new
        // value is a chunk, an empty one ends the transfer.
        let mut data = Vec::new();
        let mut ty = x11rb::NONE;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match self.x.conn.poll_for_event().ctx("poll_for_event")? {
                Some(Event::PropertyNotify(p))
                    if p.window == self.window
                        && p.atom == prop
                        && p.state == Property::NEW_VALUE =>
                {
                    let chunk = self
                        .x
                        .conn
                        .get_property(true, self.window, prop, AtomEnum::ANY, 0, u32::MAX / 4)
                        .ctx("GetProperty(INCR)")?
                        .reply()
                        .ctx("GetProperty(INCR)")?;
                    self.x.flush();
                    if chunk.value.is_empty() {
                        return Ok(Some((ty, data)));
                    }
                    ty = chunk.type_;
                    data.extend_from_slice(&chunk.value);
                    if data.len() > MAX_IMAGE {
                        return Ok(None);
                    }
                }
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        Ok(None)
    }
}

fn decode_png(png: &[u8]) -> Option<ClipImage> {
    let decoded = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .inspect_err(|e| tracing::debug!("decoding clipboard image: {e}"))
        .ok()?;
    let (width, height) = (decoded.width(), decoded.height());
    let rgba = decoded.to_rgba8().into_raw();
    let thumb_img = if width.max(height) > THUMB_SIZE {
        decoded.thumbnail(THUMB_SIZE, THUMB_SIZE)
    } else {
        decoded
    };
    let thumb = thumb_img.to_rgba8();
    Some(ClipImage {
        width,
        height,
        thumb_size: [thumb.width() as usize, thumb.height() as usize],
        thumb: thumb.into_raw(),
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_png_with_its_thumbnail() {
        let img = image::RgbaImage::from_fn(600, 300, |x, _| image::Rgba([x as u8, 0, 0, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let clip = decode_png(&png.into_inner()).expect("decodes");
        assert_eq!((clip.width, clip.height), (600, 300));
        assert_eq!(clip.thumb_size, [240, 120]);
        assert_eq!(clip.thumb.len(), 240 * 120 * 4);
        assert_eq!(clip.rgba.len(), 600 * 300 * 4);
    }
}
