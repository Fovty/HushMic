//! The rail (`.hm-app-rail`): the Mark centred on top, the pages, and
//! More (Turn off, Quit) at the bottom. Wide (184 px) at the default size;
//! compact (60 px, icons only) at the minimum size. There is no status pill
//! (round 3: the pages say what HushMic does); the Mark shows the state
//! glyph, with a tooltip and an accessible name.

use eframe::egui::{self, pos2, vec2, Id, Rect, Sense, Ui, WidgetInfo, WidgetType};

use super::super::fonts::Type;
use super::super::icons::{self, Icon};
use super::super::paint::mark::{paint_logo, MarkState};
use super::super::text;
use super::super::tokens::radius;
use super::select::{menu_toggle, menu_with_about, shows_list_focus, MenuItem};
use super::{
    a11y, a11y_children, focus_ring, highlight, hovered, lerp_color, selected, shows_focus,
};
use egui::accesskit::{HasPopup, Role};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    Live,
    #[cfg(feature = "speakers")]
    Speakers,
    Files,
    Settings,
}

impl Page {
    pub const ALL: [Page; 3 + cfg!(feature = "speakers") as usize] = [
        Page::Live,
        #[cfg(feature = "speakers")]
        Page::Speakers,
        Page::Files,
        Page::Settings,
    ];

    pub fn icon(self) -> Icon {
        match self {
            Page::Live => Icon::Live,
            #[cfg(feature = "speakers")]
            Page::Speakers => Icon::Speaker,
            Page::Files => Icon::Files,
            Page::Settings => Icon::Settings,
        }
    }
}

pub const WIDE: f32 = 184.0;
pub const COMPACT: f32 = 60.0;

/// One page row: its label (from the catalog).
pub struct RailItem<'a> {
    pub page: Page,
    pub label: &'a str,
}

#[must_use]
pub struct Rail<'a> {
    pub current: Page,
    pub items: [RailItem<'a>; Page::ALL.len()],
    /// The Mark's state (the small cut): the state glyph.
    pub mark: MarkState,
    /// The Mark's tooltip and accessible name ("HushMic: On").
    pub mark_name: &'a str,
    /// The page list's accessible name ("Sections").
    pub name: &'a str,
    pub more: &'a str,
    pub menu: &'a [MenuItem<'a>],
    /// A line under some of the menu's items (`menu_with_about`).
    pub menu_about: &'a [Option<&'a str>],
    pub compact: bool,
}

/// The current page's row takes the keyboard focus when the rail next shows
/// (this frame if it has not shown yet, else the next): the fallback when the
/// page's focused control goes away and the page has no other to offer. The
/// focus ring follows the window's keyboard-or-pointer state as usual. A
/// request the rail does not see within two passes lapses.
pub fn take_focus(ctx: &egui::Context) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(take_focus_id(), pass));
    ctx.request_repaint();
}

fn take_focus_id() -> egui::Id {
    egui::Id::new("hm-rail-take-focus")
}

/// Up and Down move the keyboard between the page rows and stop at the
/// first and last (UX panel B21: egui's arrows left the list for More and
/// opened its menu). Enter or Space opens the focused page.
fn tab_arrows(ui: &Ui, ids: &[Id]) {
    let Some(at) = ui
        .memory(|m| m.focused())
        .and_then(|f| ids.iter().position(|&i| i == f))
    else {
        return;
    };
    let filter = egui::EventFilter {
        vertical_arrows: true,
        horizontal_arrows: true,
        ..Default::default()
    };
    ui.memory_mut(|m| m.set_focus_lock_filter(ids[at], filter));
    let d = ui.input_mut(|i| {
        let m = egui::Modifiers::NONE;
        if i.consume_key(m, egui::Key::ArrowDown) {
            1
        } else if i.consume_key(m, egui::Key::ArrowUp) {
            -1
        } else {
            0
        }
    });
    if d != 0 {
        let to = (at as i32 + d).clamp(0, ids.len() as i32 - 1) as usize;
        ui.memory_mut(|m| {
            m.request_focus(ids[to]);
            m.move_focus(egui::FocusDirection::None);
        });
    }
}

#[derive(Default)]
pub struct RailResponse {
    /// A page row was clicked.
    pub navigate: Option<Page>,
    /// A More menu item was chosen (its index in `menu`).
    pub menu: Option<usize>,
}

impl Rail<'_> {
    pub fn width(compact: bool) -> f32 {
        if compact {
            COMPACT
        } else {
            WIDE
        }
    }

    /// Draw into `rect` (the rail's column, full window height).
    pub fn show(self, ui: &mut Ui, rect: Rect) -> RailResponse {
        let kit = super::super::kit(ui.ctx());
        let pal = kit.pal;
        let mut out = RailResponse::default();
        let p = ui.painter().clone();
        p.rect_filled(rect, 0.0, pal.bg);
        p.rect_filled(
            Rect::from_min_max(pos2(rect.right() - 1.0, rect.top()), rect.right_bottom()),
            0.0,
            pal.line,
        );
        let (pad_x, inner_w) = if self.compact {
            (8.0, COMPACT - 16.0 - 1.0)
        } else {
            (12.0, WIDE - 24.0 - 1.0)
        };
        let x0 = rect.left() + pad_x;

        // ---- top: the Mark (20 px) centred in a 28 px band
        let top = rect.top() + 14.0 + 2.0;
        let top_h = 28.0;
        {
            let m = Rect::from_min_size(
                pos2(rect.left() + (rect.width() - 1.0 - 20.0) / 2.0, top + 4.0),
                vec2(20.0, 20.0),
            );
            paint_logo(&p, m.min, 20.0, self.mark, &pal);
            let r = ui.interact(m, ui.id().with("hm-rail-mark"), Sense::hover());
            let name = self.mark_name.to_owned();
            r.widget_info(|| WidgetInfo::labeled(WidgetType::Image, true, &name));
            side_tip(&r, self.mark_name);
        }

        // ---- the pages: 34 px rows, 2 px apart, from 64 px (62 compact); a
        // tab list for screen readers ("Live, tab, selected, 1 of 5")
        let mut y = top + top_h + 14.0 + 4.0;
        let tabs = a11y_children(ui, rect, Role::TabList, self.name);
        let asked = ui.ctx().data_mut(|d| {
            let pass = d.get_temp::<u64>(take_focus_id());
            d.remove::<u64>(take_focus_id());
            pass
        });
        let take = asked.is_some_and(|p| ui.ctx().cumulative_pass_nr() <= p + 2);
        let ids: Vec<Id> = self
            .items
            .iter()
            .map(|item| ui.id().with(("hm-rail", item.page)))
            .collect();
        tab_arrows(ui, &ids);
        for item in self.items.iter() {
            let row = Rect::from_min_size(pos2(x0, y), vec2(inner_w, 34.0));
            let id = ui.id().with(("hm-rail", item.page));
            if take && item.page == self.current {
                ui.memory_mut(|m| m.request_focus(id));
            }
            let r = tabs.interact(row, id, Sense::click());
            let mut row_elided = false;
            let cur = item.page == self.current;
            let label = item.label.to_owned();
            r.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, cur, &label));
            a11y(ui.ctx(), id, |n| {
                n.set_role(Role::Tab);
                n.clear_toggled();
                n.set_selected(cur);
            });
            // the boards light rows at once (no transition)
            let hov = if hovered(&r) && !cur { 1.0 } else { 0.0 };
            if cur {
                // the current page: the filled pill, in both themes (UX
                // panel round 2, A18; a teal edge in light looked like the
                // focus ring)
                selected(&p, row, radius::SM, &pal);
            } else {
                highlight(&p, row, radius::SM, &pal, hov);
            }
            let c = if cur {
                pal.ink
            } else {
                lerp_color(pal.ink_muted, pal.ink, hov)
            };
            if self.compact {
                icons::paint(&p, item.page.icon(), row.center(), 16.0, c);
            } else {
                icons::paint(
                    &p,
                    item.page.icon(),
                    pos2(row.left() + 10.0 + 8.0, row.center().y),
                    16.0,
                    c,
                );
                // long translations end in "…" (the full label is the
                // accessible name and the tooltip)
                let b = text::layout_elided(ui.ctx(), item.label, Type::LABEL, c, inner_w - 36.0);
                b.paint_centered_y(&p, row.left() + 10.0 + 16.0 + 10.0, row.center().y);
                row_elided = b.galley.elided;
            }
            if shows_focus(&kit, &r) {
                focus_ring(&p, row, radius::SM, pal.focus_ring());
            }
            if r.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if self.compact || row_elided {
                side_tip(&r, item.label);
            }
            if r.clicked() && !cur {
                out.navigate = Some(item.page);
            }
            y += 36.0;
        }

        // ---- More, at the bottom, with its menu above it
        let row = Rect::from_min_size(pos2(x0, rect.bottom() - 12.0 - 34.0), vec2(inner_w, 34.0));
        let id = ui.id().with("hm-rail-more");
        let open_id = id.with("open");
        let r = ui.interact(row, id, Sense::click());
        let more = self.more.to_owned();
        r.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &more));
        // the row lights on hover only (not while its menu is open)
        let hov = if hovered(&r) { 1.0 } else { 0.0 };
        highlight(&p, row, radius::SM, &pal, hov);
        let c = lerp_color(pal.ink_muted, pal.ink, hov);
        let mut elided = false;
        if self.compact {
            icons::paint(&p, Icon::More, row.center(), 16.0, c);
        } else {
            icons::paint(
                &p,
                Icon::More,
                pos2(row.left() + 18.0, row.center().y),
                16.0,
                c,
            );
            let b = text::layout_elided(ui.ctx(), self.more, Type::LABEL, c, inner_w - 36.0);
            b.paint_centered_y(&p, row.left() + 36.0, row.center().y);
            elided |= b.galley.elided;
        }
        if shows_list_focus(&kit, &r) {
            focus_ring(&p, row, radius::SM, pal.focus_ring());
        }
        if self.compact || elided {
            side_tip(&r, self.more);
        }
        let at = pos2(rect.left() + 12.0, rect.bottom() - 52.0);
        // the page scrolling behind the menu takes it away (UX panel
        // round 2, A24): it never floats over content that moved
        if ui.input(|i| i.raw_scroll_delta != egui::Vec2::ZERO) {
            ui.data_mut(|d| d.insert_temp(open_id, false));
        }
        let (open, chosen) = menu_toggle(ui, &r, open_id, |ui, key| {
            menu_with_about(
                ui.ctx(),
                id.with("menu"),
                at,
                true,
                self.menu,
                self.menu_about,
                Some((r.id, row)),
                key,
            )
        });
        out.menu = chosen;
        a11y(ui.ctx(), id, |n| {
            n.set_has_popup(HasPopup::Menu);
            n.set_expanded(open);
        });
        out
    }
}

/// A rail tooltip: beside the rail, to the right of its row, never over
/// the row below (UX panel round 2, A24; egui's default sits under the
/// pointer, over the next icon). Shown with egui's own tooltip timing.
fn side_tip(r: &egui::Response, text: &str) {
    if !egui::containers::Tooltip::should_show_tooltip(r, false) {
        return;
    }
    egui::Popup::from_response(r)
        .kind(egui::PopupKind::Tooltip)
        .align(egui::RectAlign::RIGHT)
        .align_alternatives(&[])
        .gap(8.0)
        .open(true)
        .show(|ui| {
            ui.label(text);
        });
}
