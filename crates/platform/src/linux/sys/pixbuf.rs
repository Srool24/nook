//! Decoding pictures the `image` crate cannot read (JPEG XL, which Fedora's
//! own wallpapers use, WebP, AVIF, HEIF...) through GDK-Pixbuf's
//! thumbnailer, which has every loader the desktop has installed.

use std::path::Path;

/// `path` decoded, scaled to fit `size` x `size` when it had to go through
/// the thumbnailer.
pub fn open_any(path: &Path, size: u32) -> Option<image::DynamicImage> {
    if let Ok(img) = image::open(path) {
        return Some(img);
    }
    super::cmd::which("gdk-pixbuf-thumbnailer")?;
    let out = std::env::temp_dir().join(format!(
        "nook-thumb-{}-{}.png",
        std::process::id(),
        std::thread::current()
            .name()
            .unwrap_or("t")
            .replace('/', "_")
    ));
    let res = super::cmd::run(
        "gdk-pixbuf-thumbnailer",
        &[
            "-s",
            &size.to_string(),
            &path.display().to_string(),
            &out.display().to_string(),
        ],
    );
    let img = res.ok().and_then(|()| image::open(&out).ok());
    let _ = std::fs::remove_file(&out);
    img
}
