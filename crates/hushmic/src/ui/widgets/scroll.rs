//! A scroll area's vertical bar as a keyboard control. egui makes the bar a
//! Tab stop (it can be dragged) but gives it no keys, so a page without
//! controls further down (About's credits) could not be read from the
//! keyboard. [`bar`] turns it into a real control: the arrows step, Page
//! Up / Page Down page, Home / End jump, a focus ring and the handle show
//! where it is, and it reads as a named scroll bar with its position.

use eframe::egui::{
    self, accesskit, pos2, scroll_area::ScrollAreaOutput, vec2, EventFilter, Key, Rangef, Rect, Ui,
};

use super::{focus_ring, kit};

/// One arrow press, as browsers step.
const STEP: f32 = 40.0;

/// Call after `ScrollArea::vertical().show(..)` with its output and the
/// page's name ("Settings"). Does nothing while the content fits (there is
/// no bar then).
pub fn bar<R>(ui: &Ui, out: &ScrollAreaOutput<R>, name: &str) {
    let ctx = ui.ctx();
    let view = out.inner_rect;
    let max = (out.content_size.y - view.height()).max(0.0);
    if max <= 0.5 {
        return;
    }
    let id = out.id.with(1usize);
    let mut offset = out.state.offset.y.clamp(0.0, max);
    if let Some(to) = follow_focus(ui, out, id, offset, max) {
        offset = to;
    }
    if ctx.memory(|m| m.has_focus(id)) {
        // the arrows scroll instead of moving focus
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                EventFilter {
                    vertical_arrows: true,
                    ..Default::default()
                },
            )
        });
        let page = (view.height() * 0.875).max(STEP);
        let to = ctx.input(|i| {
            let mut to = offset;
            for (key, d) in [
                (Key::ArrowDown, STEP),
                (Key::ArrowUp, -STEP),
                (Key::PageDown, page),
                (Key::PageUp, -page),
            ] {
                if i.key_pressed(key) {
                    to += d;
                }
            }
            if i.key_pressed(Key::Home) {
                to = 0.0;
            }
            if i.key_pressed(Key::End) {
                to = max;
            }
            to.clamp(0.0, max)
        });
        if to != offset {
            offset = to;
            let mut st = out.state;
            st.offset.y = offset;
            st.store(ctx, out.id);
            ctx.request_repaint();
        }
        let k = kit(ctx);
        if k.focus_visible {
            // the bar's strip at the right edge, and the handle in it (the
            // floating bar itself shows only under the pointer)
            let strip = Rect::from_min_max(
                pos2(view.right() - 14.0, view.top() + 4.0),
                pos2(view.right() - 4.0, view.bottom() - 4.0),
            );
            let h = (strip.height() * view.height() / out.content_size.y).max(12.0);
            let y = strip.top() + (strip.height() - h) * offset / max;
            let handle = Rect::from_center_size(pos2(strip.center().x, y + h / 2.0), vec2(6.0, h));
            let p = ui
                .ctx()
                .layer_painter(ui.layer_id())
                .with_clip_rect(ui.clip_rect());
            p.rect_filled(handle, 3.0, k.pal.ink_subtle);
            focus_ring(&p, strip, 5.0, k.pal.focus_ring());
        }
    }
    ctx.accesskit_node_builder(id, |n| {
        n.set_role(accesskit::Role::ScrollBar);
        n.set_label(name);
        n.set_orientation(accesskit::Orientation::Vertical);
        n.set_min_numeric_value(0.0);
        n.set_max_numeric_value(max as f64);
        n.set_numeric_value(offset as f64);
    });
}

/// Room kept between a control the keyboard moved to and the view's edge.
const FOCUS_MARGIN: f32 = 16.0;

/// The keyboard's focus stays in view (UX panel A7): when it moves to a
/// control of this area that lies (partly) outside the visible part, the
/// area scrolls just enough to show it, with a margin. Only on a change of
/// focus: a mouse wheel may still scroll the focused control away. The
/// new offset, if it moved.
fn follow_focus<R>(
    ui: &Ui,
    out: &ScrollAreaOutput<R>,
    bar: egui::Id,
    offset: f32,
    max: f32,
) -> Option<f32> {
    let ctx = ui.ctx();
    let seen = out.id.with("focus-seen");
    let focused = ctx.memory(|m| m.focused());
    let last: Option<egui::Id> = ctx.data(|d| d.get_temp(seen)).flatten();
    if focused == last {
        return None;
    }
    ctx.data_mut(|d| d.insert_temp(seen, focused));
    let r = ctx.read_response(focused.filter(|f| *f != bar)?)?;
    let view = out.inner_rect;
    // a control of this area: its layer, within its columns
    if r.layer_id != ui.layer_id() || r.rect.max.x <= view.min.x || r.rect.min.x >= view.max.x {
        return None;
    }
    let to = scroll_to_show(offset, view.y_range(), r.rect.y_range(), max)?;
    let mut st = out.state;
    st.offset.y = to;
    st.store(ctx, out.id);
    ctx.request_repaint();
    Some(to)
}

/// The offset that shows `target` (screen y) in `view` from `offset`:
/// the least move, `FOCUS_MARGIN` from the edge it came in at (the top
/// when it is taller than the view). None: it shows already.
fn scroll_to_show(offset: f32, view: Rangef, target: Rangef, max: f32) -> Option<f32> {
    let m = FOCUS_MARGIN.min((view.span() - target.span()).max(0.0) / 2.0);
    let delta = if target.min < view.min + m || target.span() > view.span() - 2.0 * m {
        target.min - (view.min + m)
    } else if target.max > view.max - m {
        target.max - (view.max - m)
    } else {
        return None;
    };
    let to = (offset + delta).clamp(0.0, max);
    (to != offset).then_some(to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{install, Theme};
    use eframe::egui::{self, Event, Modifiers, RawInput};

    fn frame(ctx: &egui::Context, events: Vec<Event>, focus: bool) -> f32 {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(400.0, 300.0))),
            events,
            ..Default::default()
        };
        let mut y = 0.0;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let out = egui::ScrollArea::vertical()
                    .id_salt("page")
                    .show(ui, |ui| ui.allocate_space(vec2(300.0, 2000.0)));
                if focus {
                    ui.memory_mut(|m| m.request_focus(out.id.with(1usize)));
                }
                bar(ui, &out, "Settings");
                y = out.state.offset.y;
            });
        });
        y
    }

    fn key(key: Key) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn the_focused_bar_scrolls_with_the_keys() {
        let ctx = egui::Context::default();
        install(&ctx, Theme::Dark);
        frame(&ctx, vec![], true);
        frame(&ctx, vec![], false);
        frame(&ctx, vec![key(Key::ArrowDown)], false);
        assert_eq!(frame(&ctx, vec![], false), STEP);
        frame(&ctx, vec![key(Key::End)], false);
        let end = frame(&ctx, vec![], false);
        assert!(end > 1500.0, "{end}");
        frame(&ctx, vec![key(Key::PageUp)], false);
        let up = frame(&ctx, vec![], false);
        assert!(up < end && up > end - 300.0, "{up}");
        frame(&ctx, vec![key(Key::Home)], false);
        assert_eq!(frame(&ctx, vec![], false), 0.0);
        // the arrows stayed with the bar
        assert!(ctx.memory(|m| m.focused()).is_some());
    }

    #[test]
    fn the_least_move_shows_the_target() {
        let view = Rangef::new(100.0, 400.0);
        // in view: nothing
        assert_eq!(
            scroll_to_show(50.0, view, Rangef::new(200.0, 240.0), 1000.0),
            None
        );
        // below: its bottom comes in, a margin from the edge
        assert_eq!(
            scroll_to_show(50.0, view, Rangef::new(500.0, 540.0), 1000.0),
            Some(50.0 + 540.0 - (400.0 - FOCUS_MARGIN))
        );
        // above: its top
        assert_eq!(
            scroll_to_show(300.0, view, Rangef::new(20.0, 60.0), 1000.0),
            Some(300.0 - (100.0 + FOCUS_MARGIN - 20.0))
        );
        // taller than the view: its top
        assert_eq!(
            scroll_to_show(0.0, view, Rangef::new(500.0, 900.0), 1000.0),
            Some(400.0)
        );
        // never past the ends
        assert_eq!(
            scroll_to_show(10.0, view, Rangef::new(40.0, 60.0), 1000.0),
            Some(0.0)
        );
        assert_eq!(
            scroll_to_show(990.0, view, Rangef::new(600.0, 640.0), 1000.0),
            Some(1000.0)
        );
    }

    #[test]
    fn a_control_the_keyboard_moves_to_scrolls_into_view() {
        let ctx = egui::Context::default();
        install(&ctx, Theme::Dark);
        let target = egui::Id::new("far-button");
        let run = |focus: bool| {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(400.0, 300.0))),
                ..Default::default()
            };
            let mut y = 0.0;
            let mut seen = Rect::NOTHING;
            let mut view = Rect::NOTHING;
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let out = egui::ScrollArea::vertical().id_salt("page").show(ui, |ui| {
                        ui.allocate_space(vec2(300.0, 1200.0));
                        let at = ui.allocate_space(vec2(120.0, 32.0)).1;
                        let r = ui.interact(at, target, egui::Sense::click());
                        seen = r.rect;
                        ui.allocate_space(vec2(300.0, 600.0));
                    });
                    if focus {
                        ui.memory_mut(|m| m.request_focus(target));
                    }
                    bar(ui, &out, "Settings");
                    y = out.state.offset.y;
                    view = out.inner_rect;
                });
            });
            (y, seen, view)
        };
        run(false);
        let (y, ..) = run(true);
        assert_eq!(y, 0.0, "the request lands next frame");
        run(false);
        let (_, seen, view) = run(false);
        assert!(
            view.contains_rect(seen),
            "the focused control shows: {seen:?} in {view:?}"
        );
    }
}
