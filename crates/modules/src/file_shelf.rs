//! File shelf: park files on the notch and drag them out later, like
//! Dropover or Yoink.
//!
//! Drag files onto the notch (the pill opens as they arrive) and drop them
//! anywhere on it. The tile shows how many are parked; drag the tile to
//! take all of them, or right click it (or click) for the shelf page,
//! where each file shows its thumbnail. There, files are selected as in a
//! file manager (click, Ctrl+click to add or drop one, Shift+click for a
//! range, Ctrl+A for all, Esc for none), and dragging a selected file
//! copies all the selected ones out: into a folder, or onto an email as
//! attachments. Double click opens a file; Delete takes the selected ones
//! off the shelf.
//!
//! The shelf keeps references, not copies: the files stay where they are,
//! and ones that were moved away (dragged out with Shift, deleted) drop off.
//! The list survives restarts in `%APPDATA%\Nook\shelf.txt`.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align2, Color32, ColorImage, Id, Pos2, Rect, Sense, TextureHandle, TextureOptions, Ui, pos2,
    vec2,
};
use nook_platform::files::{self, DropResult};
use nook_platform::{shell, window};
use nook_ui::module::{Badge, Module, TileCtx, TileSize};
use nook_ui::theme::{SIZE_CAPTION, SIZE_ICON, SIZE_LABEL, Theme, with_alpha};
use nook_ui::{anim, icons, paint, widgets};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

const COLS: usize = 5;
const GAP: f32 = 8.0;
const CARD_H: f32 = 84.0;
/// Thumbnail size requested from the shell, pixels.
const THUMB: i32 = 96;

struct Item {
    path: PathBuf,
    name: String,
    size: Option<u64>,
    dir: bool,
    thumb: Option<TextureHandle>,
    /// A thumbnail was asked for.
    asked: bool,
}

impl Item {
    fn new(path: PathBuf) -> Self {
        let meta = std::fs::metadata(&path).ok();
        Self {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            size: meta.as_ref().filter(|m| m.is_file()).map(|m| m.len()),
            dir: meta.is_some_and(|m| m.is_dir()),
            path,
            thumb: None,
            asked: false,
        }
    }
}

/// "12 KB", "3.4 MB".
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 || v >= 10.0 {
        format!("{v:.0} {}", UNITS[unit])
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

fn shelf_file() -> Option<PathBuf> {
    shell::app_dir().map(|d| d.join("shelf.txt"))
}

/// Paths on the shelf from `text` (one per line), dropping blanks.
fn parse(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

type Thumb = (PathBuf, ColorImage);

/// A worker that asks the shell for thumbnails.
fn spawn_thumbnailer(ctx: egui::Context) -> Option<(Sender<PathBuf>, Receiver<Thumb>)> {
    let (req_tx, req_rx) = mpsc::channel::<PathBuf>();
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("nook-thumbs".into())
        .spawn(move || {
            files::init_thumbnail_thread();
            for path in req_rx {
                match files::thumbnail(&path, THUMB) {
                    Ok((w, h, rgba)) => {
                        let img =
                            ColorImage::from_rgba_premultiplied([w as usize, h as usize], &rgba);
                        if tx.send((path, img)).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                    Err(e) => tracing::debug!("thumbnail {}: {e}", path.display()),
                }
            }
        });
    match spawned {
        Ok(_) => Some((req_tx, rx)),
        Err(e) => {
            tracing::warn!("spawning thumbnail thread: {e}");
            None
        }
    }
}

#[derive(Default)]
pub struct FileShelf {
    items: Vec<Item>,
    thumbs: Option<(Sender<PathBuf>, Receiver<Thumb>)>,
    /// Thumbnails that arrived, waiting for a draw to upload them.
    pending: Vec<Thumb>,
    open_page: bool,
    ctx: Option<egui::Context>,
    /// Selected files on the shelf page.
    selected: HashSet<PathBuf>,
    /// Where a Shift+click range starts.
    anchor: Option<usize>,
}

impl FileShelf {
    fn save(&self) {
        let Some(file) = shelf_file() else {
            return;
        };
        let text: String = self
            .items
            .iter()
            .map(|i| format!("{}\n", i.path.display()))
            .collect();
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&file, text) {
            tracing::warn!("file shelf: saving {}: {e}", file.display());
        }
    }

    fn add(&mut self, paths: Vec<PathBuf>) {
        let mut added = 0;
        for p in paths {
            if !self.items.iter().any(|i| i.path == p) && p.exists() {
                self.items.push(Item::new(p));
                added += 1;
            }
        }
        if added > 0 {
            tracing::info!("file shelf: {added} added, {} parked", self.items.len());
            self.save();
        }
    }

    /// Drops files that are gone (moved, deleted).
    fn prune(&mut self) {
        let before = self.items.len();
        self.items.retain(|i| i.path.exists());
        self.forget_missing_selection();
        if self.items.len() != before {
            self.save();
        }
    }

    /// Drags `paths` out; blocks until the drop (the shell runs the drag).
    fn drag_out(&mut self, paths: Vec<PathBuf>) {
        let result = files::drag_out(&paths);
        // The drag loop ate the button release; tell egui it happened.
        if let Some(main) = window::find_own("Nook") {
            window::post_left_button_up(main);
        }
        match result {
            Ok(r) => {
                tracing::info!("file shelf: {} file(s) dragged out, {r:?}", paths.len());
                if r == DropResult::Moved {
                    self.prune();
                }
            }
            Err(e) => tracing::warn!("file shelf: drag out: {e}"),
        }
    }

    fn forget_missing_selection(&mut self) {
        let items = &self.items;
        self.selected.retain(|p| items.iter().any(|i| &i.path == p));
        if self.anchor.is_some_and(|a| a >= self.items.len()) {
            self.anchor = None;
        }
    }

    /// The selected files, in shelf order.
    fn selection(&self) -> Vec<PathBuf> {
        self.items
            .iter()
            .filter(|i| self.selected.contains(&i.path))
            .map(|i| i.path.clone())
            .collect()
    }

    /// A click on card `i`, with Ctrl (toggle) or Shift (range from the
    /// last click) held, as a file manager does.
    fn click(&mut self, i: usize, toggle: bool, range: bool) {
        let path = self.items[i].path.clone();
        match (range, self.anchor) {
            (true, Some(a)) => {
                let (lo, hi) = (a.min(i), a.max(i));
                if !toggle {
                    self.selected.clear();
                }
                for item in &self.items[lo..=hi] {
                    self.selected.insert(item.path.clone());
                }
                return;
            }
            _ if toggle => {
                if !self.selected.remove(&path) {
                    self.selected.insert(path);
                }
            }
            _ => {
                self.selected.clear();
                self.selected.insert(path);
            }
        }
        self.anchor = Some(i);
    }

    fn request_thumbs(&mut self) {
        let Some((tx, _)) = &self.thumbs else {
            return;
        };
        for item in self.items.iter_mut().filter(|i| !i.asked) {
            item.asked = true;
            let _ = tx.send(item.path.clone());
        }
    }

    fn card(&mut self, ui: &mut Ui, i: usize, card: Rect, theme: &Theme) -> CardAction {
        let mut action = CardAction::None;
        widgets::card(ui.painter(), card, theme);
        let id = Id::new(("shelf-card", &self.items[i].path));
        let resp = ui.interact(card, id, Sense::click_and_drag());
        let selected = self.selected.contains(&self.items[i].path);
        let sel = anim::tween(ui.ctx(), id.with("sel"), selected, 0.12);
        if sel > 0.0 {
            let radius = nook_ui::theme::TILE_RADIUS;
            ui.painter().rect_filled(
                card,
                radius,
                with_alpha(theme.accent, if theme.dark { 0.22 } else { 0.14 } * sel),
            );
            ui.painter().rect_stroke(
                card,
                radius,
                egui::Stroke::new(2.0, with_alpha(theme.accent, sel)),
                egui::StrokeKind::Inside,
            );
            // A check in the top left corner, so the state isn't color alone.
            let c = card.left_top() + vec2(12.0, 12.0);
            ui.painter()
                .circle_filled(c, 8.0, with_alpha(theme.accent, sel));
            paint::icon(
                ui.painter(),
                c,
                icons::CHECK,
                10.0,
                with_alpha(Color32::WHITE, sel),
                true,
            );
        }
        let hover = anim::tween(ui.ctx(), id.with("hover"), resp.hovered(), 0.12);
        if hover > 0.0 {
            let wash = if theme.dark {
                Color32::from_white_alpha(16)
            } else {
                Color32::from_black_alpha(10)
            };
            ui.painter()
                .rect_filled(card, nook_ui::theme::TILE_RADIUS, with_alpha(wash, hover));
        }
        let item = &self.items[i];
        let icon_box =
            Rect::from_center_size(pos2(card.center().x, card.min.y + 30.0), vec2(48.0, 40.0));
        match &item.thumb {
            Some(t) => {
                let s = t.size_vec2();
                let k = (icon_box.width() / s.x)
                    .min(icon_box.height() / s.y)
                    .min(1.0);
                ui.painter().image(
                    t.id(),
                    Rect::from_center_size(icon_box.center(), s * k),
                    Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => paint::icon(
                ui.painter(),
                icon_box.center(),
                if item.dir { icons::FOLDER } else { icons::FILE },
                30.0,
                theme.text_dim,
                false,
            ),
        }
        one_line(
            ui,
            pos2(card.min.x + 8.0, card.max.y - 30.0),
            &item.name,
            card.width() - 16.0,
            Theme::font(SIZE_LABEL),
            theme.text,
        );
        ui.painter().text(
            pos2(card.center().x, card.max.y - 9.0),
            Align2::CENTER_CENTER,
            match item.size {
                Some(b) => human_size(b),
                None if item.dir => "Folder".to_owned(),
                None => String::new(),
            },
            Theme::font(SIZE_LABEL - 1.0),
            theme.text_faint,
        );
        // Remove button on hover.
        if hover > 0.01 {
            let x = Rect::from_center_size(card.right_top() + vec2(-12.0, 12.0), vec2(18.0, 18.0));
            let xr = ui.interact(x, id.with("x"), Sense::click());
            let bg = if xr.hovered() {
                theme.text
            } else {
                theme.text_dim
            };
            ui.painter()
                .circle_filled(x.center(), 9.0, with_alpha(bg, hover));
            paint::icon(
                ui.painter(),
                x.center(),
                icons::X,
                11.0,
                with_alpha(
                    if theme.dark {
                        Color32::BLACK
                    } else {
                        Color32::WHITE
                    },
                    hover,
                ),
                true,
            );
            if xr.on_hover_text("Remove from the shelf").clicked() {
                return CardAction::Remove;
            }
        }
        // egui only sees keys while the window has the keyboard, which the
        // notch doesn't take for a click: ask the system too.
        let (toggle, range) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        let (toggle, range) = if resp.clicked() {
            let (ctrl, shift) = nook_platform::monitor::modifiers();
            (toggle || ctrl, range || shift)
        } else {
            (toggle, range)
        };
        if resp.drag_started() {
            action = CardAction::Drag;
        } else if resp.double_clicked() {
            action = CardAction::Open;
        } else if resp.clicked() {
            action = CardAction::Click { toggle, range };
        }
        resp.on_hover_text(format!(
            "{}\nClick to select (Ctrl or Shift for more), double click to open, drag to copy out",
            item.path.display()
        ));
        action
    }
}

enum CardAction {
    None,
    Click { toggle: bool, range: bool },
    Open,
    Drag,
    Remove,
}

fn one_line(ui: &Ui, pos: Pos2, text: &str, width: f32, font: egui::FontId, color: Color32) {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat::simple(font, color));
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    job.halign = egui::Align::Center;
    let galley = ui.painter().layout_job(job);
    let x = pos.x + width * 0.5;
    ui.painter().galley(pos2(x, pos.y), galley, color);
}

impl Module for FileShelf {
    fn id(&self) -> &'static str {
        "file_shelf"
    }

    fn name(&self) -> &str {
        "File shelf"
    }

    fn icon(&self) -> &'static str {
        icons::TRAY
    }

    fn size_hint(&self) -> TileSize {
        TileSize::Small
    }

    fn attach(&mut self, ctx: &egui::Context) {
        self.ctx = Some(ctx.clone());
        self.thumbs = spawn_thumbnailer(ctx.clone());
        if let Some(text) = shelf_file().and_then(|f| std::fs::read_to_string(f).ok()) {
            self.items = parse(&text)
                .into_iter()
                .filter(|p| p.exists())
                .map(Item::new)
                .collect();
        }
    }

    /// Lit while files are dragged over the notch.
    fn is_active(&self) -> bool {
        self.ctx
            .as_ref()
            .is_some_and(|c| c.input(|i| !i.raw.hovered_files.is_empty()))
    }

    fn collapsed_badge(&self) -> Option<Badge> {
        (!self.items.is_empty()).then_some(Badge::Glyph { icon: icons::TRAY })
    }

    fn accepts_files(&self) -> bool {
        true
    }

    fn drop_files(&mut self, paths: Vec<PathBuf>) {
        self.add(paths);
    }

    fn on_tick(&mut self) {
        if let Some((_, rx)) = &self.thumbs {
            self.pending.extend(rx.try_iter());
        }
    }

    fn take_page_request(&mut self) -> bool {
        std::mem::take(&mut self.open_page)
    }

    fn ui_tile(&mut self, ui: &mut Ui, ctx: &mut TileCtx<'_>) {
        let theme = ctx.theme;
        let hovering = ctx.files_hovering;
        self.load_pending(ui.ctx());
        let rect = ui.max_rect();
        let resp = ui.interact(
            rect.expand(8.0),
            ui.id().with("shelf-tile"),
            Sense::click_and_drag(),
        );
        let count = self.items.len();
        let (glyph, color, fill) = if hovering {
            (icons::DOWNLOAD_SIMPLE, theme.accent_fg(), true)
        } else if ctx.hovered {
            (icons::TRAY, theme.text, false)
        } else {
            (icons::TRAY, theme.text_dim, false)
        };
        paint::icon(
            ui.painter(),
            rect.center(),
            glyph,
            SIZE_ICON + 2.0,
            color,
            fill,
        );
        if count > 0 && !hovering {
            let c = rect.center() + vec2(11.0, -9.0);
            ui.painter().circle_filled(c, 7.5, theme.accent);
            ui.painter().text(
                c,
                Align2::CENTER_CENTER,
                count.min(99),
                Theme::font_semibold(SIZE_CAPTION),
                Color32::WHITE,
            );
        }
        if resp.drag_started() && count > 0 {
            let all = self.items.iter().map(|i| i.path.clone()).collect();
            self.drag_out(all);
        } else if resp.clicked() || resp.secondary_clicked() {
            self.prune();
            self.open_page = true;
        }
        resp.on_hover_text(if count > 0 {
            "Drag to take everything  \u{00B7}  Click to see the shelf"
        } else {
            "Drop files on the notch to park them here"
        });
    }

    fn ui_page(&mut self, ui: &mut Ui, header: &mut Ui, body: Rect, theme: &Theme) -> bool {
        self.load_pending(ui.ctx());
        if !self.items.is_empty()
            && widgets::icon_button(header, icons::BROOM, 26.0, false, theme)
                .on_hover_text("Clear the shelf (the files stay where they are)")
                .clicked()
        {
            self.items.clear();
            self.save();
        }
        self.forget_missing_selection();
        if !self.items.is_empty() {
            let picked = self.selection();
            let tip = match picked.len() {
                0 => "Drag to copy everything out".to_owned(),
                1 => "Drag to copy the selected file out".to_owned(),
                n => format!("Drag to copy the {n} selected files out"),
            };
            let drag = widgets::icon_button(header, icons::HAND_GRABBING, 26.0, false, theme)
                .interact(Sense::drag())
                .on_hover_text(tip);
            if drag.drag_started() {
                let paths = if picked.is_empty() {
                    self.items.iter().map(|i| i.path.clone()).collect()
                } else {
                    picked.clone()
                };
                self.drag_out(paths);
            }
            let all = picked.len() == self.items.len();
            let pick_all = widgets::icon_button(header, icons::SELECTION_ALL, 26.0, all, theme)
                .on_hover_text(if all { "Select none" } else { "Select all" });
            if pick_all.clicked() {
                if all {
                    self.selected.clear();
                    self.anchor = None;
                } else {
                    self.selected = self.items.iter().map(|i| i.path.clone()).collect();
                }
            }
            if !picked.is_empty() {
                header.label(
                    egui::RichText::new(format!("{} selected", picked.len()))
                        .font(Theme::font(SIZE_LABEL))
                        .color(theme.text_dim),
                );
            }
        }
        if self.items.is_empty() {
            let hovering = ui.input(|i| !i.raw.hovered_files.is_empty());
            paint::icon(
                ui.painter(),
                body.center() - vec2(0.0, 18.0),
                if hovering {
                    icons::DOWNLOAD_SIMPLE
                } else {
                    icons::TRAY
                },
                30.0,
                if hovering {
                    theme.accent_fg()
                } else {
                    theme.text_faint
                },
                hovering,
            );
            ui.painter().text(
                body.center() + vec2(0.0, 16.0),
                Align2::CENTER_CENTER,
                if hovering {
                    "Drop to park them here"
                } else {
                    "Drop files on the notch to park them here."
                },
                Theme::font(SIZE_LABEL + 1.0),
                theme.text_dim,
            );
            return false;
        }
        self.request_thumbs();
        let card_w = (body.width() - GAP * (COLS - 1) as f32) / COLS as f32;
        let rows = self.items.len().div_ceil(COLS);
        let mut action = None;
        egui::ScrollArea::vertical()
            .id_salt("nook-shelf-scroll")
            .max_height(body.height())
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (area, _) = ui.allocate_exact_size(
                    vec2(body.width(), rows as f32 * (CARD_H + GAP) - GAP),
                    Sense::hover(),
                );
                for i in 0..self.items.len() {
                    let card = Rect::from_min_size(
                        pos2(
                            area.min.x + (i % COLS) as f32 * (card_w + GAP),
                            area.min.y + (i / COLS) as f32 * (CARD_H + GAP),
                        ),
                        vec2(card_w, CARD_H),
                    );
                    if !ui.is_rect_visible(card) {
                        continue;
                    }
                    match self.card(ui, i, card, theme) {
                        CardAction::None => {}
                        a => action = Some((i, a)),
                    }
                }
            });
        match action {
            Some((i, CardAction::Click { toggle, range })) => self.click(i, toggle, range),
            Some((i, CardAction::Open)) => {
                if let Err(e) = shell::open(&self.items[i].path.to_string_lossy()) {
                    tracing::warn!("file shelf: open: {e}");
                }
            }
            Some((i, CardAction::Drag)) => {
                // A selected file takes the whole selection along; any
                // other file goes alone (and becomes the selection).
                let path = self.items[i].path.clone();
                if !self.selected.contains(&path) {
                    self.click(i, false, false);
                }
                let paths = self.selection();
                self.drag_out(paths);
            }
            Some((i, CardAction::Remove)) => {
                let path = self.items.remove(i).path;
                self.selected.remove(&path);
                self.anchor = None;
                self.save();
            }
            _ => {}
        }
        self.selection_keys(ui);
        false
    }
}

impl FileShelf {
    /// Ctrl+A selects all, Esc clears, Delete takes the selection off the
    /// shelf (the files stay where they are), Enter opens it. Only while
    /// no text field has the keyboard.
    fn selection_keys(&mut self, ui: &Ui) {
        if ui.ctx().text_edit_focused() {
            return;
        }
        let (all, none, delete, open) = ui.input(|i| {
            (
                i.modifiers.command && i.key_pressed(egui::Key::A),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::Delete),
                i.key_pressed(egui::Key::Enter),
            )
        });
        if all {
            self.selected = self.items.iter().map(|i| i.path.clone()).collect();
        }
        if none {
            self.selected.clear();
            self.anchor = None;
        }
        if delete && !self.selected.is_empty() {
            let selected = std::mem::take(&mut self.selected);
            self.items.retain(|i| !selected.contains(&i.path));
            self.anchor = None;
            self.save();
        }
        if open {
            for path in self.selection() {
                if let Err(e) = shell::open(&path.to_string_lossy()) {
                    tracing::warn!("file shelf: open: {e}");
                }
            }
        }
    }

    /// Uploads arrived thumbnails as textures (that needs the UI context).
    fn load_pending(&mut self, ctx: &egui::Context) {
        for (path, img) in std::mem::take(&mut self.pending) {
            if let Some(item) = self.items.iter_mut().find(|i| i.path == path) {
                item.thumb = Some(ctx.load_texture(
                    format!("nook-shelf-{}", path.display()),
                    img,
                    TextureOptions::LINEAR,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shelf_of(n: usize) -> FileShelf {
        let mut s = FileShelf::default();
        for k in 0..n {
            s.items
                .push(Item::new(PathBuf::from(format!("/nonexistent/f{k}"))));
        }
        s
    }

    #[test]
    fn selection_works_like_a_file_manager() {
        let mut s = shelf_of(5);
        s.click(1, false, false);
        assert_eq!(s.selection().len(), 1);
        // Ctrl adds, and drops again.
        s.click(3, true, false);
        assert_eq!(s.selection().len(), 2);
        s.click(3, true, false);
        assert_eq!(s.selection().len(), 1);
        // Shift selects the range from the last click (Ctrl+click or not).
        s.click(4, false, true);
        assert_eq!(s.selection().len(), 2);
        s.click(1, false, false);
        s.click(4, false, true);
        let names: Vec<String> = s
            .selection()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "/nonexistent/f1",
                "/nonexistent/f2",
                "/nonexistent/f3",
                "/nonexistent/f4"
            ]
        );
        // A plain click starts over.
        s.click(0, false, false);
        assert_eq!(s.selection(), vec![PathBuf::from("/nonexistent/f0")]);
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(25 * 1024 * 1024), "25 MB");
    }

    #[test]
    fn parses_the_shelf_file() {
        let paths = parse("C:\\a.txt\n\n  C:\\b dir\\c.png  \n");
        assert_eq!(
            paths,
            vec![
                PathBuf::from("C:\\a.txt"),
                PathBuf::from("C:\\b dir\\c.png")
            ]
        );
    }

    #[test]
    fn items_read_name_size_and_kind() {
        let dir = std::env::temp_dir().join(format!("nook-shelf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let f = dir.join("a.txt");
        std::fs::write(&f, b"hi").expect("write");
        let file = Item::new(f);
        assert_eq!(
            (file.name.as_str(), file.size, file.dir),
            ("a.txt", Some(2), false)
        );
        let folder = Item::new(dir.clone());
        assert!(folder.dir && folder.size.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
