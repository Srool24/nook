//! The notch shell: a squircle that springs between the collapsed pill and
//! the expanded panel, painted as glass, hosting the pill badges and the
//! panel content.
//!
//! The OS window spans the whole width of the monitor's work area, so where
//! the notch sits (left corner, center, right corner) only moves the shape
//! inside the window, and switching slots springs it across; the window
//! itself never moves sideways. A notch in a corner grows away from its edge.
//! Dragging the glass moves the notch anywhere along the edge (`free_x`,
//! where it then grows both ways around its center, kept on the monitor);
//! the app moves the window down for the vertical part.

use crate::anim::{self, ease_out_cubic};
use crate::geometry::{CORNER_EXPONENT, Squircle, lerp};
use crate::paint;
use crate::shell_state::{ShellState, ShellTimings};
use crate::spring::SpringParams;
use crate::theme::Theme;
use egui::emath::TSTransform;
use egui::{Id, Pos2, Rect, Sense, Ui, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};

/// Where the notch rests along the top edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    /// Top-left corner; the panel opens to the right.
    Left,
    /// Top center; the panel opens both ways.
    #[default]
    Center,
    /// Top-right corner; the panel opens to the left.
    Right,
}

impl Placement {
    pub const ALL: [Placement; 3] = [Placement::Left, Placement::Center, Placement::Right];

    /// Horizontal anchor: 0 = against the left edge, 1 = against the right.
    pub fn frac(self) -> f32 {
        match self {
            Placement::Left => 0.0,
            Placement::Center => 0.5,
            Placement::Right => 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ShellMetrics {
    pub collapsed: Vec2,
    /// Extra size while the pointer rests on the closed pill.
    pub hover_swell: Vec2,
    pub expanded: Vec2,
    pub collapsed_radius: f32,
    pub expanded_radius: f32,
    /// Gap between the top of the monitor and the shell.
    pub top_margin: f32,
    /// Gap between the side of the monitor and a notch in a corner.
    pub side_margin: f32,
}

impl Default for ShellMetrics {
    fn default() -> Self {
        Self {
            collapsed: vec2(140.0, 34.0),
            hover_swell: vec2(12.0, 2.0),
            expanded: vec2(568.0, 236.0),
            collapsed_radius: 17.0,
            expanded_radius: 32.0,
            top_margin: 6.0,
            side_margin: 6.0,
        }
    }
}

impl ShellMetrics {
    /// Smallest OS window that fits the expanded panel plus spring
    /// overshoot. The app widens it to the monitor's work area; the height
    /// is what matters.
    pub fn window_size(&self) -> Vec2 {
        let overshoot = 1.08;
        let w = self.collapsed.x + (self.expanded.x - self.collapsed.x) * overshoot;
        let h = self.collapsed.y + (self.expanded.y - self.collapsed.y) * overshoot;
        vec2((w + 16.0).ceil(), (h + self.top_margin + 16.0).ceil())
    }
}

/// What the content callback gets each frame.
pub struct ContentCtx {
    /// The panel rect at its final expanded size (layout uses this so tiles
    /// do not reflow while the shell is still springing).
    pub panel: Rect,
    /// Global content opacity (fade in/out).
    pub alpha: f32,
    /// Seconds since the stagger reveal began (`None` before it starts).
    pub reveal: Option<f32>,
    /// True while content is going away (no stagger, just fade).
    pub closing: bool,
    /// Content scale transform (0.96 -> 1.0 about the panel center).
    pub transform: TSTransform,
    /// The shell's current rect: content outside it is not on glass yet
    /// (a tall page still springing open).
    pub shape: Rect,
}

impl ContentCtx {
    /// Stagger alpha for the `index`-th element: 20 ms apart, 180 ms each.
    pub fn stagger(&self, index: usize) -> f32 {
        if self.closing {
            return 1.0;
        }
        match self.reveal {
            Some(t) => ease_out_cubic((t - index as f32 * 0.020) / 0.180),
            None => 0.0,
        }
    }
}

/// A drag of the glass this frame (moving the notch).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShellDrag {
    /// The pointer has moved past the drag threshold; `moved` is how far
    /// it went since the press, in points.
    Started {
        moved: Vec2,
    },
    Moved,
    /// Released; `free_x` and `placement` hold where it was dropped.
    Ended,
}

pub struct ShellOutput {
    /// Current shape in window coordinates (points). Drive hit-testing with it.
    pub shape: Squircle,
    /// 0 = pill, 1 = panel (may overshoot slightly).
    pub progress: f32,
    /// False when fully hidden (fullscreen app in front).
    pub visible: bool,
    /// Whole-shell opacity (fullscreen fade).
    pub opacity: f32,
    pub drag: Option<ShellDrag>,
}

pub struct Shell {
    pub metrics: ShellMetrics,
    pub timings: ShellTimings,
    pub state: ShellState,
    /// Resting slot along the top edge.
    pub placement: Placement,
    /// Where the notch was dragged, instead of `placement`: its center as
    /// a fraction of the window width.
    pub free_x: Option<f32>,
    /// Treat the shell as hovered whatever the pointer does (typing in it,
    /// files being dragged over it).
    pub hold_open: bool,
    /// Expanded size wanted by the page on show (phone link), instead of
    /// `metrics.expanded`.
    pub expanded_override: Option<Vec2>,
    /// Height the OS window must keep: it grows as soon as a taller page
    /// opens, and shrinks only once the shell has settled back down.
    tall: f32,
    id: Id,
    outline: Vec<Pos2>,
    last_shape: Option<Squircle>,
    reveal_start: Option<f64>,
    /// Shell center and pointer x when the current drag began.
    drag_from: Option<(f32, f32)>,
    /// Whether the last frame was placed by `free_x` (leaving it springs
    /// from there to the slot).
    was_free: bool,
}

impl Shell {
    pub fn new(metrics: ShellMetrics, timings: ShellTimings) -> Self {
        Self {
            metrics,
            timings,
            state: ShellState::default(),
            placement: Placement::default(),
            free_x: None,
            hold_open: false,
            expanded_override: None,
            tall: metrics.expanded.y,
            id: Id::new("nook-shell"),
            outline: Vec::new(),
            last_shape: None,
            reveal_start: None,
            drag_from: None,
            was_free: false,
        }
    }

    /// Follows a drag of the glass: the shell's center moves with the
    /// pointer. A drop within a few points of a slot snaps to it.
    fn drag(&mut self, resp: &egui::Response, window: Rect, shape: Squircle) -> Option<ShellDrag> {
        const SNAP: f32 = 12.0;
        let x = resp.interact_pointer_pos().map(|p| p.x);
        if resp.drag_started()
            && let Some(p) = resp.interact_pointer_pos()
        {
            // Measured from the press, not from where the drag threshold
            // was crossed, so the notch does not lag the pointer.
            let origin = resp.ctx.input(|i| i.pointer.press_origin()).unwrap_or(p);
            self.drag_from = Some((shape.rect.center().x, origin.x));
            self.free_x =
                Some(((shape.rect.center().x + p.x - origin.x) / window.width()).clamp(0.0, 1.0));
            return Some(ShellDrag::Started { moved: p - origin });
        }
        let (cx, x0) = self.drag_from?;
        if let Some(x) = x {
            self.free_x = Some(((cx + x - x0) / window.width()).clamp(0.0, 1.0));
        }
        // Also ends a drag whose release this response never saw.
        if resp.dragged() && !resp.drag_stopped() {
            return Some(ShellDrag::Moved);
        }
        self.drag_from = None;
        let (lo, hi) = self.left_range(window, shape.rect.width());
        let left = self.left_for(window, shape.rect.width(), 0.5);
        let center = self.free_x.unwrap_or(0.5) * window.width();
        let snap = if left <= lo + SNAP {
            Some(Placement::Left)
        } else if left >= hi - SNAP {
            Some(Placement::Right)
        } else if (center - window.center().x).abs() < SNAP {
            Some(Placement::Center)
        } else {
            None
        };
        if let Some(p) = snap {
            self.placement = p;
            self.free_x = None;
        }
        Some(ShellDrag::Ended)
    }

    /// The expanded size currently wanted.
    pub fn expanded(&self) -> Vec2 {
        self.expanded_override.unwrap_or(self.metrics.expanded)
    }

    /// OS window size that fits the shell now, including a taller page that
    /// is still springing open or closed.
    pub fn window_size(&self) -> Vec2 {
        let m = ShellMetrics {
            expanded: vec2(self.expanded().x, self.tall.max(self.expanded().y)),
            ..self.metrics
        };
        m.window_size()
    }

    /// Lowest and highest left edge for a shell of `width`.
    fn left_range(&self, window: Rect, width: f32) -> (f32, f32) {
        let lo = window.min.x + self.metrics.side_margin;
        let hi = (window.max.x - self.metrics.side_margin - width).max(lo);
        (lo, hi)
    }

    /// Left edge for a shell of `width`: centered on `free_x` if the notch
    /// was dragged, else at anchor fraction `frac`.
    fn left_for(&self, window: Rect, width: f32, frac: f32) -> f32 {
        let (lo, hi) = self.left_range(window, width);
        match self.free_x {
            Some(c) => (c * window.width() - width * 0.5).clamp(lo, hi),
            None => lerp(lo, hi, frac),
        }
    }

    /// The anchor fraction a shell of `width` is at when placed by `free_x`.
    fn free_frac(&self, window: Rect, width: f32) -> f32 {
        let (lo, hi) = self.left_range(window, width);
        if hi <= lo {
            return 0.5;
        }
        (self.left_for(window, width, 0.5) - lo) / (hi - lo)
    }

    fn shape_for(&self, window: Rect, size: Vec2, progress: f32, frac: f32) -> Squircle {
        let m = &self.metrics;
        let min = pos2(
            self.left_for(window, size.x, frac),
            window.min.y + m.top_margin,
        );
        let radius = lerp(
            m.collapsed_radius,
            m.expanded_radius,
            progress.clamp(0.0, 1.0),
        );
        // Corners morph from a circular capsule (pill) to a squircle (panel).
        let exponent = lerp(2.0, CORNER_EXPONENT, progress.clamp(0.0, 1.0));
        Squircle::new(Rect::from_min_size(min, size), radius.min(size.y * 0.5))
            .with_exponent(exponent)
    }

    /// Runs one frame. `hidden` fades the whole shell out (fullscreen apps).
    /// `composited` means the compositor paints the glass tint, so only
    /// grain, rim and highlight are drawn here.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        theme: &Theme,
        hidden: bool,
        composited: bool,
        pill: impl FnOnce(&mut Ui, Rect, f32),
        content: impl FnOnce(&mut Ui, &ContentCtx),
    ) -> ShellOutput {
        let ctx = ui.ctx().clone();
        let window = ctx.content_rect();
        let now = ctx.input(|i| i.time);
        let m = self.metrics;

        let hovered = !hidden
            && (self.hold_open
                || match (self.last_shape, ctx.input(|i| i.pointer.hover_pos())) {
                    (Some(s), Some(p)) => s.contains(p),
                    _ => false,
                });

        // Click on the glass (not on a tile) pins / unpins; dragging it
        // moves the notch.
        let mut drag = None;
        if let Some(shape) = self.last_shape
            && !hidden
        {
            let resp = ui.interact(shape.rect, self.id.with("bg"), Sense::click_and_drag());
            if resp.clicked() {
                self.state.toggle_pin(now);
            }
            drag = self.drag(&resp, window, shape);
        }

        let targets = self.state.update(now, hovered, &self.timings);
        if let Some(at) = targets.wake_at {
            ctx.request_repaint_after_secs(((at - now).max(0.0)) as f32 + 0.001);
        }

        let open = targets.shell_open && !hidden;
        let expanded = self.expanded();
        let rest = if self.state.is_hovering_closed() {
            m.collapsed + m.hover_swell
        } else {
            m.collapsed
        };
        let shell_params = SpringParams::shell();
        let w = anim::spring_from(
            &ctx,
            self.id.with("w"),
            m.collapsed.x,
            if open { expanded.x } else { rest.x },
            shell_params,
        );
        let h = anim::spring_from(
            &ctx,
            self.id.with("h"),
            m.collapsed.y,
            if open { expanded.y } else { rest.y },
            SpringParams::with_overshoot(220.0, 0.04),
        );
        let opacity = anim::spring_from(
            &ctx,
            self.id.with("opacity"),
            0.0,
            if hidden { 0.0 } else { 1.0 },
            SpringParams::critical(180.0),
        )
        .clamp(0.0, 1.0);

        // Progress is measured against the regular panel, so a taller page
        // reads as fully open (1.0) instead of re-running the morph.
        let mut progress = ((h - m.collapsed.y) / (m.expanded.y - m.collapsed.y)).max(0.0);
        if expanded.y > m.expanded.y {
            progress = progress.min(1.0);
        }
        // Grow the window at once; shrink it once the shape has come down.
        if expanded.y >= self.tall || h <= expanded.y.max(m.expanded.y) + 0.5 {
            self.tall = expanded.y.max(m.expanded.y);
        }

        // Horizontal anchor; a new placement springs across the top edge
        // (from where a dragged notch was, once it goes back to a slot).
        let x_id = self.id.with("x");
        let frac = match self.free_x {
            Some(_) => {
                self.was_free = true;
                self.free_frac(window, w)
            }
            None => {
                if std::mem::take(&mut self.was_free) {
                    anim::reset_spring(&ctx, x_id);
                }
                let from = self.last_shape.map_or(self.placement.frac(), |s| {
                    let (lo, hi) = self.left_range(window, s.rect.width());
                    if hi > lo {
                        (s.rect.min.x - lo) / (hi - lo)
                    } else {
                        0.5
                    }
                });
                anim::spring_from(
                    &ctx,
                    x_id,
                    from,
                    self.placement.frac(),
                    SpringParams::with_overshoot(240.0, 0.03),
                )
            }
        };
        let shape = self.shape_for(window, vec2(w, h), progress, frac);
        shape.outline_into(&mut self.outline);
        self.last_shape = Some(shape);

        let visible = opacity > 0.002;
        if !visible {
            return ShellOutput {
                shape,
                progress,
                visible,
                opacity,
                drag,
            };
        }

        let painter = ui.painter().clone();
        paint::glass(
            &painter,
            &shape,
            &self.outline,
            theme,
            opacity,
            progress,
            !composited,
        );

        // Collapsed badges fade out as the shell grows.
        let pill_alpha = (1.0 - progress / 0.35).clamp(0.0, 1.0) * opacity;
        if pill_alpha > 0.01 {
            let mut pill_ui = ui.new_child(egui::UiBuilder::new().max_rect(shape.rect));
            pill_ui.set_clip_rect(shape.rect);
            pill_ui.multiply_opacity(pill_alpha);
            pill(&mut pill_ui, shape.rect, pill_alpha);
        }

        // Content: fade (100 ms) + stagger once the shell is ~60% open.
        if targets.content_open && progress >= 0.6 && self.reveal_start.is_none() {
            self.reveal_start = Some(now);
        }
        if !targets.content_open {
            self.reveal_start = None;
        }
        let fade = anim::tween(&ctx, self.id.with("content"), targets.content_open, 0.10);
        let reveal = self.reveal_start.map(|t| (now - t) as f32);
        if let Some(t) = reveal
            && t < 0.6
        {
            ctx.request_repaint();
        }
        let content_alpha = fade * opacity;
        if content_alpha > 0.001 && (reveal.is_some() || !targets.content_open) {
            let panel = self.shape_for(window, expanded, 1.0, frac).rect;
            let scale = lerp(0.96, 1.0, ease_out_cubic(progress.clamp(0.0, 1.0)));
            // Grow out of the anchored side: the top center for a centered
            // notch, the top outer corner for one in a corner, the dragged
            // notch's center otherwise.
            let cx = match self.free_x {
                Some(c) => (c * window.width()).clamp(panel.min.x, panel.max.x),
                None => lerp(panel.min.x, panel.max.x, frac),
            };
            let c = vec2(cx, panel.min.y);
            let transform = TSTransform::new(c * (1.0 - scale), scale);
            let cctx = ContentCtx {
                panel,
                alpha: content_alpha,
                reveal,
                closing: !targets.content_open,
                transform,
                shape: shape.rect,
            };
            content(ui, &cctx);
        }

        if self.state.is_pinned() && progress > 0.85 {
            paint::icon(
                &painter,
                pos2(shape.rect.center().x, shape.rect.min.y + 8.0),
                egui_phosphor::fill::PUSH_PIN,
                9.0,
                crate::theme::with_alpha(theme.text_faint, content_alpha),
                true,
            );
        }

        ShellOutput {
            shape,
            progress,
            visible,
            opacity,
            drag,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs frames of a shell in a 1536 x 300 window, feeding `events` one
    /// frame each, and returns what each frame reported.
    fn drive(shell: &mut Shell, events: Vec<Vec<egui::Event>>) -> Vec<ShellOutput> {
        let ctx = egui::Context::default();
        let theme = Theme::styled(true, egui::Color32::WHITE, false, &Default::default());
        let mut outs = Vec::new();
        for (frame, events) in std::iter::repeat_n(Vec::new(), 3).chain(events).enumerate() {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1536.0, 300.0))),
                time: Some(frame as f64 / 60.0),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| {
                outs.push(shell.show(ui, &theme, false, false, |_, _, _| {}, |_, _| {}));
            });
            out.textures_delta.clear();
        }
        outs
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }

    #[test]
    fn dragging_the_glass_moves_the_notch() {
        let mut shell = Shell::new(ShellMetrics::default(), ShellTimings::default());
        let start = pos2(768.0, 20.0);
        let mut events = vec![
            vec![egui::Event::PointerMoved(start)],
            vec![button(start, true)],
        ];
        for i in 1..=10 {
            events.push(vec![egui::Event::PointerMoved(
                start + vec2(-40.0 * i as f32, 0.0),
            )]);
        }
        let end = start + vec2(-400.0, 0.0);
        events.push(vec![button(end, false)]);
        let outs = drive(&mut shell, events);
        let drags: Vec<ShellDrag> = outs.iter().filter_map(|o| o.drag).collect();
        assert!(matches!(drags.first(), Some(ShellDrag::Started { .. })));
        assert_eq!(drags.last(), Some(&ShellDrag::Ended));
        // The notch's center ends under the pointer.
        let center = shell.free_x.expect("dragged off its slot") * 1536.0;
        assert!(
            (center - end.x).abs() < 1.0,
            "center {center}, pointer {}",
            end.x
        );
        let shape = outs.last().unwrap().shape.rect;
        assert!((shape.center().x - end.x).abs() < 1.0, "{shape:?}");
        // Not a click: the drag does not pin the panel.
        assert!(!shell.state.is_pinned());
    }

    #[test]
    fn a_drop_near_a_slot_snaps_to_it() {
        let mut shell = Shell::new(ShellMetrics::default(), ShellTimings::default());
        shell.free_x = Some(0.3);
        let m = shell.metrics;
        let start = pos2(0.3 * 1536.0, 20.0);
        // To 4 points short of the left corner.
        let end = pos2(m.side_margin + m.collapsed.x * 0.5 + 4.0, 20.0);
        let events = vec![
            vec![egui::Event::PointerMoved(start)],
            vec![button(start, true)],
            vec![egui::Event::PointerMoved(start.lerp(end, 0.5))],
            vec![egui::Event::PointerMoved(end)],
            vec![button(end, false)],
        ];
        drive(&mut shell, events);
        assert_eq!(shell.placement, Placement::Left);
        assert_eq!(shell.free_x, None);
    }

    #[test]
    fn corner_shells_hug_their_edge() {
        let shell = Shell::new(ShellMetrics::default(), ShellTimings::default());
        let window = Rect::from_min_size(Pos2::ZERO, vec2(1536.0, 300.0));
        let m = shell.metrics;
        let left = shell.shape_for(window, m.expanded, 1.0, 0.0).rect;
        let right = shell.shape_for(window, m.expanded, 1.0, 1.0).rect;
        let center = shell.shape_for(window, m.expanded, 1.0, 0.5).rect;
        assert_eq!(left.min.x, m.side_margin);
        assert_eq!(right.max.x, window.max.x - m.side_margin);
        assert!((center.center().x - window.center().x).abs() < 1e-3);
        // A pill in the left corner keeps its left edge as it opens.
        let pill = shell.shape_for(window, m.collapsed, 0.0, 0.0).rect;
        assert_eq!(pill.min.x, left.min.x);
        // ...and one in the right corner keeps its right edge.
        let pill = shell.shape_for(window, m.collapsed, 0.0, 1.0).rect;
        assert_eq!(pill.max.x, right.max.x);
    }
}
