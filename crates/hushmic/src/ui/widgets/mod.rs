//! The kit's widgets, drawn to the app boards' CSS (`chrome.py` and the
//! design system's `bundle.css`). Every control is a real egui widget: it
//! takes keyboard focus, shows the focus ring on keyboard focus, and names
//! itself for screen readers and UI tests.

pub mod button;
pub mod cards;
pub mod choice;
pub mod rail;
pub mod scroll;
pub mod select;
pub mod switch;

pub use button::{Button, ButtonSize, ButtonVariant};
pub use cards::{
    card_edge, card_fill_shape, chip, copy_chip, divider, figure, group, kbd, level_meter, link,
    note, pick, step_dots, tick, GroupUi, NoteKind, PickBy, PickChoice, PickGroup, StepState,
    PICK_SETTLE,
};
pub use choice::{Segmented, Strength};
pub use rail::{Page, Rail, RailItem, RailResponse};
pub use select::{menu, select, MenuItem, SelectItem};
pub use switch::Switch;

use eframe::egui::{self, Color32, Id, Painter, Rect, Response, Stroke, StrokeKind, Ui};

use super::motion::{ease_standard, smooth};
use super::tokens::Palette;
use super::{kit, Kit};

/// A child `Ui` over `rect` whose own accessibility node is the container
/// `role` (a radio group, the rail's tab list, a list, a menu): widgets made
/// in it are that node's children, so screen readers say "2 of 4".
pub(crate) fn a11y_children(
    ui: &mut Ui,
    rect: Rect,
    role: egui::accesskit::Role,
    label: &str,
) -> Ui {
    let child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    a11y(ui.ctx(), child.unique_id(), |n| {
        n.set_role(role);
        if !label.is_empty() {
            n.set_label(label);
        }
    });
    child
}

/// Adjust a widget's accessibility node after `widget_info`, for the roles
/// egui's `WidgetType` has no word for (tabs, list options, menus).
pub(crate) fn a11y(ctx: &egui::Context, id: Id, f: impl FnOnce(&mut egui::accesskit::Node)) {
    ctx.accesskit_node_builder(id, f);
}

/// The focus ring: 2 px `focus-ring`, 2 px outside the control, following
/// its radius (CSS `outline: 2px solid; outline-offset: 2px`).
/// A highlighted row (pointer, keyboard highlight): the hover fill at `k`,
/// and in light the edge that makes it read (`Palette::highlight_edge`).
pub fn highlight(painter: &Painter, rect: Rect, radius: f32, pal: &Palette, k: f32) {
    if k <= 0.0 {
        return;
    }
    painter.rect_filled(rect, radius, pal.surface_hover.gamma_multiply(k));
    if let Some(edge) = pal.highlight_edge() {
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(1.0_f32, edge.gamma_multiply(k)),
            StrokeKind::Inside,
        );
    }
}

/// A selected row, the current tab or the chosen option of a list: the
/// selection's fill in its hairline, one model in both themes (UX panel
/// round 2, A18; `Palette::selected_fill`).
pub fn selected(painter: &Painter, rect: Rect, radius: f32, pal: &Palette) {
    painter.rect(
        rect,
        radius,
        pal.selected_fill(),
        Stroke::new(1.0_f32, pal.selected_line()),
        egui::StrokeKind::Inside,
    );
}

pub fn focus_ring(painter: &Painter, rect: Rect, radius: f32, pal_accent: Color32) {
    painter.rect_stroke(
        rect.expand(3.0),
        radius + 3.0,
        Stroke::new(2.0_f32, pal_accent),
        StrokeKind::Middle,
    );
}

/// Whether to show `response`'s focus ring (keyboard focus only).
pub fn shows_focus(kit: &Kit, response: &Response) -> bool {
    (kit.focus_visible && response.has_focus()) || forced(&response.ctx) == Some(Force::Focus)
}

/// A state the widget gallery forces on the widgets it draws, so every state
/// can be screenshotted without a pointer or keyboard. Never set in the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Force {
    Hover,
    Press,
    Focus,
    /// Dropdowns drawn open.
    Open,
}

fn force_id() -> Id {
    Id::new("hm-force")
}

/// Draw `add` with every kit widget in it showing `state`.
pub fn with_forced<R>(ui: &mut egui::Ui, state: Force, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.ctx()
        .data_mut(|d| d.insert_temp(force_id(), Some(state)));
    let r = add(ui);
    ui.ctx()
        .data_mut(|d| d.insert_temp::<Option<Force>>(force_id(), None));
    r
}

pub(crate) fn forced(ctx: &egui::Context) -> Option<Force> {
    ctx.data(|d| d.get_temp::<Option<Force>>(force_id()))
        .flatten()
}

/// Hovered, or forced to look hovered.
pub fn hovered(r: &Response) -> bool {
    r.hovered() || forced(&r.ctx) == Some(Force::Hover)
}

/// Held down by the pointer, or forced to look pressed.
pub fn pressed(r: &Response) -> bool {
    r.is_pointer_button_down_on() || forced(&r.ctx) == Some(Force::Press)
}

/// A 0..1 transition towards `on` over `secs` with the standard ease
/// (instant under reduced motion). Asks for frames only while it moves.
pub fn anim(ctx: &egui::Context, id: Id, on: bool, secs: f32) -> f32 {
    anim_with(ctx, id, on, secs, ease_standard)
}

/// Like [`anim`] with the brand ease-out (things that enter or settle).
pub fn anim_out(ctx: &egui::Context, id: Id, on: bool, secs: f32) -> f32 {
    anim_with(ctx, id, on, secs, super::motion::ease_out)
}

/// A transition on the kit's own clock. egui's `animate_bool` asks for
/// every display frame while it runs; this one asks through
/// [`super::motion::moment_repaint`], so a software renderer stays capped
/// near 30 fps. Linear progress, then `easing` as egui applies it (the way
/// back mirrors the way in). The first sight of an id shows its target.
pub fn anim_with(ctx: &egui::Context, id: Id, on: bool, secs: f32, easing: fn(f32) -> f32) -> f32 {
    #[derive(Clone, Copy)]
    struct Tween {
        v: f32,
        t: f64,
    }
    let secs = if kit(ctx).reduced_motion { 0.0 } else { secs };
    let now = ctx.input(|i| i.time);
    let target = if on { 1.0 } else { 0.0 };
    let key = id.with("hm-tween");
    let v = match ctx.data(|d| d.get_temp::<Tween>(key)) {
        Some(s) if secs > 0.0 => {
            let mut dt = (now - s.t).max(0.0);
            if s.v == 0.0 || s.v == 1.0 {
                // a start from rest: one frame's step, not the whole rest
                // (egui caps its first step the same way)
                dt = dt.min(ctx.input(|i| i.predicted_dt) as f64);
            }
            let step = (dt / secs as f64) as f32;
            if s.v < target {
                (s.v + step).min(target)
            } else {
                (s.v - step).max(target)
            }
        }
        _ => target,
    };
    ctx.data_mut(|d| d.insert_temp(key, Tween { v, t: now }));
    if v != target {
        super::motion::moment_repaint(ctx);
    }
    if on {
        easing(v)
    } else {
        1.0 - easing(1.0 - v)
    }
}

/// Mix two colours by a transition value (premultiplied, as CSS
/// transitions interpolate).
pub fn lerp_color(a: Color32, b: Color32, k: f32) -> Color32 {
    let k = k.clamp(0.0, 1.0);
    if k <= 0.0 {
        return a;
    }
    if k >= 1.0 {
        return b;
    }
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round() as u8;
    Color32::from_rgba_premultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

/// The opaque surface behind what `ui` paints: `panel_fill`, which the
/// kit's panels (figure, group, note, banner) and popups (lists, menus) set
/// for their contents. A page that paints its own surface (a `Frame` with
/// another fill, an `Area`) calls [`set_backdrop`] in that ui, or a
/// disabled control inside fades against `bg`.
pub fn backdrop(ui: &Ui) -> Color32 {
    ui.visuals().panel_fill
}

/// Mark `c` as the surface behind `ui`'s contents (see [`backdrop`]).
pub fn set_backdrop(ui: &mut Ui, c: Color32) {
    ui.visuals_mut().panel_fill = c;
}

/// CSS `opacity: op` on a whole control: the colour a part `c` that sits
/// on `under` (the control's own layers over `behind`, opaque) shows once
/// the control fades over `behind`. The result is opaque, so a label on its
/// fill or an icon's overlapping strokes fade as one, never stacking.
/// Transparent parts stay transparent.
pub fn faded(behind: Color32, under: Color32, c: Color32, op: f32) -> Color32 {
    if c.a() == 0 {
        return Color32::TRANSPARENT;
    }
    lerp_color(behind, under.blend(c), op)
}

/// Ease a 0..1 hover value (for colours that should not snap).
pub fn soft(x: f32) -> f32 {
    smooth(x)
}

#[cfg(test)]
mod pick_tests {
    use super::*;
    use eframe::egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput};

    const NAMES: [&str; 4] = [
        "System default",
        "RODE NT-USB",
        "Built-in Audio",
        "USB Headset",
    ];

    /// First run's list over a fake daemon: `selected` is what it confirmed.
    struct List {
        ctx: egui::Context,
        group: Id,
        t: f64,
        /// The microphones present now.
        names: Vec<&'static str>,
        ids: Vec<Id>,
        rects: Vec<Rect>,
    }

    impl List {
        fn new() -> List {
            let ctx = egui::Context::default();
            super::super::install(&ctx, super::super::Theme::Dark);
            List {
                ctx,
                group: Id::new("mics"),
                t: 1.0,
                names: NAMES.to_vec(),
                ids: vec![],
                rects: vec![],
            }
        }
        /// One frame 50 ms after the last; returns the choice and the highlight.
        fn frame(&mut self, events: Vec<Event>, selected: usize) -> (Option<PickChoice>, usize) {
            self.t += 0.05;
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(600.0, 500.0))),
                time: Some(self.t),
                events,
                ..Default::default()
            };
            let (mut out, mut ids, mut rects) = ((None, 0), vec![], vec![]);
            let (group, names) = (self.group, self.names.clone());
            let _ = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut g = PickGroup::begin(ui, group, &names, selected);
                    for (i, n) in names.iter().enumerate() {
                        let r = g.pick(ui, i, n, "");
                        ids.push(r.id);
                        rects.push(r.rect);
                    }
                    let hl = g.highlight();
                    out = (g.end(ui), hl);
                    let _ = ui.add(Button::secondary("Continue"));
                });
            });
            self.ids = ids;
            self.rects = rects;
            out
        }
        fn focused(&self) -> Option<usize> {
            let f = self.ctx.memory(|m| m.focused())?;
            self.ids.iter().position(|id| *id == f)
        }
    }

    fn key(key: Key) -> Vec<Event> {
        vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }]
    }

    fn arrow(i: usize) -> Option<PickChoice> {
        Some(PickChoice {
            index: i,
            by: PickBy::Arrow,
        })
    }

    #[test]
    fn arrows_run_ahead_of_the_daemon_and_focus_follows() {
        let mut l = List::new();
        l.frame(vec![], 0);
        l.frame(key(Key::Tab), 0);
        l.frame(vec![], 0);
        assert_eq!(l.focused(), Some(0), "the chosen pick is the Tab stop");
        // three Downs before the daemon confirms anything reach the fourth
        for want in 1..=3 {
            assert_eq!(l.frame(key(Key::ArrowDown), 0).0, arrow(want));
            let (_, hl) = l.frame(vec![], 0);
            assert_eq!(hl, want);
            assert_eq!(l.focused(), Some(want), "focus follows the highlight");
        }
        // the daemon confirms the last one: nothing moves
        assert_eq!(l.frame(vec![], 3), (None, 3));
        // Space on it sends at once
        let (c, _) = l.frame(key(Key::Space), 3);
        assert_eq!(c.map(|c| c.by), Some(PickBy::Activate));
        // Tab leaves the group and is not pulled back
        l.frame(key(Key::Tab), 3);
        l.frame(vec![], 3);
        assert_eq!(l.focused(), None);
    }

    #[test]
    fn an_unconfirmed_highlight_goes_back() {
        let mut l = List::new();
        l.frame(vec![], 1);
        l.frame(key(Key::Tab), 1);
        l.frame(vec![], 1);
        l.frame(key(Key::ArrowUp), 1);
        assert_eq!(l.frame(vec![], 1).1, 0);
        // the daemon refuses (nothing changes): back after the settle time
        let frames = (PICK_SETTLE / 0.05) as usize + 2;
        let mut hl = 0;
        for _ in 0..frames {
            hl = l.frame(vec![], 1).1;
        }
        assert_eq!(hl, 1);
        assert_eq!(l.focused(), Some(1), "focus went back with it");
        // or at once, when the page knows it was refused
        l.frame(key(Key::ArrowDown), 1);
        assert_eq!(l.frame(vec![], 1).1, 2);
        PickGroup::snap(&l.ctx, l.group);
        assert_eq!(l.frame(vec![], 1).1, 1);
    }

    #[test]
    fn the_highlight_stays_with_its_microphone() {
        let mut l = List::new();
        l.frame(vec![], 0);
        l.frame(key(Key::Tab), 0);
        l.frame(vec![], 0);
        l.frame(key(Key::ArrowDown), 0);
        l.frame(key(Key::ArrowDown), 0);
        assert_eq!(l.frame(vec![], 0).1, 2, "on Built-in Audio");
        // RODE NT-USB is unplugged: Built-in Audio moves up to place 1
        l.names.retain(|n| *n != "RODE NT-USB");
        assert_eq!(l.frame(vec![], 0).1, 1, "still on Built-in Audio");
        assert_eq!(l.focused(), Some(1), "focus stays with it");
        // the daemon confirms it
        assert_eq!(l.frame(vec![], 1).1, 1);
        // now it is unplugged too, before a new choice arrives: the
        // highlight goes to what the daemon says (System default)
        l.frame(key(Key::ArrowDown), 1);
        assert_eq!(l.frame(vec![], 1).1, 2, "on USB Headset");
        l.names.retain(|n| *n != "USB Headset");
        assert_eq!(l.frame(vec![], 0).1, 0);
        assert_eq!(l.frame(vec![], 0).1, 0);
        assert_eq!(l.focused(), Some(0), "focus followed");
    }

    #[test]
    fn a_click_chooses_at_once() {
        let mut l = List::new();
        l.frame(vec![], 0);
        let at = l.rects[2].center();
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        l.frame(vec![Event::PointerMoved(at)], 0);
        l.frame(vec![button(true)], 0);
        let (c, _) = l.frame(vec![button(false)], 0);
        assert_eq!(
            c,
            Some(PickChoice {
                index: 2,
                by: PickBy::Activate
            })
        );
        assert_eq!(
            l.frame(vec![], 0).1,
            2,
            "shown as chosen before the daemon confirms"
        );
    }
}

#[cfg(test)]
mod roving_tests {
    //! The radio groups' one Tab stop against egui's own arrow-key moves.
    use super::*;
    use eframe::egui::{Event, Key, Modifiers, Pos2, RawInput};

    #[derive(Clone, Copy, PartialEq)]
    enum W {
        Seg,
        Str,
        Picks,
    }

    struct H {
        ctx: egui::Context,
        w: W,
        value: usize,
        show: bool,
        take: bool,
        t: f64,
        ids: Vec<Id>,
        left: Id,
        right: Id,
    }

    fn key(k: Key) -> Vec<Event> {
        vec![Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }]
    }

    impl H {
        fn new(w: W) -> H {
            let ctx = egui::Context::default();
            super::super::install(&ctx, super::super::Theme::Dark);
            let mut h = H {
                ctx,
                w,
                value: 0,
                show: true,
                take: false,
                t: 1.0,
                ids: vec![],
                left: Id::NULL,
                right: Id::NULL,
            };
            h.frame(vec![]);
            h
        }
        fn frame(&mut self, events: Vec<Event>) {
            self.t += 0.05;
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(700.0, 500.0))),
                time: Some(self.t),
                events,
                ..Default::default()
            };
            let (w, show, take) = (self.w, self.show, self.take);
            let mut value = self.value;
            let (mut ids, mut left, mut right) = (vec![], Id::NULL, Id::NULL);
            let _ = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        left = ui.add(Button::secondary("Left")).id;
                        if !show {
                            return;
                        }
                        match w {
                            W::Seg => {
                                let opts = [(0usize, "System"), (1, "Dark"), (2, "Light")];
                                // the options' ids, as the group makes them
                                let gid = ui.make_persistent_id(Id::new("Theme"));
                                ids = (0..3).map(|k| gid.with(k)).collect();
                                let _ = Segmented::new("Theme", &mut value, &opts)
                                    .take_focus(take)
                                    .show(ui);
                            }
                            W::Str => {
                                let labels = ["Gentle", "Medium", "Strong", "Maximum"];
                                let gid =
                                    ui.make_persistent_id(Id::new("Strength").with("strength"));
                                ids = (0..4).map(|k| gid.with(k)).collect();
                                let _ = Strength::new("Strength", &mut value, &labels)
                                    .take_focus(take)
                                    .show(ui);
                            }
                            W::Picks => {
                                ui.vertical(|ui| {
                                    let names = ["A", "B", "C", "D"];
                                    let mut g =
                                        PickGroup::begin(ui, Id::new("mics"), &names, value);
                                    for (i, n) in ["A", "B", "C", "D"].iter().enumerate() {
                                        ids.push(g.pick(ui, i, n, "").id);
                                    }
                                    if let Some(c) = g.end(ui) {
                                        value = c.index;
                                    }
                                });
                            }
                        }
                        // where egui's own arrow move would go
                        right = ui.add(Button::secondary("Right")).id;
                    });
                });
            });
            self.value = value;
            self.ids = ids;
            self.left = left;
            self.right = right;
        }
        fn focused(&self) -> Option<Id> {
            self.ctx.memory(|m| m.focused())
        }
        /// Focus the first stop of the group with Tab from Left.
        fn tab_into_group(&mut self) {
            self.ctx.memory_mut(|m| m.request_focus(self.left));
            self.frame(vec![]);
            self.frame(key(Key::Tab));
            self.frame(vec![]);
        }
    }

    #[test]
    fn an_arrow_from_a_neighbour_lands_on_the_stop() {
        for w in [W::Seg, W::Str, W::Picks] {
            let mut h = H::new(w);
            h.tab_into_group();
            // move the choice along, so earlier stops are no stops now
            for _ in 0..2 {
                h.frame(key(Key::ArrowRight));
                h.frame(vec![]);
                h.frame(vec![]);
            }
            assert_eq!(h.value, 2);
            // back to Left, then egui's own arrow move into the group
            h.ctx.memory_mut(|m| m.request_focus(h.left));
            h.frame(vec![]);
            h.frame(vec![]);
            h.frame(key(Key::ArrowRight));
            h.frame(vec![]);
            h.frame(vec![]);
            let f = h.focused();
            assert!(f.is_some() && f != Some(h.left), "focus lost");
            // the next arrow steps the choice from the stop
            h.frame(key(Key::ArrowRight));
            h.frame(vec![]);
            let n = if w == W::Seg { 3 } else { 4 };
            assert_eq!(h.value, 3 % n);
        }
    }

    #[test]
    fn back_to_back_arrows_stay_in_the_group() {
        for w in [W::Seg, W::Str, W::Picks] {
            let mut h = H::new(w);
            h.tab_into_group();
            let n = if w == W::Seg { 3 } else { 4 };
            // a key repeat faster than the frames: an arrow every frame
            for k in 0..4 {
                h.frame(key(Key::ArrowRight));
                let f = h.focused();
                assert!(
                    f.is_some() && f != Some(h.left) && f != Some(h.right),
                    "press {k}: focus left the group"
                );
            }
            h.frame(vec![]);
            assert_eq!(h.value, 4 % n);
        }
    }

    #[test]
    fn a_group_takes_the_focus_when_asked() {
        for w in [W::Seg, W::Str] {
            let mut h = H::new(w);
            h.value = 1;
            h.take = true;
            h.frame(vec![]);
            h.take = false;
            h.frame(vec![]);
            assert_eq!(h.focused(), Some(h.ids[1]), "the stop: the chosen option");
            // Tab goes on from there
            h.frame(key(Key::Tab));
            h.frame(vec![]);
            assert_eq!(h.focused(), Some(h.right));
        }
        // a strength set from the command line (none checked): the first
        let mut h = H::new(W::Str);
        h.value = 9;
        h.take = true;
        h.frame(vec![]);
        h.take = false;
        h.frame(vec![]);
        assert_eq!(h.focused(), Some(h.ids[0]));
    }

    #[test]
    fn a_hidden_group_does_not_pull_focus_back() {
        let mut h = H::new(W::Picks);
        h.tab_into_group();
        // hide the list with focus in it; the user focuses Left
        h.show = false;
        h.frame(vec![]);
        h.ctx.memory_mut(|m| m.request_focus(h.left));
        h.frame(vec![]);
        // shown again with another choice: focus stays on Left
        h.value = 2;
        h.show = true;
        h.frame(vec![]);
        h.frame(vec![]);
        assert_eq!(h.focused(), Some(h.left));
    }
}

#[cfg(test)]
mod rail_focus_tests {
    use super::*;
    use crate::ui::paint::mark::MarkState;
    use eframe::egui::{pos2, vec2, Pos2, RawInput};

    /// One frame of a rail on Files; `before`/`after` run around it.
    /// Returns the current row's id.
    fn frame(
        ctx: &egui::Context,
        show: bool,
        before: impl Fn(&egui::Context),
        after: impl Fn(&egui::Context),
    ) -> Id {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(700.0, 500.0))),
            ..Default::default()
        };
        let mut row = Id::NULL;
        let _ = ctx.run(input, |ctx| {
            before(ctx);
            egui::CentralPanel::default().show(ctx, |ui| {
                row = ui.id().with(("hm-rail", Page::Files));
                if show {
                    let items = [
                        (Page::Live, "Live"),
                        #[cfg(feature = "speakers")]
                        (Page::Speakers, "Speakers"),
                        (Page::Files, "Files"),
                        (Page::Settings, "Settings"),
                    ]
                    .map(|(page, label)| RailItem { page, label });
                    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(184.0, 500.0));
                    let _ = Rail {
                        current: Page::Files,
                        items,
                        mark: MarkState::Active,
                        mark_name: "HushMic: On",
                        name: "Sections",
                        more: "More",
                        menu: &[],
                        menu_about: &[],
                        compact: false,
                    }
                    .show(ui, rect);
                }
            });
            after(ctx);
        });
        row
    }

    fn focused(ctx: &egui::Context) -> Option<Id> {
        ctx.memory(|m| m.focused())
    }

    #[test]
    fn the_current_row_takes_the_focus_when_asked() {
        let ctx = egui::Context::default();
        super::super::install(&ctx, super::super::Theme::Dark);
        let row = frame(&ctx, true, |_| {}, |_| {});
        assert_eq!(focused(&ctx), None);
        // asked before the rail shows: this frame
        frame(&ctx, true, rail::take_focus, |_| {});
        assert_eq!(focused(&ctx), Some(row));
        // asked after it showed (the page draws later): the next frame
        ctx.memory_mut(|m| m.surrender_focus(row));
        frame(&ctx, true, |_| {}, rail::take_focus);
        assert_eq!(focused(&ctx), None);
        frame(&ctx, true, |_| {}, |_| {});
        assert_eq!(focused(&ctx), Some(row));
        // a request no rail saw for a while lapses
        ctx.memory_mut(|m| m.surrender_focus(row));
        frame(&ctx, false, rail::take_focus, |_| {});
        for _ in 0..3 {
            frame(&ctx, false, |_| {}, |_| {});
        }
        frame(&ctx, true, |_| {}, |_| {});
        assert_eq!(focused(&ctx), None);
    }
}

#[cfg(test)]
mod anim_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn transitions_run_on_time_and_cap_on_software() {
        let ctx = egui::Context::default();
        super::super::install(&ctx, super::super::Theme::Dark);
        super::super::set_software(&ctx, true);
        super::super::set_frame_period(&ctx, 1.0 / 60.0);
        let delays = Arc::new(Mutex::new(Vec::new()));
        let d = delays.clone();
        ctx.set_request_repaint_callback(move |info| d.lock().unwrap().push(info.delay));
        let id = Id::new("t");
        let frame = |t: f64, on: bool| {
            let mut v = 0.0;
            let input = egui::RawInput {
                time: Some(t),
                predicted_dt: 1.0 / 60.0,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| v = anim_with(ctx, id, on, 0.2, |x| x));
            v
        };
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        let first = (1.0 / 60.0) / 0.2;
        assert_eq!(frame(1.0, false), 0.0, "first sight: the target");
        delays.lock().unwrap().clear();
        // switched on after 5 s at rest: one frame's step, not a snap
        assert!(near(frame(6.0, true), first));
        assert!(near(frame(6.05, true), first + 0.25));
        assert!(near(frame(6.15, true), first + 0.75));
        // the frames it asked for: capped (25 ms), never "now"
        let asked = delays.lock().unwrap().clone();
        assert!(
            !asked.is_empty() && asked.iter().all(|d| *d >= Duration::from_millis(24)),
            "{asked:?}"
        );
        assert_eq!(frame(6.25, true), 1.0);
        // settled: it stays, and asks for no frame (both ends)
        delays.lock().unwrap().clear();
        for k in 0..4 {
            assert_eq!(frame(6.3 + 0.05 * k as f64, true), 1.0);
        }
        assert!(
            delays.lock().unwrap().is_empty(),
            "{:?}",
            delays.lock().unwrap()
        );
        // a frame a second later (the pointer moved): still there, no frame
        assert_eq!(frame(7.45, true), 1.0);
        assert!(delays.lock().unwrap().is_empty());
        // the way back mirrors the way in, again from one frame's step
        assert!(near(frame(8.0, false), 1.0 - first));
        for k in 1..8 {
            frame(8.0 + 0.05 * k as f64, false);
        }
        delays.lock().unwrap().clear();
        for k in 0..4 {
            assert_eq!(frame(9.0 + 0.05 * k as f64, false), 0.0);
        }
        assert!(
            delays.lock().unwrap().is_empty(),
            "{:?}",
            delays.lock().unwrap()
        );
    }
}

#[cfg(test)]
mod rest_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn controls_at_rest_ask_for_no_frames() {
        let ctx = egui::Context::default();
        super::super::install(&ctx, super::super::Theme::Dark);
        let asked = Arc::new(Mutex::new(0usize));
        let a = asked.clone();
        ctx.set_request_repaint_callback(move |_| *a.lock().unwrap() += 1);
        let (mut on, mut off, mut seg, mut st) = (true, false, 1usize, 2usize);
        let mut t = 1.0;
        let mut frame = || {
            t += 0.05;
            let input = egui::RawInput {
                time: Some(t),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                super::super::begin_frame(ctx);
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = ui.add(Switch::new(&mut on).label("Noise suppression"));
                    let _ = ui.add(Switch::new(&mut off).name("Off"));
                    let _ = ui.add(Button::primary("Replay"));
                    let _ = ui.add(Button::secondary("Mute").enabled(false));
                    let opts = [(0usize, "System"), (1, "Dark"), (2, "Light")];
                    let _ = Segmented::new("Theme", &mut seg, &opts).show(ui);
                    let labels = ["Gentle", "Medium", "Strong", "Maximum"];
                    let _ = Strength::new("Strength", &mut st, &labels).show(ui);
                    let _ = link(ui, "Show again", false, false);
                });
            });
        };
        for _ in 0..10 {
            frame();
        }
        *asked.lock().unwrap() = 0;
        for _ in 0..10 {
            frame();
        }
        assert_eq!(*asked.lock().unwrap(), 0, "frames asked for at rest");
    }
}

#[cfg(test)]
mod fade_tests {
    use super::*;

    #[test]
    fn a_disabled_control_fades_as_one() {
        // opacity .4 on a primary button: the fill goes 40 % of the way to
        // ink, and the bg-coloured label on it stays bg (CSS), where per-part
        // alpha would leave it at 60 % of the fill
        let (bg, ink) = (Color32::from_gray(0), Color32::from_gray(250));
        assert_eq!(faded(bg, bg, ink, 0.4), Color32::from_gray(100));
        assert_eq!(faded(bg, bg.blend(ink), bg, 0.4), bg);
        // light: ink on white
        let (bg, ink) = (Color32::WHITE, Color32::from_gray(10));
        assert_eq!(faded(bg, bg, ink, 0.4), Color32::from_gray(157));
        assert_eq!(faded(bg, bg.blend(ink), bg, 0.4), bg);
        // a transparent fill stays transparent; a translucent line mixes
        // with what it sits on
        assert_eq!(
            faded(bg, bg, Color32::TRANSPARENT, 0.4),
            Color32::TRANSPARENT
        );
        let line = Color32::from_rgba_unmultiplied(0, 0, 0, 128);
        assert_eq!(faded(bg, bg, line, 0.4).a(), 255);
    }
}

#[cfg(test)]
mod a11y_tests {
    use std::collections::HashMap;

    use eframe::egui::accesskit::{Action, Node, NodeId, Role, Toggled};
    use eframe::egui::{pos2, vec2, Pos2};

    use super::super::paint::mark::MarkState;
    use super::super::tokens::Theme;
    use super::*;

    /// The accessibility tree of a few frames of `f` (the last one).
    fn tree(mut f: impl FnMut(&mut Ui)) -> (NodeId, HashMap<NodeId, Node>) {
        let ctx = egui::Context::default();
        super::super::install(&ctx, Theme::Dark);
        ctx.enable_accesskit();
        let mut last = None;
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(960.0, 640.0))),
                ..Default::default()
            };
            let out = ctx.run(input, |ctx| {
                super::super::begin_frame(ctx);
                egui::CentralPanel::default().show(ctx, |ui| f(ui));
            });
            last = out.platform_output.accesskit_update;
        }
        let update = last.expect("an accesskit update");
        (
            update.tree.expect("a tree").root,
            update.nodes.into_iter().collect(),
        )
    }

    fn find(nodes: &HashMap<NodeId, Node>, role: Role, label: &str) -> NodeId {
        *nodes
            .iter()
            .find(|(_, n)| n.role() == role && n.label() == Some(label))
            .unwrap_or_else(|| panic!("no {role:?} {label:?}"))
            .0
    }

    fn labels(nodes: &HashMap<NodeId, Node>, id: NodeId) -> Vec<String> {
        nodes[&id]
            .children()
            .iter()
            .map(|c| nodes[c].label().unwrap_or_default().to_owned())
            .collect()
    }

    #[test]
    fn steps_are_a_list_of_named_items() {
        let (_, nodes) = tree(|ui| {
            let steps = [
                ("Microphone", StepState::Done),
                ("Hear it work", StepState::Current),
                ("Use it in your apps", StepState::Ahead),
            ];
            let _ = step_dots(ui, "Step 2 of 3", &steps, true);
        });
        let list = find(&nodes, Role::List, "Step 2 of 3");
        assert_eq!(
            labels(&nodes, list),
            ["Microphone", "Hear it work", "Use it in your apps"]
        );
        let current = find(&nodes, Role::ListItem, "Hear it work");
        assert_eq!(nodes[&current].is_selected(), Some(true));
    }

    #[test]
    fn rail_pages_are_tabs_before_more() {
        let (root, nodes) = tree(|ui| {
            let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(184.0, 640.0));
            let items = [
                (Page::Live, "Live"),
                #[cfg(feature = "speakers")]
                (Page::Speakers, "Speakers"),
                (Page::Files, "Files"),
                (Page::Settings, "Settings"),
            ]
            .map(|(page, label)| RailItem { page, label });
            let _ = Rail {
                current: Page::Files,
                items,
                mark: MarkState::Active,
                mark_name: "HushMic: On",
                name: "Sections",
                more: "More",
                menu: &[],
                menu_about: &[],
                compact: false,
            }
            .show(ui, rect);
        });
        let list = nodes
            .iter()
            .find(|(_, n)| n.role() == Role::TabList)
            .map(|(id, _)| *id)
            .expect("a tab list");
        #[cfg(feature = "speakers")]
        assert_eq!(
            labels(&nodes, list),
            ["Live", "Speakers", "Files", "Settings"]
        );
        #[cfg(not(feature = "speakers"))]
        assert_eq!(labels(&nodes, list), ["Live", "Files", "Settings"]);
        for c in nodes[&list].children() {
            let n = &nodes[c];
            assert_eq!(n.role(), Role::Tab);
            assert_eq!(n.is_selected(), Some(n.label() == Some("Files")));
            assert_eq!(n.toggled(), None);
        }
        // reading order (depth first): the pages come before More
        fn order(nodes: &HashMap<NodeId, Node>, id: NodeId, out: &mut Vec<NodeId>) {
            out.push(id);
            for c in nodes[&id].children() {
                order(nodes, *c, out);
            }
        }
        assert_eq!(nodes[&root].label(), Some("HushMic"));
        let mut seq = Vec::new();
        order(&nodes, root, &mut seq);
        let more = find(&nodes, Role::Button, "More");
        let at = |id| seq.iter().position(|c| *c == id).expect("in the tree");
        assert!(at(list) < at(more), "the tab list comes after More");
        assert_eq!(nodes[&more].is_expanded(), Some(false));
    }

    #[test]
    fn radio_options_sit_in_their_group() {
        let (_, nodes) = tree(|ui| {
            let mut v = 1;
            let _ = Segmented::new("Model", &mut v, &[(0, "Quality"), (1, "Light")]).show(ui);
            let mut s = 2;
            let _ = Strength::new("Strength", &mut s, &["Gentle", "Medium", "Strong"]).show(ui);
        });
        let model = find(&nodes, Role::RadioGroup, "Model");
        assert_eq!(labels(&nodes, model), ["Quality", "Light"]);
        let light = nodes[&model].children()[1];
        assert_eq!(nodes[&light].toggled(), Some(Toggled::True));
        let strength = find(&nodes, Role::RadioGroup, "Strength");
        assert_eq!(labels(&nodes, strength), ["Gentle", "Medium", "Strong"]);
    }

    #[test]
    fn strength_can_show_no_step_and_take_any() {
        let labels = ["Gentle", "Medium", "Strong", "Maximum"];
        let ctx = egui::Context::default();
        super::super::install(&ctx, Theme::Dark);
        ctx.enable_accesskit();
        let mut value = usize::MAX;
        let frame = |events: Vec<egui::Event>, value: &mut usize| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(960.0, 640.0))),
                events,
                ..Default::default()
            };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = Strength::new("Strength", value, &labels).show(ui);
                });
            });
            out.platform_output.accesskit_update.expect("a tree").nodes
        };
        frame(vec![], &mut value);
        let nodes: HashMap<NodeId, Node> = frame(vec![], &mut value).into_iter().collect();
        let group = find(&nodes, Role::RadioGroup, "Strength");
        for c in nodes[&group].children() {
            assert_ne!(nodes[c].toggled(), Some(Toggled::True), "nothing checked");
        }
        // a click on Maximum picks it
        let max = find(&nodes, Role::RadioButton, "Maximum");
        let b = nodes[&max].bounds().expect("bounds");
        let at = pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32);
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(vec![egui::Event::PointerMoved(at)], &mut value);
        frame(vec![button(true)], &mut value);
        frame(vec![button(false)], &mut value);
        assert_eq!(value, 3);
    }

    #[test]
    fn the_dropdown_is_a_named_combo_box() {
        let (_, nodes) = tree(|ui| {
            let mut sel = 1;
            let items = [
                SelectItem {
                    label: "Built-in Audio",
                    note: "",
                },
                SelectItem {
                    label: "USB Headset",
                    note: "",
                },
            ];
            let _ = select(ui, "Microphone", &mut sel, &items);
        });
        let combo = find(&nodes, Role::ComboBox, "Microphone");
        assert_eq!(nodes[&combo].value(), Some("USB Headset"));
        assert_eq!(nodes[&combo].is_expanded(), Some(false));
    }

    #[test]
    fn open_popups_are_a_list_box_and_a_menu_with_no_focus_stop_of_their_own() {
        let (_, nodes) = tree(|ui| {
            let mut sel = 0;
            let items = [
                SelectItem {
                    label: "Built-in Audio",
                    note: "",
                },
                SelectItem {
                    label: "USB Headset",
                    note: "Unplugged",
                },
            ];
            with_forced(ui, Force::Open, |ui| {
                select(ui, "Microphone", &mut sel, &items)
            });
            let entries = [MenuItem {
                label: "Quit HushMic",
                icon: None,
                separator: false,
                hint: None,
            }];
            let _ = select::menu_at(
                ui.ctx(),
                Id::new("menu"),
                pos2(300.0, 300.0),
                false,
                &entries,
                None,
                select::ListKey::Nothing,
            );
        });
        let combo = find(&nodes, Role::ComboBox, "Microphone");
        assert_eq!(nodes[&combo].is_expanded(), Some(true));
        let list = find(&nodes, Role::ListBox, "Microphone");
        assert_eq!(labels(&nodes, list), ["Built-in Audio", "USB Headset"]);
        let first = &nodes[&nodes[&list].children()[0]];
        assert_eq!(first.role(), Role::ListBoxOption);
        assert_eq!(first.is_selected(), Some(true));
        let menu = nodes
            .iter()
            .find(|(_, n)| n.role() == Role::Menu)
            .map(|(id, _)| *id)
            .expect("a menu");
        assert_eq!(labels(&nodes, menu), ["Quit HushMic"]);
        assert_eq!(nodes[&nodes[&menu].children()[0]].role(), Role::MenuItem);
        // the popups' own surfaces take no keyboard focus
        assert!(!nodes
            .values()
            .any(|n| n.role() == Role::Unknown && n.supports_action(Action::Focus)));
    }
}
