//! What the collapsed pill shows.
//!
//! - A running line (marquee): new notifications first, else what is
//!   playing (a module's [`Module::ticker`]). The pill widens for it.
//! - A running timer's ring and time on the left, live glyphs (coffee, a
//!   phone pulse) on the right, a battery icon only when it runs low.
//! - Nothing else going on: a pair of eyes that look around, blink, and
//!   watch the pointer when it comes near.
//!
//! Animation only runs while something moves: the eyes wake the UI for
//! their next glance or blink, the marquee only while its text overflows.
//! With reduced motion the line is cut with "…" and the eyes hold still.
//!
//! [`Module::ticker`]: nook_ui::module::Module::ticker

use egui::{Color32, FontFamily, FontId, Id, Pos2, Rect, Ui, Vec2, pos2, vec2};
use nook_platform::system::Battery;
use nook_ui::anim;
use nook_ui::module::Badge;
use nook_ui::paint;
use nook_ui::spring::SpringParams;
use nook_ui::theme::{self, SIZE_BODY, Theme};
use std::time::Duration;

/// Pill size while it runs a line.
pub const TICKER_PILL: Vec2 = vec2(300.0, 34.0);
/// Marquee speed, points per second, and the pause before it starts.
const SPEED: f32 = 38.0;
const START_PAUSE: f64 = 1.2;
/// Space between the end of the line and its next lap.
const LAP_GAP: f32 = 48.0;
const PAD: f32 = 15.0;

/// Everything the pill may show this frame.
pub struct PillContent {
    pub badges: Vec<Badge>,
    /// Glyph and text of the running line.
    pub ticker: Option<(&'static str, String)>,
    pub battery: Option<Battery>,
}

impl PillContent {
    /// The closed size it wants, when not the default.
    pub fn size(&self) -> Option<Vec2> {
        self.ticker.as_ref().map(|_| TICKER_PILL)
    }
}

pub fn draw(c: PillContent, ui: &mut Ui, rect: Rect, theme: &Theme) {
    let painter = ui.painter().clone();
    let cy = rect.center().y;
    let mut left = rect.min.x + PAD;
    let mut right = rect.max.x - PAD;

    // Left: a running timer's ring and countdown, else the line's glyph.
    let ring = c.badges.iter().find_map(|b| match b {
        Badge::Ring { progress, label } => Some((*progress, label.clone())),
        _ => None,
    });
    if let Some((progress, label)) = &ring {
        let r = 6.0;
        paint::ring(
            &painter,
            pos2(left + r, cy),
            r,
            2.0,
            *progress,
            theme::with_alpha(theme.pill_text_dim, 0.35),
            theme.accent,
        );
        let ink = ink_text(
            &painter,
            label,
            Theme::font_semibold(SIZE_BODY),
            theme.pill_text,
            left + 2.0 * r + 7.0,
            cy,
            false,
        );
        left = ink.max.x + 10.0;
    } else if let Some((glyph, _)) = &c.ticker {
        let ink = ink_text(
            &painter,
            glyph,
            icon_font(14.0, true),
            theme.accent_fg(),
            left,
            cy,
            false,
        );
        left = ink.max.x + 8.0;
    }

    // Right: live glyphs (not the line's own), else a low battery.
    let ticker_glyph = c.ticker.as_ref().map(|(g, _)| *g);
    let mut any_right = false;
    for badge in &c.badges {
        match badge {
            Badge::Ring { .. } => {}
            Badge::Glyph { icon } => {
                if Some(*icon) == ticker_glyph {
                    continue;
                }
                let ink = ink_text(
                    &painter,
                    icon,
                    icon_font(14.0, true),
                    theme.accent_fg(),
                    right,
                    cy,
                    true,
                );
                right = ink.min.x - 6.0;
                any_right = true;
            }
            Badge::Pulse => {
                let now = ui.input(|i| i.time);
                let phase = ((now * 1.6).sin() * 0.5 + 0.5) as f32;
                let p = pos2(right - 3.0, cy);
                painter.circle_filled(
                    p,
                    3.0 + 3.0 * phase,
                    theme::with_alpha(theme.accent, 0.35 * (1.0 - phase)),
                );
                painter.circle_filled(p, 3.0, theme.accent);
                right -= 12.0;
                any_right = true;
                ui.ctx().request_repaint_after(Duration::from_millis(33));
            }
        }
    }
    if let Some(b) = c.battery.filter(|b| !b.charging && b.percent < 20) {
        let (icon, color) = battery_icon(b, theme);
        let ink = ink_text(
            &painter,
            icon,
            icon_font(15.0, false),
            color,
            right,
            cy,
            true,
        );
        right = ink.min.x - 6.0;
        any_right = true;
    }

    match &c.ticker {
        Some((_, text)) => {
            let lane = Rect::from_x_y_ranges(left..=right, rect.y_range());
            marquee(ui, lane, text, theme);
        }
        None if ring.is_none() && !any_right => eyes(ui, rect, theme),
        None => {}
    }
}

/// `text` on one line in `lane`: still when it fits, else scrolling in its
/// reading direction (leftwards for English, rightwards for Hebrew), lap
/// after lap.
fn marquee(ui: &mut Ui, lane: Rect, text: &str, theme: &Theme) {
    if lane.width() < 12.0 {
        return;
    }
    let ctx = ui.ctx().clone();
    let painter = ui.painter().with_clip_rect(lane.intersect(ui.clip_rect()));
    let font = Theme::font(SIZE_BODY);
    let galley = painter.layout_no_wrap(text.to_owned(), font.clone(), theme.pill_text);
    let w = galley.size().x;
    let y = lane.center().y - galley.size().y * 0.5;
    let rtl = nook_ui::widgets::reading_align(text) == egui::Align::RIGHT;
    let reduced = anim::motion(&ctx).reduced;
    if w <= lane.width() {
        let x = if rtl { lane.max.x - w } else { lane.min.x };
        painter.galley(pos2(x, y), galley, theme.pill_text);
        return;
    }
    if reduced {
        let mut job = egui::text::LayoutJob::single_section(
            text.to_owned(),
            egui::TextFormat::simple(font, theme.pill_text),
        );
        job.wrap = egui::text::TextWrapping {
            max_width: lane.width(),
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let g = painter.layout_job(job);
        let x = if rtl {
            lane.max.x - g.size().x
        } else {
            lane.min.x
        };
        painter.galley(pos2(x, y), g, theme.pill_text);
        return;
    }
    // Restart from the beginning whenever the text changes.
    let now = ctx.input(|i| i.time);
    let id = Id::new("nook-pill-marquee");
    let key = egui::util::hash(text);
    let start = ctx.data_mut(|d| {
        let entry = d.get_temp_mut_or(id, (key, now));
        if entry.0 != key {
            *entry = (key, now);
        }
        entry.1
    });
    let lap = w + LAP_GAP;
    let shift = ((now - start - START_PAUSE).max(0.0) as f32 * SPEED) % lap;
    for k in 0..2 {
        let offset = shift - k as f32 * lap;
        let x = if rtl {
            lane.max.x - w + offset
        } else {
            lane.min.x - offset
        };
        painter.galley(pos2(x, y), galley.clone(), theme.pill_text);
    }
    // Soft edges where the line enters and leaves.
    let fade = 14.0;
    for (edge, dir) in [(lane.min.x, 1.0), (lane.max.x, -1.0)] {
        let r = Rect::from_x_y_ranges(
            (edge.min(edge + dir * fade))..=(edge.max(edge + dir * fade)),
            lane.y_range(),
        );
        let mut mesh = egui::Mesh::default();
        let solid = theme.pill_bottom;
        let clear = theme::with_alpha(solid, 0.0);
        let (l, r_c) = if dir > 0.0 {
            (solid, clear)
        } else {
            (clear, solid)
        };
        mesh.colored_vertex(r.left_top(), l);
        mesh.colored_vertex(r.right_top(), r_c);
        mesh.colored_vertex(r.left_bottom(), l);
        mesh.colored_vertex(r.right_bottom(), r_c);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
    }
    ctx.request_repaint_after(Duration::from_millis(30));
}

/// Two eyes in the middle of the pill. They glance somewhere new every few
/// seconds, blink now and then, and follow the pointer while it is over
/// the window.
fn eyes(ui: &mut Ui, rect: Rect, theme: &Theme) {
    let ctx = ui.ctx().clone();
    let painter = ui.painter();
    let now = ctx.input(|i| i.time);
    let reduced = anim::motion(&ctx).reduced;
    let c = rect.center();
    let (rx, ry) = (5.5, 7.5);
    let centers = [c - vec2(11.0, 0.0), c + vec2(11.0, 0.0)];

    // Where to look: the pointer when it is around, else a glance that
    // changes every couple of seconds (about one in three straight ahead).
    const GLANCE: f64 = 2.8;
    let slot = (now / GLANCE).floor();
    let pointer = ctx.input(|i| i.pointer.latest_pos());
    let target = match pointer {
        Some(p) if !reduced => {
            let d = p - c;
            let reach = (d.length() / 120.0).min(1.0);
            d.normalized() * reach
        }
        _ if reduced => Vec2::ZERO,
        _ => {
            let h = hash01(slot);
            if h < 0.33 {
                Vec2::ZERO
            } else {
                let a = hash01(slot + 0.5) * std::f32::consts::TAU;
                vec2(a.cos(), a.sin() * 0.6)
            }
        }
    };
    let params = SpringParams::critical(260.0);
    let gx = anim::spring(&ctx, Id::new("nook-eyes-x"), target.x, params);
    let gy = anim::spring(&ctx, Id::new("nook-eyes-y"), target.y, params);

    // A blink (0.16 s) at a random moment of every 4.5 s.
    const BLINK_EVERY: f64 = 4.5;
    const BLINK_LEN: f64 = 0.16;
    let b_slot = (now / BLINK_EVERY).floor();
    let blink_at = b_slot * BLINK_EVERY + hash01(b_slot + 7.0) as f64 * (BLINK_EVERY - BLINK_LEN);
    let t = ((now - blink_at) / BLINK_LEN) as f32;
    let open = if reduced || !(0.0..1.0).contains(&t) {
        1.0
    } else {
        (t * std::f32::consts::TAU).cos() * 0.5 + 0.5
    };

    let white = theme.pill_text;
    let pupil = theme.pill_bottom;
    for e in centers {
        let h = (ry * open).max(0.8);
        painter.add(egui::Shape::ellipse_filled(e, vec2(rx, h), white));
        if open > 0.35 {
            let p: Pos2 = e + vec2(gx * 2.4, gy * 3.0 * open);
            painter.circle_filled(p, 2.8 * open.min(1.0), pupil);
        }
    }

    if reduced {
        return;
    }
    // Wake for the next glance or blink, not every frame.
    let next_glance = (slot + 1.0) * GLANCE;
    let next_blink = if now < blink_at {
        blink_at
    } else if t < 1.0 {
        now + 0.016
    } else {
        let s = b_slot + 1.0;
        s * BLINK_EVERY + hash01(s + 7.0) as f64 * (BLINK_EVERY - BLINK_LEN)
    };
    let wake = next_glance.min(next_blink) - now;
    ctx.request_repaint_after(Duration::from_secs_f64(wake.max(0.016)));
}

/// A stable pseudo-random number in 0..1 for `x`.
fn hash01(x: f64) -> f32 {
    let v = (x * 12.9898).sin() * 43_758.545_3;
    (v - v.floor()) as f32
}

/// Paints `text` so its ink (not its line box) starts at `x`, or ends at `x`
/// when `from_right`, and is vertically centered on `cy`. Segoe UI's line
/// box carries descender space and Phosphor glyphs are narrower than their
/// em box, so aligning line boxes leaves the pill visibly off-center.
fn ink_text(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    x: f32,
    cy: f32,
    from_right: bool,
) -> Rect {
    let galley = painter.layout_no_wrap(text.to_owned(), font, color);
    let ink = galley.mesh_bounds;
    let dx = if from_right {
        x - ink.max.x
    } else {
        x - ink.min.x
    };
    let offset = vec2(dx, cy - ink.center().y);
    painter.galley(pos2(offset.x, offset.y), galley, color);
    ink.translate(offset)
}

fn icon_font(size: f32, fill: bool) -> FontId {
    let family = if fill {
        theme::FAMILY_ICON_FILL
    } else {
        theme::FAMILY_ICON_REGULAR
    };
    FontId::new(size, FontFamily::Name(family.into()))
}

fn battery_icon(b: Battery, theme: &Theme) -> (&'static str, Color32) {
    use nook_ui::icons;
    if b.charging {
        return (icons::BATTERY_CHARGING, Color32::from_rgb(98, 214, 132));
    }
    let icon = match b.percent {
        90.. => icons::BATTERY_FULL,
        60.. => icons::BATTERY_HIGH,
        30.. => icons::BATTERY_MEDIUM,
        12.. => icons::BATTERY_LOW,
        _ => icons::BATTERY_WARNING,
    };
    let color = if b.percent < 12 {
        Color32::from_rgb(255, 99, 88)
    } else {
        theme.pill_text_dim
    };
    (icon, color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_in_range() {
        for i in 0..200 {
            let h = hash01(i as f64);
            assert!((0.0..1.0).contains(&h));
            assert_eq!(h, hash01(i as f64));
        }
    }

    #[test]
    fn only_a_running_line_widens_the_pill() {
        let idle = PillContent {
            badges: vec![],
            ticker: None,
            battery: None,
        };
        assert_eq!(idle.size(), None);
        let music = PillContent {
            ticker: Some(("x", "Song — Artist".into())),
            ..idle
        };
        assert_eq!(music.size(), Some(TICKER_PILL));
    }
}
