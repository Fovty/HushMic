//! The switch (`.hm-switch`): a setting that applies at once. 44 × 24 track,
//! 16 px knob; off is an outlined `line-strong` track with an `ink-muted`
//! knob, on fills with the accent and the knob turns `on-accent`. The knob
//! slides 20 px over 220 ms (ease-out), the colours change over 220 ms
//! (ease-standard). The optional label sits 12 px right, 500 15/22.

use eframe::egui::{
    self, pos2, vec2, Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, WidgetInfo,
    WidgetType,
};

use super::super::fonts::Type;
use super::super::motion::ease_out;
use super::super::text;
use super::super::tokens::duration;
use super::{anim, anim_with, backdrop, faded, focus_ring, lerp_color, shows_focus};

#[must_use]
pub struct Switch<'a> {
    on: &'a mut bool,
    label: Option<&'a str>,
    name: Option<&'a str>,
    enabled: bool,
    /// On, but nothing comes of it right now (Live's Noise suppression
    /// while nothing is filtered, UX panel B8): the knob stays on the
    /// right, the track is outlined in the accent instead of filled.
    idle: bool,
}

impl<'a> Switch<'a> {
    pub fn new(on: &'a mut bool) -> Switch<'a> {
        Switch {
            on,
            label: None,
            name: None,
            enabled: true,
            idle: false,
        }
    }
    /// A visible label to the right (clicking it toggles too).
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }
    /// The accessible name when the label sits elsewhere.
    pub fn name(mut self, name: &'a str) -> Self {
        self.name = Some(name);
        self
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    /// See the `idle` field; only while on.
    pub fn idle(mut self, idle: bool) -> Self {
        self.idle = idle;
        self
    }
}

pub const TRACK: egui::Vec2 = egui::vec2(44.0, 24.0);

/// The track's fill and border and the knob's colour, `k` of the way to
/// on; `idle` on: outlined in the accent, the knob in the accent.
fn tones(pal: &super::super::tokens::Palette, k: f32, idle: bool) -> (Color32, Color32, Color32) {
    let (fill, edge, knob) = pal.switch_off();
    if idle {
        return (
            lerp_color(fill, pal.accent_soft, k),
            lerp_color(edge, pal.accent, k),
            lerp_color(knob, pal.accent, k),
        );
    }
    (
        lerp_color(fill, pal.accent, k),
        lerp_color(edge, pal.accent, k),
        lerp_color(knob, pal.on_accent, k),
    )
}

impl Widget for Switch<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let kit = super::super::kit(ui.ctx());
        let pal = kit.pal;
        let block = self
            .label
            .map(|l| text::layout(ui.ctx(), l, Type::SWITCH, pal.ink, f32::INFINITY));
        let w = TRACK.x + block.as_ref().map_or(0.0, |b| 12.0 + b.size().x);
        let h = block.as_ref().map_or(TRACK.y, |b| b.size().y.max(TRACK.y));
        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, mut resp) = ui.allocate_exact_size(vec2(w, h), sense);
        if resp.clicked() {
            *self.on = !*self.on;
            resp.mark_changed();
        }
        let on = *self.on;
        let name = self.label.or(self.name).unwrap_or_default().to_owned();
        let enabled = self.enabled;
        resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, on, &name));
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let ctx = ui.ctx();
        let k_pos = anim_with(ctx, resp.id.with("knob"), on, duration::BASE, ease_out);
        let k_col = anim(ctx, resp.id.with("col"), on, duration::BASE);
        let track = Rect::from_min_size(pos2(rect.left(), rect.center().y - TRACK.y / 2.0), TRACK);
        let (mut fill, mut border, mut knob_c) = tones(&pal, k_col, self.idle && on);
        let mut ink = pal.ink;
        if !self.enabled {
            // opacity .4 on the whole switch, the knob fading with its track
            let behind = backdrop(ui);
            let under = behind.blend(fill);
            border = faded(behind, under, border, 0.4);
            knob_c = faded(behind, under, knob_c, 0.4);
            ink = faded(behind, behind, ink, 0.4);
            fill = faded(behind, behind, fill, 0.4);
        }
        let p = ui.painter();
        p.rect(
            track,
            TRACK.y / 2.0,
            fill,
            Stroke::new(1.0_f32, border),
            StrokeKind::Inside,
        );
        let cx = track.left() + 4.0 + 8.0 + 20.0 * k_pos;
        p.circle_filled(pos2(cx, track.center().y), 8.0, knob_c);
        if let Some(b) = block {
            let b = text::layout(ctx, b.text(), Type::SWITCH, ink, f32::INFINITY);
            b.paint_centered_y(p, track.right() + 12.0, rect.center().y);
        }
        if shows_focus(&kit, &resp) {
            focus_ring(p, track, TRACK.y / 2.0, pal.focus_ring());
        }
        if self.enabled && resp.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tokens::{Palette, Theme};

    #[test]
    fn an_idle_switch_is_on_but_not_filled() {
        for theme in [Theme::Dark, Theme::Light] {
            let pal = Palette::of(theme);
            let (fill, border, knob) = tones(&pal, 1.0, false);
            assert_eq!(
                (fill, border, knob),
                (pal.accent, pal.accent, pal.on_accent)
            );
            let (fill, border, knob) = tones(&pal, 1.0, true);
            assert_ne!(fill, pal.accent, "{theme:?}: not filled");
            assert_eq!((border, knob), (pal.accent, pal.accent), "{theme:?}");
            // off looks off either way
            assert_eq!(tones(&pal, 0.0, true), tones(&pal, 0.0, false));
        }
    }
}
