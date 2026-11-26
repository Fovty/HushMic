//! Radio groups: the segmented control (`.hm-seg`, two to four short
//! options with a sliding thumb) and the strength picker (`.hm-app-steps`,
//! four options in one row, or 2 × 2 when a label does not fit its share).
//!
//! Both are real radio groups: only the selected option is in the Tab
//! order, the arrow keys move the selection (and the focus with it).

use eframe::egui::{
    self, pos2, vec2, Color32, EventFilter, Id, Key, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    WidgetInfo, WidgetType,
};

use super::super::fonts::Type;
use super::super::motion::{c01, ease_out};
use super::super::text;
use super::super::tokens::duration;
use super::{anim, focus_ring, hovered, lerp_color, shows_focus};
use eframe::egui::accesskit::Role;

/// A value that eases to `target` over `secs` (ease-out), restarting from
/// where it is when the target changes. Asks for frames only while moving.
pub fn slide(ctx: &egui::Context, id: Id, target: f32, secs: f32) -> f32 {
    #[derive(Clone, Copy)]
    struct S {
        from: f32,
        to: f32,
        t0: f64,
    }
    let now = ctx.input(|i| i.time);
    let reduced = super::super::kit(ctx).reduced_motion;
    let mut s = ctx.data(|d| d.get_temp::<S>(id)).unwrap_or(S {
        from: target,
        to: target,
        t0: now,
    });
    let value_at = |s: &S| {
        let k = if secs <= 0.0 {
            1.0
        } else {
            c01(((now - s.t0) as f32) / secs)
        };
        s.from + (s.to - s.from) * ease_out(k)
    };
    if (s.to - target).abs() > 1e-6 {
        let cur = value_at(&s);
        s = if reduced {
            S {
                from: target,
                to: target,
                t0: now,
            }
        } else {
            S {
                from: cur,
                to: target,
                t0: now,
            }
        };
        ctx.data_mut(|d| d.insert_temp(id, s));
    }
    let v = value_at(&s);
    if (v - s.to).abs() > 1e-4 {
        super::super::motion::moment_repaint(ctx);
    }
    v
}

/// Arrow keys on a focused option of a radio group: −1, +1 or 0.
pub(super) fn arrow(ui: &Ui, focused: &Response, vertical_too: bool) -> i32 {
    if !focused.has_focus() {
        return 0;
    }
    let filter = EventFilter {
        horizontal_arrows: true,
        vertical_arrows: vertical_too,
        ..Default::default()
    };
    ui.memory_mut(|m| m.set_focus_lock_filter(focused.id, filter));
    let d = ui.input_mut(|i| {
        let mut d = 0;
        if i.consume_key(egui::Modifiers::NONE, Key::ArrowRight) {
            d = 1;
        }
        if i.consume_key(egui::Modifiers::NONE, Key::ArrowLeft) {
            d = -1;
        }
        if vertical_too {
            if i.consume_key(egui::Modifiers::NONE, Key::ArrowDown) {
                d = 1;
            }
            if i.consume_key(egui::Modifiers::NONE, Key::ArrowUp) {
                d = -1;
            }
        }
        d
    });
    if d != 0 {
        // the group moved its focus itself: no spatial move on top of it (the
        // lock filter only holds from the second frame a stop has focus)
        ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
    }
    d
}

/// egui's arrow-key navigation can land on an option that was the group's
/// Tab stop earlier (egui remembers every widget that took focus); such an
/// option is no longer focusable and drops the focus. Hand it to the stop.
pub(super) fn keep_focus_on_stop(ui: &Ui, mut ids: impl Iterator<Item = Id>, stop: Id) {
    let f = ui.memory(|m| m.focused());
    if f.is_some_and(|f| f != stop && ids.any(|i| i == f)) {
        ui.memory_mut(|m| m.request_focus(stop));
    }
}

fn pending_focus_id(group: Id) -> Id {
    group.with("focus-next")
}

/// The segmented control. `options` are (value, label); `name` is the
/// group's accessible name ("Model").
#[must_use]
pub struct Segmented<'a, T: PartialEq + Clone> {
    value: &'a mut T,
    options: &'a [(T, &'a str)],
    name: &'a str,
    width: Option<f32>,
    id_salt: Option<Id>,
    take_focus: bool,
}

impl<'a, T: PartialEq + Clone> Segmented<'a, T> {
    pub fn new(name: &'a str, value: &'a mut T, options: &'a [(T, &'a str)]) -> Self {
        Segmented {
            value,
            options,
            name,
            width: None,
            id_salt: None,
            take_focus: false,
        }
    }
    /// Fixed width (the boards' `max-width` / `width`); default: all of it.
    pub fn width(mut self, w: f32) -> Self {
        self.width = Some(w);
        self
    }
    pub fn id_salt(mut self, salt: impl std::hash::Hash) -> Self {
        self.id_salt = Some(Id::new(salt));
        self
    }
    /// On `true`, the group's Tab stop (the chosen option) takes the
    /// keyboard focus this frame: the fallback when the control that had it
    /// goes away. The focus ring follows the window's keyboard-or-pointer
    /// state as usual.
    pub fn take_focus(mut self, on: bool) -> Self {
        self.take_focus = on;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let kit = super::super::kit(ui.ctx());
        let pal = kit.pal;
        let n = self.options.len().max(1);
        let w = self
            .width
            .unwrap_or(ui.available_width())
            .min(ui.available_width().max(self.width.unwrap_or(0.0)));
        let gid = ui.make_persistent_id(self.id_salt.unwrap_or(Id::new(self.name)));
        let (outer, mut group) = ui.allocate_exact_size(vec2(w, 38.0), Sense::hover());
        let idx = self
            .options
            .iter()
            .position(|(v, _)| v == &*self.value)
            .unwrap_or(0);
        let p = ui.painter().clone();
        p.rect(
            outer,
            8.0,
            pal.inset(),
            Stroke::new(1.0_f32, pal.line),
            StrokeKind::Inside,
        );
        let opt_w = (w - 2.0 - 6.0) / n as f32;
        let base = outer.min + vec2(4.0, 4.0);
        // the thumb: surface-raised with a 1 px line-strong ring outside
        let tx = slide(ui.ctx(), gid.with("thumb"), idx as f32, duration::BASE);
        let thumb = Rect::from_min_size(base + vec2(tx * opt_w, 0.0), vec2(opt_w, 30.0));
        p.rect_filled(thumb.expand(1.0), 7.0, pal.line_strong);
        p.rect_filled(thumb, 6.0, pal.surface_raised);
        let pending = ui.data(|d| d.get_temp::<usize>(pending_focus_id(gid)));
        keep_focus_on_stop(ui, (0..n).map(|k| gid.with(k)), gid.with(idx));
        if self.take_focus {
            ui.memory_mut(|m| m.request_focus(gid.with(idx)));
        }
        let mut new_idx = None;
        let opts = super::a11y_children(ui, outer, Role::RadioGroup, self.name);
        for (i, (_, label)) in self.options.iter().enumerate() {
            let cell = Rect::from_min_size(base + vec2(i as f32 * opt_w, 0.0), vec2(opt_w, 30.0));
            let id = gid.with(i);
            let sense = if i == idx {
                Sense::click()
            } else {
                Sense::CLICK
            };
            let r = opts.interact(cell, id, sense);
            let sel = i == idx;
            let lbl = label.to_string();
            r.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, sel, &lbl));
            if sel && pending == Some(i) {
                r.request_focus();
                ui.data_mut(|d| d.remove::<usize>(pending_focus_id(gid)));
            }
            if r.clicked() && !sel {
                new_idx = Some(i);
            }
            if sel {
                let d = arrow(ui, &r, false);
                if d != 0 {
                    new_idx = Some((idx as i32 + d).rem_euclid(n as i32) as usize);
                }
            }
            let hov = anim(ui.ctx(), id.with("h"), hovered(&r) || sel, duration::FAST);
            let c = lerp_color(pal.ink_muted, pal.ink, hov);
            let b = text::layout_elided(
                ui.ctx(),
                label,
                Type::OPTION.with_line(17.0),
                c,
                (opt_w - 12.0).max(8.0),
            );
            b.paint_centered_y(&p, cell.center().x - b.size().x / 2.0, cell.center().y);
            if sel && shows_focus(&kit, &r) {
                focus_ring(&p, cell, 6.0, pal.focus_ring());
            }
            if r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        if let Some(i) = new_idx {
            *self.value = self.options[i].0.clone();
            group.mark_changed();
            if ui.memory(|m| (0..n).any(|k| m.has_focus(gid.with(k)))) {
                ui.data_mut(|d| d.insert_temp(pending_focus_id(gid), i));
                ui.ctx().request_repaint();
            }
        }
        group
    }
}

/// The strength picker: a radio group of four, one row when every label
/// fits its quarter (with 4 px padding each side), otherwise 2 × 2 (a
/// normal state in longer languages, not an edge case).
#[must_use]
pub struct Strength<'a> {
    value: &'a mut usize,
    labels: &'a [&'a str],
    name: &'a str,
    two_by_two: Option<bool>,
    take_focus: bool,
    dense: bool,
    tips: &'a [&'a str],
    /// See [`Strength::fitted`].
    fitted: Option<(f32, bool)>,
    enabled: bool,
}

impl<'a> Strength<'a> {
    /// After an option that leads the scale: 4 px, a hairline, 4 px.
    const LEAD_GAP: f32 = 9.0;

    /// The dense group's height: as tall as a small button (Files'
    /// toolbar, `.s3-fx .hm-app-steps`: padding 1, 28 px options).
    pub const DENSE_H: f32 = 32.0;

    /// `value` past the last label selects none (a custom strength set from
    /// the command line): no option checked, every option takes a click,
    /// and an arrow on the first option picks the first (or, backwards, the
    /// last).
    pub fn new(name: &'a str, value: &'a mut usize, labels: &'a [&'a str]) -> Self {
        Strength {
            value,
            labels,
            name,
            two_by_two: None,
            take_focus: false,
            dense: false,
            tips: &[],
            fitted: None,
            enabled: true,
        }
    }
    /// Disabled: the whole group at 40 % like a disabled button, out of
    /// the tab order, taking neither a click nor a key; the checked
    /// option still shows and the tooltips stay.
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    /// One row whose options are as wide as their own words with `pad`
    /// px a side (room beyond that is shared equally) instead of equal
    /// parts. `lead`: the first option is not a step of the scale (Off
    /// before the strengths): a hairline stands after it.
    pub fn fitted(mut self, pad: f32, lead: bool) -> Self {
        self.fitted = Some((pad, lead));
        self
    }
    /// The width a dense [`Strength::fitted`] group takes at least.
    pub fn fitted_width(ctx: &egui::Context, labels: &[&str], pad: f32, lead: bool) -> f32 {
        let words: f32 = labels
            .iter()
            .map(|l| text::width(ctx, l, Type::OPTION).ceil() + 2.0 * pad)
            .sum();
        4.0 + words + if lead { Self::LEAD_GAP } else { 0.0 }
    }
    /// What each option does, in its order: an option's tooltip, and its
    /// description for screen readers.
    pub fn tips(mut self, tips: &'a [&'a str]) -> Self {
        self.tips = tips;
        self
    }
    /// One row of 28 px options in 1 px of padding, [`Self::DENSE_H`] in
    /// all, beside small buttons.
    pub fn dense(mut self) -> Self {
        self.dense = true;
        self
    }
    /// Force the layout (boards' tweak); default: decided by the labels.
    pub fn two_by_two(mut self, on: bool) -> Self {
        self.two_by_two = Some(on);
        self
    }
    /// On `true`, the group's Tab stop (the checked option, else the first)
    /// takes the keyboard focus this frame: the fallback when the control
    /// that had it goes away. The focus ring follows the window's
    /// keyboard-or-pointer state as usual.
    pub fn take_focus(mut self, on: bool) -> Self {
        self.take_focus = on;
        self
    }

    /// Whether `labels` need 2 × 2 at `width`.
    pub fn needs_two_by_two(ctx: &egui::Context, labels: &[&str], width: f32) -> bool {
        let share = (width - 8.0) / labels.len().max(1) as f32;
        labels
            .iter()
            .any(|l| text::width(ctx, l, Type::OPTION) + 8.0 > share)
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let kit = super::super::kit(ui.ctx());
        let pal = kit.pal;
        let w = ui.available_width();
        let n = self.labels.len().max(1);
        let grid = self.fitted.is_none()
            && self
                .two_by_two
                .unwrap_or_else(|| Self::needs_two_by_two(ui.ctx(), self.labels, w));
        let (cols, rows, gap) = if grid {
            (2, n.div_ceil(2), 2.0)
        } else {
            (n, 1, 0.0)
        };
        // the border and the padding inside it, and an option's height
        let (inset, oh) = if self.dense { (2.0, 28.0) } else { (4.0, 30.0) };
        let h = 2.0 * inset + rows as f32 * oh + (rows - 1) as f32 * gap;
        let gid = ui.make_persistent_id(Id::new(self.name).with("strength"));
        let (outer, mut group) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        let p = ui.painter().clone();
        // disabled: every part mixed against the surface behind, opaque
        let behind = super::backdrop(ui);
        let able = self.enabled;
        let tone = |under: Color32, c: Color32| {
            if able {
                c
            } else {
                super::faded(behind, under, c, 0.4)
            }
        };
        let ground = behind.blend(pal.inset());
        p.rect(
            outer,
            8.0,
            tone(behind, pal.inset()),
            Stroke::new(1.0_f32, tone(ground, pal.line)),
            StrokeKind::Inside,
        );
        let cw = (w - 2.0 * inset - (cols - 1) as f32 * gap) / cols as f32;
        // fitted: each option's left edge and width in the one row
        let fit: Option<Vec<(f32, f32)>> = self.fitted.map(|(pad, lead)| {
            let lead_gap = if lead { Self::LEAD_GAP } else { 0.0 };
            let natural: Vec<f32> = self
                .labels
                .iter()
                .map(|l| text::width(ui.ctx(), l, Type::OPTION).ceil() + 2.0 * pad)
                .collect();
            let spare = (w - 2.0 * inset - lead_gap - natural.iter().sum::<f32>()).max(0.0);
            let each = spare / n as f32;
            let mut x = inset;
            natural
                .iter()
                .enumerate()
                .map(|(i, nw)| {
                    let at = (x, nw + each);
                    x += nw + each + if i == 0 { lead_gap } else { 0.0 };
                    at
                })
                .collect()
        });
        // the hairline after the option that leads; the checked option's
        // ring beside it stands in for it
        if let (Some(fit), Some((_, true))) = (&fit, self.fitted) {
            if let Some((x0, w0)) = fit.first().filter(|_| *self.value > 1) {
                let x = outer.left() + x0 + w0 + (Self::LEAD_GAP - 1.0) / 2.0;
                p.rect_filled(
                    Rect::from_min_max(
                        pos2(x, outer.top() + inset + 5.0),
                        pos2(x + 1.0, outer.bottom() - inset - 5.0),
                    ),
                    0.0,
                    tone(ground, pal.line_strong),
                );
            }
        }
        let idx = (*self.value < self.labels.len()).then_some(*self.value);
        // the group's one Tab stop: the checked option, else the first
        let stop = idx.unwrap_or(0);
        let pending = ui.data(|d| d.get_temp::<usize>(pending_focus_id(gid)));
        if able {
            keep_focus_on_stop(ui, (0..n).map(|k| gid.with(k)), gid.with(stop));
        }
        if able && self.take_focus {
            ui.memory_mut(|m| m.request_focus(gid.with(stop)));
        }
        let mut new_idx = None;
        let opts = super::a11y_children(ui, outer, Role::RadioGroup, self.name);
        for (i, label) in self.labels.iter().enumerate() {
            let (c, r) = (i % cols, i / cols);
            let cell = match fit.as_ref().and_then(|f| f.get(i)) {
                Some((x, fw)) => Rect::from_min_size(outer.min + vec2(*x, inset), vec2(*fw, oh)),
                None => Rect::from_min_size(
                    outer.min + vec2(inset + c as f32 * (cw + gap), inset + r as f32 * (oh + gap)),
                    vec2(cw, oh),
                ),
            };
            let cw = cell.width();
            let id = gid.with(i);
            let sel = Some(i) == idx;
            let sense = if !able {
                Sense::hover()
            } else if i == stop {
                Sense::click()
            } else {
                Sense::CLICK
            };
            let mut resp = opts.interact(cell, id, sense);
            let lbl = label.to_string();
            resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, able, sel, &lbl));
            if let Some(tip) = self.tips.get(i).filter(|t| !t.is_empty()) {
                super::a11y(ui.ctx(), id, |n| n.set_description(*tip));
                resp = resp.on_hover_text(*tip);
            }
            if sel && pending == Some(i) {
                resp.request_focus();
                ui.data_mut(|d| d.remove::<usize>(pending_focus_id(gid)));
            }
            if able && resp.clicked() && !sel {
                new_idx = Some(i);
            }
            if able && i == stop {
                let d = arrow(ui, &resp, true);
                if d != 0 {
                    new_idx = Some(match idx {
                        Some(k) => (k as i32 + d).rem_euclid(n as i32) as usize,
                        None if d > 0 => 0,
                        None => n - 1,
                    });
                }
            }
            // checked: surface-raised with a 1 px line-strong ring outside; the
            // fill eases over 120 ms, the ring (a box-shadow) does not
            let on = anim(ui.ctx(), id.with("sel"), sel, duration::FAST);
            if sel {
                p.rect_filled(cell.expand(1.0), 7.0, tone(ground, pal.line_strong));
            }
            let fill = lerp_color(pal.inset(), pal.surface_raised, on);
            if on > 0.0 || sel {
                p.rect_filled(cell, 6.0, tone(behind, fill));
            }
            let hov = anim(
                ui.ctx(),
                id.with("h"),
                able && hovered(&resp) || sel,
                duration::FAST,
            );
            let under = if on > 0.0 || sel {
                behind.blend(fill)
            } else {
                ground
            };
            let col = tone(under, lerp_color(pal.ink_muted, pal.ink, hov));
            let b = text::layout_elided(ui.ctx(), label, Type::OPTION, col, (cw - 8.0).max(8.0));
            b.paint_centered_y(&p, cell.center().x - b.size().x / 2.0, cell.center().y);
            if able && i == stop && shows_focus(&kit, &resp) {
                focus_ring(&p, cell, 6.0, pal.focus_ring());
            }
            if able && resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        if let Some(i) = new_idx {
            *self.value = i;
            group.mark_changed();
            if ui.memory(|m| (0..n).any(|k| m.has_focus(gid.with(k)))) {
                ui.data_mut(|d| d.insert_temp(pending_focus_id(gid), i));
                ui.ctx().request_repaint();
            }
        }
        group
    }
}
