//! Shortcuts, a section of Settings: the four actions, each with a small
//! press/hold diagram and the key the desktop bound to it (its own text,
//! "Not set", or "Not shown by your desktop"), and the one button that
//! opens the desktop's shortcut window. The desktop keeps the keys; HushMic
//! never records them itself.

use eframe::egui::{self, pos2, vec2, Rect, Shape, Stroke, Ui};

use super::util::{self, child, rect, text_as, text_at, Speak};
use super::Cx;
use crate::appstate::{Action, Keys, ShortcutsBackend};
use crate::shortcuts::Action as Shortcut;
use crate::tr;
use crate::ui::fonts::Type;
use crate::ui::text;
use crate::ui::tokens::Palette;
use crate::ui::widgets::{self, Button};

const DIAGRAM: egui::Vec2 = egui::vec2(132.0, 30.0);

/// What one row's key column shows.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyText {
    Chip(String),
    Note(String),
    /// Where shortcuts can't be registered: the commands to bind to the
    /// key's press and to its release (UX panel round 2, A12).
    Hold {
        press: String,
        release: String,
    },
    Nothing,
}

/// The section's state, from the daemon's.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub button: Option<String>,
    /// The button is a white primary: setting the keys up is the step
    /// that moves you on; changing them (or opening Plasma's settings) is
    /// not (round 3).
    pub primary: bool,
    pub unavailable: bool,
    pub keys: Vec<(Shortcut, KeyText)>,
    /// Said once under the rows in place of a note repeated on each
    /// ("Not shown by your desktop", UX panel B15).
    pub notice: Option<String>,
}

pub fn view(s: &crate::appstate::AppState) -> View {
    let sc = &s.shortcuts;
    let key_of = |a: Shortcut| {
        sc.keys
            .iter()
            .find(|k| k.action == a)
            .map(|k| k.keys.clone())
            .unwrap_or_default()
    };
    if !sc.available {
        return View {
            button: None,
            primary: false,
            unavailable: true,
            // the toggles can be bound by hand (the commands above);
            // holding a key, by binding its press and its release apart
            keys: Shortcut::ALL
                .iter()
                .map(|a| {
                    let hold = |press: &str, release: &str| KeyText::Hold {
                        press: format!("hushmic mode {press}"),
                        release: format!("hushmic mode {release}"),
                    };
                    let k = match a {
                        Shortcut::PushToTalk => hold("suppress", "mute"),
                        Shortcut::PushToMute => hold("mute", "suppress"),
                        _ => KeyText::Nothing,
                    };
                    (*a, k)
                })
                .collect(),
            notice: None,
        };
    }
    let button = if sc.backend == ShortcutsBackend::KdeService {
        tr!("sc-open-settings")
    } else if !sc.set_up {
        tr!("sc-setup")
    } else {
        tr!("sc-change")
    };
    let mut keys: Vec<(Shortcut, KeyText)> = Shortcut::ALL
        .iter()
        .map(|a| {
            let k = if !sc.set_up && sc.backend != ShortcutsBackend::KdeService {
                KeyText::Note(tr!("sc-not-set"))
            } else {
                match key_of(*a) {
                    Keys::Text(t) if !t.trim().is_empty() => KeyText::Chip(t),
                    Keys::Text(_) | Keys::NotSet => KeyText::Note(tr!("sc-not-set")),
                    Keys::NotReported => KeyText::Note(tr!("sc-not-reported")),
                }
            };
            (*a, k)
        })
        .collect();
    // the desktop shows none of its keys: said once, under the rows
    let hidden = KeyText::Note(tr!("sc-not-reported"));
    let notice = (keys.iter().filter(|(_, k)| *k == hidden).count() > 1).then(|| {
        for (_, k) in keys.iter_mut().filter(|(_, k)| *k == hidden) {
            *k = KeyText::Nothing;
        }
        tr!("sc-not-reported-all")
    });
    let primary = sc.backend != ShortcutsBackend::KdeService && !sc.set_up;
    View {
        button: Some(button),
        primary,
        unavailable: false,
        keys,
        notice,
    }
}

fn row_copy(a: Shortcut) -> (String, String) {
    match a {
        Shortcut::ToggleMute => (tr!("sc-toggle-mute"), tr!("sc-toggle-mute-desc")),
        Shortcut::ToggleBypass => (tr!("sc-toggle-bypass"), tr!("sc-toggle-bypass-desc")),
        Shortcut::PushToTalk => (tr!("sc-push-to-talk"), tr!("sc-push-to-talk-desc")),
        Shortcut::PushToMute => (tr!("sc-push-to-mute"), tr!("sc-push-to-mute-desc")),
    }
}

/// The section in Settings: its heading, the legend that reads the
/// diagrams, the row with the button that opens the desktop's shortcut
/// window, the four actions, and what Mute silences. Returns its bottom.
pub fn section(ui: &mut Ui, cx: &mut Cx<'_>, x: f32, y: f32, w: f32) -> f32 {
    let ctx = ui.ctx().clone();
    let pal = crate::ui::kit(&ctx).pal;
    let v = view(cx.state);
    let mut y = text_as(
        ui,
        text::layout(&ctx, &tr!("sc-title"), Type::EYEBROW, pal.ink_subtle, w),
        pos2(x, y),
        Speak::Heading(2),
    )
    .bottom()
        + 10.0;

    // ---- not available here: the commands to bind by hand
    if v.unavailable {
        let mut c = child(ui, rect(x, y, w, 400.0), "sc-unavailable");
        widgets::group(&mut c, |g| {
            g.row(|ui| {
                text::label(ui, &tr!("sc-unavailable-title"), Type::SUB, pal.ink);
                ui.add_space(10.0);
                text::label(ui, &tr!("sc-unavailable-body"), Type::CAP, pal.ink_subtle);
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                    widgets::copy_chip(ui, "hushmic toggle mute", 32.0);
                    widgets::copy_chip(ui, "hushmic toggle bypass", 32.0);
                });
                ui.add_space(10.0);
                text::label(ui, &tr!("sc-unavailable-hold"), Type::CAP, pal.ink_subtle);
                ui.add_space(6.0);
                ui.spacing_mut().interact_size.y = 0.0;
                if util::row_link(ui, &tr!("sc-hold-recipe"), true, false).clicked() {
                    util::open_url(util::url::HOLD_RECIPE);
                }
            });
        });
        y = c.min_rect().bottom() + 20.0;
    }

    // ---- the legend that reads the diagrams, then the rows
    y = legend(ui, x, y, w, &pal) + 12.0;
    let mut setup = false;
    let mut c = child(ui, rect(x, y, w, 2000.0), "sc-rows");
    widgets::group(&mut c, |g| {
        // who keeps the keys, and the way to them
        if let Some(label) = &v.button {
            let btn = if v.primary {
                Button::primary(label)
            } else {
                Button::secondary(label)
            }
            .sm();
            let bw = btn.width(&ctx);
            super::settings::srow(g, &tr!("sc-keys-kept"), "", false, bw, 32.0, |ui| {
                setup = ui.add(btn).clicked();
            });
        }
        for (a, k) in &v.keys {
            g.row(|ui| shortcut_row(ui, *a, k, &pal, cx.compact));
        }
    });
    if setup {
        cx.send(Action::SetupShortcuts);
    }
    y = c.min_rect().bottom() + 10.0;
    if let Some(n) = &v.notice {
        y = text_at(
            ui,
            text::layout(&ctx, n, Type::CAP, pal.ink_muted, w),
            pos2(x, y),
        )
        .bottom()
            + 6.0;
    }

    // ---- what Mute silences
    text_at(
        ui,
        text::layout(&ctx, &tr!("sc-mute-note"), Type::CAP, pal.ink_subtle, w),
        pos2(x, y),
    )
    .bottom()
}

/// One row: the action, its diagram, its key. What the action does is the
/// diagram's to show; the sentence is the name's tooltip and description.
fn shortcut_row(ui: &mut Ui, a: Shortcut, k: &KeyText, pal: &Palette, compact: bool) {
    let ctx = ui.ctx().clone();
    let w = ui.available_width();
    let (title, desc) = row_copy(a);
    // the key column: at least 150 px, as wide as its content
    let key_block = match k {
        KeyText::Chip(t) => Some(text::layout_elided(
            &ctx,
            t,
            Type::MONO,
            pal.ink,
            200.0 - 18.0,
        )),
        KeyText::Note(t) => Some(text::layout(
            &ctx,
            t,
            Type::CAP,
            pal.ink_subtle,
            f32::INFINITY,
        )),
        KeyText::Hold { .. } | KeyText::Nothing => None,
    };
    // press and release: a word, then the command, on a line each
    let hold = match k {
        KeyText::Hold { press, release } => {
            let words = [tr!("sc-on-press"), tr!("sc-on-release")]
                .map(|w| text::layout(&ctx, &w, Type::CAP, pal.ink_subtle, f32::INFINITY));
            let word_w = words.iter().map(|b| b.size().x).fold(0.0, f32::max) + 8.0;
            let cmds =
                [press, release].map(|c| text::layout(&ctx, c, Type::MONO, pal.ink, f32::INFINITY));
            let cmd_w = cmds.iter().map(|b| b.size().x).fold(0.0, f32::max);
            Some((words, cmds, word_w, cmd_w))
        }
        _ => None,
    };
    let key_w = match (k, &key_block, &hold) {
        (KeyText::Chip(_), Some(b), _) => (b.size().x + 18.0).min(200.0),
        (_, _, Some((_, _, word_w, cmd_w))) => word_w + cmd_w,
        (_, Some(b), _) => b.size().x,
        _ => 0.0,
    }
    .max(150.0);
    // narrow windows put the diagram and the key under the text
    let stacked = compact && w < 132.0 + 150.0 + 40.0 + 200.0;
    let text_w = if stacked {
        w
    } else {
        (w - DIAGRAM.x - key_w - 40.0).max(80.0)
    };
    let tb = text::layout(&ctx, &title, Type::LABEL, pal.ink, text_w);
    let text_h = tb.size().y;
    let o = ui.cursor().min;
    // the two lines of a press and a release
    let hold_line = Type::MONO.line.max(Type::CAP.line) + 2.0;
    let key_h = if hold.is_some() {
        2.0 * hold_line
    } else {
        26.0
    };
    let row_h = if stacked {
        text_h + 8.0 + DIAGRAM.y.max(key_h)
    } else {
        text_h.max(DIAGRAM.y).max(key_h)
    };
    let ty = if stacked {
        o.y
    } else {
        o.y + (row_h - text_h) / 2.0
    };
    let title_at = Rect::from_min_size(pos2(o.x, ty), tb.size());
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(title_at));
    let named = text::show(&mut c, tb).on_hover_text(&desc);
    ctx.accesskit_node_builder(named.id, |n| n.set_description(desc.clone()));
    let (dx, dy) = if stacked {
        (o.x, o.y + text_h + 8.0)
    } else {
        (o.x + text_w + 20.0, o.y + (row_h - DIAGRAM.y) / 2.0)
    };
    diagram(ui.painter(), pos2(dx, dy), a, pal);
    let right = o.x + w;
    let cy = if stacked {
        dy + DIAGRAM.y.max(key_h) / 2.0
    } else {
        o.y + row_h / 2.0
    };
    match (k, key_block) {
        (KeyText::Chip(t), Some(_)) => {
            let mut c = child(
                ui,
                rect(right - 200.0, cy - 13.0, 200.0, 26.0),
                ("sc-key", a.id()),
            );
            c.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                widgets::kbd(ui, t)
            });
        }
        (KeyText::Hold { .. }, _) => {
            if let Some((words, cmds, word_w, cmd_w)) = hold {
                // two lines, centred on the row
                let x = right - word_w - cmd_w;
                let top = cy - hold_line;
                for (i, (wb, cb)) in words.into_iter().zip(cmds).enumerate() {
                    let y = top + i as f32 * hold_line;
                    text_at(ui, wb, pos2(x, y));
                    text_at(ui, cb, pos2(x + word_w, y));
                }
            }
        }
        (_, Some(b)) => {
            let bw = b.size().x;
            text_at(ui, b, pos2(right - bw, cy - Type::CAP.line / 2.0));
        }
        _ => {}
    }
    ui.advance_cursor_after_rect(Rect::from_min_size(o, vec2(w, row_h)));
}

/// The static press/hold diagram: the key on top, what apps hear below.
pub fn diagram(p: &egui::Painter, o: egui::Pos2, a: Shortcut, pal: &Palette) {
    let (key_y, line_y) = (6.0, 22.0);
    let at = |x: f32, y: f32| o + vec2(x, y);
    p.line_segment(
        [at(2.0, key_y), at(130.0, key_y)],
        Stroke::new(1.0_f32, pal.line),
    );
    #[derive(Clone, Copy)]
    enum Seg {
        Live,
        Raw,
        Mute,
    }
    let seg = |x1: f32, x2: f32, s: Seg| match s {
        Seg::Live => {
            p.line_segment(
                [at(x1, line_y), at(x2, line_y)],
                Stroke::new(2.0_f32, pal.accent),
            );
            p.circle_filled(at(x1, line_y), 1.0, pal.accent);
            p.circle_filled(at(x2, line_y), 1.0, pal.accent);
        }
        Seg::Raw => {
            p.line_segment(
                [at(x1, line_y), at(x2, line_y)],
                Stroke::new(1.5_f32, pal.ink_muted),
            );
        }
        Seg::Mute => {
            p.add(Shape::dashed_line(
                &[at(x1, line_y), at(x2, line_y)],
                Stroke::new(1.0_f32, pal.ink_subtle),
                2.0,
                3.0,
            ));
        }
    };
    let tap = |x: f32| {
        p.rect_filled(
            Rect::from_min_size(at(x - 3.0, key_y - 3.0), vec2(6.0, 6.0)),
            1.5,
            pal.ink,
        )
    };
    let hold = |x1: f32, x2: f32| {
        p.rect_filled(
            Rect::from_min_max(at(x1, key_y - 3.0), at(x2, key_y + 3.0)),
            3.0,
            pal.ink,
        )
    };
    match a {
        Shortcut::ToggleMute => {
            tap(36.0);
            tap(96.0);
            seg(2.0, 36.0, Seg::Live);
            seg(36.0, 96.0, Seg::Mute);
            seg(96.0, 130.0, Seg::Live);
        }
        Shortcut::ToggleBypass => {
            tap(36.0);
            tap(96.0);
            seg(2.0, 36.0, Seg::Live);
            seg(36.0, 96.0, Seg::Raw);
            seg(96.0, 130.0, Seg::Live);
        }
        Shortcut::PushToTalk => {
            hold(40.0, 92.0);
            seg(2.0, 40.0, Seg::Mute);
            seg(40.0, 92.0, Seg::Live);
            seg(92.0, 130.0, Seg::Mute);
        }
        Shortcut::PushToMute => {
            hold(40.0, 92.0);
            seg(2.0, 40.0, Seg::Live);
            seg(40.0, 92.0, Seg::Mute);
            seg(92.0, 130.0, Seg::Live);
        }
    }
}

/// The legend under the rows (decoration: the rows say it in words).
fn legend(ui: &mut Ui, x: f32, y: f32, w: f32, pal: &Palette) -> f32 {
    let ctx = ui.ctx().clone();
    let items = [
        tr!("sc-legend-held"),
        tr!("sc-legend-clean"),
        tr!("sc-legend-raw"),
        tr!("sc-legend-silence"),
    ];
    let p = ui.painter().clone();
    let (mut cx, mut cy) = (x, y);
    for (i, label) in items.iter().enumerate() {
        let b = text::layout(&ctx, label, Type::CAP, pal.ink_subtle, f32::INFINITY);
        let iw = 22.0 + 6.0 + b.size().x;
        if cx > x && cx + iw > x + w {
            cx = x;
            cy += Type::CAP.line + 6.0;
        }
        let mid = cy + Type::CAP.line / 2.0;
        match i {
            0 => {
                p.rect_filled(
                    Rect::from_min_size(pos2(cx + 1.0, mid - 3.0), vec2(20.0, 6.0)),
                    3.0,
                    pal.ink,
                );
            }
            1 => {
                p.line_segment(
                    [pos2(cx + 1.0, mid), pos2(cx + 21.0, mid)],
                    Stroke::new(2.0_f32, pal.accent),
                );
            }
            2 => {
                p.line_segment(
                    [pos2(cx + 1.0, mid), pos2(cx + 21.0, mid)],
                    Stroke::new(1.5_f32, pal.ink_muted),
                );
            }
            _ => {
                p.add(Shape::dashed_line(
                    &[pos2(cx + 1.0, mid), pos2(cx + 21.0, mid)],
                    Stroke::new(1.0_f32, pal.ink_subtle),
                    2.0,
                    3.0,
                ));
            }
        }
        b.paint(&p, pos2(cx + 28.0, cy));
        cx += iw + 18.0;
    }
    let bottom = cy + Type::CAP.line;
    // one node for screen readers: the legend's words, in order
    let name = tr!("sc-legend-name", items = items.join(", "));
    let r = ui.interact(
        Rect::from_min_max(pos2(x, y), pos2(x + w, bottom)),
        ui.id().with("sc-legend"),
        egui::Sense::hover(),
    );
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &name));
    bottom
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock::{state, Scenario};
    use crate::appstate::Page;
    use crate::pages::testing;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn the_keys_read_as_the_desktop_reports_them() {
        crate::i18n::pin_english();
        let v = view(&state(Scenario::OnInCall));
        assert_eq!(v.button.as_deref(), Some("Change shortcuts"));
        // one white primary per page, for the step that moves you on
        assert!(!v.primary);
        assert_eq!(v.keys[0].1, KeyText::Chip("Ctrl+Alt+M".into()));
        assert_eq!(v.keys[3].1, KeyText::Note("Not set".into()));
        let v = view(&state(Scenario::ShortcutsNotSetUp));
        assert_eq!(v.button.as_deref(), Some("Set up shortcuts"));
        assert!(v.primary);
        assert!(v
            .keys
            .iter()
            .all(|k| k.1 == KeyText::Note("Not set".into())));
        // none shown: said once under the rows, not on each (UX panel B15)
        let v = view(&state(Scenario::KeysNotReported));
        assert!(v.keys.iter().all(|k| k.1 == KeyText::Nothing));
        assert_eq!(
            v.notice.as_deref(),
            Some("Your desktop doesn’t show which keys it set. Change shortcuts opens them.")
        );
        let v = view(&state(Scenario::PlasmaBefore64));
        assert_eq!(v.button.as_deref(), Some("Open System Settings"));
        assert!(!v.primary);
        assert_eq!(v.keys[0].1, KeyText::Chip("Meta+M".into()));
        let v = view(&state(Scenario::ShortcutsUnavailable));
        assert!(v.unavailable && v.button.is_none());
        // holding a key: the commands for its press and its release
        // (UX panel round 2, A12)
        assert_eq!(v.keys[0].1, KeyText::Nothing);
        assert_eq!(
            v.keys[2].1,
            KeyText::Hold {
                press: "hushmic mode suppress".into(),
                release: "hushmic mode mute".into()
            }
        );
        assert_eq!(
            v.keys[3].1,
            KeyText::Hold {
                press: "hushmic mode mute".into(),
                release: "hushmic mode suppress".into()
            }
        );
    }

    #[test]
    fn the_button_opens_the_desktops_shortcut_window() {
        for (sc, label) in [
            (Scenario::OnInCall, "Change shortcuts"),
            (Scenario::ShortcutsNotSetUp, "Set up shortcuts"),
            (Scenario::PlasmaBefore64, "Open System Settings"),
        ] {
            let mut w = testing::scenario(sc, Page::Shortcuts);
            w.harness.get_by_label(label).click();
            w.harness.run_steps(2);
            assert_eq!(w.link.sent(), vec![Action::SetupShortcuts], "{sc:?}");
        }
    }

    #[test]
    fn a_command_chip_copies_itself() {
        crate::i18n::pin_english();
        let mut w = testing::scenario(Scenario::ShortcutsUnavailable, Page::Shortcuts);
        w.harness.get_by_label("hushmic toggle mute").click();
        w.harness.run_steps(2);
        assert!(w
            .harness
            .query_by_label("hushmic toggle mute (Copied)")
            .is_some());
        // push to talk and push to mute: press and release, and the recipe
        assert_eq!(w.harness.query_all_by_label("Press").count(), 2);
        assert_eq!(w.harness.query_all_by_label("Release").count(), 2);
        assert_eq!(
            w.harness
                .query_all_by_label("hushmic mode suppress")
                .count(),
            2
        );
        assert!(w
            .harness
            .query_by_label_contains("How to bind press and release")
            .is_some());
        assert!(w
            .harness
            .query_by_label("Not available on this desktop")
            .is_none());
    }

    #[test]
    fn the_legend_reads_before_the_rows() {
        let w = testing::scenario(Scenario::OnInCall, Page::Shortcuts);
        let legend = w
            .harness
            .get_by_label_contains("apps hear your cleaned voice")
            .rect();
        let first = w.harness.get_by_label("Toggle mute").rect();
        assert!(legend.bottom() <= first.top(), "{legend:?} {first:?}");
    }

    #[test]
    fn not_available_shows_the_commands_and_no_button() {
        let w = testing::scenario(Scenario::ShortcutsUnavailable, Page::Shortcuts);
        assert!(w.harness.query_by_label("hushmic toggle mute").is_some());
        assert!(w.harness.query_by_label("Change shortcuts").is_none());
        assert!(w.harness.query_by_label("Toggle mute").is_some());
    }
}
