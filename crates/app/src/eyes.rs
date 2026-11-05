//! The pill's eyes: two eyes with eyelids and a handful of expressions.
//!
//! Each eye is an ellipse cut by an upper lid (a line that comes down and
//! tilts) and a lower lid (a line that comes up when smiling). The white
//! and the pupil are the ellipse and a circle clipped to what the lids
//! leave open, and the upper lid's edge is drawn along the cut, so the lid
//! reads as a lid and not a shadow.
//!
//! What they show, first match wins: asleep (closed); surprised for a
//! moment on waking; happy while the pointer is on the pill; sad when the
//! battery runs low; drowsy after a while without input; and now and then,
//! on their own, curious, suspicious or happy for a couple of seconds.
//! Every shape change springs, and the UI wakes only for the next glance,
//! blink or mood.

use egui::{Id, Pos2, Rect, Shape, Stroke, Ui, Vec2, vec2};
use nook_ui::anim;
use nook_ui::spring::SpringParams;
use nook_ui::theme::{self, Theme};
use std::time::Duration;

/// What the rest of the app knows about the moment.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mood {
    pub asleep: bool,
    /// A while without input: lids heavy.
    pub drowsy: bool,
    /// Battery running low and not charging.
    pub low_battery: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expression {
    Neutral,
    Happy,
    Surprised,
    Curious,
    Suspicious,
    Sad,
    Drowsy,
    Asleep,
}

/// The lids and eyes for an expression.
#[derive(Clone, Copy, Debug)]
struct Look {
    /// Upper lid, 0 open to 1 closed.
    lid: f32,
    /// Lid tilt: positive brings the inner corners down (cross, wary),
    /// negative raises them (sad).
    tilt: f32,
    /// Lower lid, 0 down to 1 up (a smile squints from below).
    lower: f32,
    /// One eye more open than the other (curious).
    lopsided: f32,
    /// Eye size.
    size: f32,
    /// Pupil size.
    pupil: f32,
}

impl Expression {
    fn look(self) -> Look {
        let base = Look {
            lid: 0.16,
            tilt: 0.0,
            lower: 0.0,
            lopsided: 0.0,
            size: 1.0,
            pupil: 1.0,
        };
        match self {
            Expression::Neutral => base,
            Expression::Happy => Look {
                lid: 0.08,
                lower: 0.5,
                ..base
            },
            Expression::Surprised => Look {
                lid: 0.0,
                size: 1.15,
                pupil: 0.7,
                ..base
            },
            Expression::Curious => Look {
                lopsided: 0.35,
                ..base
            },
            Expression::Suspicious => Look {
                lid: 0.48,
                tilt: 0.3,
                lower: 0.15,
                ..base
            },
            Expression::Sad => Look {
                lid: 0.3,
                tilt: -0.4,
                ..base
            },
            Expression::Drowsy => Look { lid: 0.62, ..base },
            Expression::Asleep => Look { lid: 1.0, ..base },
        }
    }
}

/// How long an unprompted mood lasts, and how often one may come.
const MOOD_EVERY: f64 = 9.0;
const MOOD_LEN: f64 = 2.6;
const GLANCE: f64 = 2.8;
const BLINK_EVERY: f64 = 4.5;
const BLINK_LEN: f64 = 0.16;
/// Surprise after waking up.
const SURPRISE_LEN: f64 = 1.4;

pub fn draw(ui: &mut Ui, rect: Rect, theme: &Theme, mood: Mood) {
    let ctx = ui.ctx().clone();
    let painter = ui.painter().clone();
    let now = ctx.input(|i| i.time);
    let reduced = anim::motion(&ctx).reduced;
    let hovered = ui.rect_contains_pointer(rect);

    // When did it last wake up?
    let wake_id = Id::new("nook-eyes-woke");
    let woke_at = ctx.data_mut(|d| {
        let (was_asleep, at) = d.get_temp_mut_or(wake_id, (mood.asleep, f64::NEG_INFINITY));
        if *was_asleep && !mood.asleep {
            *at = now;
        }
        *was_asleep = mood.asleep;
        *at
    });

    let mood_slot = (now / MOOD_EVERY).floor();
    let in_mood = now - mood_slot * MOOD_EVERY < MOOD_LEN;
    let expression = if mood.asleep {
        Expression::Asleep
    } else if now - woke_at < SURPRISE_LEN && !reduced {
        Expression::Surprised
    } else if hovered {
        Expression::Happy
    } else if mood.low_battery {
        Expression::Sad
    } else if mood.drowsy {
        Expression::Drowsy
    } else if in_mood && !reduced {
        match (hash01(mood_slot + 3.0) * 8.0) as u32 {
            0 => Expression::Curious,
            1 => Expression::Suspicious,
            2 => Expression::Happy,
            _ => Expression::Neutral,
        }
    } else {
        Expression::Neutral
    };
    let target = expression.look();
    let slow = SpringParams::critical(160.0);
    let s = |name: &str, v: f32| anim::spring(&ctx, Id::new(("nook-eyes", name)), v, slow);
    let look = Look {
        lid: s("lid", target.lid),
        tilt: s("tilt", target.tilt),
        lower: s("lower", target.lower),
        lopsided: s("lopsided", target.lopsided),
        size: s("size", target.size),
        pupil: s("pupil", target.pupil),
    };

    // Where to look: the pointer when it's on the window, else a glance
    // that changes every few seconds (a wary look goes sideways).
    let c = rect.center();
    let k = (rect.height() / 34.0).clamp(0.5, 1.5);
    let glance_slot = (now / GLANCE).floor();
    let gaze = if reduced || expression == Expression::Asleep {
        Vec2::ZERO
    } else if let Some(p) = ctx.input(|i| i.pointer.latest_pos()) {
        let d = p - c;
        d.normalized() * (d.length() / 120.0).min(1.0)
    } else if expression == Expression::Suspicious {
        vec2(if hash01(mood_slot) < 0.5 { -1.0 } else { 1.0 }, 0.0)
    } else if hash01(glance_slot) < 0.33 {
        Vec2::ZERO
    } else {
        let a = hash01(glance_slot + 0.5) * std::f32::consts::TAU;
        vec2(a.cos(), a.sin() * 0.6)
    };
    let params = SpringParams::critical(260.0);
    let gx = anim::spring(&ctx, Id::new("nook-eyes-x"), gaze.x, params);
    let gy = anim::spring(&ctx, Id::new("nook-eyes-y"), gaze.y, params);

    // Blinks close the upper lid briefly.
    let b_slot = (now / BLINK_EVERY).floor();
    let blink_at = b_slot * BLINK_EVERY + hash01(b_slot + 7.0) as f64 * (BLINK_EVERY - BLINK_LEN);
    let t = ((now - blink_at) / BLINK_LEN) as f32;
    let blink = if reduced || !(0.0..1.0).contains(&t) {
        0.0
    } else {
        0.5 - (t * std::f32::consts::TAU).cos() * 0.5
    };

    let (rx, ry) = (5.5 * k * look.size, 7.5 * k * look.size);
    let spacing = 11.0 * k;
    for side in [-1.0f32, 1.0] {
        let e = c + vec2(side * spacing, 0.0);
        let lid = (look.lid + look.lopsided * if side > 0.0 { 0.9 } else { -0.4 })
            .max(blink)
            .clamp(0.0, 1.0);
        eye(&painter, e, side, rx, ry, lid, look, vec2(gx, gy), theme);
    }

    if reduced || expression == Expression::Asleep && look.lid > 0.97 {
        return;
    }
    // Wake for the next glance, blink or mood, not every frame.
    let next_glance = (glance_slot + 1.0) * GLANCE;
    let next_mood = if in_mood {
        mood_slot * MOOD_EVERY + MOOD_LEN
    } else {
        (mood_slot + 1.0) * MOOD_EVERY
    };
    let next_blink = if now < blink_at {
        blink_at
    } else if t < 1.0 {
        now + 0.016
    } else {
        let s = b_slot + 1.0;
        s * BLINK_EVERY + hash01(s + 7.0) as f64 * (BLINK_EVERY - BLINK_LEN)
    };
    let surprise_end = woke_at + SURPRISE_LEN;
    let mut wake = next_glance.min(next_mood).min(next_blink);
    if surprise_end > now {
        wake = wake.min(surprise_end);
    }
    ctx.request_repaint_after(Duration::from_secs_f64((wake - now).max(0.016)));
}

/// One eye at `e` (`side` -1 left, 1 right), its upper lid at `lid`.
#[allow(clippy::too_many_arguments)]
fn eye(
    painter: &egui::Painter,
    e: Pos2,
    side: f32,
    rx: f32,
    ry: f32,
    lid: f32,
    look: Look,
    gaze: Vec2,
    theme: &Theme,
) {
    let white = theme.pill_text;
    if lid > 0.97 {
        // Closed: a sleepy curve.
        let points: Vec<Pos2> = (0..=10)
            .map(|i| {
                let a = std::f32::consts::PI * i as f32 / 10.0;
                e + vec2(-rx * a.cos(), ry * 0.35 * a.sin())
            })
            .collect();
        painter.add(Shape::line(points, Stroke::new(1.8, white)));
        return;
    }
    // The upper lid: a line `lid` of the way down, its inner end (toward
    // the other eye) lower with a positive tilt.
    let inner = -side;
    let top = e.y - ry + 2.0 * ry * lid;
    let upper = move |p: Pos2| p.y - (top + look.tilt * ry * ((p.x - e.x) / rx) * inner);
    // The lower lid: a level line coming up.
    let bottom = e.y + ry - 1.4 * ry * look.lower;
    let lower = move |p: Pos2| bottom - p.y;

    let ellipse: Vec<Pos2> = (0..32)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / 32.0;
            e + vec2(rx * a.cos(), ry * a.sin())
        })
        .collect();
    let open = clip(&clip(&ellipse, &upper), &lower);
    if open.len() < 3 {
        return;
    }
    painter.add(Shape::convex_polygon(open.clone(), white, Stroke::NONE));

    let pr = 2.8 * (ry / 7.5) * look.pupil;
    let pc = e + vec2(gaze.x * (rx - pr - 0.6), gaze.y * (ry - pr - 0.6) * 0.8);
    let disc: Vec<Pos2> = (0..20)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / 20.0;
            pc + vec2(pr * a.cos(), pr * a.sin())
        })
        .collect();
    let pupil = clip(&clip(&disc, &upper), &lower);
    if pupil.len() >= 3 {
        painter.add(Shape::convex_polygon(
            pupil,
            theme.pill_bottom,
            Stroke::NONE,
        ));
    }

    // The lid's edge along the cut.
    if lid > 0.04 {
        let edge: Vec<Pos2> = open
            .iter()
            .copied()
            .filter(|p| upper(*p).abs() < 0.05)
            .collect();
        if let [a, .., b] = edge.as_slice() {
            painter.line_segment(
                [*a, *b],
                Stroke::new(1.4, theme::with_alpha(theme.pill_text_dim, 0.9)),
            );
        }
    }
}

/// Sutherland–Hodgman: the part of convex polygon `poly` where `keep` is
/// >= 0 (`keep` linear in the point).
fn clip(poly: &[Pos2], keep: &dyn Fn(Pos2) -> f32) -> Vec<Pos2> {
    let mut out = Vec::with_capacity(poly.len() + 2);
    for (i, &a) in poly.iter().enumerate() {
        let b = poly[(i + 1) % poly.len()];
        let (fa, fb) = (keep(a), keep(b));
        if fa >= 0.0 {
            out.push(a);
        }
        if (fa >= 0.0) != (fb >= 0.0) {
            let t = fa / (fa - fb);
            out.push(a + (b - a) * t);
        }
    }
    out
}

/// A stable pseudo-random number in 0..1 for `x`.
fn hash01(x: f64) -> f32 {
    let v = (x * 12.9898).sin() * 43_758.545_3;
    (v - v.floor()) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn clipping_keeps_the_open_part() {
        let square = vec![
            pos2(0.0, 0.0),
            pos2(2.0, 0.0),
            pos2(2.0, 2.0),
            pos2(0.0, 2.0),
        ];
        // Keep y >= 1: the bottom half.
        let half = clip(&square, &|p| p.y - 1.0);
        assert_eq!(half.len(), 4);
        assert!(half.iter().all(|p| p.y >= 1.0 - 1e-6));
        // Keep nothing.
        assert!(clip(&square, &|p| -p.y - 1.0).is_empty());
    }

    #[test]
    fn expressions_differ() {
        assert!(Expression::Asleep.look().lid > Expression::Drowsy.look().lid);
        assert!(Expression::Sad.look().tilt < 0.0);
        assert!(Expression::Suspicious.look().tilt > 0.0);
        assert!(Expression::Happy.look().lower > 0.0);
    }
}
