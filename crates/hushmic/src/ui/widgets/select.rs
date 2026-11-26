//! The dropdown (`.hm-app-select` with its `.hm-app-list`) and menus
//! (`.hm-app-menu`). Popups float on egui's Foreground order (above the
//! light layer), with `shadow-pop`; a click outside or Escape closes them.
//!
//! The keyboard stays on the field (or the menu's button), as in a combo
//! box: Enter, Space or an arrow opens the list, arrows and Home/End move a
//! highlight, Enter or Space chooses, Escape closes; focus never leaves the
//! field, and screen readers follow the highlight as its active descendant.

use eframe::egui::{
    self, pos2, vec2, Area, Context, EventFilter, Id, Key, Modifiers, Order, PointerButton, Pos2,
    Rect, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType,
};

use super::super::fonts::Type;
use super::super::icons::{self, Icon};
use super::super::text;
use super::super::tokens::{radius, Palette};
use super::{a11y, a11y_children, focus_ring, highlight, hovered, lerp_color, shows_focus};
use egui::accesskit::{HasPopup, NodeId, Role};

pub struct SelectItem<'a> {
    pub label: &'a str,
    /// A second line ("Now: Built-in Audio"; "Unplugged · using …").
    pub note: &'a str,
}

/// The floating surface: `shadow-pop` (a 1 px `line` ring and a soft drop)
/// on `surface-raised`.
pub fn popup_surface(p: &egui::Painter, rect: Rect, r: f32, pal: &Palette) {
    let shadow = super::super::shadow_pop(pal);
    p.add(shadow.as_shape(rect, egui::CornerRadius::same(r as u8)));
    p.rect_filled(rect.expand(1.0), r + 1.0, pal.line);
    p.rect_filled(rect, r, pal.surface_raised);
}

/// What a key did on a list's anchor (the dropdown's field, a menu's
/// button) while it has keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKey {
    Nothing,
    /// Arrow down (+1) or up (−1).
    Step(i32),
    First,
    Last,
    /// Enter or Space.
    Choose,
    /// Escape.
    Close,
}

/// The keys of a list on its focused anchor. The anchor keeps arrows and
/// Escape (egui would move or drop focus); call it every frame after the
/// anchor's `interact`.
pub fn list_keys(ui: &Ui, anchor: &Response) -> ListKey {
    // the keyboard is on the anchor, or on one of its open list's items
    let item = focused_item(ui.ctx(), anchor.id);
    if !anchor.has_focus() && item.is_none() {
        return ListKey::Nothing;
    }
    let holder = item.unwrap_or(anchor.id);
    ui.memory_mut(|m| m.set_focus_lock_filter(holder, list_filter()));
    // Enter and Space "click" the focused anchor (an item's own click
    // chooses it)
    if item.is_none() && anchor.clicked() && !anchor.clicked_by(PointerButton::Primary) {
        return ListKey::Choose;
    }
    let key = ui.input_mut(|i| {
        let m = Modifiers::NONE;
        if i.consume_key(m, Key::ArrowDown) {
            ListKey::Step(1)
        } else if i.consume_key(m, Key::ArrowUp) {
            ListKey::Step(-1)
        } else if i.consume_key(m, Key::Home) {
            ListKey::First
        } else if i.consume_key(m, Key::End) {
            ListKey::Last
        } else if i.key_pressed(Key::Escape) {
            ListKey::Close
        } else {
            ListKey::Nothing
        }
    });
    if matches!(key, ListKey::Step(_)) {
        // the list moves its own focus: no spatial move on top of it (the
        // lock filter holds only from an item's second frame with focus)
        ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
    }
    key
}

/// A list's open state and keyboard highlight (`None`: none yet).
#[derive(Clone, Copy, Default)]
struct ListState {
    open: bool,
    hi: Option<usize>,
}

fn state_id(id: Id) -> Id {
    id.with("hm-list")
}

fn list_state(ctx: &Context, id: Id) -> ListState {
    ctx.data(|d| d.get_temp::<ListState>(state_id(id)))
        .unwrap_or_default()
}

fn set_list_state(ctx: &Context, id: Id, st: ListState) {
    ctx.data_mut(|d| d.insert_temp(state_id(id), st));
}

// The items of an open list or menu take keyboard focus in turn, so a
// screen reader speaks each one as the highlight moves (UX panel B16):
// AccessKit's AT-SPI adapter has no active descendant, and Orca speaks
// only what has focus. The keys stay the list's (`list_keys` reads them on
// the anchor's behalf); Tab leaves the list.

fn items_key(anchor: Id) -> Id {
    anchor.with("hm-items")
}

/// The ids of the items the list on `anchor` shows this frame.
fn set_items(ctx: &Context, anchor: Id, ids: Vec<Id>) {
    ctx.data_mut(|d| d.insert_temp(items_key(anchor), ids));
}

fn clear_items(ctx: &Context, anchor: Id) {
    ctx.data_mut(|d| d.remove::<Vec<Id>>(items_key(anchor)));
}

fn items_of(ctx: &Context, anchor: Id) -> Vec<Id> {
    ctx.data(|d| d.get_temp::<Vec<Id>>(items_key(anchor)))
        .unwrap_or_default()
}

/// The item of the list on `anchor` that has keyboard focus.
fn focused_item(ctx: &Context, anchor: Id) -> Option<Id> {
    let f = ctx.memory(|m| m.focused())?;
    items_of(ctx, anchor).contains(&f).then_some(f)
}

/// Whether keyboard focus is on `anchor` or on one of its list's items.
pub fn holds_focus(ctx: &Context, anchor: Id) -> bool {
    ctx.memory(|m| m.has_focus(anchor)) || focused_item(ctx, anchor).is_some()
}

/// Whether to ring `anchor`: its own keyboard focus, or its list's.
pub fn shows_list_focus(kit: &super::super::Kit, anchor: &Response) -> bool {
    shows_focus(kit, anchor)
        || (kit.focus_visible && focused_item(&anchor.ctx, anchor.id).is_some())
}

/// What a list's focus keeps from egui: the arrows and Escape.
fn list_filter() -> EventFilter {
    EventFilter {
        vertical_arrows: true,
        escape: true,
        ..Default::default()
    }
}

/// Keyboard focus follows the highlight while it is in the list on
/// `anchor` (on the anchor or an item). Called after the items are drawn,
/// so an item takes focus a frame after the key that moved the highlight
/// (or opened the list), and that key never clicks it.
fn follow_highlight(ctx: &Context, anchor: Id, item: Option<Id>) {
    let Some(item) = item else {
        return;
    };
    if holds_focus(ctx, anchor) && !ctx.memory(|m| m.has_focus(item)) {
        ctx.memory_mut(|m| {
            m.request_focus(item);
            m.set_focus_lock_filter(item, list_filter());
        });
    }
}

/// Tab is taking the keyboard out of the list on `anchor`: an item that
/// has it lets go, so egui gives it to the next control (Shift+Tab: back
/// to the anchor).
fn leave_items(ctx: &Context, anchor: Id) {
    if let Some(item) = focused_item(ctx, anchor) {
        ctx.memory_mut(|m| m.surrender_focus(item));
    }
}

/// Move a highlight over `n` items.
fn step(hi: Option<usize>, key: ListKey, n: usize) -> Option<usize> {
    if n == 0 {
        return None;
    }
    match key {
        ListKey::Step(d) => Some(match hi {
            None if d > 0 => 0,
            None => n - 1,
            Some(i) => (i as i32 + d).clamp(0, n as i32 - 1) as usize,
        }),
        ListKey::First => Some(0),
        ListKey::Last => Some(n - 1),
        _ => hi,
    }
}

/// Open the dropdown whose field is `id` (its response's id) from elsewhere
/// ("Choose another"), with keyboard focus on the field.
pub fn open_select(ctx: &Context, id: Id) {
    set_list_state(
        ctx,
        id,
        ListState {
            open: true,
            hi: None,
        },
    );
    ctx.memory_mut(|m| m.request_focus(id));
}

/// The room for the label in a field `w` wide: padding 0 10 0 12 inside
/// the 1 px border, the 16 px chevron and 8 px before it.
fn label_room(w: f32) -> f32 {
    w - 2.0 - 12.0 - 10.0 - 8.0 - 16.0
}

/// The most lines the field's label takes before it is shortened.
const FIELD_ROWS: usize = 3;

/// The field's label: one line, or up to three when the name needs them;
/// only a name longer than that is shortened with "…" (UX panel B19:
/// "Built-in Audio Di…" in the Input card, 117 px of text at 960, where
/// "Digital Microphone" alone takes 123).
fn field_text(ctx: &Context, label: &str, color: egui::Color32, w: f32) -> text::TextBlock {
    text::layout_rows(ctx, label, Type::LABEL, color, label_room(w), FIELD_ROWS)
}

/// Whether the field `w` wide shortens `label` with "…".
#[cfg(test)]
pub fn shortens(ctx: &Context, label: &str, w: f32) -> bool {
    field_text(ctx, label, egui::Color32::WHITE, w).elided()
}

/// The field's height for `label` at `w`: 34 px for one line, a line more
/// for each more.
pub fn field_height(ctx: &Context, label: &str, w: f32) -> f32 {
    let rows = field_text(ctx, label, egui::Color32::WHITE, w)
        .rows()
        .max(1);
    34.0 + (rows - 1) as f32 * Type::LABEL.line
}

/// The microphone dropdown: the selected item's label in a 34 px field
/// (a line taller for each line more a long name needs), a
/// chevron, and a list 4 px below it, 24 px wider.
pub fn select(ui: &mut Ui, name: &str, selected: &mut usize, items: &[SelectItem<'_>]) -> Response {
    select_as(ui, name, selected, items, None)
}

/// A quiet select's padding left and right of its label, the gap before
/// its chevron, and the chevron.
const QUIET: (f32, f32, f32) = (6.0, 4.0, 14.0);

/// The width a quiet select takes for `label`.
pub fn quiet_width(ctx: &Context, label: &str) -> f32 {
    2.0 * QUIET.0 + text::width(ctx, label, Type::CAP.medium()).ceil() + QUIET.1 + QUIET.2
}

/// The same dropdown as a part of a line of text: the chosen item's label
/// in `ink` (one line, shortened where the ui is narrower than
/// [`quiet_width`]) and a small chevron, no field around them; hovered or
/// focused, a faint ground. It fills the ui it is given. The list and the
/// keyboard are [`select`]'s. `around` is the palette of what it stands
/// in (a picture of sound has its own): its faint ground and its ring
/// are that palette's.
pub fn select_quiet(
    ui: &mut Ui,
    name: &str,
    selected: &mut usize,
    items: &[SelectItem<'_>],
    ink: egui::Color32,
    around: &super::super::tokens::Palette,
) -> Response {
    select_as(ui, name, selected, items, Some((ink, *around)))
}

fn select_as(
    ui: &mut Ui,
    name: &str,
    selected: &mut usize,
    items: &[SelectItem<'_>],
    quiet: Option<(egui::Color32, super::super::tokens::Palette)>,
) -> Response {
    let kit = super::super::kit(ui.ctx());
    let pal = kit.pal;
    let w = ui.available_width();
    let cur_label = items.get(*selected).map_or("", |i| i.label);
    let fh = match quiet {
        Some(_) => ui.available_height(),
        None => field_height(ui.ctx(), cur_label, w),
    };
    let (rect, mut resp) = ui.allocate_exact_size(vec2(w, fh), Sense::click());
    let id = resp.id;
    let mut st = list_state(ui.ctx(), id);
    let key = list_keys(ui, &resp);
    let cur = items.get(*selected).map_or("", |i| i.label).to_owned();
    let n = name.to_owned();
    resp.widget_info(|| {
        let mut info = WidgetInfo::labeled(WidgetType::ComboBox, true, &n);
        info.current_text_value = Some(cur.clone());
        info
    });
    // fields, list rows and menu items light at once (no transition on
    // the boards)
    let hov = if hovered(&resp) { 1.0 } else { 0.0 };
    let p = ui.painter();
    match quiet {
        None => {
            p.rect(
                rect,
                radius::SM,
                lerp_color(pal.field_fill(), pal.surface_hover, hov),
                Stroke::new(1.0_f32, pal.line_strong),
                StrokeKind::Inside,
            );
            let b = field_text(ui.ctx(), &cur, pal.ink, w);
            // padding 0 10 0 12 inside the 1 px border
            b.paint_centered_y(p, rect.left() + 13.0, rect.center().y);
            icons::paint(
                p,
                Icon::Chevron,
                pos2(rect.right() - 11.0 - 8.0, rect.center().y),
                16.0,
                pal.ink,
            );
        }
        Some((ink, around)) => {
            if hov > 0.0 {
                p.rect_filled(rect, radius::SM, around.surface_hover);
            }
            let room = (w - 2.0 * QUIET.0 - QUIET.1 - QUIET.2).max(16.0);
            let b = text::layout_elided(ui.ctx(), &cur, Type::CAP.medium(), ink, room);
            let end = b.paint_centered_y(p, rect.left() + QUIET.0, rect.center().y);
            icons::paint(
                p,
                Icon::Chevron,
                pos2(end.right() + QUIET.1 + QUIET.2 / 2.0, rect.center().y),
                QUIET.2,
                ink,
            );
        }
    }
    if shows_list_focus(&kit, &resp) {
        let ring = match quiet {
            Some((_, around)) => around.focus_ring(),
            None => pal.focus_ring(),
        };
        focus_ring(p, rect, radius::SM, ring);
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let was_open = st.open;
    // the list hands focus back to the field when it closes from inside
    // (a choice, Escape); a click elsewhere or Tab leaves focus where it went
    let mut refocus = false;
    if st.open && moved_away(ui.ctx(), id) {
        // Tab took the keyboard to another control: the list goes too
        st.open = false;
        leave_items(ui.ctx(), id);
    } else if resp.clicked_by(PointerButton::Primary) {
        st.open = !st.open;
    } else if !st.open && matches!(key, ListKey::Choose | ListKey::Step(_)) {
        st.open = true;
    } else if st.open {
        match key {
            ListKey::Choose => {
                if let Some(i) = st.hi.filter(|&i| i < items.len()) {
                    if i != *selected {
                        *selected = i;
                        resp.mark_changed();
                    }
                }
                st.open = false;
                refocus = true;
            }
            ListKey::Close => {
                st.open = false;
                refocus = true;
            }
            k => st.hi = step(st.hi.or(Some(*selected)), k, items.len()),
        }
    }
    if st.open && !was_open {
        st.hi = Some(*selected);
    }
    // drawn open for a screenshot: not remembered as opened
    let forced = super::forced(ui.ctx()) == Some(super::Force::Open);
    let mut open = st.open || forced;
    if open {
        let ctx = ui.ctx().clone();
        let hi = st.hi;
        let at = pos2(rect.left(), rect.bottom() + 4.0);
        // a quiet select is as narrow as its label: its list is not
        let lw = if quiet.is_some() {
            (w + 24.0).max(260.0)
        } else {
            w + 24.0
        };
        let area = Area::new(id.with("list"))
            .order(Order::Foreground)
            .fixed_pos(at)
            .sense(Sense::hover())
            .show(&ctx, |ui| {
                super::set_backdrop(ui, pal.surface_raised);
                let rows: Vec<(text::TextBlock, Option<text::TextBlock>)> = items
                    .iter()
                    .map(|it| {
                        let l = text::layout(&ctx, it.label, Type::LIST, pal.ink, lw - 8.0 - 16.0);
                        let s = (!it.note.is_empty()).then(|| {
                            text::layout(
                                &ctx,
                                it.note,
                                Type::LIST_NOTE,
                                pal.ink_subtle,
                                lw - 8.0 - 16.0,
                            )
                        });
                        (l, s)
                    })
                    .collect();
                let hs: Vec<f32> = rows
                    .iter()
                    .map(|(l, s)| 12.0 + l.size().y + s.as_ref().map_or(0.0, |s| s.size().y))
                    .collect();
                let total =
                    8.0 + hs.iter().sum::<f32>() + 2.0 * (hs.len().saturating_sub(1)) as f32;
                let (frame, _) = ui.allocate_exact_size(vec2(lw, total), Sense::hover());
                let opts = a11y_children(ui, frame, Role::ListBox, &n);
                let p = ui.painter().clone();
                popup_surface(&p, frame, radius::SM, &pal);
                let mut y = frame.top() + 4.0;
                let mut chosen = None;
                for (i, ((l, s), h)) in rows.iter().zip(&hs).enumerate() {
                    let r = Rect::from_min_size(pos2(frame.left() + 4.0, y), vec2(lw - 8.0, *h));
                    let rid = id.with(("opt", i));
                    // focus comes here with the highlight (focusable, or
                    // egui would drop it); Tab leaves the list (moved_away)
                    let resp = opts.interact(r, rid, Sense::click());
                    let lab = items[i].label.to_owned();
                    let sel = i == *selected;
                    resp.widget_info(|| {
                        WidgetInfo::selected(WidgetType::SelectableLabel, true, sel, &lab)
                    });
                    // its second line ("Now: …") is its description (UX
                    // panel round 2, A34)
                    let note = (!items[i].note.is_empty()).then(|| items[i].note.to_owned());
                    a11y(&ctx, rid, |node| {
                        node.set_role(Role::ListBoxOption);
                        node.clear_toggled();
                        node.set_selected(sel);
                        if let Some(note) = note {
                            node.set_description(note);
                        }
                    });
                    let lit = hovered(&resp) || hi == Some(i);
                    // the chosen one: the selection's filled pill, as the
                    // rail's current page (UX panel round 2, A18); the
                    // highlight over it
                    if sel {
                        super::selected(&p, r, 6.0, &pal);
                    }
                    highlight(&p, r, 6.0, &pal, if lit { 1.0 } else { 0.0 });
                    l.paint(&p, r.min + vec2(8.0, 6.0));
                    if let Some(s) = s {
                        s.paint(&p, r.min + vec2(8.0, 6.0 + l.size().y));
                    }
                    if resp.clicked() {
                        chosen = Some(i);
                    }
                    y += h + 2.0;
                }
                chosen
            });
        if let Some(i) = area.inner {
            if i != *selected {
                *selected = i;
                resp.mark_changed();
            }
            open = false;
            refocus = true;
        } else if closes(&ctx, area.response.rect, Some(rect)) {
            open = false;
            refocus = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        }
        if open {
            set_items(
                &ctx,
                id,
                (0..items.len()).map(|i| id.with(("opt", i))).collect(),
            );
            if let Some(i) = hi {
                let opt = id.with(("opt", i));
                a11y(&ctx, id, |node| {
                    node.set_active_descendant(NodeId(opt.value()))
                });
                follow_highlight(&ctx, id, Some(opt));
            }
        }
    }
    if was_open && !open && refocus {
        resp.request_focus();
    }
    if !open {
        clear_items(ui.ctx(), id);
    }
    a11y(ui.ctx(), id, |node| {
        node.set_has_popup(HasPopup::Listbox);
        node.set_expanded(open);
        // the choice, where Orca reads it: AT-SPI shows a combo box's
        // value to no one (UX panel B16)
        node.set_description(cur.clone());
    });
    set_list_state(
        ui.ctx(),
        id,
        ListState {
            open: open && !forced,
            hi: st.hi,
        },
    );
    resp
}

/// Whether Tab (or Shift+Tab) is taking keyboard focus away from `anchor`
/// this frame, while its list is open. Only Tab: a press on a list row
/// also drops egui's focus from the anchor, and that must not close the
/// list before the click lands.
fn moved_away(ctx: &Context, anchor: Id) -> bool {
    let items = items_of(ctx, anchor);
    ctx.memory(|m| {
        m.had_focus_last_frame(anchor) || items.iter().any(|&i| m.had_focus_last_frame(i))
    }) && ctx.input(|i| i.key_pressed(egui::Key::Tab))
}

/// Whether a click landed outside `popup` (and outside `anchor`, whose own
/// click toggles), or Escape was pressed.
fn closes(ctx: &Context, popup: Rect, anchor: Option<Rect>) -> bool {
    ctx.input(|i| {
        if i.key_pressed(egui::Key::Escape) {
            return true;
        }
        if i.pointer.any_pressed() {
            if let Some(pos) = i.pointer.interact_pos() {
                return !popup.contains(pos) && !anchor.is_some_and(|a| a.contains(pos));
            }
        }
        false
    })
}

/// A menu item's height (one line).
const ITEM_H: f32 = 34.0;

pub struct MenuItem<'a> {
    pub label: &'a str,
    pub icon: Option<Icon>,
    /// A hairline before this item.
    pub separator: bool,
    /// Its keyboard shortcut, at the right in muted ink (`Ctrl+Q`); not
    /// part of its name (screen readers get it as the item's shortcut).
    pub hint: Option<&'a str>,
}

/// A menu (`.hm-app-menu`): 34 px items with a 16 px icon, 6 px padding,
/// 12 px radius, its content at least 196 px wide. With `above`, `at` is its
/// bottom-left corner (the rail's More menu opens upwards). `key` is what
/// [`list_keys`] read on the menu's button this frame. Returns the chosen
/// item and whether it should close.
pub fn menu_at(
    ctx: &Context,
    id: Id,
    at: Pos2,
    above: bool,
    items: &[MenuItem<'_>],
    anchor: Option<(Id, Rect)>,
    key: ListKey,
) -> (Option<usize>, bool) {
    menu_with_about(ctx, id, at, above, items, &[], anchor, key)
}

/// [`menu_at`] with a line under some items' labels (`about[i]`, in
/// subtle ink; the item's description for screen readers): the rail's
/// Turn off and Quit say how they differ (UX panel round 2, A19).
#[allow(clippy::too_many_arguments)]
pub fn menu_with_about(
    ctx: &Context,
    id: Id,
    at: Pos2,
    above: bool,
    items: &[MenuItem<'_>],
    about: &[Option<&str>],
    anchor: Option<(Id, Rect)>,
    key: ListKey,
) -> (Option<usize>, bool) {
    let pal = super::super::kit(ctx).pal;
    let mut st = list_state(ctx, id);
    let mut keyed = None;
    match key {
        ListKey::Choose => keyed = Some(st.hi),
        ListKey::Close | ListKey::Nothing => {}
        k => st.hi = step(st.hi, k, items.len()),
    }
    set_list_state(ctx, id, st);
    let hi = st.hi;
    let blocks: Vec<text::TextBlock> = items
        .iter()
        .map(|it| text::layout(ctx, it.label, Type::LABEL, pal.ink, f32::INFINITY))
        .collect();
    let abouts: Vec<Option<text::TextBlock>> = (0..items.len())
        .map(|i| {
            about
                .get(i)
                .copied()
                .flatten()
                .map(|a| text::layout(ctx, a, Type::CAP, pal.ink_subtle, f32::INFINITY))
        })
        .collect();
    // an item with a line under its label: 7 px above the label and
    // below the line
    let item_h = |i: usize| {
        abouts[i]
            .as_ref()
            .map_or(ITEM_H, |a| 14.0 + blocks[i].size().y + a.size().y)
    };
    let hints: Vec<Option<text::TextBlock>> = items
        .iter()
        .map(|it| {
            it.hint
                .map(|h| text::layout(ctx, h, Type::CAP, pal.ink_muted, f32::INFINITY))
        })
        .collect();
    // items: padding 0 10, a 16 px icon, a 10 px gap, a hint 24 px after
    // the label; the menu pads 6 px
    let w = blocks
        .iter()
        .zip(&hints)
        .zip(&abouts)
        .map(|((b, h), a)| {
            let line = b.size().x + h.as_ref().map_or(0.0, |h| 24.0 + h.size().x);
            10.0 + 16.0 + 10.0 + line.max(a.as_ref().map_or(0.0, |a| a.size().x)) + 10.0
        })
        .fold(196.0, f32::max)
        + 12.0;
    let mut h = 12.0;
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            h += 2.0;
        }
        if it.separator && i > 0 {
            h += 9.0 + 2.0;
        }
        h += item_h(i);
    }
    let pos = if above { pos2(at.x, at.y - h) } else { at };
    // hover only: the surface still takes the clicks that miss an item,
    // but it is no focus stop of its own
    let area = Area::new(id)
        .order(Order::Foreground)
        .fixed_pos(pos)
        .sense(Sense::hover())
        .show(ctx, |ui| {
            super::set_backdrop(ui, pal.surface_raised);
            let (frame, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
            let entries = a11y_children(ui, frame, Role::Menu, "");
            let p = ui.painter().clone();
            popup_surface(&p, frame, radius::MD, &pal);
            let mut y = frame.top() + 6.0;
            let mut chosen = None;
            for (i, ((it, b), hint)) in items.iter().zip(&blocks).zip(&hints).enumerate() {
                if i > 0 {
                    y += 2.0;
                }
                if it.separator && i > 0 {
                    p.rect_filled(
                        Rect::from_min_size(pos2(frame.left() + 6.0, y + 4.0), vec2(w - 12.0, 1.0)),
                        0.0,
                        pal.line,
                    );
                    y += 9.0 + 2.0;
                }
                let r = Rect::from_min_size(pos2(frame.left() + 6.0, y), vec2(w - 12.0, item_h(i)));
                // the label's line: the whole item, or its top 34 px over
                // the line under it
                let line_y = if abouts[i].is_some() {
                    r.top() + 7.0 + b.size().y / 2.0
                } else {
                    r.center().y
                };
                let rid = id.with(i);
                // focus comes here with the highlight (focusable, or
                // egui would drop it); Tab leaves the menu (moved_away)
                let resp = entries.interact(r, rid, Sense::click());
                let lab = it.label.to_owned();
                resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &lab));
                let described = about.get(i).copied().flatten().map(str::to_owned);
                a11y(ctx, rid, |n| {
                    n.set_role(Role::MenuItem);
                    if let Some(h) = it.hint {
                        n.set_keyboard_shortcut(h);
                    }
                    if let Some(d) = described {
                        n.set_description(d);
                    }
                });
                let lit = hovered(&resp) || hi == Some(i);
                highlight(&p, r, 6.0, &pal, if lit { 1.0 } else { 0.0 });
                let mut x = r.left() + 10.0;
                if let Some(icon) = it.icon {
                    icons::paint(&p, icon, pos2(x + 8.0, line_y), 16.0, pal.ink);
                    x += 26.0;
                }
                b.paint_centered_y(&p, x, line_y);
                if let Some(a) = &abouts[i] {
                    a.paint(&p, pos2(x, line_y + b.size().y / 2.0));
                }
                if let Some(h) = hint {
                    h.paint_centered_y(&p, r.right() - 10.0 - h.size().x, line_y);
                }
                if resp.clicked() {
                    chosen = Some(i);
                }
                y += item_h(i);
            }
            chosen
        });
    let chosen = area.inner.or(keyed.flatten());
    let close = chosen.is_some()
        || keyed.is_some()
        || key == ListKey::Close
        || closes(ctx, area.response.rect, anchor.map(|(_, r)| r));
    if close {
        set_list_state(ctx, id, ListState::default());
    } else if let Some((anchor, _)) = anchor {
        set_items(ctx, anchor, (0..items.len()).map(|i| id.with(i)).collect());
        follow_highlight(ctx, anchor, hi.map(|i| id.with(i)));
    }
    (chosen, close)
}

/// A menu below `anchor` (opened by a button), kept open in temp memory.
pub fn menu(ui: &mut Ui, anchor: &Response, items: &[MenuItem<'_>]) -> Option<usize> {
    let open_id = anchor.id.with("menu-open");
    let (open, out) = menu_toggle(ui, anchor, open_id, |ui, key| {
        menu_at(
            ui.ctx(),
            anchor.id.with("menu"),
            anchor.rect.left_bottom() + vec2(0.0, 4.0),
            false,
            items,
            Some((anchor.id, anchor.rect)),
            key,
        )
    });
    a11y(ui.ctx(), anchor.id, |n| {
        n.set_has_popup(HasPopup::Menu);
        n.set_expanded(open);
    });
    out
}

/// A menu's open state on its button: a click toggles it; Enter or Space
/// opens it from the keyboard, an arrow never does (UX panel B21: Down
/// from the last tab opened More with Quit highlighted); the first arrow
/// in it highlights; `show` draws it while open. Focus returns to the button when it closes
/// from inside; Tab away closes it.
/// Returns whether it is open and the chosen item.
pub(crate) fn menu_toggle(
    ui: &mut Ui,
    anchor: &Response,
    open_id: Id,
    show: impl FnOnce(&mut Ui, ListKey) -> (Option<usize>, bool),
) -> (bool, Option<usize>) {
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    if super::forced(ui.ctx()) == Some(super::Force::Open) {
        open = true;
    }
    let mut key = list_keys(ui, anchor);
    if anchor.clicked_by(PointerButton::Primary) {
        open = !open;
        key = ListKey::Nothing;
    } else if !open && key == ListKey::Choose {
        open = true;
        key = ListKey::Nothing;
    } else if !open {
        key = ListKey::Nothing;
    }
    let mut out = None;
    if open && moved_away(ui.ctx(), anchor.id) {
        // Tab took the keyboard to another control: the menu goes too
        open = false;
        leave_items(ui.ctx(), anchor.id);
    }
    if open {
        let (chosen, close) = show(ui, key);
        if close {
            open = false;
            // back to the button when the menu closed from inside (a choice,
            // Escape), not after a click elsewhere
            let escape = ui.input(|i| i.key_pressed(Key::Escape));
            if chosen.is_some() || matches!(key, ListKey::Close | ListKey::Choose) || escape {
                anchor.request_focus();
            }
        }
        out = chosen;
    }
    if !open {
        clear_items(ui.ctx(), anchor.id);
    }
    ui.data_mut(|d| d.insert_temp(open_id, open));
    (open, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tokens::Theme;
    use eframe::egui::{Event, Modifiers};

    fn press(key: Key) -> Vec<Event> {
        [true, false]
            .map(|pressed| Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Modifiers::NONE,
            })
            .to_vec()
    }

    struct Harness {
        ctx: Context,
    }

    impl Harness {
        fn new() -> Harness {
            let ctx = Context::default();
            crate::ui::install(&ctx, Theme::Dark);
            Harness { ctx }
        }
        fn frame(&self, events: Vec<Event>, mut f: impl FnMut(&mut Ui)) {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(960.0, 640.0))),
                events,
                ..Default::default()
            };
            let _ = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.allocate_ui(vec2(300.0, 34.0), |ui| f(ui));
                    // something after it, where egui's arrow focus would go
                    let _ = ui.add(crate::ui::widgets::Button::secondary("Next"));
                });
            });
        }
        fn focused(&self, id: Id) -> bool {
            self.ctx.memory(|m| m.has_focus(id))
        }
    }

    const ITEMS: [SelectItem<'static>; 3] = [
        SelectItem {
            label: "System default",
            note: "",
        },
        SelectItem {
            label: "RODE NT-USB",
            note: "",
        },
        SelectItem {
            label: "USB Headset",
            note: "",
        },
    ];

    #[test]
    fn the_dropdown_works_from_the_keyboard() {
        let h = Harness::new();
        let mut sel = 0;
        let run = |events: Vec<Event>, sel: &mut usize| {
            let mut id = Id::NULL;
            h.frame(events, |ui| id = select(ui, "Microphone", sel, &ITEMS).id);
            id
        };
        let id = run(vec![], &mut sel);
        h.ctx.memory_mut(|m| m.request_focus(id));
        run(vec![], &mut sel);
        assert!(h.focused(id));
        run(press(Key::Enter), &mut sel);
        assert!(list_state(&h.ctx, id).open, "Enter opens");
        run(vec![], &mut sel);
        assert!(
            h.focused(id.with(("opt", 0))),
            "the selected item takes focus, not the click that opened it"
        );
        assert_eq!(sel, 0);
        run(press(Key::ArrowDown), &mut sel);
        run(press(Key::ArrowDown), &mut sel);
        // focus follows the highlight (a screen reader speaks each item),
        // the keys stay the list's
        assert!(
            h.focused(id.with(("opt", 2))),
            "focus follows the highlight"
        );
        assert!(holds_focus(&h.ctx, id));
        assert_eq!(list_state(&h.ctx, id).hi, Some(2));
        run(press(Key::Enter), &mut sel);
        assert_eq!(sel, 2, "Enter chooses the highlight");
        assert!(!list_state(&h.ctx, id).open);
        run(vec![], &mut sel);
        assert!(h.focused(id), "focus stays on the field");
        run(press(Key::ArrowUp), &mut sel);
        assert!(list_state(&h.ctx, id).open, "an arrow opens");
        run(press(Key::Escape), &mut sel);
        run(vec![], &mut sel);
        assert!(!list_state(&h.ctx, id).open, "Escape closes");
        assert!(h.focused(id), "Escape keeps the field focused");
        assert_eq!(sel, 2);
        // opened from elsewhere ("Choose another")
        open_select(&h.ctx, id);
        run(vec![], &mut sel);
        assert!(list_state(&h.ctx, id).open && holds_focus(&h.ctx, id));
    }

    #[test]
    fn tab_away_closes_the_list_and_leaves_focus_there() {
        let h = Harness::new();
        let mut sel = 0;
        let run = |events: Vec<Event>, sel: &mut usize| {
            let mut id = Id::NULL;
            h.frame(events, |ui| id = select(ui, "Microphone", sel, &ITEMS).id);
            id
        };
        let id = run(vec![], &mut sel);
        h.ctx.memory_mut(|m| m.request_focus(id));
        run(vec![], &mut sel);
        run(press(Key::Enter), &mut sel);
        assert!(list_state(&h.ctx, id).open);
        run(press(Key::Tab), &mut sel);
        run(vec![], &mut sel);
        assert!(!list_state(&h.ctx, id).open, "Tab closes the list");
        let next = h.ctx.memory(|m| m.focused());
        assert!(next.is_some() && next != Some(id), "focus moved on");
        run(vec![], &mut sel);
        run(press(Key::Escape), &mut sel);
        run(vec![], &mut sel);
        assert!(!h.focused(id), "nothing pulls focus back to the field");
        assert_eq!(sel, 0);
    }

    /// A mouse click at `at`: the pointer arrives, presses, releases (egui
    /// hit-tests against the previous frame's widgets).
    fn click(at: Pos2) -> [Vec<Event>; 3] {
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        [
            vec![Event::PointerMoved(at)],
            vec![button(true)],
            vec![button(false)],
        ]
    }

    #[test]
    fn mouse_picks_land_one_after_another() {
        // a press on a list row drops egui's focus from the field (focused
        // by the previous pick): that is no Tab away, the pick must land
        let h = Harness::new();
        let mut sel = 0;
        let run = |events: Vec<Event>, sel: &mut usize| {
            let mut field = Rect::NOTHING;
            h.frame(events, |ui| {
                field = select(ui, "Microphone", sel, &ITEMS).rect;
            });
            field
        };
        let field = run(vec![], &mut sel);
        // the list's rows: 4 px below the field, 4 px padding, 30 + 2 px each
        let row = |i: usize| {
            pos2(
                field.left() + 20.0,
                field.bottom() + 4.0 + 4.0 + 32.0 * i as f32 + 15.0,
            )
        };
        for pick in [1, 2, 0] {
            for e in click(field.center()) {
                run(e, &mut sel);
            }
            for e in click(row(pick)) {
                run(e, &mut sel);
            }
            run(vec![], &mut sel);
            assert_eq!(sel, pick, "pick {pick}");
        }
    }

    #[test]
    fn menu_picks_land_one_after_another() {
        let h = Harness::new();
        let items = [
            MenuItem {
                label: "Turn off HushMic",
                icon: None,
                separator: false,
                hint: None,
            },
            MenuItem {
                label: "Quit HushMic",
                icon: None,
                separator: true,
                hint: None,
            },
        ];
        let run = |events: Vec<Event>| {
            let (mut rect, mut chosen) = (Rect::NOTHING, None);
            h.frame(events, |ui| {
                let b = ui.add(crate::ui::widgets::Button::ghost("More"));
                rect = b.rect;
                chosen = menu(ui, &b, &items);
            });
            (rect, chosen)
        };
        let (button, _) = run(vec![]);
        // items: 4 px below the button, 6 px padding, 34 px rows, the
        // separator before the second (2 + 9 + 2)
        let top = button.bottom() + 4.0 + 6.0;
        let item = [top + 17.0, top + 34.0 + 13.0 + 17.0];
        for pick in [0, 1, 0] {
            for e in click(button.center()) {
                run(e);
            }
            let mut got = None;
            for e in click(pos2(button.left() + 40.0, item[pick])) {
                got = got.or(run(e).1);
            }
            assert_eq!(got, Some(pick), "pick {pick}");
        }
    }

    #[test]
    fn a_menu_works_from_its_button() {
        let h = Harness::new();
        let items = [
            MenuItem {
                label: "Turn off HushMic",
                icon: None,
                separator: false,
                hint: None,
            },
            MenuItem {
                label: "Quit HushMic",
                icon: None,
                separator: true,
                hint: None,
            },
        ];
        let run = |events: Vec<Event>| {
            let (mut id, mut chosen) = (Id::NULL, None);
            h.frame(events, |ui| {
                let b = ui.add(crate::ui::widgets::Button::ghost("More"));
                id = b.id;
                chosen = menu(ui, &b, &items);
            });
            (id, chosen)
        };
        let (id, _) = run(vec![]);
        h.ctx.memory_mut(|m| m.request_focus(id));
        run(vec![]);
        // an arrow never opens a menu (UX panel B21); Enter does
        run(press(Key::ArrowDown));
        assert!(!h
            .ctx
            .data(|d| d.get_temp::<bool>(id.with("menu-open")))
            .unwrap_or(false));
        assert!(h.focused(id), "the arrow went nowhere");
        run(press(Key::Enter));
        run(press(Key::ArrowDown));
        run(press(Key::ArrowDown));
        // the highlighted item has focus (a screen reader speaks it), the
        // keys stay the menu's
        let item = |i: usize| id.with("menu").with(i);
        assert!(h.focused(item(1)), "focus follows the highlight");
        assert!(holds_focus(&h.ctx, id));
        let (_, chosen) = run(press(Key::Enter));
        assert_eq!(chosen, Some(1));
        run(vec![]);
        assert!(h.focused(id), "focus returns to the button");
        // Escape from an item: closed, back on the button
        run(press(Key::Enter));
        run(press(Key::ArrowDown));
        assert!(h.focused(item(0)));
        run(press(Key::Escape));
        run(vec![]);
        assert!(h.focused(id), "Escape returns to the button");
        assert!(!h
            .ctx
            .data(|d| d.get_temp::<bool>(id.with("menu-open")))
            .unwrap_or(false));
    }

    #[test]
    fn tab_and_shift_tab_leave_a_list_from_its_item() {
        let h = Harness::new();
        let mut sel = 0;
        let run = |events: Vec<Event>, sel: &mut usize| {
            let mut id = Id::NULL;
            h.frame(events, |ui| id = select(ui, "Microphone", sel, &ITEMS).id);
            id
        };
        let id = run(vec![], &mut sel);
        h.ctx.memory_mut(|m| m.request_focus(id));
        run(vec![], &mut sel);
        run(press(Key::Enter), &mut sel);
        run(vec![], &mut sel);
        assert!(
            h.focused(id.with(("opt", 0))),
            "the selected item has focus"
        );
        // Shift+Tab: back to the field, the list closed
        let shift_tab = [true, false]
            .map(|pressed| Event::Key {
                key: Key::Tab,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Modifiers::SHIFT,
            })
            .to_vec();
        run(shift_tab, &mut sel);
        run(vec![], &mut sel);
        assert!(!list_state(&h.ctx, id).open);
        assert!(h.focused(id), "Shift+Tab lands on the field");
        assert_eq!(sel, 0, "nothing chosen on the way out");
    }
}
