//! The window's keys (appux proposal §2): Ctrl+W closes the window, Ctrl+Q
//! quits HushMic and, while files clean or an app is using it, asks first.
//! Escape closes an open list, menu, sheet or dialog, and never the window
//! (UX panel A8: pressed by reflex, it made the window vanish, first run
//! and a replay included). The More menu shows both shortcuts. Closing the
//! window never stops noise suppression; the daemon says so on the first
//! close.

use eframe::egui::{self, Key, Modifiers};

use crate::appstate::{Action, AppState, Ask, Mode};
use crate::tr;
use crate::ui::fonts::Type;
use crate::ui::text;
use crate::ui::widgets::{self, Button};
use crate::window::Link;

pub struct Keys {
    /// The window's own question is up (an app uses HushMic): Quit, or
    /// Turn off.
    asking: Option<Ask>,
    /// Its first frame: the keyboard starts on the safe answer, with the
    /// focus ring when the keyboard opened it (true).
    fresh: Option<bool>,
    /// The daemon's files question is not up: when it comes, the keyboard
    /// starts on Keep cleaning.
    files_fresh: bool,
    /// What had the keyboard when a question came up (More, after its
    /// menu; the control under Ctrl+Q), and whether the ring showed: the
    /// keyboard goes back there when the question is answered (UX panel
    /// B21: Escape left it nowhere). The outer None: no question yet.
    opener: Option<Option<(egui::Id, bool)>>,
    /// The opener, to take the keyboard back the frame after the answer:
    /// the controls under the sheet let go of focus while it shows, for a
    /// frame after it closes too.
    refocus: Option<(egui::Id, bool)>,
}

impl Default for Keys {
    fn default() -> Self {
        Keys {
            asking: None,
            fresh: None,
            files_fresh: true,
            opener: None,
            refocus: None,
        }
    }
}

impl Keys {
    /// This frame's keys, before the pages draw (a list, menu or sheet
    /// takes its own Escape).
    pub fn read(&mut self, ctx: &egui::Context, s: &AppState, link: &Link) {
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::W)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Q)) {
            self.quit(s, link, true);
        }
    }

    /// Quit HushMic (Ctrl+Q, More › Quit HushMic): while files clean, the
    /// daemon asks (the sheet, as for the tray's Quit), and a second Quit
    /// while that shows quits; while an app is using it, ask here first.
    /// `keyboard`: the keyboard asked (the sheet opens with Cancel's focus
    /// ring).
    pub fn quit(&mut self, s: &AppState, link: &Link, keyboard: bool) {
        if self.asking.is_some() {
            return;
        }
        if s.files.ask_quit || s.ask == Some(Ask::Quit) {
            link.send(Action::Quit);
        } else if super::files::at_stake(s) > 0 {
            link.send(Action::FilesAskQuit);
        } else if s.in_use() {
            self.asking = Some(Ask::Quit);
            self.fresh = Some(keyboard);
        } else {
            link.send(Action::Quit);
        }
    }

    /// Turn off HushMic (More › Turn off HushMic): while an app is using
    /// it, ask first, as for Quit (UX panel A3: off, the app hears the
    /// microphone with the noise).
    pub fn turn_off(&mut self, s: &AppState, link: &Link, keyboard: bool) {
        if self.asking.is_some() {
            return;
        }
        if s.in_use() && s.ask.is_none() {
            self.asking = Some(Ask::TurnOff);
            self.fresh = Some(keyboard);
        } else {
            link.send(Action::SetMode(Mode::Off));
        }
    }

    /// The quit question, while it is up (drawn over the pages): one sheet
    /// for both questions (round 3), centred on the page column. Quit while
    /// an app is using HushMic (Cancel · Quit), and Quit while files clean,
    /// the tray's or the window's (Keep cleaning · Quit). Quit is the
    /// primary: the user chose it; the keyboard starts on the safe answer
    /// and Escape is it.
    pub fn ui(&mut self, ctx: &egui::Context, s: &AppState, link: &Link) {
        if let Some((id, shown)) = self.refocus.take() {
            if shown {
                crate::ui::keyboard_focus(ctx, id);
            } else {
                ctx.memory_mut(|m| m.request_focus(id));
            }
        }
        let files = s.files.ask_quit;
        // the daemon's questions first (the files Quit would stop, the
        // tray's Quit or Off), then the window's own
        let asked = if files {
            Asked::Files
        } else if let Some(a) = s.ask {
            Asked::Daemon(a)
        } else if let Some(a) = self.asking {
            Asked::Window(a)
        } else {
            self.files_fresh = true;
            self.opener = None;
            return;
        };
        if self.opener.is_none() {
            let shown = crate::ui::kit(ctx).focus_visible;
            self.opener = Some(ctx.memory(|m| m.focused()).map(|id| (id, shown)));
        }
        if !matches!(asked, Asked::Window(_))
            && std::mem::take(&mut self.files_fresh)
            && self.fresh.is_none()
        {
            // the daemon's question just came up: the safe answer has the
            // keyboard
            self.fresh = Some(false);
        }
        let pal = crate::ui::kit(ctx).pal;
        let full = ctx.content_rect();
        let w = (full.width() - 64.0).min(420.0);
        // the page column's centre: the space right of the rail
        let compact = full.width() < crate::ui::tokens::window::COMPACT_BELOW;
        let rail = crate::ui::widgets::Rail::width(compact);
        let id = egui::Id::new("hm-quit-ask");
        let area = egui::Modal::default_area(id.with("area"))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(rail / 2.0, 0.0));
        let ask = Question::of(s, asked);
        let modal = egui::Modal::new(id)
            .area(area)
            .backdrop_color(pal.scrim)
            .frame(
                egui::Frame::new()
                    .fill(pal.surface_raised)
                    .stroke(egui::Stroke::new(1.0_f32, pal.line))
                    .corner_radius(12)
                    .inner_margin(20),
            )
            .show(ctx, |ui| {
                widgets::set_backdrop(ui, pal.surface_raised);
                ui.set_width(w - 40.0);
                // one alert dialog for screen readers: named by the
                // question, the sentence as its description, the page
                // behind out of reach while it is up
                let outer = ui.max_rect();
                let mut d = widgets::a11y_children(
                    ui,
                    outer,
                    egui::accesskit::Role::AlertDialog,
                    &ask.title,
                );
                let described = format!("{} {}", ask.body, ask.means);
                widgets::a11y(ctx, d.unique_id(), |n| {
                    n.set_modal();
                    n.set_description(described.trim_start());
                });
                let out = self.sheet(&mut d, &ask);
                ui.advance_cursor_after_rect(d.min_rect());
                out
            });
        let answer = match modal.inner {
            Some(a) => Some(a),
            None if modal.should_close() => Some(Answer::Safe),
            None => None,
        };
        let Some(answer) = answer else {
            return;
        };
        let yes = answer != Answer::Safe;
        self.asking = None;
        // back to where the keyboard was, next frame (a control that went
        // meanwhile drops it, and the page offers its own)
        self.refocus = self.opener.take().flatten();
        match asked {
            Asked::Files if answer == Answer::SaveGo => link.send(Action::FilesSaveAndQuit),
            Asked::Files => link.send(Action::FilesQuit(yes)),
            Asked::Daemon(_) => link.send(Action::Answer(yes)),
            Asked::Window(Ask::Quit) if yes => link.send(Action::Quit),
            Asked::Window(Ask::TurnOff) if yes => link.send(Action::SetMode(Mode::Off)),
            Asked::Window(_) => {}
        }
    }

    /// The question's words and buttons, and how it was answered.
    fn sheet(&mut self, ui: &mut egui::Ui, ask: &Question) -> Option<Answer> {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        text::label(ui, &ask.title, Type::T3, pal.ink);
        ui.add_space(6.0);
        if !ask.body.is_empty() {
            text::label(ui, &ask.body, Type::SUB, pal.ink_muted);
            ui.add_space(8.0);
        }
        // how Quit and Turn off differ (UX panel round 2, A19)
        text::label(ui, &ask.means, Type::SUB, pal.ink);
        ui.add_space(20.0);
        // the safe answer, then the acts, at the right (reading and Tab
        // order); "Save all and quit" is the primary where it shows. The
        // act takes something away: the danger outline, never the white
        // primary (A19)
        let save = ask.save_go.as_deref().map(|l| Button::primary(l).sm());
        let (q, c) = (
            Button::danger(&ask.go).sm(),
            Button::secondary(&ask.safe).sm(),
        );
        let row = c.width(&ctx)
            + 8.0
            + q.width(&ctx)
            + save.as_ref().map_or(0.0, |b| 8.0 + b.width(&ctx));
        let mut out = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space((ui.available_width() - row).max(0.0));
            let r = ui.add(c);
            match self.fresh.take() {
                Some(true) => crate::ui::keyboard_focus(&ctx, r.id),
                Some(false) => r.request_focus(),
                None => {}
            }
            if r.clicked() {
                out = Some(Answer::Safe);
            }
            ui.add_space(8.0);
            if ui.add(q).clicked() {
                out = Some(Answer::Go);
            }
            if let Some(b) = save {
                ui.add_space(8.0);
                if ui.add(b).clicked() {
                    out = Some(Answer::SaveGo);
                }
            }
        });
        out
    }
}

/// Which question is up, and whose: the daemon's because files clean,
/// the daemon's for the tray's Quit or Off, or the window's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    Files,
    Daemon(Ask),
    Window(Ask),
}

/// The quit sheet's words.
struct Question {
    title: String,
    body: String,
    /// The act: Quit, or Turn off.
    go: String,
    /// The safe answer: Cancel, or Keep cleaning while files clean.
    safe: String,
    /// While files would be lost: "Save all and quit" (the primary then).
    save_go: Option<String>,
    /// How Quit and Turn off differ: what this one keeps or ends.
    means: String,
}

/// How the sheet was answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    Safe,
    Go,
    /// Save every file Quit would lose (once cleaned), then quit.
    SaveGo,
}

impl Question {
    fn of(s: &AppState, asked: Asked) -> Question {
        let off = matches!(
            asked,
            Asked::Daemon(Ask::TurnOff) | Asked::Window(Ask::TurnOff)
        );
        let files = asked == Asked::Files;
        let n = s.apps.using.len();
        let apps = (n > 0).then(|| {
            let what = if off {
                tr!("off-ask-body", count = n)
            } else {
                tr!("quit-ask-body", count = n)
            };
            format!("{} {what}", super::live::apps_line(s))
        });
        let (body, safe) = if !files {
            (apps.unwrap_or_default(), tr!("quit-ask-cancel"))
        } else {
            // what Quit stops and loses (once they finished while it
            // asked: what it loses, or quitting alone)
            let (files_line, cleaning) = super::files::quit_line(s);
            let body = match apps {
                Some(a) => format!("{a} {files_line}"),
                None => files_line,
            };
            let safe = if cleaning {
                tr!("files-keep-cleaning")
            } else {
                tr!("quit-ask-cancel")
            };
            (body, safe)
        };
        let (title, go, means) = if off {
            let means = if s.tray == crate::appstate::TrayHost::Registered {
                tr!("off-ask-means-tray")
            } else {
                tr!("off-ask-means-running")
            };
            (tr!("off-ask-title"), tr!("off-ask-turn-off"), means)
        } else {
            (
                tr!("quit-ask-title"),
                tr!("quit-ask-quit"),
                tr!("quit-ask-means"),
            )
        };
        // files to lose, and somewhere to save them (in a sandbox a folder
        // is chosen first, on the page)
        let place = !s.files.needs_folder || s.files.folder.is_some();
        let save_go = (files && place && super::files::at_stake(s) > 0)
            .then(|| tr!("files-save-all-and-quit"));
        Question {
            title,
            body,
            go,
            safe,
            save_go,
            means,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::appstate::mock::Scenario;
    use crate::appstate::{Action, Page};
    use crate::pages::testing;
    use eframe::egui::{self, Key, Modifiers, ViewportCommand, ViewportId};
    use egui_kittest::kittest::{NodeT, Queryable};

    /// The frame that read `key` asked to close the window (one frame with
    /// the press, so its output is the one read).
    fn press(w: &mut testing::Window, m: Modifiers, key: Key) -> bool {
        let input = w.harness.input_mut();
        input.modifiers = m;
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: m,
        });
        w.harness.step();
        w.harness.input_mut().modifiers = Modifiers::NONE;
        let closed = w
            .harness
            .output()
            .viewport_output
            .get(&ViewportId::ROOT)
            .is_some_and(|v| {
                v.commands
                    .iter()
                    .any(|c| matches!(c, ViewportCommand::Close))
            });
        w.harness.run_steps(2);
        closed
    }

    /// Ctrl+W closes the window; Escape never does (UX panel A8), on any
    /// page, first run and a replay included.
    #[test]
    fn ctrl_w_closes_the_window_and_escape_never_does() {
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        assert!(press(&mut w, Modifiers::COMMAND, Key::W));
        // closing never touches the engine
        assert!(w.link.sent().is_empty());
        for page in [
            Page::Live,
            Page::Files,
            Page::Shortcuts,
            Page::Settings,
            Page::FirstRun,
        ] {
            let mut w = testing::scenario(Scenario::OnInCall, page);
            assert!(!press(&mut w, Modifiers::NONE, Key::Escape), "{page:?}");
        }
    }

    /// The More menu: the two ways to stop, Quit HushMic with its key; no
    /// "Close window" (the window's own close button and Ctrl+W do that).
    #[test]
    fn more_holds_turn_off_and_quit_with_its_key() {
        crate::i18n::pin_english();
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        w.harness.get_by_label("More").click();
        w.harness.run_steps(2);
        let n = w.harness.get_by_label("Quit HushMic");
        assert_eq!(
            n.accesskit_node().data().keyboard_shortcut(),
            Some("Ctrl+Q")
        );
        assert!(w.harness.query_by_label("Turn off HushMic").is_some());
        assert!(w.harness.query_by_label("Close window").is_none());
    }

    /// More's Turn off says how it differs from Quit, under its label and
    /// to screen readers (UX panel round 2, A19); without a tray, it keeps
    /// HushMic running. Quit has no line: its name says it.
    #[test]
    fn more_says_how_turn_off_differs_from_quit() {
        crate::i18n::pin_english();
        for (tray, off) in [
            (
                crate::appstate::TrayHost::Registered,
                "Keeps HushMic in the tray",
            ),
            (crate::appstate::TrayHost::Waiting, "Keeps HushMic running"),
        ] {
            let mut s = crate::appstate::mock::state(Scenario::OnInCall);
            s.tray = tray;
            let mut w = testing::window(s, Page::Live, egui::vec2(960.0, 640.0));
            w.harness.get_by_label("More").click();
            w.harness.run_steps(2);
            let item = |name: &str| {
                w.harness
                    .get_by_label(name)
                    .accesskit_node()
                    .description()
                    .map(|d| d.to_string())
            };
            assert_eq!(item("Turn off HushMic").as_deref(), Some(off), "{tray:?}");
            // Quit says what it does in its name: no line under it
            assert_eq!(item("Quit HushMic"), None);
            // the line under the label makes the item taller
            let off = w.harness.get_by_label("Turn off HushMic").rect();
            let quit = w.harness.get_by_label("Quit HushMic").rect();
            assert!(off.height() > quit.height() + 10.0, "{off:?} {quit:?}");
        }
    }

    #[test]
    fn escape_closes_an_open_menu_or_sheet_first() {
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        w.harness.get_by_label("More").click();
        w.harness.run_steps(2);
        assert!(w.harness.query_by_label("Quit HushMic").is_some());
        assert!(!press(&mut w, Modifiers::NONE, Key::Escape));
        assert!(w.harness.query_by_label("Quit HushMic").is_none());
        // and the next one leaves the window be
        assert!(!press(&mut w, Modifiers::NONE, Key::Escape));

        let mut w = testing::scenario(Scenario::OnInCall, Page::About);
        testing::by_role(&w.harness, "Licenses", testing::Role::Link).click();
        w.harness.run_steps(3);
        assert!(w.harness.query_by_label("Close").is_some());
        // a dialog named Licenses for screen readers
        testing::by_role(&w.harness, "Licenses", testing::Role::Dialog);
        assert!(!press(&mut w, Modifiers::NONE, Key::Escape));
        assert!(w.harness.query_by_label("Close").is_none());
    }

    #[test]
    fn ctrl_q_quits_and_asks_first_while_an_app_uses_hushmic() {
        // nobody uses it: quits at once
        let mut w = testing::scenario(Scenario::OnNoApp, Page::Live);
        press(&mut w, Modifiers::COMMAND, Key::Q);
        assert_eq!(w.link.sent(), vec![Action::Quit]);

        // an app uses it: the question, the keyboard on Cancel
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        assert!(!press(&mut w, Modifiers::COMMAND, Key::Q));
        assert!(w.link.sent().is_empty());
        assert!(w.harness.get_by_label("Cancel").is_focused());
        // opened from the keyboard: the ring shows where the keyboard is
        assert!(crate::ui::kit(&w.harness.ctx).focus_visible);
        // a screen reader meets it as one alert dialog: the question its
        // name, the sentence its description (the app by name), the page
        // behind out of reach
        let question = "Quit HushMic?";
        let d = testing::by_role(&w.harness, question, testing::Role::AlertDialog);
        let n = d.accesskit_node();
        let s = crate::appstate::mock::state(Scenario::OnInCall);
        assert_eq!(
            n.description(),
            Some(format!(
                "{} Quitting removes the HushMic microphone from it. Your call will hear \
                 your microphone directly, with the noise. Quit closes HushMic \
                 completely.",
                crate::pages::live::apps_line(&s)
            ))
        );
        // Escape answers the question, not the window
        assert!(!press(&mut w, Modifiers::NONE, Key::Escape));
        assert!(w.harness.query_by_label(question).is_none());
        assert!(w.link.sent().is_empty());
        // asked again, Quit quits
        press(&mut w, Modifiers::COMMAND, Key::Q);
        w.harness.get_by_label("Quit").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::Quit]);
    }

    /// UX panel B21: the arrows stay in their group and never open a menu.
    #[test]
    fn arrows_stay_in_their_group() {
        crate::i18n::pin_english();
        use testing::Role;
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        let tab = |w: &testing::Window, name: &str| {
            testing::by_role(&w.harness, name, Role::Tab).is_focused()
        };
        testing::by_role(&w.harness, "Live", Role::Tab).focus();
        w.harness.run_steps(2);
        w.harness.key_press(Key::ArrowDown);
        w.harness.run_steps(2);
        let next = if cfg!(feature = "speakers") {
            "Speakers"
        } else {
            "Files"
        };
        assert!(tab(&w, next), "Down moves to the next page row");
        for _ in 0..5 {
            w.harness.key_press(Key::ArrowDown);
            w.harness.run_steps(2);
        }
        assert!(tab(&w, "Settings"), "and stops at the last");
        assert!(
            w.harness.query_by_label("Quit HushMic").is_none(),
            "More stays shut"
        );
        w.harness.key_press(Key::ArrowUp);
        w.harness.run_steps(2);
        assert!(tab(&w, "Files"));
        // first run: Down on Continue goes nowhere (it went to Skip)
        let mut w = testing::scenario(Scenario::FirstRun1, Page::FirstRun);
        w.harness.get_by_label("Continue").focus();
        w.harness.run_steps(2);
        w.harness.key_press(Key::ArrowDown);
        w.harness.key_press(Key::ArrowUp);
        w.harness.run_steps(2);
        assert!(w.harness.get_by_label("Continue").is_focused());
    }

    /// UX panel A3: More › Turn off HushMic asks first while an app uses
    /// HushMic, as Quit does; with none, it turns off at once.
    #[test]
    fn turn_off_asks_first_while_an_app_uses_hushmic() {
        crate::i18n::pin_english();
        let turn_off = |w: &mut testing::Window| {
            w.harness.get_by_label("More").click();
            w.harness.run_steps(2);
            w.harness.get_by_label("Turn off HushMic").click();
            w.harness.run_steps(2);
        };
        let mut w = testing::scenario(Scenario::OnNoApp, Page::Live);
        turn_off(&mut w);
        assert_eq!(
            w.link.sent(),
            vec![Action::SetMode(crate::appstate::Mode::Off)]
        );

        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        turn_off(&mut w);
        assert!(w.link.sent().is_empty());
        let question = "Turn off HushMic?";
        let d = testing::by_role(&w.harness, question, testing::Role::AlertDialog);
        let s = crate::appstate::mock::state(Scenario::OnInCall);
        assert_eq!(
            d.accesskit_node().description(),
            Some(format!(
                "{} Turning it off removes the HushMic microphone from it. Your call will \
                 hear your microphone directly, with the noise. Turn off keeps HushMic \
                 in the tray, so you can turn it back on.",
                crate::pages::live::apps_line(&s)
            ))
        );
        assert!(w.harness.get_by_label("Cancel").is_focused());
        // Escape keeps it on, and the keyboard goes back to More (B21)
        assert!(!press(&mut w, Modifiers::NONE, Key::Escape));
        assert!(w.harness.query_by_label(question).is_none());
        assert!(w.link.sent().is_empty());
        w.harness.run_steps(2);
        assert!(w.harness.get_by_label("More").is_focused());
        // asked again, Turn off turns it off
        turn_off(&mut w);
        w.harness.get_by_label("Turn off").click();
        w.harness.run_steps(2);
        assert_eq!(
            w.link.sent(),
            vec![Action::SetMode(crate::appstate::Mode::Off)]
        );
    }

    /// UX panel A3: the tray's Quit or Off has the window ask; the answer
    /// goes to the daemon, which acts on it.
    #[test]
    fn the_trays_question_is_asked_and_answered() {
        use crate::appstate::Ask;
        crate::i18n::pin_english();
        let size = egui::vec2(960.0, 640.0);
        for (ask, title, go) in [
            (Ask::Quit, "Quit HushMic?", "Quit"),
            (Ask::TurnOff, "Turn off HushMic?", "Turn off"),
        ] {
            let mut s = crate::appstate::mock::state(Scenario::OnInCall);
            s.ask = Some(ask);
            let mut w = testing::at_60hz(s.clone(), Page::Live, size);
            w.harness.run_steps(3);
            testing::by_role(&w.harness, title, testing::Role::AlertDialog);
            assert!(w.harness.get_by_label("Cancel").is_focused(), "{ask:?}");
            assert!(!press(&mut w, Modifiers::NONE, Key::Escape));
            assert_eq!(w.link.sent(), vec![Action::Answer(false)], "{ask:?}");
            let mut w = testing::at_60hz(s, Page::Live, size);
            w.harness.run_steps(3);
            w.harness.get_by_label(go).click();
            w.harness.run_steps(2);
            assert_eq!(w.link.sent(), vec![Action::Answer(true)], "{ask:?}");
        }
    }

    /// An app in a call, and files that Quit would stop (two of three).
    fn cleaning(ask_quit: bool) -> crate::appstate::AppState {
        use crate::appstate::{FileJob, JobState};
        let mut s = crate::appstate::mock::state(Scenario::OnInCall);
        s.files.jobs = [JobState::Cleaning, JobState::Waiting, JobState::Done]
            .into_iter()
            .zip(1..)
            .map(|(state, id)| FileJob {
                id,
                path: format!("/nowhere/{id}.wav"),
                state,
                ..FileJob::default()
            })
            .collect();
        s.files.ask_quit = ask_quit;
        s
    }

    #[test]
    fn quit_in_the_window_while_files_clean_asks_as_the_tray_does() {
        let size = egui::vec2(960.0, 640.0);
        // Ctrl+Q: the daemon's question (the files it would stop), not the
        // window's own about the call
        let mut w = testing::at_60hz(cleaning(false), Page::Live, size);
        w.harness.run_steps(2);
        assert!(!press(&mut w, Modifiers::COMMAND, Key::Q));
        assert_eq!(w.link.sent(), vec![Action::FilesAskQuit]);
        assert!(w.harness.query_by_label("Quit HushMic?").is_none());
        // More › Quit HushMic the same
        let mut w = testing::at_60hz(cleaning(false), Page::Live, size);
        w.harness.run_steps(2);
        w.harness.get_by_label("More").click();
        w.harness.run_steps(2);
        w.harness.get_by_label("Quit HushMic").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::FilesAskQuit]);
        // a second Quit while the question shows quits for real
        let mut w = testing::at_60hz(cleaning(true), Page::Live, size);
        w.harness.run_steps(3);
        assert!(!press(&mut w, Modifiers::COMMAND, Key::Q));
        assert_eq!(w.link.sent(), vec![Action::Quit]);
    }

    /// The question the daemon asks while files clean, as the window's own
    /// sheet: on Files (it moves there), Quit the primary, Keep cleaning
    /// the safe answer with the keyboard, the call in the same sentence.
    #[test]
    fn quit_while_files_clean_is_the_one_quit_sheet() {
        let size = egui::vec2(960.0, 640.0);
        let mut w = testing::at_60hz(cleaning(true), Page::Live, size);
        w.harness.run_steps(3);
        assert_eq!(*w.page.borrow(), Page::Files);
        let d = testing::by_role(&w.harness, "Quit HushMic?", testing::Role::AlertDialog);
        let s = crate::appstate::mock::state(Scenario::OnInCall);
        assert_eq!(
            d.accesskit_node().description(),
            Some(format!(
                "{} Quitting removes the HushMic microphone from it. Your call will hear \
                 your microphone directly, with the noise. 2 files are still \
                 cleaning. 1 cleaned file isn’t saved. Files that aren’t saved yet are \
                 cleaned again when you add them next time. Quit closes HushMic \
                 completely.",
                crate::pages::live::apps_line(&s)
            ))
        );
        assert!(w.harness.get_by_label("Keep cleaning").is_focused());
        // Escape keeps cleaning, and the window stays
        assert!(!press(&mut w, Modifiers::NONE, Key::Escape));
        assert_eq!(w.link.sent(), vec![Action::FilesQuit(false)]);
        // Quit stops them and quits
        let mut w = testing::at_60hz(cleaning(true), Page::Files, size);
        w.harness.run_steps(3);
        w.harness.get_by_label("Quit").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::FilesQuit(true)]);
        // F3: or saves them all first, then quits
        let mut w = testing::at_60hz(cleaning(true), Page::Files, size);
        w.harness.run_steps(3);
        w.harness.get_by_label("Save all and quit").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::FilesSaveAndQuit]);
    }

    #[test]
    fn a_question_whose_files_finished_asks_about_quitting_alone() {
        use crate::appstate::{FileJob, JobState};
        let mut s = crate::appstate::mock::state(Scenario::OnNoApp);
        s.files.jobs = vec![FileJob {
            id: 1,
            path: "/nowhere/0.wav".into(),
            state: JobState::Done,
            saved: Some("/nowhere/0 (cleaned).wav".into()),
            ..FileJob::default()
        }];
        s.files.ask_quit = true;
        let mut w = testing::at_60hz(s, Page::Files, egui::vec2(960.0, 640.0));
        w.harness.run_steps(3);
        let d = testing::by_role(&w.harness, "Quit HushMic?", testing::Role::AlertDialog);
        assert_eq!(
            d.accesskit_node().description().as_deref(),
            Some(
                "Files that aren’t saved yet are cleaned again when you add them next \
                 time. Quit closes HushMic completely."
            )
        );
        // nothing cleans any more: the safe answer is Cancel, and nothing
        // is left to save
        assert!(w.harness.query_by_label("Keep cleaning").is_none());
        assert!(w.harness.query_by_label("Save all and quit").is_none());
        assert!(w.harness.get_by_label("Cancel").is_focused());
        w.harness.get_by_label("Quit").click();
        w.harness.run_steps(2);
        assert!(w.link.sent().contains(&Action::FilesQuit(true)));
    }

    /// Quit with cleaned files no Save wrote asks too (A1): they would be
    /// cleaned again. Next to one still cleaning, both are named.
    #[test]
    fn quit_with_cleaned_files_not_saved_asks_first() {
        use crate::appstate::{FileJob, JobState};
        let job = |id: u64, state: JobState, saved: bool| FileJob {
            id,
            path: format!("/nowhere/{id}.wav"),
            state,
            saved: saved.then(|| format!("/nowhere/{id} (cleaned).wav")),
            ..FileJob::default()
        };
        let mut s = crate::appstate::mock::state(Scenario::OnNoApp);
        s.files.jobs = vec![
            job(1, JobState::Done, false),
            job(2, JobState::Done, false),
            job(3, JobState::Done, true),
        ];
        let size = egui::vec2(960.0, 640.0);
        // Ctrl+Q goes to the daemon, which asks
        let mut w = testing::at_60hz(s.clone(), Page::Live, size);
        w.harness.run_steps(2);
        assert!(!press(&mut w, Modifiers::COMMAND, Key::Q));
        assert_eq!(w.link.sent(), vec![Action::FilesAskQuit]);
        s.files.ask_quit = true;
        let mut w = testing::at_60hz(s.clone(), Page::Files, size);
        w.harness.run_steps(3);
        let d = testing::by_role(&w.harness, "Quit HushMic?", testing::Role::AlertDialog);
        assert_eq!(
            d.accesskit_node().description().as_deref(),
            Some(
                "2 cleaned files aren’t saved. Files that aren’t saved yet are cleaned \
                 again when you add them next time. Quit closes HushMic completely."
            )
        );
        // nothing cleans: the safe answer is Cancel
        assert!(w.harness.get_by_label("Cancel").is_focused());
        s.files.jobs.push(job(4, JobState::Cleaning, false));
        let mut w = testing::at_60hz(s, Page::Files, size);
        w.harness.run_steps(3);
        let d = testing::by_role(&w.harness, "Quit HushMic?", testing::Role::AlertDialog);
        assert_eq!(
            d.accesskit_node().description().as_deref(),
            Some(
                "1 file is still cleaning. 2 cleaned files aren’t saved. Files that \
                 aren’t saved yet are cleaned again when you add them next time. Quit \
                 closes HushMic completely."
            )
        );
        assert!(w.harness.get_by_label("Keep cleaning").is_focused());
    }

    /// The sheet sits over the page column, not the window: its centre is
    /// the column's (right of the rail).
    #[test]
    fn the_quit_sheet_is_centred_on_the_page_column() {
        for (size, rail) in [
            (egui::vec2(960.0, 640.0), 184.0),
            (egui::vec2(720.0, 520.0), 60.0),
        ] {
            let mut w = testing::at_60hz(cleaning(true), Page::Files, size);
            w.harness.run_steps(3);
            let id = egui::Id::new("hm-quit-ask").with("area");
            let sheet = w
                .harness
                .ctx
                .memory(|m| m.area_rect(id))
                .expect("the sheet");
            let cx = sheet.center().x;
            let want = rail + (size.x - rail) / 2.0;
            assert!((cx - want).abs() < 2.0, "{size:?}: {cx} vs {want}");
        }
    }
}
