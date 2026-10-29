//! Screen grabs (physical pixels) for the snip overlay, OCR, lens and the
//! color picker.
//!
//! `BitBlt` from the screen DC returns what DWM composited, layered and
//! accelerated windows included. Nook's own windows are left out of the
//! grab: they are marked `WDA_EXCLUDEFROMCAPTURE` for the moment of the
//! copy, so the open panel never shows up in what you select.
//!
//! On a GNOME Wayland session only the compositor can see the whole
//! screen, so the grab goes through the desktop portal's `Screenshot`
//! (GNOME asks once whether Nook may take screenshots); Nook's windows are
//! faded out for that moment. In an X11 session the root window is read
//! directly.

#[cfg(windows)]
use crate::Win32Context;
use crate::monitor::Rect;
use crate::{PlatformError, Result};
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM};
#[cfg(windows)]
use windows::Win32::Graphics::Dwm::DwmFlush;
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC,
    SRCCOPY, SelectObject,
};
#[cfg(windows)]
use windows::Win32::System::Threading::GetCurrentProcessId;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowDisplayAffinity, GetWindowThreadProcessId, IsWindowVisible,
    SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WINDOW_DISPLAY_AFFINITY,
};
#[cfg(windows)]
use windows::core::BOOL;

/// A captured image: opaque BGRA pixels (`0xFFRRGGBB`), top-down.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Screen position of the top-left pixel.
    pub origin: (i32, i32),
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

impl Frame {
    pub fn rect(&self) -> Rect {
        Rect {
            left: self.origin.0,
            top: self.origin.1,
            right: self.origin.0 + self.width as i32,
            bottom: self.origin.1 + self.height as i32,
        }
    }

    /// `[r, g, b]` at frame coordinates, clamped to the edges.
    pub fn rgb(&self, x: i32, y: i32) -> [u8; 3] {
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        let p = self.pixels[y * self.width as usize + x];
        [(p >> 16) as u8, (p >> 8) as u8, p as u8]
    }

    /// The part of the frame inside `r` (screen coordinates), or `None` if
    /// they do not overlap.
    pub fn crop(&self, r: Rect) -> Option<Frame> {
        crop_pixels(&self.pixels, self.width, self.origin, r)
    }

    /// Packed BGRA bytes (what `SoftwareBitmap` and GDI expect).
    pub fn bgra_bytes(&self) -> Vec<u8> {
        self.pixels.iter().flat_map(|p| p.to_le_bytes()).collect()
    }

    pub fn to_rgb_image(&self) -> image::RgbImage {
        let raw = self
            .pixels
            .iter()
            .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8])
            .collect();
        image::RgbImage::from_raw(self.width, self.height, raw).unwrap_or_default()
    }

    /// PNG bytes at full size.
    pub fn encode_png(&self) -> Result<Vec<u8>> {
        let mut out = std::io::Cursor::new(Vec::new());
        self.to_rgb_image()
            .write_to(&mut out, image::ImageFormat::Png)
            .map_err(|e| PlatformError::Other(format!("encoding PNG: {e}")))?;
        Ok(out.into_inner())
    }

    /// JPEG bytes, scaled down so the longest edge is at most `max_edge`.
    pub fn encode_jpeg(&self, max_edge: u32, quality: u8) -> Result<Vec<u8>> {
        let mut img = image::DynamicImage::ImageRgb8(self.to_rgb_image());
        if img.width().max(img.height()) > max_edge {
            img = img.resize(max_edge, max_edge, image::imageops::FilterType::Triangle);
        }
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode_image(&img)
            .map_err(|e| PlatformError::Other(format!("encoding JPEG: {e}")))?;
        Ok(out)
    }
}

/// The part of a `width`-wide, top-down pixel buffer at screen position
/// `origin` that lies inside `r`, or `None` if they do not overlap.
pub fn crop_pixels(pixels: &[u32], width: u32, origin: (i32, i32), r: Rect) -> Option<Frame> {
    let height = (pixels.len() / width.max(1) as usize) as i32;
    let (l, t) = (r.left.max(origin.0), r.top.max(origin.1));
    let (rr, bb) = (
        r.right.min(origin.0 + width as i32),
        r.bottom.min(origin.1 + height),
    );
    if rr <= l || bb <= t {
        return None;
    }
    let (w, h) = ((rr - l) as usize, (bb - t) as usize);
    let (ox, oy) = ((l - origin.0) as usize, (t - origin.1) as usize);
    let stride = width as usize;
    let mut out = Vec::with_capacity(w * h);
    for y in oy..oy + h {
        out.extend_from_slice(&pixels[y * stride + ox..y * stride + ox + w]);
    }
    Some(Frame {
        origin: (l, t),
        width: w as u32,
        height: h as u32,
        pixels: out,
    })
}

/// Copies `r` from the screen, leaving Nook's own windows out.
#[cfg(target_os = "linux")]
pub fn grab(r: Rect) -> Result<Frame> {
    linux::grab_desktop()?
        .crop(r)
        .ok_or_else(|| PlatformError::Other("capture rectangle is off screen".into()))
}

/// Copies `r` from the screen, leaving Nook's own windows out.
#[cfg(windows)]
pub fn grab(r: Rect) -> Result<Frame> {
    let hidden = hide_own_windows();
    let frame = blit(r);
    for (hwnd, affinity) in hidden {
        // SAFETY: restoring the affinity we read from our own window.
        let _ = unsafe { SetWindowDisplayAffinity(hwnd, affinity) };
    }
    frame
}

#[cfg(windows)]
fn blit(r: Rect) -> Result<Frame> {
    let (w, h) = (r.width(), r.height());
    if w <= 0 || h <= 0 {
        return Err(PlatformError::Other("empty capture rectangle".into()));
    }
    // SAFETY: screen DC, a compatible memory DC and bitmap, all released
    // before returning; `GetDIBits` writes exactly w * h 32-bit pixels.
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bitmap.into());
        let copied = BitBlt(
            mem,
            0,
            0,
            w,
            h,
            Some(screen),
            r.left,
            r.top,
            SRCCOPY | CAPTUREBLT,
        );
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u32; (w * h) as usize];
        SelectObject(mem, old);
        let lines = GetDIBits(
            mem,
            bitmap,
            0,
            h as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        copied.api("BitBlt(screen)")?;
        if lines != h {
            return Err(PlatformError::Other("GetDIBits(screen) failed".into()));
        }
        // GDI leaves the alpha byte undefined.
        pixels.iter_mut().for_each(|p| *p |= 0xFF00_0000);
        Ok(Frame {
            origin: (r.left, r.top),
            width: w as u32,
            height: h as u32,
            pixels,
        })
    }
}

/// Marks every visible top-level window of this process as excluded from
/// capture and waits for DWM to compose a frame without them. Returns the
/// previous affinities to restore.
#[cfg(windows)]
fn hide_own_windows() -> Vec<(HWND, WINDOW_DISPLAY_AFFINITY)> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is the `Vec` passed to `EnumWindows` below, alive
        // for the whole enumeration.
        let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
        let mut pid = 0;
        // SAFETY: plain queries on a window handle from the enumeration.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == GetCurrentProcessId() && IsWindowVisible(hwnd).as_bool() {
                list.push(hwnd);
            }
        }
        BOOL(1)
    }
    let mut own: Vec<HWND> = Vec::new();
    // SAFETY: the callback only touches `own` through `lparam`.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut own as *mut _ as isize)) };
    let mut changed = Vec::new();
    for hwnd in own {
        let mut affinity = 0u32;
        // SAFETY: our own top-level windows.
        unsafe {
            if GetWindowDisplayAffinity(hwnd, &mut affinity).is_ok()
                && SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_ok()
            {
                changed.push((hwnd, WINDOW_DISPLAY_AFFINITY(affinity)));
            }
        }
    }
    if !changed.is_empty() {
        // Two compositions: the first may already have been under way.
        // SAFETY: plain waits on DWM.
        unsafe {
            let _ = DwmFlush();
            let _ = DwmFlush();
        }
    }
    changed
}

#[cfg(target_os = "linux")]
mod linux {
    use super::Frame;
    use crate::sys::dbus;
    use crate::sys::x11::{Ctx, X, x};
    use crate::{PlatformError, Result};
    use std::collections::HashMap;
    use std::time::Duration;
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, ImageFormat};
    use zbus::zvariant::Value;

    /// The whole X screen (every monitor), in X screen pixels.
    pub fn grab_desktop() -> Result<Frame> {
        let x = x()?;
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
        let hidden = hide_own_windows(x);
        if !wayland {
            let frame = root_image(x);
            restore(x, hidden);
            return frame;
        }
        let shot = portal_screenshot();
        restore(x, hidden);
        let img = shot?;
        let setup = &x.conn.setup().roots[x.screen];
        let (w, h) = (
            u32::from(setup.width_in_pixels),
            u32::from(setup.height_in_pixels),
        );
        // The portal hands out device pixels; Nook works in X screen pixels,
        // which are logical ones under a scaled GNOME session.
        let img = if img.dimensions() != (w, h) {
            image::imageops::resize(&img, w, h, image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let pixels = img
            .pixels()
            .map(|p| {
                let [r, g, b] = p.0;
                0xFF00_0000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
            })
            .collect();
        Ok(Frame {
            origin: (0, 0),
            width: w,
            height: h,
            pixels,
        })
    }

    /// A non-interactive screenshot through the desktop portal, read and
    /// deleted again (it only exists for this grab).
    fn portal_screenshot() -> Result<image::RgbImage> {
        let response = dbus::portal(
            "org.freedesktop.portal.Screenshot",
            "Screenshot",
            Some(Duration::from_secs(20)),
            |token| {
                let mut options: HashMap<&str, Value<'_>> = HashMap::new();
                options.insert("handle_token", Value::from(token.to_owned()));
                options.insert("interactive", Value::from(false));
                options.insert("modal", Value::from(false));
                ("", options)
            },
        )?;
        let dbus::Response::Ok(results) = response else {
            return Err(PlatformError::Other("screenshot was refused".into()));
        };
        let uri = results
            .get("uri")
            .and_then(|v| dbus::as_str(v))
            .ok_or_else(|| PlatformError::Other("screenshot portal returned no file".into()))?;
        let path = dbus::uri_to_path(&uri)
            .ok_or_else(|| PlatformError::Other(format!("unexpected screenshot uri {uri}")))?;
        let img = image::open(&path)
            .map_err(|e| PlatformError::other("reading the screenshot", e))?
            .to_rgb8();
        let _ = std::fs::remove_file(&path);
        Ok(img)
    }

    /// Reads the root window (an X11 session shows everything there).
    fn root_image(x: &X) -> Result<Frame> {
        let setup = &x.conn.setup().roots[x.screen];
        let (w, h) = (setup.width_in_pixels, setup.height_in_pixels);
        let mut pixels = Vec::with_capacity(usize::from(w) * usize::from(h));
        let strip: u16 = 128;
        let mut y = 0u16;
        while y < h {
            let rows = strip.min(h - y);
            let reply = x
                .conn
                .get_image(ImageFormat::Z_PIXMAP, x.root, 0, y as i16, w, rows, !0)
                .ctx("GetImage")?
                .reply()
                .ctx("GetImage")?;
            let data = reply.data;
            let stride = data.len() / usize::from(rows).max(1);
            for row in 0..usize::from(rows) {
                let line = &data[row * stride..row * stride + usize::from(w) * 4];
                pixels.extend(
                    line.chunks_exact(4)
                        .map(|c| 0xFF00_0000 | u32::from_le_bytes([c[0], c[1], c[2], 0])),
                );
            }
            y += rows;
        }
        Ok(Frame {
            origin: (0, 0),
            width: u32::from(w),
            height: u32::from(h),
            pixels,
        })
    }

    /// Nook's visible windows and their opacity before they were faded out.
    type Hidden = Vec<(u32, Option<u32>)>;

    /// Fades Nook's own windows out for the grab (they stay mapped, so
    /// nothing re-lays out) and waits for the compositor to draw a frame
    /// without them. GNOME always composites; on an X server without a
    /// compositor the opacity does nothing and the panel stays in the grab.
    fn hide_own_windows(x: &X) -> Hidden {
        let pid = std::process::id();
        let prop = x.atoms._NET_WM_WINDOW_OPACITY;
        let mut hidden = Vec::new();
        let mut windows = x.clients();
        // Without a window manager there is no client list.
        if windows.is_empty()
            && let Ok(tree) = x
                .conn
                .query_tree(x.root)
                .map_err(|_| ())
                .and_then(|c| c.reply().map_err(|_| ()))
        {
            windows = tree.children;
        }
        for w in windows {
            if x.pid_of(w) != Some(pid) || !x.is_viewable(w) {
                continue;
            }
            let before = x.prop32(w, prop).first().copied();
            x.set_prop32(w, prop, AtomEnum::CARDINAL, &[0]);
            hidden.push((w, before));
        }
        x.flush();
        if !hidden.is_empty() {
            std::thread::sleep(Duration::from_millis(90));
        }
        hidden
    }

    fn restore(x: &X, hidden: Hidden) {
        let prop = x.atoms._NET_WM_WINDOW_OPACITY;
        for (w, before) in hidden {
            match before {
                Some(v) => x.set_prop32(w, prop, AtomEnum::CARDINAL, &[v]),
                None => x.delete_prop(w, prop),
            }
        }
        x.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        Frame {
            origin: (100, 50),
            width: 4,
            height: 3,
            pixels: (0..12).map(|i| 0xFF00_0000 | i).collect(),
        }
    }

    #[test]
    fn crops_in_screen_coordinates() {
        let f = frame();
        let c = f
            .crop(Rect {
                left: 101,
                top: 51,
                right: 200,
                bottom: 200,
            })
            .expect("overlaps");
        assert_eq!((c.origin, c.width, c.height), ((101, 51), 3, 2));
        assert_eq!(c.pixels[0] & 0xFF, 5);
        assert_eq!(c.pixels[5] & 0xFF, 11);
        assert!(
            f.crop(Rect {
                left: 0,
                top: 0,
                right: 100,
                bottom: 50
            })
            .is_none()
        );
    }

    #[test]
    fn reads_rgb_clamped() {
        let f = frame();
        assert_eq!(f.rgb(-5, -5), [0, 0, 0]);
        assert_eq!(f.rgb(99, 99), [0, 0, 11]);
    }
}
