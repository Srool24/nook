//! Decoding pictures the `image` crate cannot read: SVG (theme and app
//! icons) with resvg, the rest (JPEG XL, which Fedora's own wallpapers use,
//! WebP, AVIF, HEIF...) through GDK-Pixbuf's thumbnailer, which has every
//! loader the desktop has installed.

use std::path::Path;

/// `path` decoded, scaled to fit `size` x `size` when it had to go through
/// the thumbnailer.
pub fn open_any(path: &Path, size: u32) -> Option<image::DynamicImage> {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
    {
        return render_svg(path, size);
    }
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

/// `path` rendered to fit `size` x `size`, keeping its aspect ratio.
fn render_svg(path: &Path, size: u32) -> Option<image::DynamicImage> {
    let data = std::fs::read(path).ok()?;
    let options = resvg::usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(&data, &options).ok()?;
    let tree_size = tree.size();
    let scale = size as f32 / tree_size.width().max(tree_size.height());
    let w = ((tree_size.width() * scale).round() as u32).max(1);
    let h = ((tree_size.height() * scale).round() as u32).max(1);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia stores premultiplied RGBA; callers expect straight alpha.
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    image::RgbaImage::from_raw(w, h, rgba).map(image::DynamicImage::ImageRgba8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_svg_icons() {
        let dir = std::env::temp_dir().join(format!("nook-svg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("icon.svg");
        std::fs::write(
            &file,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="8"><rect width="16" height="8" fill="#ff0000"/></svg>"##,
        )
        .unwrap();
        let img = open_any(&file, 64).unwrap().to_rgba8();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(img.dimensions(), (64, 32));
        assert_eq!(img.get_pixel(32, 16).0, [255, 0, 0, 255]);
    }
}
