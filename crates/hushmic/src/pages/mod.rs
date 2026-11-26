//! The window's pages: Live (with its last 10 seconds), Speakers, Files, Shortcuts,
//! Settings (with About) and First run, drawn with the design system in [`crate::ui`] from the
//! daemon's [`AppState`], asking for changes with [`Action`]s through the
//! window's [`Link`].
//!
//! The pages never decide what HushMic does: they draw the newest snapshot
//! and send what the user asked for; the daemon's next snapshot is the
//! answer. Their own state is only what the window shows (the page's
//! scroll, a menu that is open, the Live picture's columns, the replay
//! backend while Live is visible).
//!
//! Frames: a page that does not move asks for none. The Live picture asks
//! for 20 Hz while the window shows it and the microphone moves, the
//! moments for every display frame while they run, a replay while it
//! plays.

pub mod files;
pub mod firstrun;
mod frame;
pub(crate) mod hold;
mod keys;
mod launch;
pub mod live;
pub mod look;
pub mod replay;
pub mod scenes;
pub mod settings;
pub mod shortcuts;
#[cfg(feature = "speakers")]
pub mod speakers;
mod status;
mod util;

use eframe::egui;

use crate::appstate::{Action, AppState, Page};
use crate::ui::paint::light::LightLayer;
use crate::window::Link;

pub use status::{status, Status};

/// Where the Live picture's columns come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Feed {
    /// The running chain's engine meter (the daemon's window).
    #[default]
    Meter,
    /// The boards' keyboard clip, looped while an app uses HushMic (a mock
    /// window; screenshots prefill it).
    Demo,
}

/// What the pages share in one frame.
pub struct Cx<'a> {
    pub link: &'a Link,
    pub state: &'a AppState,
    /// egui's input time, seconds.
    pub now: f64,
    /// The window is at its compact size (icon rail, stacked cards).
    pub compact: bool,
    /// Over the whole window (lit edge, beam).
    pub light: &'a mut LightLayer,
    /// Set to move to another page.
    pub goto: Option<Page>,
    /// Set to quit HushMic (asked first while an app uses it).
    pub quit: bool,
    /// Set to turn HushMic off (asked first while an app uses it).
    pub turn_off: bool,
    pub diag: &'a mut util::Diagnostics,
    /// `UiBeamUsed` went out from this window (sent once, not on every
    /// frame until the snapshot catches up).
    pub beam_reported: &'a mut bool,
}

impl Cx<'_> {
    pub fn send(&self, action: Action) {
        self.link.send(action);
    }

    /// The first-start beam fell (or was used up): tell the daemon once.
    pub fn beam_used(&mut self) {
        if !self.state.ui.beam_used && !*self.beam_reported {
            *self.beam_reported = true;
            self.link.send(Action::UiBeamUsed);
        }
    }
}

/// The window's page layer.
pub struct Pages {
    pub(crate) live: live::LivePage,
    pub(crate) files: files::FilesPage,
    #[cfg(feature = "speakers")]
    speakers: speakers::SpeakersPage,
    settings: settings::SettingsPage,
    first_run: firstrun::FirstRunPage,
    frame: frame::Frame,
    look: look::Look,
    diag: util::Diagnostics,
    launch: launch::Launch,
    keys: keys::Keys,
    /// Minimised: the keyboard focus to give back on the restore.
    refocus: Option<Option<egui::Id>>,
    /// The daemon's `engine.recovered` last seen (a change is "Running
    /// again.").
    recovered: Option<u64>,
    /// The daemon asked whether to stop the files and quit (last frame).
    ask_quit: bool,
    shown: Option<Page>,
    beam_reported: bool,
}

impl Pages {
    /// Install the design system into `ctx` and build the pages.
    pub fn new(ctx: &egui::Context, feed: Feed) -> Pages {
        crate::ui::install(ctx, crate::ui::Theme::Dark);
        Pages::with_kit(feed)
    }

    /// The pages, for a context the design system is already installed in.
    pub fn with_kit(feed: Feed) -> Pages {
        Pages {
            live: live::LivePage::new(feed),
            files: files::FilesPage::new(feed),
            #[cfg(feature = "speakers")]
            speakers: speakers::SpeakersPage::default(),
            settings: settings::SettingsPage::default(),
            first_run: firstrun::FirstRunPage::new(feed),
            frame: frame::Frame::default(),
            look: look::Look::default(),
            diag: util::Diagnostics::default(),
            launch: launch::Launch::default(),
            keys: keys::Keys::default(),
            refocus: None,
            recovered: None,
            ask_quit: false,
            shown: None,
            beam_reported: false,
        }
    }

    /// Screenshots and tests: the Live picture starts with these columns
    /// (the boards' `--ticks`), and its last 10 seconds with a take of them.
    pub fn prefill(&mut self, cols: usize) {
        self.live.prefill(cols);
        self.first_run.prefill(cols);
    }

    /// Screenshots: Settings opens on its Files section.
    pub fn settings_on_files(&mut self) {
        self.settings.scroll_to_files();
    }

    /// Screenshots: Quit HushMic was chosen with the pointer.
    pub fn ask_quit(&mut self, link: &Link) {
        if let Some(s) = link.state() {
            self.keys.quit(&s, link, false);
        }
    }

    /// Screenshots: the rail's More menu open.
    pub fn open_more(&mut self) {
        self.frame.more_open = true;
    }

    /// Screenshots: no launch layer (a board state, not a launch).
    pub fn no_launch(&mut self) {
        self.launch.skip();
    }

    /// Screenshots and tests: draw in this theme whatever Appearance says.
    pub fn pin_theme(&mut self, theme: crate::ui::Theme) {
        self.look.pinned = Some(theme);
    }

    /// Screenshots: diagnostics come from this function instead of probing
    /// PipeWire.
    pub fn set_diagnostics(&mut self, collect: fn() -> String) {
        self.diag = util::Diagnostics::with(collect);
    }

    /// Draw one frame of the window. `page` is the shell's routing: the
    /// pages move it (the rail, Done in first run) and the shell tells the
    /// daemon.
    pub fn show(&mut self, ctx: &egui::Context, link: &Link, page: &mut Page) {
        crate::ui::begin_frame(ctx);
        let Some(state) = link.state() else {
            // No word from the daemon yet: a plain window (the shell held
            // the first frame as long as it could).
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(crate::ui::kit(ctx).pal.bg))
                .show(ctx, |_| {});
            return;
        };
        self.look.apply(ctx, &state);
        let mut want = route(*page, &state);
        // Quit in the tray while files clean: the question is on Files
        let ask = state.files.ask_quit;
        if ask && !self.ask_quit {
            want = Page::Files;
        }
        self.ask_quit = ask;
        if want == Page::About {
            self.settings.scroll_to_about();
            want = Page::Settings;
        }
        if want == Page::Shortcuts {
            self.settings.scroll_to_shortcuts();
            want = Page::Settings;
        }
        if self.shown != Some(want) {
            self.leave(self.shown, want, link);
            self.shown = Some(want);
        }
        *page = want;
        // "Running again." on Live when the daemon recovered while the
        // window shows Live (elsewhere the daemon notifies instead)
        let recovered = state.engine.recovered;
        if self.recovered.is_some_and(|r| r != recovered) && want == Page::Live {
            self.live.running_again(ctx.input(|i| i.time));
        }
        self.recovered = Some(recovered);
        if ctx.input(|i| i.viewport().minimized == Some(true)) {
            // Minimised (X11 says so; on Wayland the frames stop by
            // themselves): nothing shows, so nothing paints, reads the
            // meter or listens to the microphone; Live keeps its replay's
            // result. A look a second: a window manager may map the window
            // again before it clears the minimised state, and then no event
            // says so.
            if self.refocus.is_none() {
                self.refocus = Some(ctx.memory(|m| m.focused()));
            }
            self.live.pause();
            #[cfg(feature = "speakers")]
            self.speakers.hide(link);
            self.first_run.let_go();
            self.files.background(&state, link, ctx.input(|i| i.time));
            crate::ui::motion::repaint_in(ctx, std::time::Duration::from_secs(1));
            return;
        }
        if let Some(Some(id)) = self.refocus.take() {
            // the keyboard where it was before the minimise
            ctx.memory_mut(|m| m.request_focus(id));
        }
        self.keys.read(ctx, &state, link);
        let now = ctx.input(|i| i.time);
        let full = ctx.content_rect();
        self.launch.open(&state, now, want == Page::FirstRun);
        if self.launch.covers() {
            // Launch B: nothing under the layer yet
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(crate::ui::kit(ctx).pal.bg))
                .show(ctx, |_| {});
            self.launch.ui(ctx, &state, now, full);
            self.keys.ui(ctx, &state, link);
            crate::ui::end_frame(ctx);
            return;
        }
        let compact = full.width() < crate::ui::tokens::window::COMPACT_BELOW;
        let mut light = LightLayer::new();
        let mut cx = Cx {
            link,
            state: &state,
            now,
            compact,
            light: &mut light,
            goto: None,
            quit: false,
            turn_off: false,
            diag: &mut self.diag,
            beam_reported: &mut self.beam_reported,
        };
        cx.diag.poll(ctx);
        let pal = crate::ui::kit(ctx).pal;
        let Pages {
            live,
            files,
            #[cfg(feature = "speakers")]
            speakers,
            settings,
            first_run,
            frame,
            ..
        } = self;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal.bg))
            .show(ctx, |ui| {
                if want == Page::FirstRun {
                    first_run.ui(ui, full, &mut cx);
                } else {
                    frame.ui(ui, full, want, &mut cx, |ui, cx| match want {
                        Page::Files => files.ui(ui, cx),
                        #[cfg(feature = "speakers")]
                        Page::Speakers => speakers.ui(ui, cx),
                        Page::Settings | Page::About | Page::Shortcuts => settings.ui(ui, cx),
                        _ => live.ui(ui, cx),
                    });
                }
            });
        // Pages that keep running while hidden (none draw then) learn it
        // here; the Live picture keeps its columns while on another page.
        if want != Page::Live {
            live.background();
        }
        let goto = cx.goto.take();
        let (quit, turn_off) = (cx.quit, cx.turn_off);
        light.paint(ctx, full);
        #[cfg(test)]
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("hm-light-last"), light));
        self.launch.ui(ctx, &state, now, full);
        // from the keyboard when it was moving through the menu
        let keyboard = crate::ui::kit(ctx).focus_visible;
        if quit {
            self.keys.quit(&state, link, keyboard);
        }
        if turn_off {
            self.keys.turn_off(&state, link, keyboard);
        }
        self.keys.ui(ctx, &state, link);
        crate::ui::end_frame(ctx);
        if let Some(p) = goto {
            *page = p;
            ctx.request_repaint();
        }
    }

    /// A page was left: Live lets go of the microphone, first run drops
    /// its level probe, Speakers ends a hold.
    fn leave(
        &mut self,
        from: Option<Page>,
        to: Page,
        #[cfg_attr(not(feature = "speakers"), allow(unused_variables))] link: &Link,
    ) {
        #[cfg(feature = "speakers")]
        if from == Some(Page::Speakers) && to != Page::Speakers {
            self.speakers.hide(link);
        }
        if from == Some(Page::Live) && to != Page::Live {
            self.live.hide();
        }
        if from == Some(Page::FirstRun) && to != Page::FirstRun {
            self.first_run.hide();
        }
        if from == Some(Page::Files) && to != Page::Files {
            self.files.hide();
        }
    }
}

impl crate::window::Layer for Pages {
    fn ui(&mut self, ctx: &egui::Context, link: &Link, nav: &mut crate::window::Nav) {
        self.show(ctx, link, &mut nav.page);
    }
}

/// The shell's [`crate::window::MakeLayer`]: the pages, on the engine's
/// meter, or on the boards' clip for a mock window.
pub fn layer(
    cc: &eframe::CreationContext<'_>,
    opts: &crate::window::Options,
) -> Box<dyn crate::window::Layer> {
    let feed = if opts.mock.is_some() {
        Feed::Demo
    } else {
        Feed::Meter
    };
    let mut pages = Pages::new(&cc.egui_ctx, feed);
    pages.look.start(
        &cc.egui_ctx,
        opts.theme.unwrap_or_default(),
        opts.reduced_motion,
    );
    // a software renderer draws the moving parts at 10 Hz (PLAN §1.4)
    crate::ui::set_software(
        &cc.egui_ctx,
        crate::ui::renderer_is_software(cc.gl.as_deref()),
    );
    Box::new(pages)
}

/// The page to draw for the shell's `page`: first run only while it is
/// under way, and the daemon's first-run step wins over a remembered page.
fn route(page: Page, s: &AppState) -> Page {
    match (page, s.ui.first_run_step) {
        (Page::FirstRun, None) => Page::Live,
        (p, _) => p,
    }
}

/// Shared by the page tests.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::appstate::mock::{self, Scenario};
    use egui_kittest::Harness;
    use std::cell::RefCell;
    use std::rc::Rc;

    pub struct Window {
        pub harness: Harness<'static>,
        pub link: Rc<Link>,
        pub page: Rc<RefCell<Page>>,
    }

    impl Window {
        /// What the light layer drew over the window in the last frame.
        pub fn light(&self) -> LightLayer {
            self.harness
                .ctx
                .data(|d| d.get_temp(egui::Id::new("hm-light-last")))
                .unwrap_or_default()
        }
    }

    /// A window on a fixed state that records what it sends.
    pub fn window(state: AppState, page: Page, size: egui::Vec2) -> Window {
        with_link(Link::recording(state), page, size)
    }

    /// A window on a mock daemon: what it sends changes the state.
    pub fn mock_window(state: AppState, page: Page, size: egui::Vec2) -> Window {
        with_link(Link::mock(state, || {}), page, size)
    }

    pub fn with_link(link: Link, page: Page, size: egui::Vec2) -> Window {
        with_setup(link, page, size, |_| {})
    }

    /// A window whose pages `setup` prepares first (a scene's preset).
    pub fn with_setup(
        link: Link,
        page: Page,
        size: egui::Vec2,
        setup: impl Fn(&mut Pages) + 'static,
    ) -> Window {
        build(link, page, size, setup, 0.25)
    }

    /// A window on 60 Hz frames (kittest's default frame is 250 ms):
    /// for timings shorter than that.
    pub fn at_60hz(state: AppState, page: Page, size: egui::Vec2) -> Window {
        build(Link::recording(state), page, size, |_| {}, 1.0 / 60.0)
    }

    /// [`with_setup`] on 60 Hz frames.
    pub fn at_60hz_with(
        link: Link,
        page: Page,
        size: egui::Vec2,
        setup: impl Fn(&mut Pages) + 'static,
    ) -> Window {
        build(link, page, size, setup, 1.0 / 60.0)
    }

    fn build(
        link: Link,
        page: Page,
        size: egui::Vec2,
        setup: impl Fn(&mut Pages) + 'static,
        step_dt: f32,
    ) -> Window {
        let link = Rc::new(link);
        let page = Rc::new(RefCell::new(page));
        let l = link.clone();
        let harness = harness(move || l.clone(), page.clone(), size, setup, step_dt);
        Window {
            harness,
            link,
            page,
        }
    }

    /// A window on `links[at]`: moving `at` changes the state under the
    /// running window (a microphone unplugged, an engine restarting).
    pub struct Switched {
        pub harness: Harness<'static>,
        pub links: Rc<Vec<Rc<Link>>>,
        pub at: Rc<std::cell::Cell<usize>>,
    }

    /// Recording windows on `states`, the first showing; 60 Hz frames.
    pub fn switched(states: Vec<AppState>, page: Page, size: egui::Vec2) -> Switched {
        switched_with(states, page, size, |_| {})
    }

    /// [`switched`], with the pages set up first (a page's source).
    pub fn switched_with(
        states: Vec<AppState>,
        page: Page,
        size: egui::Vec2,
        setup: impl Fn(&mut Pages) + 'static,
    ) -> Switched {
        let links: Rc<Vec<Rc<Link>>> = Rc::new(
            states
                .into_iter()
                .map(|s| Rc::new(Link::recording(s)))
                .collect(),
        );
        let at = Rc::new(std::cell::Cell::new(0));
        let (l, a) = (links.clone(), at.clone());
        let page = Rc::new(RefCell::new(page));
        let harness = harness(move || l[a.get()].clone(), page, size, setup, 1.0 / 60.0);
        Switched { harness, links, at }
    }

    fn harness(
        link: impl Fn() -> Rc<Link> + 'static,
        page: Rc<RefCell<Page>>,
        size: egui::Vec2,
        setup: impl Fn(&mut Pages) + 'static,
        step_dt: f32,
    ) -> Harness<'static> {
        crate::i18n::pin_english();
        let mut pages: Option<Pages> = None;
        let mut harness = Harness::builder()
            .with_size(size)
            .with_step_dt(step_dt)
            .build(move |ctx| {
                // the fonts land on the frame after they are installed
                let Some(pages) = pages.as_mut() else {
                    let mut ps = Pages::new(ctx, Feed::Demo);
                    ps.set_diagnostics(|| "diagnostics".into());
                    setup(&mut ps);
                    pages = Some(ps);
                    ctx.request_repaint();
                    return;
                };
                let mut pg = page.borrow_mut();
                pages.show(ctx, &link(), &mut pg);
            });
        harness.run_steps(3);
        harness
    }

    pub use egui::accesskit::Role;

    /// The node named `label` with `role` (a text and a control may share
    /// words: the picture's name and the title, a link and a switch).
    pub fn by_role<'a>(
        h: &'a Harness<'static>,
        label: &'a str,
        role: Role,
    ) -> egui_kittest::Node<'a> {
        use egui_kittest::kittest::Queryable;
        h.query_by_role_and_label(role, label)
            .unwrap_or_else(|| panic!("no {role:?} named {label}"))
    }

    pub fn scenario(sc: Scenario, page: Page) -> Window {
        window(mock::state(sc), page, egui::vec2(960.0, 640.0))
    }

    pub fn compact(sc: Scenario, page: Page) -> Window {
        window(mock::state(sc), page, egui::vec2(720.0, 520.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock::{self, Scenario};
    use std::time::Duration;

    /// One frame of `pages` on `page`, minimised or not; the delay it asks
    /// the next frame after.
    fn frame(
        ctx: &egui::Context,
        pages: &mut Pages,
        link: &Link,
        page: &mut Page,
        minimised: bool,
    ) -> Duration {
        frame_at(ctx, pages, link, page, minimised, None)
    }

    fn frame_at(
        ctx: &egui::Context,
        pages: &mut Pages,
        link: &Link,
        page: &mut Page,
        minimised: bool,
        time: Option<f64>,
    ) -> Duration {
        let mut input = egui::RawInput {
            time,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(960.0, 640.0),
            )),
            ..Default::default()
        };
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .minimized = Some(minimised);
        let out = ctx.run(input, |ctx| pages.show(ctx, link, page));
        out.viewport_output[&egui::ViewportId::ROOT].repaint_delay
    }

    #[test]
    fn a_minimised_window_rests_and_lets_go_of_the_microphone() {
        crate::i18n::pin_english();
        for start in [Page::Live, Page::Files] {
            let ctx = egui::Context::default();
            let mut pages = Pages::new(&ctx, Feed::Demo);
            pages.no_launch();
            let link = Link::recording(mock::state(Scenario::OnInCall));
            let mut page = start;
            // (another page that does not move asks for no frames)
            let live = start == Page::Live;
            for _ in 0..3 {
                let d = frame(&ctx, &mut pages, &link, &mut page, false);
                assert!(!live || d < Duration::from_millis(100), "{start:?}: {d:?}");
            }
            assert_eq!(pages.live.replay.listening(), live);
            // minimised: a look a second only, the microphone let go
            for _ in 0..2 {
                let d = frame(&ctx, &mut pages, &link, &mut page, true);
                assert!(
                    d >= Duration::from_millis(900) && d <= Duration::from_millis(1100),
                    "{start:?}: {d:?}"
                );
            }
            assert!(!pages.live.replay.listening());
            // restored: the page runs again
            frame(&ctx, &mut pages, &link, &mut page, false);
            let d = frame(&ctx, &mut pages, &link, &mut page, false);
            assert!(!live || d < Duration::from_millis(100), "{start:?}: {d:?}");
            assert_eq!(pages.live.replay.listening(), live);
        }
    }

    #[test]
    fn leaving_live_lets_go_of_the_microphone() {
        crate::i18n::pin_english();
        let ctx = egui::Context::default();
        let mut pages = Pages::new(&ctx, Feed::Demo);
        pages.no_launch();
        let link = Link::recording(mock::state(Scenario::OnNoApp));
        let mut page = Page::Live;
        frame(&ctx, &mut pages, &link, &mut page, false);
        // no app uses HushMic: Live listens itself all the same
        assert!(pages.live.replay.listening());
        page = Page::Files;
        frame(&ctx, &mut pages, &link, &mut page, false);
        assert!(!pages.live.replay.listening());
        page = Page::Live;
        frame(&ctx, &mut pages, &link, &mut page, false);
        assert!(pages.live.replay.listening());
    }

    #[test]
    fn live_scrolls_with_sound_and_stands_still_without() {
        crate::i18n::pin_english();
        for silent in [false, true] {
            let ctx = egui::Context::default();
            let mut pages = Pages::new(&ctx, Feed::Demo);
            pages.no_launch();
            if silent {
                pages.live.silent_demo();
            }
            let link = Link::recording(mock::state(Scenario::OnInCall));
            let mut page = Page::Live;
            let mut d = Duration::ZERO;
            for i in 0..60 {
                let t = 10.0 + i as f64 * 0.05;
                d = frame_at(&ctx, &mut pages, &link, &mut page, false, Some(t));
            }
            // 3 s in: with sound the picture scrolls, a frame for every
            // one the display shows; with none it has nothing to move
            assert_eq!(pages.live.moving(), !silent);
            assert!(silent || d < Duration::from_millis(100), "{d:?}");
        }
    }

    #[test]
    fn a_restore_gives_the_keyboard_back() {
        use egui_kittest::kittest::Queryable;
        let mut w = testing::scenario(Scenario::OnInCall, Page::Settings);
        w.harness.get_by_label("Notifications").focus();
        w.harness.run_steps(2);
        let minimise = |w: &mut testing::Window, on: bool| {
            w.harness
                .input_mut()
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .minimized = Some(on);
            w.harness.run_steps(2);
        };
        minimise(&mut w, true);
        minimise(&mut w, false);
        assert!(w.harness.get_by_label("Notifications").is_focused());
    }
}
