//! The window around the pages: the rail (the Mark, the pages, More
//! with Turn off / Quit) and the page area beside it,
//! which scrolls when the window is too small for the page.

use eframe::egui::{self, pos2, vec2, Rect, Ui};

use super::{status, Cx};
use crate::appstate::{Action, Mode, Page};
use crate::tr;
use crate::ui::icons::Icon;
use crate::ui::widgets::{rail, MenuItem, Rail, RailItem};

#[derive(Default)]
pub struct Frame {
    shown: Option<Page>,
    /// Screenshots: the More menu drawn open.
    pub more_open: bool,
}

fn rail_page(p: Page) -> rail::Page {
    match p {
        #[cfg(feature = "speakers")]
        Page::Speakers => rail::Page::Speakers,
        Page::Files => rail::Page::Files,
        Page::Settings | Page::About | Page::Shortcuts => rail::Page::Settings,
        Page::Live | Page::FirstRun => rail::Page::Live,
    }
}

fn app_page(p: rail::Page) -> Page {
    match p {
        rail::Page::Live => Page::Live,
        #[cfg(feature = "speakers")]
        rail::Page::Speakers => Page::Speakers,
        rail::Page::Files => Page::Files,
        rail::Page::Settings => Page::Settings,
    }
}

impl Frame {
    /// The rail on the left of `full`, the page (drawn by `page`) in a
    /// scroll area to its right.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        full: Rect,
        page: Page,
        cx: &mut Cx<'_>,
        draw: impl FnOnce(&mut Ui, &mut Cx<'_>),
    ) {
        let st = status::status(cx.state);
        let off = cx.state.mode == Mode::Off;
        #[cfg(feature = "speakers")]
        let speakers = tr!("rail-speakers");
        let (live, files, settings, more) = (
            tr!("rail-live"),
            tr!("rail-files"),
            tr!("rail-settings"),
            tr!("rail-more"),
        );
        let (power, quit) = (
            if off {
                tr!("live-turn-on")
            } else {
                tr!("rail-turn-off")
            },
            tr!("rail-quit"),
        );
        let quit_key = tr!("rail-quit-key");
        // Turn off says how it differs from Quit (UX panel round 2, A19);
        // Quit needs no line of its own
        let off_about = if cx.state.tray == crate::appstate::TrayHost::Registered {
            tr!("rail-turn-off-about-tray")
        } else {
            tr!("rail-turn-off-about-running")
        };
        let menu_about = [(!off).then_some(off_about.as_str()), None];
        // The two ways to stop, and Quit's key where it can be found (UX
        // panel A8). No "Close window": the window's own close button and
        // Ctrl+W do that, and Turn off's line says HushMic stays.
        let menu = [
            MenuItem {
                label: &power,
                icon: Some(Icon::Power),
                separator: false,
                hint: None,
            },
            MenuItem {
                label: &quit,
                icon: Some(Icon::Quit),
                separator: true,
                hint: Some(&quit_key),
            },
        ];
        let mark_name = st.mark_name();
        let list_name = tr!("rail-sections");
        let rw = Rail::width(cx.compact);
        let rail_rect = Rect::from_min_size(full.min, vec2(rw, full.height()));
        let mut rail_ui = super::util::child(ui, rail_rect, "hm-rail");
        let rail = Rail {
            current: rail_page(page),
            items: [
                RailItem {
                    page: rail::Page::Live,
                    label: &live,
                },
                #[cfg(feature = "speakers")]
                RailItem {
                    page: rail::Page::Speakers,
                    label: &speakers,
                },
                RailItem {
                    page: rail::Page::Files,
                    label: &files,
                },
                RailItem {
                    page: rail::Page::Settings,
                    label: &settings,
                },
            ],
            mark: st.mark,
            mark_name: &mark_name,
            name: &list_name,
            more: &more,
            menu: &menu,
            menu_about: &menu_about,
            compact: cx.compact,
        };
        let r = if self.more_open {
            crate::ui::widgets::with_forced(&mut rail_ui, crate::ui::widgets::Force::Open, |ui| {
                rail.show(ui, rail_rect)
            })
        } else {
            rail.show(&mut rail_ui, rail_rect)
        };
        if let Some(p) = r.navigate {
            cx.goto = Some(app_page(p));
        }
        match r.menu {
            Some(0) if off => cx.send(Action::SetMode(Mode::Suppress)),
            // asked first while an app uses HushMic, as Quit
            Some(0) => cx.turn_off = true,
            // asked first while an app uses HushMic, as Ctrl+Q
            Some(1) => cx.quit = true,
            _ => {}
        }

        let area = Rect::from_min_max(pos2(rail_rect.right(), full.top()), full.max);
        let mut page_ui = super::util::child(ui, area, ("hm-page", page));
        page_ui.set_clip_rect(area);
        // no drag-to-scroll: the page area would be an unnamed Tab stop
        let mut scroll = egui::ScrollArea::vertical()
            .scroll_source(egui::scroll_area::ScrollSource {
                drag: false,
                ..egui::scroll_area::ScrollSource::ALL
            })
            .id_salt(("hm-scroll", page))
            .auto_shrink([false, false]);
        if self.shown != Some(page) {
            // a page opens at its top
            self.shown = Some(page);
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        let out = scroll.show(&mut page_ui, |ui| {
            ui.set_width(area.width());
            draw(ui, cx);
        });
        let name = match rail_page(page) {
            rail::Page::Live => live,
            #[cfg(feature = "speakers")]
            rail::Page::Speakers => speakers,
            rail::Page::Files => files,
            rail::Page::Settings => settings,
        };
        crate::ui::widgets::scroll::bar(&page_ui, &out, &name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock::{state, Scenario};
    use crate::pages::testing;
    use crate::window::Link;
    use eframe::egui;
    use egui_kittest::kittest::{NodeT, Queryable};

    #[test]
    fn the_rail_moves_between_pages() {
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        w.harness.get_by_label("Files").click();
        w.harness.run_steps(2);
        assert_eq!(*w.page.borrow(), Page::Files);
        assert!(w.harness.query_by_label("Mic check").is_none());
        w.harness.get_by_label("Settings").click();
        w.harness.run_steps(2);
        assert_eq!(*w.page.borrow(), Page::Settings);
    }

    #[test]
    fn more_turns_hushmic_off_and_quits() {
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        w.harness.get_by_label("More").click();
        w.harness.run_steps(2);
        w.harness.get_by_label("Turn off HushMic").click();
        w.harness.run_steps(2);
        // an app uses HushMic: asked first (UX panel A3)
        assert!(w.link.sent().is_empty());
        w.harness.get_by_label("Turn off").click();
        w.harness.run_steps(2);
        w.harness.get_by_label("More").click();
        w.harness.run_steps(2);
        w.harness.get_by_label("Quit HushMic").click();
        w.harness.run_steps(2);
        // an app uses HushMic: the question first, as Ctrl+Q (opened by
        // the pointer: no focus ring)
        assert_eq!(w.link.sent(), vec![Action::SetMode(Mode::Off)]);
        assert!(!crate::ui::kit(&w.harness.ctx).focus_visible);
        w.harness.get_by_label("Quit").click();
        w.harness.run_steps(2);
        assert_eq!(
            w.link.sent(),
            vec![Action::SetMode(Mode::Off), Action::Quit]
        );
        // nobody uses it: More quits at once
        let mut w = testing::scenario(Scenario::OnNoApp, Page::Live);
        w.harness.get_by_label("More").click();
        w.harness.run_steps(2);
        w.harness.get_by_label("Quit HushMic").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::Quit]);
        // while off, More turns it back on
        let mut w = testing::scenario(Scenario::Off, Page::Shortcuts);
        let shown = |w: &testing::Window| w.harness.query_all_by_label("Turn on HushMic").count();
        let before = shown(&w);
        w.harness.get_by_label("More").click();
        w.harness.run_steps(2);
        assert_eq!(shown(&w), before + 1);
    }

    /// UX panel round 2, A24: More's menu closes on Escape, and when the
    /// page scrolls behind it.
    #[test]
    fn more_closes_on_escape_and_on_scroll() {
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        let open = |w: &testing::Window| w.harness.query_by_label("Quit HushMic").is_some();
        w.harness.get_by_label("More").click();
        w.harness.run_steps(2);
        assert!(open(&w));
        w.harness.key_press(egui::Key::Escape);
        w.harness.run_steps(2);
        assert!(!open(&w), "Escape");
        w.harness.get_by_label("More").click();
        w.harness.run_steps(2);
        assert!(open(&w));
        w.harness.input_mut().events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -40.0),
            modifiers: egui::Modifiers::NONE,
        });
        w.harness.run_steps(2);
        assert!(!open(&w), "scroll");
    }

    /// UX panel round 2, A24: in the narrow rail a tooltip sits to the
    /// right of its icon, never over the icon below.
    #[test]
    fn a_rail_tooltip_sits_beside_its_icon() {
        let mut w = testing::window(
            crate::appstate::mock::state(Scenario::OnInCall),
            Page::Live,
            egui::vec2(720.0, 520.0),
        );
        let files = w.harness.get_by_label("Files").rect();
        let below = w.harness.get_by_label("Settings").rect();
        w.harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(files.center()));
        w.harness.run_steps(8);
        let tips: Vec<egui::Rect> = w
            .harness
            .query_all_by_label("Files")
            .map(|n| n.rect())
            .filter(|r| *r != files)
            .collect();
        assert!(!tips.is_empty(), "a tooltip shows");
        for tip in tips {
            assert!(tip.left() >= files.right(), "{tip:?} beside {files:?}");
            assert!(!tip.intersects(below), "{tip:?} over {below:?}");
        }
    }

    /// The focused node's name (or value, for a label).
    fn focused(w: &testing::Window) -> Option<String> {
        w.harness.query_by(|n| n.is_focused()).map(|n| {
            let ak = n.accesskit_node();
            ak.label().or(ak.value()).unwrap_or_default().to_string()
        })
    }

    #[test]
    fn the_keyboard_walks_the_rail_into_the_page_and_moves_pages() {
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        let mut seen = Vec::new();
        for _ in 0..16 {
            w.harness.key_press(egui::Key::Tab);
            w.harness.run_steps(2);
            seen.extend(focused(&w));
        }
        for want in ["Live", "Files", "Settings", "Mute", "Microphone", "Maximum"] {
            assert!(
                seen.iter().any(|s| s == want),
                "{want} not reached by Tab: {seen:?}"
            );
        }
        // Enter on a rail page opens it
        w.harness.get_by_label("Settings").focus();
        w.harness.run_steps(2);
        w.harness.key_press(egui::Key::Enter);
        w.harness.run_steps(2);
        assert_eq!(*w.page.borrow(), Page::Settings);
    }

    #[test]
    fn every_tab_stop_has_a_name() {
        for (sc, page) in [
            (Scenario::DirectMic, Page::Live),
            (Scenario::OnInCall, Page::Live),
            (Scenario::OnNoApp, Page::Live),
            (Scenario::OnInCall, Page::Shortcuts),
            (Scenario::OnInCall, Page::Settings),
            (Scenario::PipewireUnreachable, Page::Live),
        ] {
            for compact in [false, true] {
                let mut w = if compact {
                    testing::compact(sc, page)
                } else {
                    testing::scenario(sc, page)
                };
                tab_stops_are_named(&mut w, &format!("{sc:?} {page:?} compact={compact}"));
            }
        }
        // Files with a list that overflows (it scrolls inside its card)
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let mut w = testing::with_setup(
                Link::recording(state(Scenario::OnInCall)),
                Page::Files,
                size,
                |ps| ps.files.preset = Some(("queue".into(), 3.3)),
            );
            tab_stops_are_named(&mut w, &format!("Files queue {size:?}"));
        }
    }

    fn tab_stops_are_named(w: &mut testing::Window, what: &str) {
        for i in 0..40 {
            w.harness.key_press(egui::Key::Tab);
            w.harness.run_steps(2);
            if let Some(name) = focused(w) {
                let node = w
                    .harness
                    .query_by(|n| n.is_focused())
                    .map(|n| format!("{:?} {:?}", n.accesskit_node().role(), n.rect()));
                assert!(!name.is_empty(), "{what}: stop {i}: {node:?}");
            }
        }
    }

    #[test]
    fn a_long_page_scrolls_from_the_keyboard() {
        use egui::accesskit::Role;
        let mut w = testing::compact(Scenario::OnInCall, Page::Settings);
        let on_bar = |w: &testing::Window| {
            w.harness
                .query_by(|n| n.is_focused() && n.role() == Role::ScrollBar)
                .is_some()
        };
        for _ in 0..60 {
            if on_bar(&w) {
                break;
            }
            w.harness.key_press(egui::Key::Tab);
            w.harness.run_steps(2);
        }
        assert!(on_bar(&w), "the page's scroll bar is a Tab stop");
        w.harness.key_press(egui::Key::End);
        w.harness.run_steps(3);
        let bar = testing::by_role(&w.harness, "Settings", Role::ScrollBar);
        let n = bar.accesskit_node();
        let (at, max) = (n.numeric_value().unwrap(), n.max_numeric_value().unwrap());
        assert!(max > 0.0 && at == max, "{at} of {max}");
    }

    #[test]
    fn every_page_lights_the_top_while_hushmic_filters() {
        for page in [Page::Live, Page::Files, Page::Shortcuts, Page::Settings] {
            let mut w = testing::scenario(Scenario::OnInCall, page);
            w.harness.run_steps(2);
            let l = w.light();
            assert!(l.static_edge || l.edge.is_some(), "{page:?}: lit");
            for sc in [Scenario::Bypass, Scenario::Muted, Scenario::Off] {
                let mut w = testing::scenario(sc, page);
                w.harness.run_steps(2);
                let l = w.light();
                assert!(
                    !l.static_edge && l.edge.is_none_or(|e| e.lit == 0.0 && e.entry == 0.0),
                    "{page:?} {sc:?}: dark"
                );
            }
        }
    }

    #[test]
    fn the_rail_names_the_state_on_the_mark() {
        // no status pill at either size: the Mark carries the state
        let w = testing::scenario(Scenario::Muted, Page::Settings);
        assert!(w.harness.query_by_label("HushMic: Muted").is_some());
        assert!(w.harness.query_by_label("Muted").is_none());
        let w = testing::compact(Scenario::Muted, Page::Settings);
        assert!(w.harness.query_by_label("HushMic: Muted").is_some());
        let w = testing::compact(Scenario::PipewireUnreachable, Page::Live);
        // the rail's Mark says it (the page says it in its notice)
        assert_eq!(
            w.harness
                .query_all_by_label("HushMic needs attention")
                .count(),
            1
        );
    }
}
