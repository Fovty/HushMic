//! Surfaces and small parts: the figure (`.hm-app-figure`), groups of rows
//! (`.hm-app-group`), notes (`.hm-app-note`), chips,
//! key chips, links, pick cards, the level meter, first run's step dots and
//! tick circles.

use eframe::egui::{
    self, pos2, vec2, Color32, CornerRadius, Frame, Id, InnerResponse, Margin, Painter, Pos2, Rect,
    Response, Sense, Shape, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType,
};

use eframe::egui::accesskit::Role;

use super::super::fonts::Type;
use super::super::icons::{self, Icon};
use super::super::paint::band;
use super::super::text::{self, Rich};
use super::super::tokens::radius;
use super::{focus_ring, hovered, lerp_color, set_backdrop, shows_focus};

/// The card (UX panel D1, one style on every page): `card_fill` in a
/// `line` hairline, 12 px radius. The fill alone, as a shape: a figure
/// painted by hand reserves its place under its content and sets it once
/// its height is known.
pub fn card_fill_shape(rect: Rect, pal: &super::super::tokens::Palette) -> Shape {
    let fill = Shape::rect_filled(rect, radius::MD, pal.card_fill());
    match pal.card_shadow() {
        Some(s) => Shape::Vec(vec![
            s.as_shape(rect, CornerRadius::same(radius::MD as u8))
                .into(),
            fill,
        ]),
        None => fill,
    }
}

/// The card's hairline, over its content.
pub fn card_edge(p: &Painter, rect: Rect, pal: &super::super::tokens::Palette) {
    p.rect_stroke(
        rect,
        radius::MD,
        Stroke::new(1.0_f32, pal.line),
        StrokeKind::Inside,
    );
}

/// `.hm-app-figure`: a card (`card_fill`, `line` border, 12 px radius) and
/// the frame light along its top edge (12 % to 88 %, fading in and out).
pub fn figure<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    let pal = super::super::kit(ui.ctx()).pal;
    let r = Frame::new()
        .fill(pal.card_fill())
        .stroke(Stroke::new(1.0_f32, pal.line))
        .shadow(pal.card_shadow().unwrap_or(egui::Shadow::NONE))
        .corner_radius(CornerRadius::same(radius::MD as u8))
        .show(ui, |ui| {
            set_backdrop(ui, pal.card_fill());
            ui.set_width(ui.available_width());
            add(ui)
        });
    let rect = r.response.rect;
    let (x0, x1) = (
        rect.left() + 0.12 * rect.width(),
        rect.right() - 0.12 * rect.width(),
    );
    let w = x1 - x0;
    let fl = pal.frame_light;
    ui.painter().add(Shape::mesh(band(
        x0,
        x1,
        rect.top(),
        rect.top() + 1.0,
        2.0,
        |x| {
            let f = (x - x0) / w;
            let k = if f < 0.3 {
                f / 0.3
            } else if f > 0.7 {
                (1.0 - f) / 0.3
            } else {
                1.0
            };
            fl.gamma_multiply(k.clamp(0.0, 1.0))
        },
    )));
    r
}

/// A 1 px `line` divider across the available width.
pub fn divider(ui: &mut Ui) -> Response {
    let pal = super::super::kit(ui.ctx()).pal;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, pal.line);
    resp
}

/// Rows inside a [`group`].
pub struct GroupUi<'a> {
    ui: &'a mut Ui,
    rows: usize,
}

impl GroupUi<'_> {
    /// A row with the group's padding (12 px top and bottom, 16 px sides).
    pub fn row<R>(&mut self, add: impl FnOnce(&mut Ui) -> R) -> R {
        self.row_with(Margin::symmetric(16, 12), add)
    }

    /// A row with its own padding (About's head row: 20 16).
    pub fn row_with<R>(&mut self, pad: Margin, add: impl FnOnce(&mut Ui) -> R) -> R {
        if self.rows > 0 {
            divider(self.ui);
        }
        self.rows += 1;
        Frame::new()
            .inner_margin(pad)
            .show(self.ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui)
            })
            .inner
    }

    pub fn ui(&mut self) -> &mut Ui {
        self.ui
    }
}

/// `.hm-app-group`: rows on a card (`card_fill`, `line` border, 12 px
/// radius), a `line` hairline between rows.
pub fn group<R>(ui: &mut Ui, add: impl FnOnce(&mut GroupUi<'_>) -> R) -> InnerResponse<R> {
    let pal = super::super::kit(ui.ctx()).pal;
    Frame::new()
        .fill(pal.card_fill())
        .stroke(Stroke::new(1.0_f32, pal.line))
        .shadow(pal.card_shadow().unwrap_or(egui::Shadow::NONE))
        .corner_radius(CornerRadius::same(radius::MD as u8))
        .show(ui, |ui| {
            set_backdrop(ui, pal.card_fill());
            ui.set_width(ui.available_width());
            ui.vertical(|ui| {
                let mut g = GroupUi { ui, rows: 0 };
                add(&mut g)
            })
            .inner
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    /// A warning triangle in `warn`.
    Warn,
    /// An info circle in `ink-muted`.
    Info,
}

/// `.hm-app-note`: a 16 px icon and a sentence (`lead` in ink 500, then
/// `body` in ink-muted), with an optional link under it. Returns the
/// link's response.
pub fn note(
    ui: &mut Ui,
    kind: NoteKind,
    lead: &str,
    body: &str,
    link_label: Option<&str>,
) -> Option<Response> {
    let pal = super::super::kit(ui.ctx()).pal;
    let (icon, c) = match kind {
        NoteKind::Warn => (Icon::Warn, pal.warn),
        NoteKind::Info => (Icon::Info, pal.ink_muted),
    };
    let mut out = None;
    Frame::new()
        .fill(pal.surface_raised)
        .stroke(Stroke::new(1.0_f32, pal.line))
        .corner_radius(CornerRadius::same(radius::SM as u8))
        .inner_margin(Margin {
            left: 12,
            right: 12,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            set_backdrop(ui, pal.surface_raised);
            let w = ui.available_width();
            ui.set_width(w);
            let start = ui.cursor().min;
            icons::paint(ui.painter(), icon, start + vec2(8.0, 2.0 + 8.0), 16.0, c);
            ui.horizontal_top(|ui| {
                ui.add_space(24.0);
                ui.vertical(|ui| {
                    let mut rich = Rich::new();
                    if !lead.is_empty() {
                        rich = rich.span(lead, Type::CAP.medium(), pal.ink).span(
                            " ",
                            Type::CAP,
                            pal.ink_muted,
                        );
                    }
                    let block = rich
                        .span(body, Type::CAP, pal.ink_muted)
                        .layout(ui.ctx(), w - 24.0);
                    text::show(ui, block);
                    if let Some(l) = link_label {
                        ui.add_space(4.0);
                        out = Some(link(ui, l, false, false));
                    }
                });
            });
        });
    out
}

/// `.hm-app-chip`: a command in mono 12 on `surface-raised`, `line` border,
/// 4 px radius. `height` is the outer height: the boards' CSS height is the
/// content box, so it is that plus the two borders (24 → 26, Shortcuts'
/// `height: 30px` → 32).
pub fn chip(ui: &mut Ui, label: &str, height: f32) -> Response {
    let pal = super::super::kit(ui.ctx()).pal;
    let b = text::layout(ui.ctx(), label, Type::MONO, pal.ink, f32::INFINITY);
    let (rect, resp) =
        ui.allocate_exact_size(vec2(b.size().x + 16.0 + 2.0, height), Sense::hover());
    let p = ui.painter();
    p.rect(
        rect,
        radius::XS,
        pal.surface_raised,
        Stroke::new(1.0_f32, pal.line),
        StrokeKind::Inside,
    );
    b.paint_centered_y(p, rect.left() + 9.0, rect.center().y);
    let l = label.to_owned();
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &l));
    resp
}

/// How long a copied chip says "Copied".
pub const COPIED_SECS: f64 = 1.5;

/// A command chip that copies itself (UX panel B15): a click (or Space /
/// Enter) puts `label` on the clipboard and the chip says "Copied" for
/// [`COPIED_SECS`], at the same width. A button named by the command.
pub fn copy_chip(ui: &mut Ui, label: &str, height: f32) -> Response {
    let ctx = ui.ctx().clone();
    let k = super::super::kit(&ctx);
    let pal = k.pal;
    let b = text::layout(&ctx, label, Type::MONO, pal.ink, f32::INFINITY);
    let copied_word = crate::tr!("copy-diagnostics-done");
    let c = text::layout(
        &ctx,
        &copied_word,
        Type::CAP.medium(),
        pal.accent,
        f32::INFINITY,
    );
    let w = b.size().x.max(c.size().x) + 16.0 + 2.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, height), Sense::click());
    let key = resp.id.with("copied-at");
    let now = ctx.input(|i| i.time);
    if resp.clicked() {
        ctx.copy_text(label.to_owned());
        ctx.data_mut(|d| d.insert_temp(key, now));
    }
    let at: Option<f64> = ctx.data(|d| d.get_temp(key));
    let copied = at.is_some_and(|t| now - t < COPIED_SECS);
    if let Some(t) = at.filter(|_| copied) {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(
            COPIED_SECS - (now - t) + 0.01,
        ));
    }
    let hover = super::hovered(&resp);
    let p = ui.painter();
    p.rect(
        rect,
        radius::XS,
        if hover {
            pal.surface_hover
        } else {
            pal.surface_raised
        },
        Stroke::new(1.0_f32, if hover { pal.line_strong } else { pal.line }),
        StrokeKind::Inside,
    );
    if copied {
        c.paint_centered_y(p, rect.center().x - c.size().x / 2.0, rect.center().y);
    } else {
        b.paint_centered_y(p, rect.left() + 9.0, rect.center().y);
    }
    if super::shows_focus(&k, &resp) {
        super::focus_ring(p, rect, radius::XS, pal.focus_ring());
    }
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let name = if copied {
        format!("{label} ({copied_word})")
    } else {
        label.to_owned()
    };
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &name));
    resp
}

/// `.hm-app-kbd`: the desktop's key text exactly as it sends it, one chip,
/// never split, shortened with "…" past 200 px. 26 px tall, `line-strong`
/// border.
pub fn kbd(ui: &mut Ui, keys: &str) -> Response {
    let pal = super::super::kit(ui.ctx()).pal;
    let b = text::layout_elided(ui.ctx(), keys, Type::MONO, pal.ink, 200.0 - 18.0);
    let (rect, resp) =
        ui.allocate_exact_size(vec2((b.size().x + 18.0).min(200.0), 26.0), Sense::hover());
    let p = ui.painter();
    p.rect(
        rect,
        radius::XS,
        pal.surface_raised,
        Stroke::new(1.0_f32, pal.line_strong),
        StrokeKind::Inside,
    );
    b.paint_centered_y(p, rect.left() + 9.0, rect.center().y);
    let k = keys.to_owned();
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &k));
    resp
}

/// `.hm-app-link`: 500 13/18 ink, underlined in `line-strong` 3 px below
/// the baseline (ink on hover); `external` adds the 14 px arrow 6 px after;
/// `quiet` is ink-muted.
pub fn link(ui: &mut Ui, label: &str, external: bool, quiet: bool) -> Response {
    let kit = super::super::kit(ui.ctx());
    let pal = kit.pal;
    let color = if quiet { pal.ink_muted } else { pal.ink };
    let b = text::layout(
        ui.ctx(),
        label,
        Type::LINK,
        color,
        ui.available_width() - if external { 20.0 } else { 0.0 },
    );
    let w = b.size().x + if external { 20.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, b.size().y), Sense::click());
    let l = label.to_owned();
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Link, true, &l));
    let p = ui.painter();
    // no transition on the boards: the underline changes at once
    let hov = if hovered(&resp) { 1.0 } else { 0.0 };
    b.paint(p, rect.min);
    // the underline under each row of the text
    let under = lerp_color(pal.line_strong, pal.ink, hov);
    let base = b.baseline(ui.ctx().pixels_per_point());
    for row in b.galley.rows.iter() {
        let y = (rect.top() + row.rect().top() + base + 3.0).round();
        let x0 = rect.left() + row.rect().left();
        p.rect_filled(
            Rect::from_min_size(pos2(x0, y), vec2(row.rect().width(), 1.0)),
            0.0,
            under,
        );
    }
    if external {
        icons::paint(
            p,
            Icon::External,
            pos2(rect.right() - 7.0, rect.center().y),
            14.0,
            color,
        );
    }
    if shows_focus(&kit, &resp) {
        focus_ring(p, rect, 2.0, pal.focus_ring());
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// `.hm-app-pick`: a radio card (first run's microphone list): a 16 px radio,
/// the name (label) and a note (caption); selected has an `ink` border on
/// `surface-raised`.
pub fn pick(ui: &mut Ui, selected: bool, name: &str, note: &str) -> Response {
    pick_with(ui, selected, name, note, Sense::click(), None)
}

/// How a pick was chosen: an arrow key (the highlight moved; debounce the
/// send) or a click, Space or Enter (send at once).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickBy {
    Arrow,
    Activate,
}

/// The pick a group's user chose this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PickChoice {
    pub index: usize,
    pub by: PickBy,
}

/// An unconfirmed highlight goes back to the confirmed pick after this
/// long (the daemon refused, or nothing was sent).
pub const PICK_SETTLE: f64 = 2.5;

/// Picks are known by their item (`keys`), not their place: an item that
/// leaves the list takes its highlight and focus with it.
#[derive(Clone, Copy, Default)]
struct PickState {
    /// The highlight ahead of the confirmed pick, and when it last moved.
    pending: Option<Id>,
    since: f64,
    /// The pick that had keyboard focus at the end of the pass `pass`.
    focus: Option<Id>,
    pass: u64,
}

/// [`pick`]s as one radio group (first run's microphone list). The
/// highlight is the group's one Tab stop and shows as chosen; the arrow
/// keys move it at once, counting from the highlight, so three Downs reach
/// the third pick before the daemon confirms the first. Keyboard focus
/// follows it. The highlight snaps back to the confirmed pick when that
/// arrives, after [`PICK_SETTLE`] without it, or on [`PickGroup::snap`].
/// The group makes no accessibility node of its own: draw the picks in a
/// `RadioGroup` (`a11y_children`) named for the list.
///
/// ```ignore
/// let mut g = PickGroup::begin(ui, id, &mic_ids, selected);
/// for (i, m) in mics.iter().enumerate() {
///     g.pick(&mut child_ui, i, &m.name, &m.note);
/// }
/// if let Some(c) = g.end(ui) { /* send c.index: at once, or debounced for arrows */ }
/// ```
pub struct PickGroup {
    id: Id,
    /// One id per pick, from its item's key (also the pick's widget id).
    keys: Vec<Id>,
    hl: usize,
    now: f64,
    st: PickState,
    focus_now: Option<Id>,
    choice: Option<PickChoice>,
}

impl PickGroup {
    /// One pick per key (an item's stable name, such as the mic's node
    /// name), in list order; `selected` is the confirmed one's index.
    pub fn begin<K: std::hash::Hash>(ui: &Ui, id: Id, keys: &[K], selected: usize) -> PickGroup {
        let now = ui.input(|i| i.time);
        let keys: Vec<Id> = keys.iter().map(|k| id.with(k)).collect();
        let confirmed = keys.get(selected).copied();
        let mut st = ui.data(|d| d.get_temp::<PickState>(id)).unwrap_or_default();
        // focus seen before the group was hidden says nothing now
        if st.pass + 1 != ui.ctx().cumulative_pass_nr() {
            st.focus = None;
        }
        if let Some(p) = st.pending {
            let gone = !keys.contains(&p);
            if Some(p) == confirmed || gone || now - st.since > PICK_SETTLE {
                st.pending = None;
            } else {
                // wake up to let an unconfirmed highlight go back
                let left = (st.since + PICK_SETTLE - now).max(0.0);
                super::super::motion::repaint_in(
                    ui.ctx(),
                    std::time::Duration::from_secs_f64(left),
                );
            }
        }
        let hl = st
            .pending
            .and_then(|p| keys.iter().position(|k| *k == p))
            .unwrap_or(selected)
            .min(keys.len().saturating_sub(1));
        if let Some(stop) = keys.get(hl).copied() {
            super::choice::keep_focus_on_stop(ui, keys.iter().copied(), stop);
        }
        PickGroup {
            id,
            keys,
            hl,
            now,
            st,
            focus_now: None,
            choice: None,
        }
    }

    /// The highlighted pick (shown as chosen).
    pub fn highlight(&self) -> usize {
        self.hl
    }

    /// Pick `i` of the group.
    pub fn pick(&mut self, ui: &mut Ui, i: usize, name: &str, note: &str) -> Response {
        let on = i == self.hl;
        let key = self
            .keys
            .get(i)
            .copied()
            .unwrap_or(self.id.with(("pick", i)));
        let sense = if on { Sense::click() } else { Sense::CLICK };
        let resp = pick_with(ui, on, name, note, sense, Some(key));
        if on && self.st.focus.is_some_and(|f| f != key) && !resp.has_focus() {
            // the highlight moved under the keyboard (or its item left the
            // list): focus goes with it
            resp.request_focus();
        }
        if resp.has_focus() {
            self.focus_now = Some(key);
        }
        let n = self.keys.len();
        if on && n > 1 {
            let step = super::choice::arrow(ui, &resp, true);
            if step != 0 {
                let j = (i as i32 + step).rem_euclid(n as i32) as usize;
                self.move_to(j, PickBy::Arrow);
                ui.ctx().request_repaint();
            }
        }
        if resp.clicked() {
            self.move_to(i, PickBy::Activate);
        }
        resp
    }

    fn move_to(&mut self, j: usize, by: PickBy) {
        self.hl = j;
        self.st.pending = self.keys.get(j).copied();
        self.st.since = self.now;
        self.choice = Some(PickChoice { index: j, by });
    }

    /// Store the group's state; what the user chose this frame, if anything.
    pub fn end(mut self, ui: &Ui) -> Option<PickChoice> {
        self.st.focus = self.focus_now;
        self.st.pass = ui.ctx().cumulative_pass_nr();
        ui.data_mut(|d| d.insert_temp(self.id, self.st));
        self.choice
    }

    /// Drop the highlight back to the confirmed pick (the daemon refused).
    pub fn snap(ctx: &egui::Context, id: Id) {
        ctx.data_mut(|d| d.get_temp_mut_or_default::<PickState>(id).pending = None);
        ctx.request_repaint();
    }
}

fn pick_with(
    ui: &mut Ui,
    selected: bool,
    name: &str,
    note: &str,
    sense: Sense,
    id: Option<Id>,
) -> Response {
    let kit = super::super::kit(ui.ctx());
    let pal = kit.pal;
    let w = ui.available_width();
    let tw = w - 32.0 - 2.0 - 16.0 - 12.0;
    let nb = text::layout(ui.ctx(), name, Type::LABEL, pal.ink, tw);
    let cb = text::layout(ui.ctx(), note, Type::CAP, pal.ink_subtle, tw);
    let h = 1.0 + 12.0 + nb.size().y + 2.0 + cb.size().y + 12.0 + 1.0;
    let (rect, resp) = match id {
        Some(id) => {
            let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
            (rect, ui.interact(rect, id, sense))
        }
        None => ui.allocate_exact_size(vec2(w, h), sense),
    };
    let n = name.to_owned();
    resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, selected, &n));
    // the card's second line is its description: screen readers read it
    // after the name (UX panel round 2, A34)
    if !note.is_empty() {
        let d = note.to_owned();
        super::a11y(ui.ctx(), resp.id, |n| n.set_description(d));
    }
    // no transition on the boards: the border changes at once
    let hov = if hovered(&resp) { 1.0 } else { 0.0 };
    let border = if selected {
        pal.ink
    } else {
        lerp_color(pal.line, pal.line_strong, hov)
    };
    let fill = if selected {
        pal.surface_raised
    } else {
        pal.surface
    };
    let p = ui.painter();
    p.rect(
        rect,
        radius::MD,
        fill,
        Stroke::new(1.0_f32, border),
        StrokeKind::Inside,
    );
    let rc = pos2(rect.left() + 17.0 + 8.0, rect.top() + 13.0 + 2.0 + 8.0);
    radio(p, rc, selected, &pal);
    let tx = rect.left() + 17.0 + 16.0 + 12.0;
    nb.paint(p, pos2(tx, rect.top() + 13.0));
    cb.paint(p, pos2(tx, rect.top() + 13.0 + nb.size().y + 2.0));
    if shows_focus(&kit, &resp) {
        focus_ring(p, rect, radius::MD, pal.focus_ring());
    }
    resp
}

/// A 16 px radio mark: Chrome's native radio with `accent-color: ink`, as
/// the boards show it, measured: checked is a 1 px ink ring, a gap (white,
/// or #3b3b3b when the accent is light) and an ink dot 4.8 px in radius;
/// unchecked is white in a 1 px #767676 ring. In dark, unchecked is a dark
/// control (design call; the boards' white disc is Chrome's light scheme):
/// `surface` in a `line-strong` ring, the control border that keeps 3:1.
pub fn radio(p: &Painter, c: Pos2, on: bool, pal: &super::super::Palette) {
    if on {
        let gap = if pal.is_dark() {
            Color32::from_gray(59)
        } else {
            Color32::WHITE
        };
        p.circle_filled(c, 8.0, pal.ink);
        p.circle_filled(c, 7.0, gap);
        p.circle_filled(c, 4.8, pal.ink);
    } else if pal.is_dark() {
        p.circle_filled(c, 8.0, pal.line_strong);
        p.circle_filled(c, 7.0, pal.surface);
    } else {
        p.circle_filled(c, 8.0, Color32::from_gray(118));
        p.circle_filled(c, 7.0, Color32::WHITE);
    }
}

/// The design system's level meter (`.hm-meter`), as first run uses it: a
/// mono label in a 56 px column, then `cells` cells (gap 3, 14 px tall,
/// 2 px radius) lit up to `level` in `ink-muted` (raw) or the accent.
pub fn level_meter(ui: &mut Ui, label: &str, level: f32, cells: usize, filtered: bool) -> Response {
    let pal = super::super::kit(ui.ctx()).pal;
    let w = ui.available_width();
    let lb = text::layout(
        ui.ctx(),
        label,
        Type::EYEBROW
            .with_size(11.0)
            .with_line(14.0)
            .with_track(0.12),
        pal.ink_subtle,
        56.0,
    );
    let h = lb.size().y.max(14.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let p = ui.painter();
    lb.paint_centered_y(p, rect.left(), rect.center().y);
    let x0 = rect.left() + 56.0 + 12.0;
    let cw = (rect.right() - x0 - 3.0 * (cells as f32 - 1.0)) / cells as f32;
    let lit = (level.clamp(0.0, 1.0) * cells as f32).round() as usize;
    for i in 0..cells {
        let r = Rect::from_min_size(
            pos2(x0 + i as f32 * (cw + 3.0), rect.center().y - 7.0),
            vec2(cw, 14.0),
        );
        let c = if i < lit {
            if filtered {
                pal.accent
            } else {
                pal.ink_muted
            }
        } else {
            pal.line
        };
        p.rect_filled(r, 2.0, c);
    }
    // a stable name, and the lit share of the cells as the value (a meter,
    // 0 to 1): it changes only when a cell does, not at the meter's 20 Hz
    let l = label.to_owned();
    let value = lit as f64 / cells.max(1) as f64;
    resp.widget_info(|| {
        let mut info = WidgetInfo::labeled(WidgetType::ProgressIndicator, true, &l);
        info.value = Some(value);
        info
    });
    super::a11y(ui.ctx(), resp.id, |n| {
        n.set_role(Role::Meter);
        n.set_min_numeric_value(0.0);
        n.set_max_numeric_value(1.0);
    });
    resp
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Done,
    Current,
    Ahead,
}

/// First run's step list: numbered 20 px circles with labels, joined by
/// 24 px hairlines. `labels_for_current_only` hides the other labels (the
/// compact window keeps them for screen readers). `name` is the list's
/// accessible name with the current step ("Step 2 of 3: Hear it work");
/// each step is an item, the current one selected.
pub fn step_dots(
    ui: &mut Ui,
    name: &str,
    steps: &[(&str, StepState)],
    labels_for_current_only: bool,
) -> Response {
    let pal = super::super::kit(ui.ctx()).pal;
    let mut x = 0.0;
    let mut parts = Vec::new();
    for (i, (label, st)) in steps.iter().enumerate() {
        let color = if *st == StepState::Current {
            pal.ink
        } else {
            pal.ink_subtle
        };
        let show_label = !labels_for_current_only || *st == StepState::Current;
        let b = text::layout(ui.ctx(), label, Type::OPTION, color, f32::INFINITY);
        let lw = if show_label { b.size().x + 8.0 } else { 0.0 };
        parts.push((x, b, *st, show_label, i + 1));
        x += 20.0 + lw;
        if i + 1 < steps.len() {
            x += 8.0 + 24.0 + 10.0;
        }
    }
    let (rect, resp) = ui.allocate_exact_size(vec2(x, 20.0), Sense::hover());
    let items = super::a11y_children(ui, rect, Role::List, name);
    let p = ui.painter();
    let cy = rect.center().y;
    // Chrome snaps the circles' borders to the pixel grid
    let ppp = ui.ctx().pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    for (k, (x, b, st, show_label, num)) in parts.iter().enumerate() {
        let c = pos2(snap(rect.left() + x + 10.0), snap(cy));
        let step = Rect::from_min_size(pos2(rect.left() + x, rect.top()), vec2(20.0, 20.0));
        let sid = resp.id.with(("step", k));
        let label = steps[k].0.to_owned();
        let sr = items.interact(step, sid, Sense::hover());
        sr.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &label));
        let current = *st == StepState::Current;
        // a list item takes its name from `label` (egui put the words in
        // `value`, which AT-SPI does not read as the name)
        super::a11y(ui.ctx(), sid, |n| {
            n.set_role(Role::ListItem);
            n.set_label(label.as_str());
            n.clear_value();
            n.set_selected(current);
        });
        let ring = if *st == StepState::Ahead {
            pal.line_strong
        } else {
            pal.ink
        };
        if *st == StepState::Done {
            p.circle_filled(c, 10.0, pal.ink);
            icons::paint(p, Icon::Tick, c, 12.0, pal.bg);
        } else {
            // a 20 px circle's 1 px border (egui strokes outside the radius)
            p.circle_stroke(c, 9.0, Stroke::new(1.0_f32, ring));
            let nc = if *st == StepState::Current {
                pal.ink
            } else {
                pal.ink_subtle
            };
            let nb = text::layout(
                ui.ctx(),
                &num.to_string(),
                Type::MONO_SMALL,
                nc,
                f32::INFINITY,
            );
            // the numeral where the box's centre falls, unsnapped
            nb.paint_centered_y(p, rect.left() + x + 10.0 - nb.size().x / 2.0, cy);
        }
        let mut lx = rect.left() + x + 20.0;
        if *show_label {
            lx += 8.0;
            b.paint_centered_y(p, lx, cy);
            lx += b.size().x;
        }
        if k + 1 < parts.len() {
            p.rect_filled(
                Rect::from_min_size(pos2(lx + 8.0, cy - 0.5), vec2(24.0, 1.0)),
                0.0,
                pal.line,
            );
        }
    }
    resp
}

/// A 20 px tick circle: done (ink with a 12 px tick in `bg`) or to do (a
/// dashed `line-strong` ring). Screen readers get a read-only check box
/// named `name`.
pub fn tick(ui: &mut Ui, name: &str, done: bool) -> Response {
    let pal = super::super::kit(ui.ctx()).pal;
    let (rect, resp) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
    // a read-only check: "Pick your microphone, check box, checked"
    let n = name.to_owned();
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, done, &n));
    super::a11y(ui.ctx(), resp.id, |node| node.set_read_only());
    let p = ui.painter();
    let c = rect.center();
    if done {
        p.circle_filled(c, 10.0, pal.ink);
        icons::paint(p, Icon::Tick, c, 12.0, pal.bg);
    } else {
        let n = 20;
        for i in 0..n {
            if i % 2 == 1 {
                continue;
            }
            let a0 = std::f32::consts::TAU * i as f32 / n as f32;
            let a1 = std::f32::consts::TAU * (i as f32 + 1.0) / n as f32;
            let pts: Vec<Pos2> = (0..=4)
                .map(|k| {
                    let a = a0 + (a1 - a0) * k as f32 / 4.0;
                    c + vec2(a.cos(), a.sin()) * 9.5
                })
                .collect();
            p.add(Shape::line(pts, Stroke::new(1.0_f32, pal.line_strong)));
        }
    }
    resp
}
