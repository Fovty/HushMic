//! Live: what HushMic does now, in one sentence with Mute beside it; the
//! picture (two spectrograms on one time axis, the microphone above and
//! what apps hear below, the last 10 seconds:
//! [`crate::ui::paint::spectro`]), where the microphone's caption is its
//! choice and ends in its level; under it one row of controls: noise
//! suppression (Off, or one of its four strengths), and the last 10
//! seconds' two buttons ([`super::replay`]).
//! What needs attention shows as one notice at the card's top, under the
//! headline, and only then. The model is chosen in Settings.
//!
//! While Live shows, the window records HushMic itself (the replay's
//! backend), so the engine runs and the picture moves with no app; leaving
//! the page or minimising the window lets go of the microphone, and the
//! picture goes with the audio it was made from.
//!
//! The card reaches from the header to the page's bottom, and the two
//! lanes share the height the rest leaves (they have a least).
//! No control ever moves: the header keeps one height in every state,
//! and a notice, or a line under the row of controls, takes its room
//! from the lanes.

use std::collections::VecDeque;
use std::time::Instant;

use eframe::egui::{self, pos2, vec2, Color32, Rect, Ui};

use super::replay::{self, LevelShow, Replay};
use super::status::{stopped, suppressing};
use super::util::{self, child, rect, text_at};
use super::{Cx, Feed};
use crate::appstate::{Action, AppState, Mode, Model, Phase, Strength};
use crate::diagnostics::EngineTier;
use crate::meter::{EngineMeter, EngineMode, MeterColumn, MeterState};
use crate::miccheck::level::LevelCheck;
use crate::miccheck::take::ColumnStats;
use crate::miccheck::types::{Blocker, Phase as CheckPhase, View};
use crate::miccheck::Engine;
use crate::tr;
use crate::ui::fonts::Type;
use crate::ui::icons::Icon;
use crate::ui::paint::live::LiveState;
use crate::ui::paint::spectro::{self, Lane, Playing, Show, Sizes, Spectro};
use crate::ui::text::{self, TextBlock};
use crate::ui::tokens::Palette;
use crate::ui::widgets::{self, Button, SelectItem};

/// The page's padding: top, sides, bottom.
pub(super) const PAD: (f32, f32, f32) = (28.0, 32.0, 32.0);
pub(super) const PAD_COMPACT: (f32, f32, f32) = (20.0, 24.0, 24.0);
pub(super) const CARD_PAD: f32 = 16.0;
#[cfg(feature = "speakers")]
pub(super) const CARD_GAP: f32 = 10.0;
/// Switches are centred in their rows (the boards' 2.5 px lift, from the
/// DS switch's 29 px inline box, was an accident): the track's top is the
/// row's centre minus half the 24 px track. Rows keep the 29 px box.
pub const SWITCH_HALF: f32 = 12.0;
/// Exact digital silence for this many columns (3 s) is a silent mic.
const SILENT_COLS: usize = 60;
/// How often Live reads the meter while the picture stands still: while
/// audio flows, and while an app holds its stream without sending.
const REST_LOOK: std::time::Duration = std::time::Duration::from_millis(250);
const IDLE_LOOK: std::time::Duration = std::time::Duration::from_secs(1);

/// A notice's links sit this far apart in their row.
const FIX_GAP: f32 = 16.0;

/// "Running again." stays this long in the header after a recovery.
const RUNNING_AGAIN_SECS: f64 = 4.0;

/// How long after the header's controls swap a click on the new ones does
/// nothing (UX panel round 2, A14): longer than a double-click.
pub(super) const SWAP_SETTLE: f64 = 0.5;

pub struct LivePage {
    feed: Feed,
    /// The picture: the two lanes of the last 10 seconds.
    spectro: Spectro,
    meter: EngineMeter,
    cols: Vec<MeterColumn>,
    /// An app was using HushMic at the last meter look (its rising edge
    /// judges the level from the use mark on).
    in_use_seen: bool,
    /// The meter counts as flowing until then (a second past its last
    /// column).
    flowing_until: f64,
    /// Tests: how many columns of the meter's past the level last took.
    #[cfg(test)]
    history_taken: usize,
    /// Tests: the picture asked for the next frame.
    #[cfg(test)]
    moving: bool,
    /// Recent input peaks, newest last (silence detection).
    recent: VecDeque<f32>,
    /// The level judged from the engine meter's columns, the recent past
    /// included (UX panel B1): it speaks as soon as Live opens, while the
    /// replay's own take is still forming.
    meter_level: LevelCheck,
    /// The header says "Running again." until then (appux §9).
    running_again_until: f64,
    /// The notification problem the page last told the daemon it shows
    /// (`ui shows`); None until told.
    told: Option<Option<crate::notify::Problem>>,
    select_id: Option<egui::Id>,
    /// Turn on (this id) was pressed while it had the focus: Mute, which
    /// replaces it, takes the focus when it shows, unless something else
    /// took it meanwhile.
    focus_mute: Option<egui::Id>,
    /// Whether the header showed Turn on (Off) last frame, and when its
    /// controls last swapped (UX panel round 2, A14).
    header_off: Option<bool>,
    swapped_at: Option<f64>,
    /// The window shows the page (false while another page is up: the
    /// picture starts empty when the page comes back).
    shown: bool,
    silent_demo: bool,
    /// The microphone's list is drawn open (screenshots).
    pick_open: bool,
    /// The last 10 seconds: the replay row and the window's own capture.
    pub(crate) replay: Replay,
}

impl LivePage {
    pub fn new(feed: Feed) -> LivePage {
        LivePage {
            feed,
            spectro: Spectro::new(),
            meter: EngineMeter::new(),
            cols: Vec::new(),
            in_use_seen: false,
            flowing_until: f64::NEG_INFINITY,
            #[cfg(test)]
            history_taken: 0,
            #[cfg(test)]
            moving: false,
            recent: VecDeque::new(),
            meter_level: LevelCheck::new(),
            running_again_until: f64::NEG_INFINITY,
            told: None,
            select_id: None,
            focus_mute: None,
            header_off: None,
            swapped_at: None,
            shown: false,
            silent_demo: false,
            pick_open: false,
            replay: Replay::new(feed),
        }
    }

    /// Screenshots and tests: the last 10 seconds start with a take of
    /// this many timeline columns, and stay as they are.
    pub fn prefill(&mut self, cols: usize) {
        self.replay.prefill(cols);
    }

    /// The microphone's list is drawn open (screenshots).
    pub fn open_pick(&mut self) {
        self.pick_open = true;
    }

    /// The demo feed sends silence ("Mic silent").
    pub fn silent_demo(&mut self) {
        self.silent_demo = true;
        self.replay.silent();
    }

    /// HushMic is running again after a failure (the daemon's
    /// `engine.recovered` moved while the page is up): the header says so
    /// for 4 s.
    pub fn running_again(&mut self, now: f64) {
        self.running_again_until = now + RUNNING_AGAIN_SECS;
    }

    /// The header's words now: the state's, or "Running again." for a few
    /// seconds after a recovery.
    fn head_now(&self, ctx: &egui::Context, s: &AppState, now: f64) -> String {
        let silent = self.silent(s);
        // only in place of the filtering header: muted, in bypass or
        // unfiltered for now, the header keeps saying what apps hear,
        // and a degraded state its own words
        if super::status::filtering(s)
            && degraded_title(s, silent).is_none()
            && now < self.running_again_until
        {
            crate::ui::motion::repaint_in(
                ctx,
                std::time::Duration::from_secs_f64(self.running_again_until - now),
            );
            return tr!("live-title-running-again");
        }
        head_for(s, silent)
    }

    /// The window is minimised: nothing draws, and the replay lets go of
    /// the microphone (it keeps its result).
    pub fn pause(&mut self) {
        self.background();
        self.replay.pause();
    }

    /// Another page took over: the replay lets go of the microphone and
    /// goes.
    pub fn hide(&mut self) {
        self.background();
        self.replay.hide();
    }

    /// Tests: what the picture holds (the columns read, and whether they
    /// are a frozen take's), or None while it holds nothing.
    #[cfg(test)]
    pub(crate) fn picture_state(&self) -> Option<(u64, bool)> {
        (!self.spectro.forgotten()).then(|| self.spectro.state())
    }

    /// Tests: the picture scrolls (it asked for the next frame).
    #[cfg(test)]
    pub(crate) fn moving(&self) -> bool {
        self.moving
    }

    /// Another page is up: the picture and what the meter told go.
    pub fn background(&mut self) {
        if self.shown {
            self.shown = false;
            self.spectro.forget();
            self.meter.set_path(None);
            self.recent.clear();
            self.meter_level.reset();
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let s = cx.state;
        // HushMic can't work (PipeWire unreachable, the chain down): the
        // page stays as it is, the picture empty and the problem in the
        // notice; the replay backend keeps the microphone closed while
        // the engine is off or PipeWire is gone
        let opened = !self.shown;
        self.shown = true;
        let ctx = ui.ctx().clone();
        // the replay's keys and view first: page Space is read before any
        // control sees it
        let v = self.replay.begin(&ctx, cx, ui.layer_id());
        let look = self.read_meter(cx, opened);
        if matches!(self.feed, Feed::Demo) {
            // the demo's ten seconds go on with the frames
            crate::ui::motion::live_repaint(&ctx);
        }
        // the window's lit top edge while HushMic filters, as on every page
        if super::status::filtering(s) {
            cx.light.static_edge = true;
        }
        let silent = self.silent(s);
        let (pt, _, pb) = if cx.compact { PAD_COMPACT } else { PAD };
        let page = ui.max_rect();
        let (origin, width) = (page.min, page.width());
        let (x, w) = util::column(page, cx.compact);
        let mut y = origin.y + pt;

        y = self.header(ui, cx, x, y, w) + util::HEAD_GAP;
        // the card reaches down to the page's padding where its lanes can
        y = self.figure(ui, cx, &v, x, y, w, page.bottom() - pb, silent, look);
        util::reach(ui, origin, width, y + pb);
    }

    // ---------------------------------------------------------------- the state

    fn live_state(s: &AppState) -> LiveState {
        if s.mode == Mode::Off || stopped(s) {
            return LiveState::Off;
        }
        match s.engine.phase {
            Phase::Starting | Phase::Off | Phase::Down => return LiveState::Starting,
            Phase::Reconnecting => return LiveState::Reconnecting,
            Phase::Ready => {}
        }
        match s.mode {
            Mode::Mute => LiveState::Muted,
            Mode::Bypass => LiveState::Bypass,
            _ if s.engine.tier == Some(EngineTier::Passthrough) => LiveState::Unfiltered,
            _ if light_for_now(s) => LiveState::Light,
            _ => LiveState::On,
        }
    }

    /// Read the engine meter for the level and for a silent microphone.
    /// `opened`: the page just came up. Returns when to read it next if
    /// no frame comes before (a picture that scrolls asks for every one):
    /// four looks a second while audio flows, one while an app holds its
    /// stream without sending; None with no meter to read.
    fn read_meter(&mut self, cx: &Cx<'_>, opened: bool) -> Option<std::time::Duration> {
        let s = cx.state;
        match self.feed {
            Feed::Demo => {
                if self.silent_demo && self.recent.len() < SILENT_COLS {
                    self.recent.resize(SILENT_COLS, 0.0);
                }
                None
            }
            Feed::Meter => {
                let path = s.engine.meter.as_deref().map(std::path::Path::new);
                if self.meter.path() != path {
                    self.meter.set_path(path);
                    self.recent.clear();
                    self.meter_level.reset();
                }
                self.cols.clear();
                let in_use = s.in_use();
                let began = in_use && !self.in_use_seen;
                self.in_use_seen = in_use;
                let st = if (opened || began) && in_use {
                    // opened mid-call, or an app just started using
                    // HushMic: the meter's recent past (it keeps 41 s)
                    // judges the level at once, but only from when an app
                    // began to hear it (the daemon's mark; none: nothing
                    // old); then the polls go on
                    let mark = s.engine.use_began_ms.unwrap_or(u64::MAX);
                    let st = self.meter.history_since(
                        Instant::now(),
                        HISTORY_COLS,
                        mark,
                        &mut self.cols,
                    );
                    #[cfg(test)]
                    {
                        self.history_taken = self.cols.len();
                    }
                    st
                } else {
                    self.meter.poll(Instant::now(), &mut self.cols)
                };
                if st == MeterState::Flowing || !self.cols.is_empty() {
                    self.flowing_until = cx.now + 1.0;
                }
                for c in &self.cols {
                    push_recent(&mut self.recent, c.input_peak);
                    judge_column(&mut self.meter_level, c);
                }
                match st {
                    MeterState::Unavailable => None,
                    _ if cx.now < self.flowing_until => Some(REST_LOOK),
                    _ => Some(IDLE_LOOK),
                }
            }
        }
    }

    /// The microphone sends exact digital silence (Live listens itself, so
    /// with or without an app).
    fn silent(&self, s: &AppState) -> bool {
        s.mode != Mode::Off
            && self.recent.len() >= SILENT_COLS
            && self.recent.iter().all(|v| *v <= 1e-7)
    }

    // ---------------------------------------------------------------- header

    /// The title, and Mute (or Turn on) at its line's end. Returns the
    /// header's bottom.
    fn header(&mut self, ui: &mut Ui, cx: &mut Cx<'_>, x: f32, y: f32, w: f32) -> f32 {
        let s = cx.state;
        let ctx = ui.ctx().clone();
        let off = s.mode == Mode::Off;
        let muted = s.mode == Mode::Mute;
        // pressed while muted, and then it says so: "Muted", seen and read
        // out (UX panel round 2, A7; "Unmute, pressed" read as a double
        // negative, B16); the strong tint shows it (B18). Both words take
        // one width, so the headline beside it never moves.
        let mute_label = tr!("live-mute");
        let muted_label = tr!("live-muted");
        let mute_room = Button::secondary(&mute_label)
            .sm()
            .icon(Icon::Mic)
            .width(&ctx)
            .max(
                Button::secondary(&muted_label)
                    .sm()
                    .icon(Icon::MicOff)
                    .width(&ctx),
            );
        // Mute is the header's one control, at the size of the row of
        // controls under the picture; Turn on is the page's 40 px primary
        let mute = Button::secondary(if muted { &muted_label } else { &mute_label })
            .sm()
            .icon(if muted { Icon::MicOff } else { Icon::Mic })
            .alarm(muted)
            .min_width(mute_room);
        let turn_on_label = tr!("live-turn-on");
        let turn_on = Button::primary(&turn_on_label);
        let mute_w = mute.width(&ctx);
        let ctl_w = if off { turn_on.width(&ctx) } else { mute_w };
        // the title is cut beside the widest control any state shows
        // (Turn on, or Mute), so a state change never moves the header
        let widest = turn_on.width(&ctx).max(mute_room);
        // while HushMic can't work the controls stay, disabled (the notice
        // holds Try again): the page never moves for it
        let works = !stopped(s);
        let stopped_tip = |r: egui::Response| {
            if works {
                r
            } else {
                r.on_hover_text(tr!("live-stopped-tip"))
            }
        };
        let title = fitted_title(
            &ctx,
            s,
            self.silent(s),
            self.head_now(&ctx, s, cx.now),
            util::head_room(w, widest),
        );
        let head = util::head(ui, &title, x, y, w, widest);
        let ctl_x = if head.stacked {
            head.acts.left()
        } else {
            head.acts.right() - ctl_w
        };
        let ctl_y = head.acts.top();
        // The control swapped (Turn on for Mute, or back): for a moment a
        // click on the new one does nothing, so the second click of a
        // double-click on Turn on never mutes (UX panel round 2, A14).
        if self.header_off.replace(off).is_some_and(|was| was != off) {
            self.swapped_at = Some(cx.now);
        }
        let settled = self.swapped_at.is_none_or(|t| cx.now - t >= SWAP_SETTLE);
        if off {
            let r = util::put(
                ui,
                rect(ctl_x, ctl_y, ctl_w, 40.0),
                "live-turn-on",
                turn_on.enabled(works),
            );
            let r = stopped_tip(r);
            if r.clicked() && settled {
                self.focus_mute = r.has_focus().then_some(r.id);
                cx.send(Action::SetMode(Mode::Suppress));
            }
        } else {
            let r = util::put(
                ui,
                rect(
                    ctl_x,
                    ctl_y + (40.0 - replay::ROW_H) / 2.0,
                    mute_w,
                    replay::ROW_H,
                ),
                "live-mute",
                mute.enabled(works),
            );
            let r = stopped_tip(r);
            // Turn on went with the Off state; its focus comes here
            if let Some(button) = self.focus_mute.take() {
                util::take_focus(&r, button);
            }
            if r.clicked() && settled {
                // the target from the state drawn, never a toggle: a second
                // click before the next snapshot must not undo the first
                cx.send(Action::SetMode(if muted {
                    s.unmute_to
                } else {
                    Mode::Mute
                }));
            }
        }
        head.bottom
    }

    // ---------------------------------------------------------------- figure

    /// The card, from `y` down to `limit` where its lanes can make it so:
    /// a notice when something needs attention, the picture, and the row
    /// of controls. The lanes take the height the rest leaves
    /// ([`Sizes::lane_for`]), so a notice or a line under the row makes
    /// them lower and moves no control. Returns the card's bottom.
    #[allow(clippy::too_many_arguments)]
    fn figure(
        &mut self,
        ui: &mut Ui,
        cx: &mut Cx<'_>,
        v: &View,
        x: f32,
        y: f32,
        w: f32,
        limit: f32,
        silent: bool,
        look: Option<std::time::Duration>,
    ) -> f32 {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let s = cx.state;
        // a card (UX panel D1): its fill goes under everything in it, its
        // height known at the end; the controls in it blend with it
        let card_at = ui.painter().add(egui::Shape::Noop);
        let page_fill = widgets::backdrop(ui);
        widgets::set_backdrop(ui, pal.card_fill());
        let (ix, iw) = (x + 1.0, w - 2.0);
        let (cl, cw) = (ix + CARD_PAD, iw - 2.0 * CARD_PAD);
        let level = if silent || stopped(s) {
            None
        } else {
            replay::level_show(s, v, self.meter_level.level().verdict)
        };
        let problem = problem(s, silent, level.as_ref());
        // only the problem the page shows is held back from the desktop
        let shows = problem.as_ref().and_then(|p| p.notify);
        if self.told != Some(shows) {
            self.told = Some(shows);
            cx.send(Action::UiShows(shows));
        }
        let light_link = problem
            .as_ref()
            .is_some_and(|p| p.fixes.contains(&Fix::AlwaysLight));
        // the lanes' height with nothing to say: what the one row of
        // controls under them and the padding leave of the page
        // (the strength's steps have 10 px a side, 6 where the row needs
        // the room)
        let leads = (strength_width(&ctx, 10.0), strength_width(&ctx, 6.0));
        let (row_h, line_h) = self.replay.height(&ctx, s, v, iw, leads);
        let rest = row_h + CARD_PAD + 2.0;
        let calm = Sizes::lane_for(cx.compact, limit - y - rest);
        // a notice, or a line under the row, takes its room from the two
        // lanes in equal parts (in whole pixels: its height is made
        // even), so nothing under the picture moves for it
        let pad = Sizes::of(cx.compact, 0.0).pad_top;
        let noted = problem.as_ref().filter(|p| p.noted());
        let notice_h = noted.map_or(0.0, |p| {
            2.0 * ((pad + notice_plan(&ctx, cx, p, cw).1) / 2.0).ceil()
        });
        let lane = (calm - ((notice_h + line_h) / 2.0).ceil()).max(spectro::LANE_MIN);
        let mut top = y + 1.0;
        if let Some(p) = noted {
            self.notice(ui, cx, p, cl, top + pad, cw);
            top += notice_h;
        }
        let pic = rect(ix, top, iw, Sizes::of(cx.compact, lane).height());
        self.picture(ui, cx, v, pic, silent, look, level.as_ref());
        // one row under the picture: the strength leads it, the last 10
        // seconds' two buttons end it (where the window is too narrow for
        // both, the buttons go on a line of their own under the strength)
        let sw = self.replay.lead(&ctx, v, iw, leads);
        let sy = pic.bottom() + replay::PAD_TOP;
        let mut c = child(ui, rect(cl, sy, sw, replay::ROW_H), "live-strength");
        c.set_width(sw);
        let pad = if sw + 0.5 < leads.0 { 6.0 } else { 10.0 };
        strength_control(&mut c, cx, light_link, pad);
        // the last 10 seconds (one group for screen readers: "The last 10
        // seconds")
        let mut row_ui = widgets::a11y_children(
            ui,
            rect(ix, pic.bottom(), iw, 400.0),
            egui::accesskit::Role::Group,
            &tr!("live-replay-group"),
        );
        let bottom = self
            .replay
            .row(&mut row_ui, cx, v, ix, pic.bottom(), iw, leads);
        if noted.is_none() {
            // no link is left to hand the keyboard over (the strength
            // took it above if one just went)
            ui.data_mut(|d| d.remove::<egui::Id>(handover_id()));
        }
        #[cfg(test)]
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("hm-live-controls"), bottom));
        let fig = Rect::from_min_max(pos2(x, y), pos2(x + w, bottom + CARD_PAD + 1.0));
        #[cfg(test)]
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("hm-live-fig"), fig));
        let p = ui.painter().clone();
        p.set(card_at, widgets::card_fill_shape(fig, &pal));
        widgets::card_edge(&p, fig, &pal);
        frame_light(&p, &pal, fig);
        widgets::set_backdrop(ui, page_fill);
        fig.bottom()
    }

    /// The picture in `pic`: the two lanes as the backend holds them now,
    /// or the take a replay plays, named for screen readers. The
    /// microphone's caption is where it is chosen, and ends in its level.
    /// `look`: when the meter wants its next read
    /// ([`LivePage::read_meter`]).
    #[allow(clippy::too_many_arguments)]
    fn picture(
        &mut self,
        ui: &mut Ui,
        cx: &mut Cx<'_>,
        v: &View,
        pic: Rect,
        silent: bool,
        look: Option<std::time::Duration>,
        level: Option<&LevelShow>,
    ) {
        let ctx = ui.ctx().clone();
        let kit = crate::ui::kit(&ctx);
        let s = cx.state;
        let replaying = v.phase == CheckPhase::Replaying;
        let original = self.replay.original(v);
        // one image for screen readers, before the rows under it: it is
        // read first
        let r = ui.interact(pic, ui.id().with("live-picture"), egui::Sense::hover());
        let name = if replaying {
            if original {
                tr!("live-pic-replay-original")
            } else {
                tr!("live-pic-replay")
            }
        } else {
            picture_name(s)
        };
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, &name));
        describe_mic(ui.ctx(), r.id, s);
        if replaying {
            // which side plays is read out when it changes
            util::tell(&ctx, r.id, util::Speak::Live);
        }
        let moving = match self.replay.picture() {
            Some(picture) => {
                let moving = self.spectro.sync(&ctx, &picture, cx.now);
                if !moving {
                    // still: the backend wakes the page when they move
                    picture.rest();
                }
                moving
            }
            None => {
                self.spectro.forget();
                false
            }
        };
        if moving {
            // the lanes scroll: every frame the display shows
            crate::ui::motion::moment_repaint(&ctx);
        } else if let Some(look) = look {
            crate::ui::motion::repaint_in(&ctx, look);
        }
        #[cfg(test)]
        {
            self.moving = moving;
        }
        // the lane that is heard stands out; the sides cross over
        let side = ctx.animate_bool_with_time_and_easing(
            ui.id().with("live-pic-side"),
            original,
            if kit.reduced_motion { 0.0 } else { 0.18 },
            egui::emath::easing::cubic_in_out,
        );
        // the level belongs to the live microphone: a take that plays
        // has none
        let level = level.filter(|_| !replaying).map(|l| match l {
            LevelShow::Line { word, waiting } => {
                let dot = if *waiting { Dot::Waiting } else { Dot::Good };
                (word.as_str(), dot)
            }
            LevelShow::Note { word, .. } => (word.as_str(), Dot::Warn),
        });
        let (mut show, apps_whole) = lanes(&ctx, s, v, silent, side, pic.width(), cx.compact);
        show.lane = (pic.height() - Sizes::of(cx.compact, 0.0).height()) / 2.0;
        show.mic.tail = level.map_or(0.0, |(word, dot)| level_width(&ctx, word, dot));
        let painted = self.spectro.paint(&ctx, ui.painter(), pic, &show);
        self.mic_select(ui, cx, &show, painted.names[0].0);
        if let Some((word, dot)) = level {
            let row = painted.rows[0];
            let at = Rect::from_min_max(pos2(row.right() - show.mic.tail, row.top()), row.max);
            let pal = spectro::palette(
                &ctx,
                spectro::LIVE_KEY,
                spectro::lit([&show.mic, &show.apps]),
            );
            level_mark(ui, at, word, dot, &pal);
        }
        // a label that leaves apps out, or is cut short, is whole on hover
        let (apps_at, apps_cut) = painted.names[1];
        if !apps_whole || apps_cut {
            ui.interact(
                apps_at,
                ui.id().with("live-app-label"),
                egui::Sense::hover(),
            )
            .on_hover_text(apps_all(s));
        }
        #[cfg(test)]
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new("hm-live-show"), show);
            d.insert_temp(egui::Id::new("hm-live-painted"), painted);
        });
    }

    /// The microphone's choice, in its lane's caption after "Microphone":
    /// the chosen one's name and a chevron, in `room` (shortened where
    /// that is narrower).
    fn mic_select(&mut self, ui: &mut Ui, cx: &mut Cx<'_>, show: &Show, room: Rect) {
        let s = cx.state;
        let ctx = ui.ctx().clone();
        // the lanes' palette: the select stands in one
        let pal = spectro::palette(
            &ctx,
            spectro::LIVE_KEY,
            spectro::lit([&show.mic, &show.apps]),
        );
        let (names, notes, chosen) = mic_items(s);
        let items: Vec<SelectItem<'_>> = names
            .iter()
            .zip(&notes)
            .map(|(n, t)| SelectItem { label: n, note: t })
            .collect();
        let name = names.get(chosen).map_or("", String::as_str);
        let natural = widgets::select::quiet_width(&ctx, name);
        // its label starts where a written name would
        let field = Rect::from_min_max(
            pos2(room.left() - 6.0, room.top() - 3.0),
            pos2(
                room.left() - 6.0 + natural.min(room.width() + 6.0),
                room.bottom() + 3.0,
            ),
        );
        let [_, there, absent] = spectro::inks(&pal);
        let ink = if show.mic.strong { there } else { absent };
        let mut idx = chosen;
        let mut c = child(ui, field, "live-mic");
        let label = tr!("live-microphone");
        let r = if self.pick_open {
            widgets::with_forced(&mut c, widgets::Force::Open, |c| {
                widgets::select::select_quiet(c, &label, &mut idx, &items, ink, &pal)
            })
        } else {
            widgets::select::select_quiet(&mut c, &label, &mut idx, &items, ink, &pal)
        };
        self.select_id = Some(r.id);
        // the list's tooltip, which screen readers get after the choice
        // (the description the list set); a name cut short is whole in it
        let tip = tr!("live-mic-remembered");
        let said = if name.is_empty() {
            tip.clone()
        } else {
            format!("{name}. {tip}")
        };
        widgets::a11y(&ctx, r.id, |n| n.set_description(said.as_str()));
        if natural > field.width() + 0.5 {
            r.on_hover_text(said);
        } else {
            r.on_hover_text(tip);
        }
        if idx != chosen {
            // past the devices: the chosen one, not listed (mic_items)
            let pick = if idx == 0 {
                None
            } else {
                s.mic
                    .devices
                    .get(idx - 1)
                    .map(|d| d.name.clone())
                    .or_else(|| s.mic.selected.clone())
            };
            cx.send(Action::SelectMic(pick));
        }
    }

    /// What needs attention, from `(x, y)`, `w` wide: a 2 px bar in its
    /// tone, the words in full, and its links in rows under them
    /// ([`notice_plan`]). It is the card's first thing, right under the
    /// headline it explains; the lanes under it give it its room.
    fn notice(&mut self, ui: &mut Ui, cx: &mut Cx<'_>, p: &Problem, x: f32, y: f32, w: f32) {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let tw = (w - BAR_IN).max(40.0);
        let (links, height) = notice_plan(&ctx, cx, p, w);
        let block = problem_block(&ctx, &p.lead, &p.body, tw, &pal);
        let words = Rect::from_min_size(pos2(x + BAR_IN, y), block.size());
        block.paint(ui.painter(), words.min);
        // read out when it appears or changes (UX panel B16)
        let full = p.words();
        let resp = ui.interact(words, ui.id().with("live-problem"), egui::Sense::hover());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &full));
        util::tell(&ctx, resp.id, util::Speak::Live);
        for (fix, label, at, lw) in &links {
            let mut row = child(
                ui,
                Rect::from_min_size(words.left_bottom() + *at, vec2(*lw, Type::LINK.line)),
                ("live-fix", *fix as usize),
            );
            let l = widgets::link(&mut row, label, false, false);
            if *fix == Fix::AlwaysLight {
                // the link may end by itself (the computer quiets down):
                // with the keyboard on it, the strength takes it then
                if l.has_focus() {
                    ui.data_mut(|d| d.insert_temp(handover_id(), l.id));
                } else {
                    ui.data_mut(|d| d.remove::<egui::Id>(handover_id()));
                }
            }
            if l.clicked() {
                match fix {
                    Fix::Retry => cx.send(Action::Retry),
                    Fix::Diagnostics => cx.diag.start(),
                    Fix::ChooseMic => self.open_mic_list(&ctx),
                    Fix::AlwaysLight => cx.send(Action::SetModel(Model::Light)),
                }
            }
        }
        // the bar runs beside the words and the links
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(x, y), pos2(x + 2.0, y + height)),
            0.0,
            p.tone.color(&pal),
        );
    }

    /// "Choose another": open the microphone list.
    fn open_mic_list(&self, ctx: &egui::Context) {
        if let Some(id) = self.select_id {
            crate::ui::widgets::select::open_select(ctx, id);
            ctx.request_repaint();
        }
    }
}

/// Where a notice's links go, `w` wide (each: its fix, its words, its
/// place from the words' bottom left, its width), and the notice's whole
/// height: the words in full, then the links in rows.
fn notice_plan(
    ctx: &egui::Context,
    cx: &Cx<'_>,
    p: &Problem,
    w: f32,
) -> (Vec<(Fix, String, egui::Vec2, f32)>, f32) {
    let pal = crate::ui::kit(ctx).pal;
    let tw = (w - BAR_IN).max(40.0);
    let mut height = problem_block(ctx, &p.lead, &p.body, tw, &pal).size().y;
    let mut links = Vec::new();
    let (mut lx, mut ly) = (0.0, LINK_GAP);
    for fix in &p.fixes {
        let label = match fix {
            Fix::Retry => tr!("live-try-again"),
            Fix::Diagnostics => cx.diag.label(),
            Fix::ChooseMic => tr!("live-mic-choose"),
            Fix::AlwaysLight => tr!("live-always-light"),
        };
        let lw = text::width(ctx, &label, Type::LINK).ceil().min(tw);
        if lx > 0.0 && lx + lw > tw {
            // the row is full: the link starts the next one
            lx = 0.0;
            ly += Type::LINK.line + LINK_GAP;
        }
        links.push((*fix, label, vec2(lx, ly), lw));
        lx += lw + FIX_GAP;
    }
    if !links.is_empty() {
        height += ly + Type::LINK.line;
    }
    (links, height)
}

/// What the picture draws for the state: the two lanes with their
/// captions and what stands in a lane that has no sound, and the replay
/// if one plays (`side`: how much of what is heard is the original).
/// False in `.1` when the apps' name leaves apps out (its tooltip lists
/// them all).
pub(super) fn lanes(
    ctx: &egui::Context,
    s: &AppState,
    v: &View,
    silent: bool,
    side: f32,
    width: f32,
    compact: bool,
) -> (Show, bool) {
    let state = LivePage::live_state(s);
    let runs = !matches!(
        state,
        LiveState::Off | LiveState::Starting | LiveState::Reconnecting
    );
    let replaying = v.phase == CheckPhase::Replaying;
    let absent = selected_absent(s);
    // this PipeWire is too old to listen along: there is no picture, and
    // the lane says why (noise suppression works all the same)
    let blind = v.phase == CheckPhase::Blocked(Blocker::NeedsNewerPipewire);
    // nothing comes in: no lane draws a sound that is not there
    let quiet = silent || s.mic.silent || absent || blind;
    let muted = state == LiveState::Muted;
    let mic_note = match state {
        _ if stopped(s) || replaying => None,
        LiveState::Starting => Some((None, tr!("live-sub-starting"))),
        LiveState::Reconnecting => Some((None, tr!("live-sub-reconnecting"))),
        LiveState::Off => None,
        _ if blind => Some((None, tr!("live-sub-no-picture"))),
        _ if absent => Some((Some(Icon::Plug), tr!("live-mic-unplugged"))),
        _ if quiet => Some((None, tr!("miccheck-level-silent"))),
        _ => None,
    };
    // muted, nothing reaches the apps: their lane's caption says so and
    // the lane is shut
    let shut = runs && muted && !replaying;
    // noise suppression off, or stepped down to nothing: apps hear the
    // microphone as it is, and their lane's caption says so
    let raw = runs && !muted && !replaying && !quiet && !super::status::filtering(s);
    let (app, whole) = match app_label(ctx, s, 0.4 * width) {
        _ if shut => ((tr!("live-apps-muted"), true), true),
        Some(((apps, strong), whole)) if raw => {
            ((tr!("live-apps-unfiltered", apps = apps), strong), whole)
        }
        None if raw => ((tr!("live-apps-unfiltered-none"), false), true),
        Some((label, whole)) => (label, whole),
        None => ((String::new(), false), true),
    };
    let show = Show {
        mic: Lane {
            glyph: Icon::Mic,
            label: tr!("live-microphone"),
            name: picture_mic(s),
            strong: runs && !absent,
            select: Some(widgets::select::quiet_width(ctx, &chosen_mic(s)) - 6.0),
            tail: 0.0,
            sound: replaying || runs && !quiet,
            note: mic_note,
            shut: false,
        },
        apps: Lane {
            glyph: if shut { Icon::MicOff } else { Icon::AppGlyph },
            label: tr!("live-apps-hear"),
            name: app.0,
            strong: app.1,
            select: None,
            tail: 0.0,
            sound: replaying || runs && !quiet && !muted,
            note: None,
            shut,
        },
        playing: replaying.then(|| Playing {
            position: v.timeline.position.unwrap_or(0.0),
            original: side,
        }),
        playing_label: tr!("live-lane-playing"),
        compact,
        lane: spectro::LANE_MIN,
    };
    (show, whole)
}

/// One engine meter column into the level check, as the replay's take
/// feeds its own (raw = the input, clean = what apps hear).
fn judge_column(check: &mut LevelCheck, c: &MeterColumn) {
    let db = |rms: f32| crate::miccheck::take::db((rms as f64) * (rms as f64));
    let engine = match c.mode {
        EngineMode::Filtering => Engine::Filtering,
        EngineMode::Bypass => Engine::Bypass,
        EngineMode::Muted => Engine::Muted,
    };
    let stats = ColumnStats {
        raw_db: db(c.input_rms),
        clean_db: db(c.output_rms),
        raw_peak: c.input_peak,
        clipped: c.input_peak >= 1.0,
    };
    // exact silence is the page's own look (`silent`)
    check.feed(&stats, engine, 0);
}

/// The level's dot.
#[derive(Clone, Copy, PartialEq)]
enum Dot {
    /// Hollow and subtle: waiting for speech, or no sound.
    Waiting,
    Good,
    /// Very quiet or too loud: the word and the dot in the warn colour.
    Warn,
}

/// The level dot's colour: the accent for a good level, the warn colour
/// for a note, subtle while waiting.
fn level_dot(dot: Dot, pal: &Palette) -> Color32 {
    match dot {
        Dot::Waiting => pal.ink_muted,
        Dot::Good => pal.accent,
        Dot::Warn => pal.warn,
    }
}

/// The dot's box and the gap to its word.
const DOT_W: f32 = 14.0;

fn level_type(dot: Dot) -> Type {
    match dot {
        Dot::Waiting => Type::CAP,
        Dot::Good | Dot::Warn => Type::CAP.medium(),
    }
}

/// The width of the level's dot and `word`.
fn level_width(ctx: &egui::Context, word: &str, dot: Dot) -> f32 {
    DOT_W + text::width(ctx, word, level_type(dot)).ceil()
}

/// The microphone's level at the right end of its lane's caption: an
/// 8 px dot and the verdict ("Sounds good"), in `at`. One node names it
/// "Level Sounds good".
fn level_mark(ui: &mut Ui, at: Rect, word: &str, dot_look: Dot, pal: &Palette) {
    let ctx = ui.ctx().clone();
    let [waiting, good, ..] = spectro::inks(pal);
    let colour = match dot_look {
        Dot::Waiting => waiting,
        Dot::Good => good,
        Dot::Warn => pal.warn,
    };
    // a good level's dot is the accent (C9); the word stays ink
    let dot_colour = level_dot(dot_look, pal);
    let block = text::layout_elided(
        &ctx,
        word,
        level_type(dot_look),
        colour,
        (at.width() - DOT_W).max(24.0),
    );
    let resp = ui.interact(at, ui.id().with("live-level"), egui::Sense::hover());
    let name = format!("{} {word}", tr!("mic-level"));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &name));
    let p = ui.painter();
    let centre = pos2(at.left() + 4.0, at.center().y);
    if dot_look == Dot::Waiting {
        p.circle_stroke(centre, 3.5, egui::Stroke::new(1.0_f32, dot_colour));
    } else {
        p.circle_filled(centre, 4.0, dot_colour);
    }
    let cut = block.elided();
    block.paint_centered_y(p, at.left() + DOT_W, at.center().y);
    if cut {
        resp.on_hover_text(word);
    }
}

fn push_recent(recent: &mut VecDeque<f32>, v: f32) {
    recent.push_back(v);
    while recent.len() > SILENT_COLS {
        recent.pop_front();
    }
}

/// Eyebrow, 10, label, 10.
#[cfg(feature = "speakers")]
pub(super) fn card_head(ui: &mut Ui, eyebrow: &str, label: &str, pal: &Palette) {
    let r = text::label(ui, eyebrow, Type::EYEBROW, pal.ink_subtle);
    util::tell(ui.ctx(), r.id, util::Speak::Heading(2));
    ui.add_space(CARD_GAP);
    if !label.is_empty() {
        text::label(ui, label, Type::LABEL, pal.ink);
        ui.add_space(CARD_GAP);
    }
}

/// Where the focus of "Always use Light" goes when the link does: the
/// strength takes it (the link's id is kept here while it has the
/// keyboard).
fn handover_id() -> egui::Id {
    egui::Id::new("live-always-light-handover")
}

/// What each step of the noise suppression control does: its tooltip.
/// Off first, then the four strengths; a strength from the command line
/// says how to replace it. While HushMic is off the steps wait, and say
/// when they apply (UX panel round 2, A6).
fn strength_tips(s: &AppState) -> [String; 5] {
    let after = if s.mode == Mode::Off {
        Some(tr!("live-strength-applies"))
    } else if suppressing(s) && s.sound.current.strength == Strength::Custom {
        Some(tr!("live-strength-custom"))
    } else {
        None
    };
    let with = |tip: String| match &after {
        Some(more) => format!("{tip} {more}"),
        None => tip,
    };
    [
        tr!("live-sub-bypass"),
        with(tr!("live-strength-gentle")),
        with(tr!("live-strength-medium")),
        with(tr!("live-strength-strong")),
        with(tr!("live-strength-maximum")),
    ]
}

/// The words of the control's five steps: Off, then the strengths.
fn strength_labels() -> [String; 5] {
    [
        tr!("live-strength-off"),
        tr!("strength-gentle"),
        tr!("strength-medium"),
        tr!("strength-strong"),
        tr!("strength-maximum"),
    ]
}

/// The control's width with `pad` px a side in its steps.
fn strength_width(ctx: &egui::Context, pad: f32) -> f32 {
    let labels = strength_labels();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    widgets::Strength::fitted_width(ctx, &refs, pad, true).ceil()
}

/// Noise suppression: Off, or one of the four strengths, each with what
/// it does as its tooltip. Off is what apps hear your raw microphone
/// with; a strength chosen while it is off turns it on at that strength.
/// The strength a microphone remembers is untouched by Off. While muted
/// the choice is what Unmute returns to; while HushMic is off or can't
/// work the group is disabled and shows the strength that waits.
fn strength_control(ui: &mut Ui, cx: &mut Cx<'_>, light_link: bool, pad: f32) {
    let s = cx.state;
    let ctx = ui.ctx().clone();
    let labels = strength_labels();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let tips = strength_tips(s);
    let tip_refs: Vec<&str> = tips.iter().map(String::as_str).collect();
    let current = s.sound.current.strength;
    let off = s.mode == Mode::Off;
    let on = suppressing(s) || off;
    let mut idx = if on {
        strength_index(current).map_or(usize::MAX, |k| k + 1)
    } else {
        0
    };
    let before = idx;
    let name = tr!("live-switch");
    // "Always use Light" (the notice's link while HushMic steps down) had
    // the keyboard and went (the model is Light now, or the step-down
    // ended): the control takes the focus
    let mut take = false;
    if !light_link {
        if let Some(link) = ui.data(|d| d.get_temp::<egui::Id>(handover_id())) {
            ui.data_mut(|d| d.remove::<egui::Id>(handover_id()));
            let f = ctx.memory(|m| m.focused());
            take = f.is_none() || f == Some(link);
        }
    }
    let group = widgets::Strength::new(&name, &mut idx, &refs)
        .tips(&tip_refs)
        .take_focus(take)
        .dense()
        .fitted(pad, true)
        .enabled(!off && !stopped(s))
        .show(ui);
    let asked = egui::Id::new("live-strength-asked");
    let muted = s.mode == Mode::Mute;
    let to = |mode: Mode| {
        if muted {
            Action::SetUnmuteTo(mode)
        } else {
            Action::SetMode(mode)
        }
    };
    if idx != before && idx == 0 {
        cx.send(to(Mode::Bypass));
    } else if idx != before && idx <= 4 {
        let picked = Strength::PRESETS[idx - 1].0;
        if !on {
            cx.send(to(Mode::Suppress));
        }
        // the strength it is at already comes back with it
        if picked != current {
            cx.send(Action::SetStrength(picked));
            ui.data_mut(|d| d.insert_temp(asked, picked));
        }
    }
    // what the choice in effect does, as a status line that only screen
    // readers get, read when it changes. A strength this window did not
    // pick (another microphone's settings, the tray, the command line)
    // says so first: an unplugged mic's stand-in ran at its own Maximum
    // unannounced (UX panel round 2, A34)
    let cap = match current {
        _ if off => tr!("live-strength-applies"),
        _ if !on => tr!("live-sub-bypass"),
        Strength::Gentle => tr!("live-strength-gentle"),
        Strength::Medium => tr!("live-strength-medium"),
        Strength::Strong => tr!("live-strength-strong"),
        Strength::Maximum => tr!("live-strength-maximum"),
        Strength::Custom => tr!("live-strength-custom"),
    };
    let seen = egui::Id::new("live-strength-seen");
    let was = ui.data(|d| d.get_temp::<(Strength, Option<String>)>(seen));
    let now_at = (current, s.sound.owner.clone());
    let own = ui.data(|d| d.get_temp::<Strength>(asked)) == Some(current);
    let told = egui::Id::new("live-strength-told");
    if was.as_ref().is_some_and(|w| w.0 != current) {
        if own {
            ui.data_mut(|d| d.remove::<String>(told));
        } else {
            let words = strength_told(s, current, &cap);
            ui.data_mut(|d| d.insert_temp(told, words));
        }
    }
    if own {
        ui.data_mut(|d| d.remove::<Strength>(asked));
    }
    ui.data_mut(|d| d.insert_temp(seen, now_at));
    let spoken = ui.data(|d| d.get_temp::<String>(told)).unwrap_or(cap);
    let at = Rect::from_min_size(group.rect.left_bottom() - vec2(0.0, 1.0), vec2(1.0, 1.0));
    let r = ui.interact(at, ui.id().with("live-strength-says"), egui::Sense::hover());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &spoken));
    util::tell(&ctx, r.id, util::Speak::Live);
}

/// What the strength caption says to screen readers when the strength
/// changed without this window's pick: the new step, whose settings they
/// are, then the caption.
fn strength_told(s: &AppState, st: Strength, cap: &str) -> String {
    let step = match st {
        Strength::Gentle => tr!("strength-gentle"),
        Strength::Medium => tr!("strength-medium"),
        Strength::Strong => tr!("strength-strong"),
        Strength::Maximum => tr!("strength-maximum"),
        Strength::Custom => tr!("strength-custom"),
    };
    let owner = s.sound.owner.as_deref().and_then(|o| s.description_of(o));
    match owner {
        Some(mic) => tr!(
            "live-strength-told-for",
            strength = step,
            mic = mic.to_owned(),
            caption = cap.to_owned()
        ),
        None => tr!(
            "live-strength-told",
            strength = step,
            caption = cap.to_owned()
        ),
    }
}

/// A notice's bar sits this far left of its words, and its links this far
/// under them.
pub(super) const BAR_IN: f32 = 10.0;
pub(super) const LINK_GAP: f32 = 6.0;

/// How a problem reads: the colour of its 2 px bar.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tone {
    /// HushMic can't work.
    Error,
    /// Something to fix.
    Warn,
    /// A state worth knowing.
    Info,
}

impl Tone {
    fn color(self, pal: &Palette) -> Color32 {
        match self {
            Tone::Error => pal.danger,
            Tone::Warn => pal.warn,
            Tone::Info => pal.ink_subtle,
        }
    }
}

/// A problem's way out, a link under its words.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fix {
    Retry,
    Diagnostics,
    ChooseMic,
    AlwaysLight,
}

/// The page's one problem: its words (a lead in medium ink that runs into
/// the body) and its links.
#[derive(Clone, Debug, PartialEq)]
struct Problem {
    tone: Tone,
    lead: String,
    body: String,
    fixes: Vec<Fix>,
    /// The desktop notification about the same thing, if there is one:
    /// while the page shows it, the window holds it back.
    notify: Option<crate::notify::Problem>,
}

impl Problem {
    fn new(tone: Tone, lead: String, body: String, fixes: &[Fix]) -> Problem {
        Problem {
            tone,
            lead,
            body,
            fixes: fixes.to_vec(),
            notify: None,
        }
    }

    fn told(mut self, n: crate::notify::Problem) -> Problem {
        self.notify = Some(n);
        self
    }

    /// It shows as a notice: something to fix, or a way out to offer. A
    /// state that is only worth knowing (off, starting, muted, noise
    /// suppression off) is said by the header and the picture.
    fn noted(&self) -> bool {
        self.tone != Tone::Info || !self.fixes.is_empty()
    }

    /// The whole text in one line: the notice's name for screen readers.
    fn words(&self) -> String {
        [self.lead.as_str(), self.body.as_str()]
            .iter()
            .filter(|t| !t.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The page's one problem, the most important first: HushMic
/// can't work, the chain keeps restarting, it starts, it is off; then the
/// microphone (not connecting, taken by another app, unplugged, silent),
/// the level; then the step-down (Light, unfiltered), Bypass and Muted. None
/// when nothing is wrong.
fn problem(s: &AppState, silent: bool, level: Option<&LevelShow>) -> Option<Problem> {
    use crate::notify::Problem as N;
    use Fix::*;
    let mic = mic_name(s);
    if !s.pipewire.reachable {
        return Some(
            Problem::new(
                Tone::Error,
                tr!("live-status-pipewire-lead"),
                tr!("live-status-pipewire-body"),
                &[Retry, Diagnostics],
            )
            .told(N::Failure),
        );
    }
    if stopped(s) {
        return Some(
            Problem::new(
                Tone::Error,
                tr!("live-down-title"),
                tr!("live-status-down-body"),
                &[Retry, Diagnostics],
            )
            .told(N::Failure),
        );
    }
    if s.engine.flapping && s.mode != Mode::Off {
        return Some(
            Problem::new(
                Tone::Warn,
                tr!("live-flapping-lead"),
                tr!("live-flapping-body"),
                &[Diagnostics],
            )
            .told(N::Flapping),
        );
    }
    if s.mode == Mode::Off {
        return Some(Problem::new(
            Tone::Info,
            String::new(),
            tr!("live-sub-off"),
            &[],
        ));
    }
    match s.engine.phase {
        Phase::Starting | Phase::Off | Phase::Down => {
            return Some(Problem::new(
                Tone::Info,
                String::new(),
                tr!("live-sub-starting"),
                &[],
            ))
        }
        Phase::Reconnecting => {
            return Some(
                Problem::new(Tone::Info, String::new(), tr!("live-sub-reconnecting"), &[])
                    .told(N::Failure),
            )
        }
        Phase::Ready => {}
    }
    // fed by nothing: outranks a thief, whose mic at least sounds
    if s.mic.stuck {
        return Some(
            Problem::new(
                Tone::Warn,
                tr!("live-mic-stuck-title", mic = mic.clone()),
                tr!("live-mic-stuck-body"),
                &[ChooseMic],
            )
            .told(N::MicStuck),
        );
    }
    if s.mic.rerouted {
        return Some(
            Problem::new(
                Tone::Warn,
                tr!("live-mic-rerouted-title"),
                tr!("live-mic-rerouted-body"),
                &[],
            )
            .told(N::Rerouted),
        );
    }
    // unplugged outranks silent: a mic that is gone sends no sound, and
    // "check that it's on" sent people to a device that isn't there (UX
    // panel round 2, A34)
    if selected_absent(s) {
        return Some(Problem::new(
            Tone::Warn,
            tr!("live-mic-absent-title", mic = mic.clone()),
            tr!("live-mic-absent-body"),
            &[ChooseMic],
        ));
    }
    if let Some(active) = standin(s) {
        return Some(
            Problem::new(
                Tone::Warn,
                tr!("live-mic-unplugged-title", mic = mic.clone()),
                tr!("live-mic-unplugged-body", device = active),
                &[],
            )
            .told(N::MicFallback),
        );
    }
    // the window's own look (3 s), or the daemon's (10 s, with an app):
    // the daemon's notification about it waits while it shows here
    if silent || s.mic.silent {
        return Some(
            Problem::new(
                Tone::Warn,
                tr!("live-title-silent", mic = mic.clone()),
                tr!("live-sub-silent"),
                &[ChooseMic],
            )
            .told(N::MicSilent),
        );
    }
    if let Some(LevelShow::Note { lead, body, .. }) = level {
        return Some(Problem::new(Tone::Warn, lead.clone(), body.clone(), &[]));
    }
    // the step-down: "Always use Light" sets it for the microphone in use
    // at once, offered only while that is not Light already (too busy even
    // for Light: nothing to offer)
    let light = if s.sound.current.model != Model::Light {
        &[AlwaysLight][..]
    } else {
        &[]
    };
    let passthrough = s.engine.tier == Some(EngineTier::Passthrough);
    match s.mode {
        Mode::Suppress if light_for_now(s) => Some(Problem::new(
            Tone::Info,
            tr!("live-light-now-title"),
            tr!("live-light-now-body"),
            light,
        )),
        Mode::Suppress if passthrough => Some(Problem::new(
            Tone::Warn,
            tr!("live-busy-title"),
            tr!("live-busy-body"),
            light,
        )),
        Mode::Bypass => Some(Problem::new(
            Tone::Info,
            String::new(),
            tr!("live-sub-bypass"),
            &[],
        )),
        // Mute silences only HushMic's microphone: an app on the raw one
        // still hears every word, so while muted that one line says so
        // (conflict ruling 2; otherwise apps go unnamed, round 3b C)
        Mode::Mute if !s.apps.direct.is_empty() => Some(Problem::new(
            Tone::Warn,
            String::new(),
            tr!(
                "live-sub-muted-direct",
                count = s.apps.direct.len(),
                apps = crate::notify::name_apps(s.apps.direct.iter().map(|d| &d.app))
            ),
            &[],
        )),
        Mode::Mute => Some(Problem::new(
            Tone::Info,
            String::new(),
            tr!("live-sub-muted"),
            &[],
        )),
        _ => None,
    }
}

/// A problem's words: the lead in medium ink, the body muted.
pub(super) fn problem_block(
    ctx: &egui::Context,
    lead: &str,
    body: &str,
    w: f32,
    pal: &Palette,
) -> TextBlock {
    let mut r = text::Rich::new();
    if !lead.is_empty() {
        r = r
            .span(lead, Type::CAP.medium(), pal.ink)
            .span(" ", Type::CAP, pal.ink_muted);
    }
    r.span(body, Type::CAP, pal.ink_muted).layout(ctx, w)
}

/// The frame light along a figure's top edge (12 % to 88 %).
pub(super) fn frame_light(p: &egui::Painter, pal: &Palette, fig: Rect) {
    let (x0, x1) = (
        fig.left() + 0.12 * fig.width(),
        fig.right() - 0.12 * fig.width(),
    );
    let fl = pal.frame_light;
    p.add(egui::Shape::mesh(crate::ui::paint::band(
        x0,
        x1,
        fig.top(),
        fig.top() + 1.0,
        2.0,
        |x| {
            let f = (x - x0) / (x1 - x0);
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
}

// ---------------------------------------------------------------- copy

/// Everything the meter keeps (41 s); the picture takes what fits.
const HISTORY_COLS: usize = hushmic_shm::meter::SLOTS / crate::meter::COLUMN_HOPS as usize;

/// The engine runs the light model only because the computer is busy.
fn light_for_now(s: &AppState) -> bool {
    s.engine.phase == Phase::Ready
        && s.engine.tier == Some(EngineTier::Light)
        && !s.engine.configured_light
}

pub fn strength_index(st: Strength) -> Option<usize> {
    Strength::PRESETS.iter().position(|(p, _)| *p == st)
}

/// The painted end label, for screen readers: the picture's description.
pub(super) fn describe_mic(ctx: &egui::Context, picture: egui::Id, s: &AppState) {
    let d = tr!("live-pic-mic", mic = picture_mic(s));
    ctx.accesskit_node_builder(picture, |n| n.set_description(d));
}

/// The picture's microphone lane: the microphone in use, by its device
/// name as the notices say it, also while HushMic follows the system default
/// ("System default" only when that can't be named), and the stand-in
/// while the chosen one is unplugged.
pub(super) fn picture_mic(s: &AppState) -> String {
    standin(s).unwrap_or_else(|| mic_name(s))
}

/// The name of the microphone HushMic cleans (the chosen one, else the
/// system default's device).
pub fn mic_name(s: &AppState) -> String {
    match s.mic.selected.as_deref() {
        Some(name) => s.description_of(name).unwrap_or(name).to_owned(),
        None => s
            .mic
            .system_default
            .as_deref()
            .and_then(|d| s.description_of(d))
            .map(str::to_owned)
            .unwrap_or_else(|| tr!("live-system-default")),
    }
}

/// While noise suppression is on, what is wrong with the microphone in a
/// title, in the notice's order (UX panel B8): "Using Built-in
/// Audio. RODE NT-USB is unplugged." and the like. None: the state's
/// usual words.
fn degraded_title(s: &AppState, silent: bool) -> Option<String> {
    degraded_titles(s, silent).map(|(named, _)| named)
}

/// A degraded state's title with the microphone's name, and without it
/// (the notice names it): a title never truncates, and a device name like
/// "Built-in Audio Digital Microphone" left no room for "isn't connected"
/// (UX panel SF-B8a). [`fitted_title`] picks.
fn degraded_titles(s: &AppState, silent: bool) -> Option<(String, String)> {
    let mic = mic_name(s);
    if s.mic.stuck {
        return Some((
            tr!("live-mic-stuck-title", mic = mic),
            tr!("live-mic-stuck-title-plain"),
        ));
    }
    if s.mic.rerouted {
        return Some((
            tr!("live-mic-rerouted-title"),
            tr!("live-mic-rerouted-title-plain"),
        ));
    }
    // unplugged outranks silent (as the notice's)
    if selected_absent(s) {
        return Some((
            tr!("live-mic-absent-title", mic = mic.clone()),
            tr!("live-mic-absent-title-plain"),
        ));
    }
    if let Some(device) = standin(s) {
        return Some((
            tr!("live-title-fallback", device = device, mic = mic.clone()),
            tr!("live-title-fallback-plain"),
        ));
    }
    if silent || s.mic.silent {
        return Some((
            tr!("live-title-silent", mic = mic),
            tr!("live-title-silent-plain"),
        ));
    }
    None
}

/// The header's title for `room` (one line): `head`, or, when that is a
/// degraded state's title that doesn't fit, its form without the
/// microphone's name.
fn fitted_title(
    ctx: &egui::Context,
    s: &AppState,
    silent: bool,
    head: String,
    room: f32,
) -> String {
    if text::width(ctx, &head, Type::H) <= room {
        return head;
    }
    match degraded_titles(s, silent) {
        Some((named, plain)) if named == head => plain,
        _ => head,
    }
}

/// The chosen microphone is not plugged in and HushMic has no fallback.
fn selected_absent(s: &AppState) -> bool {
    standin(s).is_none()
        && s.mic
            .selected
            .as_deref()
            .is_some_and(|sel| s.mic.devices.iter().any(|d| d.name == sel && !d.present))
}

/// The microphone that stands in for the chosen one, unplugged, by name:
/// only one that is there (UX panel round 2, A15: with the only
/// microphone gone, "Using  until it's back" named nobody).
fn standin(s: &AppState) -> Option<String> {
    if !s.mic.fallback {
        return None;
    }
    let active = s.mic.active.as_deref()?;
    if Some(active) == s.mic.selected.as_deref() {
        return None;
    }
    s.mic
        .devices
        .iter()
        .find(|d| d.name == active && d.present)
        .map(|d| d.description.clone())
        .filter(|d| !d.trim().is_empty())
}

/// The microphone list: System default, then every device. Returns the
/// labels, the notes and the selected index.
/// The entry of [`mic_items`] that is chosen.
fn chosen_mic(s: &AppState) -> String {
    let (mut names, _, chosen) = mic_items(s);
    if chosen < names.len() {
        names.swap_remove(chosen)
    } else {
        String::new()
    }
}

pub fn mic_items(s: &AppState) -> (Vec<String>, Vec<String>, usize) {
    let mut names = vec![tr!("live-system-default")];
    let now = s
        .mic
        .system_default
        .as_deref()
        .and_then(|d| s.description_of(d));
    let mut notes = vec![now
        .map(|d| tr!("live-system-default-now", device = d.to_owned()))
        .unwrap_or_default()];
    let active = standin(s).unwrap_or_default();
    let mut idx = 0;
    for (i, d) in s.mic.devices.iter().enumerate() {
        names.push(d.description.clone());
        let chosen = s.mic.selected.as_deref() == Some(d.name.as_str());
        notes.push(if d.present {
            String::new()
        } else if chosen && !active.is_empty() {
            // the chosen one, standing in for it right now
            tr!("live-mic-unplugged-note", device = active.clone())
        } else {
            tr!("live-mic-unplugged")
        });
        if s.mic.selected.as_deref() == Some(d.name.as_str()) {
            idx = i + 1;
        }
    }
    // the chosen microphone isn't listed (PipeWire can't be reached, so
    // nothing is): the list still says which one it is, by the name its
    // settings remember, last
    if let Some(sel) = s.mic.selected.as_deref().filter(|_| idx == 0) {
        names.push(s.description_of(sel).unwrap_or(sel).to_owned());
        notes.push(String::new());
        idx = names.len() - 1;
    }
    (names, notes, idx)
}

/// The header (round 3b): HushMic's state alone. Off, then starting,
/// muted, bypass, then what the filter does (the boards' order). While
/// noise suppression is on, a degraded state takes the title (UX panel
/// B8): see [`degraded_title`].
pub fn head_copy(s: &AppState) -> String {
    head_for(s, false)
}

/// [`head_copy`] with the page's own finding of a silent microphone.
fn head_for(s: &AppState, silent: bool) -> String {
    if stopped(s) {
        return tr!("live-title-stopped");
    }
    if s.mode == Mode::Off {
        return tr!("live-title-off");
    }
    match s.engine.phase {
        Phase::Starting | Phase::Off | Phase::Down => return tr!("live-title-starting"),
        Phase::Reconnecting => return tr!("live-title-reconnecting"),
        Phase::Ready => {}
    }
    if s.mode == Mode::Suppress {
        if let Some(t) = degraded_title(s, silent) {
            return t;
        }
    }
    match s.mode {
        Mode::Mute => tr!("live-title-muted"),
        Mode::Bypass => tr!("live-title-bypass"),
        _ if s.engine.tier == Some(EngineTier::Passthrough) => tr!("live-title-unfiltered"),
        _ if s.in_use() => tr!("live-title-on"),
        _ => tr!("live-title-ready"),
    }
}

/// Who records HushMic, in a sentence (the picture's name, Quit's
/// question): "Zoom is using HushMic.", "Zoom and OBS Studio are using
/// HushMic.", "3 apps are using HushMic.", "An app is using HushMic.", or
/// "No app is using HushMic right now.".
pub fn apps_line(s: &AppState) -> String {
    let names: Vec<Option<&str>> = s.apps.using.iter().map(|a| a.name.as_deref()).collect();
    match names.as_slice() {
        [] => tr!("live-sub-no-app"),
        [Some(app)] => tr!("fr-apps-using", app = app.to_string()),
        [None] => tr!("fr-apps-using-anon"),
        [Some(a), Some(b)] => tr!(
            "live-sub-apps-two",
            app1 = a.to_string(),
            app2 = b.to_string()
        ),
        _ => tr!("live-sub-apps-many", count = names.len()),
    }
}

/// The names of the apps recording HushMic ("An app" for one with none),
/// two of one name numbered ("Zoom", "Zoom (2)").
fn app_names(s: &AppState) -> Vec<String> {
    let anon = tr!("live-app-anon");
    util::numbered(
        s.apps
            .using
            .iter()
            .map(|a| a.name.as_deref().unwrap_or(anon.as_str())),
    )
}

/// The name under the picture's app end, at most `max` px wide: one name,
/// two short ones ("Zoom, Discord"), else the first and how many more
/// ("Zoom +3"); "No app right now" while none (false: not in use); none while
/// HushMic is off or starting. True in `.2` when it says it all (else the
/// label's tooltip lists every app).
fn app_label(ctx: &egui::Context, s: &AppState, max: f32) -> Option<((String, bool), bool)> {
    let runs = s.mode != Mode::Off && s.engine.phase == Phase::Ready;
    if !runs {
        return None;
    }
    if !s.in_use() {
        return Some(((tr!("live-app-none"), false), true));
    }
    let fits = |t: &str| {
        text::layout(
            ctx,
            t,
            Type::CAP.medium(),
            Color32::PLACEHOLDER,
            f32::INFINITY,
        )
        .size()
        .x <= max
    };
    let names = app_names(s);
    // the count always shows: a long first name gives way, with an
    // ellipsis
    let more = |names: &[String]| {
        let others = names.len() - 1;
        let with = |app: String| tr!("live-app-more", app = app, count = others);
        let mut name: Vec<char> = names[0].chars().collect();
        let mut label = with(names[0].clone());
        while !fits(&label) && name.len() > 1 {
            name.pop();
            let cut: String = name.iter().collect();
            // no bracket or separator left hanging before the ellipsis
            let cut = cut.trim_end_matches(|c: char| c.is_whitespace() || "([{-–,:/".contains(c));
            label = with(format!("{cut}…"));
        }
        label
    };
    let (label, all) = match names.as_slice() {
        [one] => (one.clone(), fits(one)),
        [a, b] => {
            let two = tr!("live-app-two", app1 = a.clone(), app2 = b.clone());
            if fits(&two) {
                (two, true)
            } else {
                (more(&names), false)
            }
        }
        _ => (more(&names), false),
    };
    Some(((label, true), all))
}

/// Every app using HushMic, in one line (the label's tooltip).
fn apps_all(s: &AppState) -> String {
    tr!("live-apps-all", apps = app_names(s).join(", "))
}

/// First run's picture name outside filtering: what apps hear.
pub fn state_name(s: &AppState) -> String {
    match LivePage::live_state(s) {
        LiveState::Off => tr!("live-pic-off"),
        LiveState::Starting | LiveState::Reconnecting => tr!("live-pic-starting"),
        LiveState::Muted => tr!("live-pic-muted"),
        LiveState::Bypass => tr!("live-pic-bypass"),
        LiveState::Unfiltered => tr!("live-pic-unfiltered"),
        _ if !s.in_use() => tr!("live-pic-ready"),
        _ => tr!("live-pic-on"),
    }
}

/// The picture's accessible name: what it shows, then who gets the sound
/// (the header's apps line; every app from three on).
pub fn picture_name(s: &AppState) -> String {
    let what = match LivePage::live_state(s) {
        LiveState::Off => return tr!("live-pic-off"),
        LiveState::Starting | LiveState::Reconnecting => return tr!("live-pic-starting"),
        LiveState::Muted => tr!("live-pic-live-muted"),
        LiveState::Bypass => tr!("live-pic-live-bypass"),
        LiveState::Unfiltered => tr!("live-pic-live-unfiltered"),
        _ => tr!("live-pic-live"),
    };
    // from three apps on the header line only counts them: the picture
    // names them all
    let apps = if s.apps.using.len() >= 3 {
        format!("{}.", apps_all(s))
    } else {
        apps_line(s)
    };
    format!("{what} {apps}")
}

// ---------------------------------------------------------------- attention

/// HushMic can't work: the attention block's title and body (first run).
fn attention_words(s: &AppState) -> (String, String) {
    if !s.pipewire.reachable {
        (tr!("live-pipewire-title"), tr!("live-pipewire-body"))
    } else {
        (tr!("live-down-title"), tr!("live-down-body"))
    }
}

/// HushMic can't work: words and actions as a block at `(x, y)`, `w` wide
/// (first run shows it in its steps; Live says it in its notice).
/// Returns the block's bottom.
pub(super) fn attention(ui: &mut Ui, cx: &mut Cx<'_>, x: f32, y: f32, w: f32) -> f32 {
    let ctx = ui.ctx().clone();
    Attention::new(&ctx, cx, w, true).show(ui, cx, x, y)
}

/// The laid-out attention block: the title, body, Try again and Copy
/// diagnostics, the troubleshooting guide.
struct Attention {
    w: f32,
    title: Option<TextBlock>,
    body: TextBlock,
    try_label: String,
    diag_label: String,
    buttons_wrap: bool,
}

impl Attention {
    fn new(ctx: &egui::Context, cx: &Cx<'_>, w: f32, with_title: bool) -> Attention {
        let pal = crate::ui::kit(ctx).pal;
        let (title, body) = attention_words(cx.state);
        let title = with_title.then(|| text::layout(ctx, &title, Type::H, pal.ink, w));
        let body = text::layout(ctx, &body, Type::SUB, pal.ink_muted, w);
        let try_label = tr!("live-try-again");
        let diag_label = cx.diag.label();
        let try_w = Button::primary(&try_label).width(ctx);
        let diag_w = Button::secondary(&diag_label).icon(Icon::Copy).width(ctx);
        let buttons_wrap = try_w + 8.0 + diag_w > w;
        Attention {
            w,
            title,
            body,
            try_label,
            diag_label,
            buttons_wrap,
        }
    }

    fn show(self, ui: &mut Ui, cx: &mut Cx<'_>, x: f32, mut y: f32) -> f32 {
        let ctx = ui.ctx().clone();
        if let Some(title) = self.title {
            y = text_at(ui, title, pos2(x, y)).bottom() + 14.0;
        }
        y = text_at(ui, self.body, pos2(x, y)).bottom() + 14.0 + 4.0;
        let try_b = Button::primary(&self.try_label);
        let diag_b = Button::secondary(&self.diag_label).icon(Icon::Copy);
        let (try_w, diag_w) = (try_b.width(&ctx), diag_b.width(&ctx));
        if util::put(ui, rect(x, y, try_w, 40.0), "takeover-retry", try_b).clicked() {
            cx.send(Action::Retry);
        }
        let (dx, dy) = if self.buttons_wrap {
            (x, y + 48.0)
        } else {
            (x + try_w + 8.0, y)
        };
        if util::put(ui, rect(dx, dy, diag_w, 40.0), "takeover-diag", diag_b).clicked() {
            cx.diag.start();
        }
        y += if self.buttons_wrap { 88.0 } else { 40.0 } + 14.0;
        let link_h = Type::LINK.line;
        let mut c = child(ui, rect(x, y, self.w, link_h), "takeover-guide");
        if widgets::link(&mut c, &tr!("live-troubleshooting"), true, false).clicked() {
            util::open_url(util::url::TROUBLESHOOTING);
        }
        y + link_h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock::{state, Scenario};
    use crate::appstate::Page;
    use crate::pages::testing;
    use crate::pages::testing::Role;
    use egui_kittest::kittest::{NodeT, Queryable};

    #[test]
    fn the_picture_tells_screen_readers_the_mic_it_names() {
        let name = picture_name(&state(Scenario::OnInCall));
        for compact in [true, false] {
            let w = if compact {
                testing::compact(Scenario::OnInCall, Page::Live)
            } else {
                testing::scenario(Scenario::OnInCall, Page::Live)
            };
            let pic = testing::by_role(&w.harness, &name, Role::Image);
            assert_eq!(
                pic.accesskit_node().description().as_deref(),
                Some(tr!("live-pic-mic", mic = "RODE NT-USB").as_str()),
                "compact={compact}"
            );
        }
    }

    /// A good level's dot is the accent; a note's the warn colour.
    #[test]
    fn a_good_levels_dot_is_the_accent() {
        use crate::ui::{tokens::Palette, Theme};
        for theme in [Theme::Dark, Theme::Light] {
            let pal = Palette::of(theme);
            assert_eq!(level_dot(Dot::Good, &pal), pal.accent);
            assert_eq!(level_dot(Dot::Warn, &pal), pal.warn);
            assert_eq!(level_dot(Dot::Waiting, &pal), pal.ink_muted);
        }
    }

    /// One row of controls under the picture, at the small size: the
    /// strength leads it and the two buttons end it. Where the window is
    /// narrower Replay takes its short label (and keeps its name); at the
    /// least window the buttons stand under the strength, as wide as it
    /// between them. The picture is the larger part of the card at every
    /// size. That Space holds too is Hold's tooltip and description.
    #[test]
    fn one_row_of_controls_stands_under_the_picture() {
        crate::i18n::pin_english();
        for (size, short, wraps) in [
            (egui::vec2(1168.0, 704.0), false, false),
            (egui::vec2(960.0, 640.0), true, false),
            (egui::vec2(720.0, 520.0), false, true),
        ] {
            // with a take (as the screenshots fill it)
            let mut w = testing::with_setup(
                crate::window::Link::recording(state(Scenario::OnInCall)),
                Page::Live,
                size,
                |p| {
                    p.no_launch();
                    p.prefill(180);
                },
            );
            w.harness.run_steps(3);
            let replay = w.harness.get_by_label("Replay the last 10 seconds").rect();
            let hold = w.harness.get_by_label("Hold to hear the original");
            assert_eq!(
                hold.accesskit_node().description().as_deref(),
                Some(
                    "Press to switch, hold to compare. Space does the same from anywhere on \
                     the page."
                ),
                "{size:?}"
            );
            let hold = hold.rect();
            let first = testing::by_role(&w.harness, "Off", Role::RadioButton).rect();
            let last = testing::by_role(&w.harness, "Maximum", Role::RadioButton).rect();
            assert_eq!((replay.height(), hold.height()), (32.0, 32.0), "{size:?}");
            assert!(first.height() <= 32.0, "{size:?}: {first:?}");
            assert!(w.harness.query_by_label_contains("or hold").is_none());
            // the short label is painted, the sentence stays the name
            let painted_short = w.harness.query_by_label("Replay 10 s").is_some();
            assert!(!painted_short, "{size:?}: its name is the sentence");
            let long = Button::secondary("Replay the last 10 seconds")
                .sm()
                .icon(Icon::Replay)
                .width(&w.harness.ctx);
            assert_eq!(replay.width() < long - 0.5, short, "{size:?}: {replay:?}");
            let fig = fig_of(&w);
            let (left, right) = (fig.left() + 1.0 + CARD_PAD, fig.right() - 1.0 - CARD_PAD);
            assert!((first.left() - left).abs() < 3.0, "{size:?}: {first:?}");
            assert!((hold.right() - right).abs() < 0.5, "{size:?}: {hold:?}");
            if wraps {
                // two lines, both ending on one edge, the buttons equal
                assert!(replay.top() >= first.bottom() + 4.0, "{size:?}");
                assert_eq!(replay.top(), hold.top(), "{size:?}");
                assert!((replay.left() - left).abs() < 0.5, "{size:?}");
                assert!((last.right() - right).abs() < 3.0, "{size:?}: {last:?}");
                assert!((replay.width() - hold.width()).abs() < 0.5, "{size:?}");
            } else {
                assert!(replay.left() > last.right() + 8.0, "{size:?}");
                assert!((replay.center().y - first.center().y).abs() < 1.0);
                assert_eq!(replay.top(), hold.top(), "{size:?}");
            }
            // the picture is the larger part of the card
            let picture = testing::by_role(
                &w.harness,
                &picture_name(&state(Scenario::OnInCall)),
                Role::Image,
            )
            .rect();
            assert!(
                picture.height() > fig.height() / 2.0,
                "{size:?}: {} of {}",
                picture.height(),
                fig.height()
            );
        }
    }

    /// The microphone is chosen in its lane's caption, and its level
    /// stands at that row's other end; the lanes start on the controls'
    /// left edge.
    #[test]
    fn the_microphones_caption_is_its_choice_and_ends_in_its_level() {
        crate::i18n::pin_english();
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let s = state(Scenario::OnInCall);
            let w = full(s.clone(), None, size);
            let picture = testing::by_role(&w.harness, &picture_name(&s), Role::Image).rect();
            let z = Sizes::of(size.x < 840.0, show_of(&w).lane);
            let row =
                egui::Rangef::new(picture.top() + z.pad_top, picture.top() + z.pad_top + 32.0);
            let mic = testing::by_role(&w.harness, "Microphone", Role::ComboBox);
            assert_eq!(
                mic.accesskit_node().value().as_deref(),
                Some("RODE NT-USB"),
                "{size:?}"
            );
            let mic = mic.rect();
            assert!(row.contains(mic.center().y), "{size:?}: {mic:?}");
            assert!(mic.height() <= 26.0, "{size:?}: a part of the caption");
            let level = w.harness.get_by_label("Level Sounds good").rect();
            assert!(row.contains(level.center().y), "{size:?}: {level:?}");
            assert!(level.left() > mic.right(), "{size:?}");
            let lane_right = picture.right() - z.pad_x;
            assert!(lane_right - level.right() < 12.0, "{size:?}: {level:?}");
            // one left edge for the lanes and the controls
            let first = testing::by_role(&w.harness, "Off", Role::RadioButton).rect();
            assert!((picture.left() + z.pad_x - first.left()).abs() < 3.0);
            // what is written stands on the solid part of its patch, and
            // the picture between the two groups has none
            let painted = w
                .harness
                .ctx
                .data(|d| d.get_temp::<spectro::Painted>(egui::Id::new("hm-live-painted")))
                .unwrap();
            let [[left, right], [apps, none]] = painted.solid;
            let lane = painted.lanes[0];
            assert!(left.left() == lane.left() && left.top() == lane.top());
            assert!(right.right() == lane.right() && right.top() == lane.top());
            assert!(
                left.contains_rect(mic.shrink2(egui::vec2(0.0, 3.0))),
                "{size:?}"
            );
            assert!(
                right.contains_rect(level),
                "{size:?}: {level:?} in {right:?}"
            );
            assert!(right.left() - left.right() > 100.0, "{size:?}");
            for (row, solid) in painted.rows.iter().zip([left, apps]) {
                assert!(solid.y_range().contains(row.top()), "{size:?}");
                assert!(solid.y_range().contains(row.bottom()), "{size:?}");
            }
            let name = painted.names[1].0;
            assert!(apps.contains_rect(name), "{size:?}: {name:?} in {apps:?}");
            assert!(apps.right() - name.right() <= 8.5, "{size:?}");
            assert_eq!(none, egui::Rect::NOTHING, "{size:?}");
        }
        // a take that plays has no level: it describes the live microphone
        let size = egui::vec2(960.0, 640.0);
        let s = state(Scenario::OnInCall);
        let w = full(s, Some(replay::Preset::Replaying), size);
        assert!(w.harness.query_by_label_contains("Level").is_none());
        assert_eq!(show_of(&w).mic.tail, 0.0);
    }

    /// Every problem the page can have, the states that raise it, and
    /// whether it shows as a notice (what the header and the picture say
    /// already is none).
    fn problems() -> Vec<(&'static str, AppState, Option<replay::Preset>, bool)> {
        use replay::Preset;
        let on = || state(Scenario::OnInCall);
        let with = |f: fn(&mut AppState)| {
            let mut s = on();
            f(&mut s);
            s
        };
        vec![
            ("pipewire", state(Scenario::PipewireUnreachable), None, true),
            ("stopped", state(Scenario::NeedsAttention), None, true),
            ("flapping", with(|s| s.engine.flapping = true), None, true),
            ("off", state(Scenario::Off), None, false),
            ("starting", state(Scenario::Starting), None, false),
            ("reconnecting", state(Scenario::Reconnecting), None, false),
            ("mic-stuck", with(|s| s.mic.stuck = true), None, true),
            ("rerouted", with(|s| s.mic.rerouted = true), None, true),
            ("silent", state(Scenario::MicSilent), None, true),
            (
                "absent",
                with(|s| {
                    let sel = s.mic.selected.clone();
                    for d in s
                        .mic
                        .devices
                        .iter_mut()
                        .filter(|d| Some(&d.name) == sel.as_ref())
                    {
                        d.present = false;
                    }
                }),
                None,
                true,
            ),
            ("fallback", state(Scenario::MicUnplugged), None, true),
            ("quiet", on(), Some(Preset::Quiet), true),
            ("loud", on(), Some(Preset::Loud), true),
            ("light-now", state(Scenario::LightForNow), None, true),
            ("busy", state(Scenario::UnfilteredForNow), None, true),
            ("bypass", state(Scenario::Bypass), None, false),
            ("muted", state(Scenario::Muted), None, false),
        ]
    }

    /// A notice shows whole at the default and the least window size (its
    /// words are its name; nothing cuts it short); a state that is only
    /// worth knowing shows none.
    #[test]
    fn a_notice_shows_whole_and_only_for_what_needs_attention() {
        use replay::Preset;
        crate::i18n::pin_english();
        for (name, s, preset, noted) in problems() {
            for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
                let mut w = testing::with_setup(
                    crate::window::Link::recording(s.clone()),
                    Page::Live,
                    size,
                    move |p| {
                        // as the screenshots set it up (`scenes.rs`)
                        p.no_launch();
                        p.live.replay.preset = preset;
                        if name == "silent" {
                            p.live.silent_demo();
                        }
                        p.prefill(200);
                    },
                );
                w.harness.run_steps(12);
                let level = match preset {
                    Some(Preset::Quiet) => Some(LevelShow::Note {
                        word: tr!("miccheck-level-quiet"),
                        lead: tr!("mic-level-quiet-note"),
                        body: tr!("mic-level-quiet-advice"),
                    }),
                    Some(Preset::Loud) => Some(LevelShow::Note {
                        word: tr!("miccheck-level-loud"),
                        lead: tr!("mic-level-loud-note"),
                        body: tr!("mic-level-loud-advice"),
                    }),
                    _ => None,
                };
                let p = problem(&s, name == "silent", level.as_ref())
                    .unwrap_or_else(|| panic!("{name}: no problem"));
                assert_eq!(p.noted(), noted, "{name}");
                let words = p.words();
                // (the line under the strength that only screen readers
                // get may say the same words: it is a point, no notice)
                let shown = w
                    .harness
                    .query_all_by_label(&words)
                    .find(|n| n.rect().height() > 2.0);
                let Some(notice) = shown else {
                    assert!(!noted, "{name} at {}: not shown: {words}", size.x);
                    continue;
                };
                assert!(noted, "{name} at {}: shown: {words}", size.x);
                // every line of it, inside the card
                let fig = fig_of(&w);
                let rows = text::layout(
                    &w.harness.ctx,
                    &words,
                    Type::CAP,
                    Color32::WHITE,
                    notice.rect().width(),
                )
                .size()
                .y;
                assert!(
                    notice.rect().height() + 0.5 >= rows && fig.contains_rect(notice.rect()),
                    "{name} at {}: {:?} in {fig:?}",
                    size.x,
                    notice.rect()
                );
            }
        }
    }

    /// The card this window shows, and its controls' bottom.
    fn fig_of(w: &testing::Window) -> Rect {
        w.harness
            .ctx
            .data(|d| d.get_temp::<Rect>(egui::Id::new("hm-live-fig")))
            .unwrap()
    }

    fn controls_of(w: &testing::Window) -> f32 {
        w.harness
            .ctx
            .data(|d| d.get_temp::<f32>(egui::Id::new("hm-live-controls")))
            .unwrap()
    }

    /// What the picture was asked to draw.
    fn show_of(w: &testing::Window) -> Show {
        w.harness
            .ctx
            .data(|d| d.get_temp::<Show>(egui::Id::new("hm-live-show")))
            .unwrap()
    }

    /// A window on `s` as the screenshots set it up: the last 10 seconds
    /// full, `preset` its replay's state.
    fn full(s: AppState, preset: Option<replay::Preset>, size: egui::Vec2) -> testing::Window {
        let mut w = testing::with_setup(
            crate::window::Link::recording(s),
            Page::Live,
            size,
            move |p| {
                p.no_launch();
                p.live.replay.preset = preset;
                p.prefill(200);
            },
        );
        w.harness.run_steps(6);
        w
    }

    /// Each state reads from the picture: which lanes hold sound, and what
    /// stands in one that holds none.
    #[test]
    fn the_lanes_say_the_state() {
        crate::i18n::pin_english();
        let size = egui::vec2(960.0, 640.0);
        let show = |s: AppState| show_of(&full(s, None, size));
        let note = |l: &Lane| l.note.as_ref().map(|(_, words)| words.clone());
        let on = show(state(Scenario::OnInCall));
        assert_eq!(
            (on.mic.label.as_str(), on.mic.name.as_str()),
            ("Microphone", "RODE NT-USB")
        );
        assert_eq!(on.apps.label, "What apps hear");
        assert!(on.mic.sound && on.apps.sound && on.apps.strong);
        assert_eq!((note(&on.mic), note(&on.apps)), (None, None));
        assert!(on.playing.is_none());
        let ready = show(state(Scenario::OnNoApp));
        assert_eq!(ready.apps.name, "No app right now");
        assert!(ready.apps.sound && !ready.apps.strong);
        // noise suppression off: both lanes hold the same sound, and the
        // lower one's caption says that it is unfiltered
        let bypass = show(state(Scenario::Bypass));
        assert!(bypass.mic.sound && bypass.apps.sound);
        assert!(
            bypass.apps.name.ends_with("· unfiltered"),
            "{:?}",
            bypass.apps.name
        );
        assert!(!on.apps.name.contains("unfiltered"));
        assert_eq!(note(&bypass.apps), None);
        // muted: the microphone goes on, apps get nothing, and their
        // lane's caption says so over a lane that is shut
        let muted = show(state(Scenario::Muted));
        assert!(muted.mic.sound && !muted.apps.sound);
        assert_eq!(
            (muted.apps.label.as_str(), muted.apps.name.as_str()),
            ("What apps hear", "Nothing, muted")
        );
        assert!(muted.apps.shut && muted.apps.glyph == Icon::MicOff);
        assert_eq!(note(&muted.apps), None);
        assert!(!on.apps.shut && on.apps.glyph == Icon::AppGlyph);
        // off: two empty lanes and nothing to read in them
        let off = show(state(Scenario::Off));
        assert!(!off.mic.sound && !off.apps.sound && !off.mic.strong);
        assert_eq!((note(&off.mic), note(&off.apps)), (None, None));
        let starting = show(state(Scenario::Starting));
        assert!(!starting.mic.sound && !starting.apps.sound);
        assert_eq!(
            note(&starting.mic).as_deref(),
            Some("Connecting to your microphone…")
        );
        // no sound comes in: neither lane draws one
        let mut s = state(Scenario::OnInCall);
        s.mic.silent = true;
        let silent = show(s);
        assert!(!silent.mic.sound && !silent.apps.sound);
        assert_eq!(note(&silent.mic).as_deref(), Some("No sound"));
        // the chosen microphone gone, with nothing standing in
        let mut s = state(Scenario::MicUnplugged);
        s.mic.fallback = false;
        s.mic.active = None;
        let gone = show(s);
        assert!(!gone.mic.sound && !gone.mic.strong);
        assert_eq!(note(&gone.mic).as_deref(), Some("Unplugged"));
    }

    /// A replay shows in the picture: the playhead, and which lane is
    /// heard; the hold crosses over to the microphone's.
    #[test]
    fn a_replay_shows_which_lane_is_heard() {
        use replay::Preset;
        crate::i18n::pin_english();
        let size = egui::vec2(960.0, 640.0);
        let s = state(Scenario::OnInCall);
        let playing = show_of(&full(s.clone(), Some(Preset::Replaying), size))
            .playing
            .expect("a playhead");
        assert!(playing.position > 0.0 && playing.position < 1.0);
        assert_eq!(playing.original, 0.0);
        for preset in [Preset::Holding, Preset::Latched] {
            let held = show_of(&full(s.clone(), Some(preset), size))
                .playing
                .unwrap();
            assert!(held.original > 0.99, "{preset:?}: {}", held.original);
        }
        // a muted take replays all the same: no note covers it
        let muted = show_of(&full(state(Scenario::Muted), Some(Preset::Replaying), size));
        assert!(muted.apps.note.is_none() && muted.apps.sound);
    }

    /// What the standing sentences said is on the controls: their
    /// tooltips, which screen readers get as descriptions.
    #[test]
    fn the_tooltips_reach_screen_readers() {
        crate::i18n::pin_english();
        let size = egui::vec2(960.0, 640.0);
        let described = |w: &testing::Window, label: &str, role: Role| {
            testing::by_role(&w.harness, label, role)
                .accesskit_node()
                .description()
        };
        let keeps = "HushMic keeps the last 10 seconds while Live is open. Nothing is saved.";
        let mut w = full(state(Scenario::OnInCall), None, size);
        assert_eq!(
            described(&w, "Replay the last 10 seconds", Role::Button).as_deref(),
            Some(keeps)
        );
        assert_eq!(
            described(&w, "Microphone", Role::ComboBox).as_deref(),
            Some("RODE NT-USB. Strength is remembered for each microphone.")
        );
        assert_eq!(
            described(&w, "Gentle", Role::RadioButton).as_deref(),
            Some("Takes the edge off. Most of the room sound stays.")
        );
        // none of it stands on the page
        for words in [keeps, "Strength is remembered for each microphone."] {
            assert!(
                w.harness.query_by_label_contains(words).is_none(),
                "{words}"
            );
        }
        assert!(w
            .harness
            .query_by_label("Nothing needs your attention.")
            .is_none());
        // and it comes up on hover
        // (under its short label the tooltip leads with the sentence)
        w.harness.get_by_label("Replay the last 10 seconds").hover();
        w.harness.run_steps(6);
        assert!(w.harness.query_by_label_contains(keeps).is_some());
        assert_eq!(
            described(&w, "Off", Role::RadioButton).as_deref(),
            Some("Apps hear your raw microphone.")
        );
        // while HushMic is off a step says when it applies
        let w = full(state(Scenario::Off), None, size);
        assert_eq!(
            described(&w, "Strong", Role::RadioButton).as_deref(),
            Some("Most of the noise is gone. Applies when noise suppression is on.")
        );
    }

    /// The card reaches the page's bottom padding at every size, and the
    /// two lanes share the height that leaves. A notice of two rows takes
    /// its room from them, and still fits the least window.
    #[test]
    fn the_lanes_take_the_height_the_window_leaves() {
        crate::i18n::pin_english();
        for (size, pad, lane) in [
            (egui::vec2(720.0, 520.0), PAD_COMPACT.2, 146.0),
            (egui::vec2(960.0, 640.0), PAD.2, 214.0),
            (egui::vec2(1168.0, 704.0), PAD.2, 246.0),
            (egui::vec2(1168.0, 1200.0), PAD.2, 494.0),
        ] {
            let limit = size.y - pad;
            for sc in [
                Scenario::OnInCall,
                Scenario::OnNoApp,
                Scenario::Bypass,
                Scenario::Muted,
                Scenario::Starting,
                Scenario::Off,
            ] {
                let w = full(state(sc), None, size);
                let fig = fig_of(&w);
                assert_eq!(show_of(&w).lane, lane, "{sc:?} at {size:?}");
                assert!(fig.bottom() <= limit + 0.5, "{sc:?} at {size:?}: {fig:?}");
                // and no page ends in a void
                assert!(fig.bottom() >= limit - 2.0, "{sc:?} at {size:?}: {fig:?}");
            }
        }
        // words and a link, at the least window
        let s = state(Scenario::LightForNow);
        let w = full(s.clone(), None, egui::vec2(720.0, 520.0));
        let words = problem(&s, false, None).unwrap().words();
        let notice = w.harness.get_by_label(&words).rect();
        let link = testing::by_role(&w.harness, "Always use Light", Role::Link).rect();
        let fig = fig_of(&w);
        let lane = show_of(&w).lane;
        assert!((spectro::LANE_MIN..146.0).contains(&lane), "{lane}");
        assert!(fig.bottom() <= 520.0 - PAD_COMPACT.2 + 0.5, "{fig:?}");
        assert!(fig.contains_rect(notice) && fig.contains_rect(link));
        // the notice is the card's first thing, over the picture
        let picture = testing::by_role(&w.harness, &picture_name(&s), Role::Image).rect();
        assert!(notice.top() > fig.top() && link.bottom() <= picture.top() + 0.5);
    }

    /// The line the replay row says stands between the strength and the
    /// buttons where the row has room for it, else on a line under the
    /// row that is there only then (the lanes give it its room); either
    /// way the controls stay where they are.
    #[test]
    fn the_rows_line_stands_in_the_row_or_under_it() {
        use replay::Preset;
        crate::i18n::pin_english();
        let said = "Same-moment switching isn’t possible for this take. \
                    Each side plays from the start.";
        for size in [egui::vec2(1168.0, 704.0), egui::vec2(720.0, 520.0)] {
            let calm = full(state(Scenario::OnInCall), None, size);
            // a replay that ended says nothing: the page is as before it
            let ended = full(state(Scenario::OnInCall), Some(Preset::Result), size);
            assert_eq!(show_of(&ended).lane, show_of(&calm).lane, "{size:?}");
            let w = full(state(Scenario::OnInCall), Some(Preset::Sequential), size);
            let line = w.harness.get_by_label(said).rect();
            // a take like this picks its side with a press: the button says so
            let hold = w.harness.get_by_label("Hear the original").rect();
            let replay = w.harness.get_by_label("Replay the last 10 seconds").rect();
            let last = testing::by_role(&w.harness, "Maximum", Role::RadioButton).rect();
            let beside = line.left() > last.right() && line.right() < replay.left();
            if beside {
                assert!(
                    line.top() >= hold.top() - 2.0 && line.bottom() <= hold.bottom() + 2.0,
                    "{size:?}: {line:?} beside {hold:?}"
                );
                assert_eq!(show_of(&w).lane, show_of(&calm).lane, "{size:?}");
            } else {
                // English leaves it no room in the row at these sizes
                assert!(line.top() >= hold.bottom(), "{size:?}: {line:?}");
                assert_eq!(line.left(), fig_of(&w).left() + 1.0 + CARD_PAD, "{size:?}");
                assert!(show_of(&w).lane < show_of(&calm).lane, "{size:?}");
            }
            assert!(
                (controls_of(&w) - controls_of(&calm)).abs() < 0.5,
                "{size:?}: the controls moved"
            );
            assert!((fig_of(&w).bottom() - fig_of(&calm).bottom()).abs() < 0.5);
        }
    }

    /// The picture holds nothing while another page is up: it goes with
    /// the audio it was made from.
    #[test]
    fn the_picture_goes_when_live_hides() {
        crate::i18n::pin_english();
        let ctx = egui::Context::default();
        let mut pages = crate::pages::Pages::new(&ctx, crate::pages::Feed::Demo);
        pages.no_launch();
        pages.prefill(200);
        let link = crate::window::Link::recording(state(Scenario::OnInCall));
        let frame = |pages: &mut crate::pages::Pages, mut page: Page, t: f64| {
            let input = egui::RawInput {
                time: Some(t),
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 640.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| pages.show(ctx, &link, &mut page));
        };
        frame(&mut pages, Page::Live, 1.0);
        let cols = crate::miccheck::spectrum::COLS as u64;
        assert_eq!(pages.live.picture_state(), Some((cols, false)));
        frame(&mut pages, Page::Settings, 1.1);
        assert_eq!(pages.live.picture_state(), None);
        assert!(!pages.live.replay.listening());
    }

    /// A microphone that does not connect (the healer gave up restarting):
    /// the notice says so with the ways out, above a thief's (whose mic at
    /// least sounds) and above "No sound", and holds the notification back.
    #[test]
    fn a_mic_that_does_not_connect_says_so_first() {
        crate::i18n::pin_english();
        let mut s = state(Scenario::OnInCall);
        s.mic.stuck = true;
        s.mic.rerouted = true;
        let p = problem(&s, true, None).unwrap();
        assert_eq!(
            p.words(),
            format!(
                "{} isn’t connecting. Unplug it and plug it back in.",
                mic_name(&s)
            )
        );
        assert_eq!(p.fixes, vec![Fix::ChooseMic]);
        assert_eq!(p.notify, Some(crate::notify::Problem::MicStuck));
        s.mic.stuck = false;
        let p = problem(&s, true, None).unwrap();
        assert_eq!(p.notify, Some(crate::notify::Problem::Rerouted));
    }

    /// Another app taking the microphone: the notice gives the advice in
    /// full, with the other way out.
    #[test]
    fn the_rerouted_advice_shows_whole() {
        crate::i18n::pin_english();
        let mut s = state(Scenario::OnInCall);
        s.mic.rerouted = true;
        let p = problem(&s, false, None).unwrap();
        let full = "Another app keeps taking this microphone. In EasyEffects, turn off \
                    Process All Input Streams, or exclude hushmic_input from it.";
        assert_eq!(p.words(), full);
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let w = testing::window(s.clone(), Page::Live, size);
            assert!(w.harness.query_by_label(full).is_some(), "{size:?}");
        }
    }

    /// With no microphone chosen, the picture's mic end names the system
    /// default's device, as the microphone list does ("System default" only
    /// when it can't be named); with the chosen one unplugged, the
    /// stand-in in use.
    #[test]
    fn the_picture_names_the_system_defaults_device() {
        let mut s = state(Scenario::OnInCall);
        s.mic.selected = None;
        assert_eq!(picture_mic(&s), "Built-in Audio");
        assert_eq!(picture_mic(&s), mic_name(&s));
        let w = testing::window(s.clone(), Page::Live, egui::vec2(960.0, 640.0));
        let name = picture_name(&s);
        let pic = testing::by_role(&w.harness, &name, Role::Image);
        assert_eq!(
            pic.accesskit_node().description().as_deref(),
            Some(tr!("live-pic-mic", mic = "Built-in Audio").as_str())
        );
        s.mic.system_default = None;
        assert_eq!(picture_mic(&s), tr!("live-system-default"));

        // the chosen mic unplugged: the label and its description name the
        // stand-in in use, not the chosen one (nor the system default)
        let mut s = state(Scenario::MicUnplugged);
        let stand_in = s
            .mic
            .devices
            .iter()
            .find(|d| d.description == "USB Headset")
            .map(|d| d.name.clone());
        s.mic.active = stand_in;
        assert_eq!(picture_mic(&s), "USB Headset");
        let w = testing::window(s.clone(), Page::Live, egui::vec2(960.0, 640.0));
        let name = picture_name(&s);
        let pic = testing::by_role(&w.harness, &name, Role::Image);
        assert_eq!(
            pic.accesskit_node().description().as_deref(),
            Some(tr!("live-pic-mic", mic = "USB Headset").as_str())
        );
    }

    /// The level is judged from where an app began to hear HushMic (the
    /// daemon's mark), on open and when an app starts using it while Live
    /// shows; with no mark nothing old counts.
    #[test]
    fn live_judges_the_past_only_from_the_use_mark() {
        use hushmic_shm::meter::{MeterWriter, MODE_PROCESS};
        crate::i18n::pin_english();
        let dir = std::env::temp_dir().join(format!("hm-pages-mark-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::meter::prepare_dir(&dir).unwrap();
        let path = crate::meter::path_for_generation(&dir, 1);
        let mut writer = MeterWriter::create(&path, 1, 0).unwrap();
        let ctx = egui::Context::default();
        let mut pages = crate::pages::Pages::new(&ctx, crate::pages::Feed::Meter);
        pages.no_launch();
        let link_of = |sc: Scenario, mark: Option<u64>| {
            let mut s = state(sc);
            s.engine.meter = Some(path.to_string_lossy().into_owned());
            s.engine.use_began_ms = mark;
            crate::window::Link::recording(s)
        };
        let frame = |pages: &mut crate::pages::Pages, link: &crate::window::Link, t: f64| {
            let input = egui::RawInput {
                time: Some(t),
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 640.0),
                )),
                ..Default::default()
            };
            let mut page = Page::Live;
            let _ = ctx.run(input, |ctx| pages.show(ctx, link, &mut page));
        };
        // 4 s of sound; an app began to hear it 1 s before the newest hop
        // (20 columns). Dated ahead of the clock (a hop every 10 ms): the
        // page looks within the meter's idle gap of the newest hop however
        // late a loaded runner gets to it.
        let voice = vec![0.5f32; 480];
        let at = std::cell::Cell::new(hushmic_shm::monotonic_ms() + 600_000);
        let sound = |writer: &mut MeterWriter| {
            for _ in 0..400 {
                at.set(at.get() + 10);
                writer.publish_at(&voice, &voice, MODE_PROCESS, at.get());
            }
        };
        let second_ago = || Some(at.get() - 1000);
        let near_20 = |pages: &crate::pages::Pages| {
            let n = pages.live.history_taken;
            assert!((18..=21).contains(&n), "{n} columns");
        };
        // opened mid-call
        sound(&mut writer);
        frame(&mut pages, &link_of(Scenario::OnInCall, second_ago()), 1.0);
        near_20(&pages);
        // no app, then one starts using HushMic while Live shows
        frame(&mut pages, &link_of(Scenario::OnNoApp, None), 1.1);
        pages.live.history_taken = 0;
        sound(&mut writer);
        frame(&mut pages, &link_of(Scenario::OnInCall, second_ago()), 1.2);
        near_20(&pages);
        // in use with no mark: nothing old
        frame(&mut pages, &link_of(Scenario::OnNoApp, None), 1.3);
        pages.live.history_taken = 0;
        sound(&mut writer);
        frame(&mut pages, &link_of(Scenario::OnInCall, None), 1.4);
        assert_eq!(pages.live.history_taken, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// On the real meter, with a picture that stands still (no audio
    /// reaches the page's own backend here), Live reads the meter four
    /// times a second while audio flows: it finds a silent microphone, and
    /// the sound after it. An app corking its stream lays it down to a
    /// look a second, and sound brings the looks back. Counted in frames
    /// run as eframe runs them (each when the page asks), not in seconds,
    /// so a slow runner only makes it take longer.
    #[test]
    fn live_reads_the_meter_at_rest_and_slows_down_on_a_cork() {
        use hushmic_shm::meter::{MeterWriter, MODE_PROCESS};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        // only a hang waits this long
        const HANG: Duration = Duration::from_secs(120);
        crate::i18n::pin_english();
        let dir = std::env::temp_dir().join(format!("hm-pages-floor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::meter::prepare_dir(&dir).unwrap();
        let path = crate::meter::path_for_generation(&dir, 1);
        let mut writer = MeterWriter::create(&path, 1, 0).unwrap();
        let (loud, stop) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (corked, cork_seen) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (l, st, ck, seen) = (
            loud.clone(),
            stop.clone(),
            corked.clone(),
            cork_seen.clone(),
        );
        // the plugin: one 10 ms hop at a time, digital silence or speech;
        // nothing while the app corks its stream
        let feeder = std::thread::spawn(move || {
            let (zero, voice) = (vec![0.0f32; 480], vec![0.5f32; 480]);
            while !st.load(Ordering::Relaxed) {
                let c = ck.load(Ordering::Relaxed);
                seen.store(c, Ordering::Relaxed);
                if !c {
                    let hop = if l.load(Ordering::Relaxed) {
                        &voice
                    } else {
                        &zero
                    };
                    writer.publish(hop, hop, MODE_PROCESS);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let mut s = state(Scenario::OnInCall);
        s.engine.meter = Some(path.to_string_lossy().into_owned());
        let link = crate::window::Link::recording(s.clone());
        let ctx = egui::Context::default();
        let mut pages = crate::pages::Pages::new(&ctx, crate::pages::Feed::Meter);
        pages.no_launch();
        let mut page = Page::Live;
        let t0 = Instant::now();
        // one frame as eframe runs it, then the wait it asks for: its
        // repaint delay
        let mut frame = |pages: &mut crate::pages::Pages| -> Duration {
            let input = egui::RawInput {
                time: Some(t0.elapsed().as_secs_f64()),
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 640.0),
                )),
                ..Default::default()
            };
            let out = ctx.run(input, |ctx| pages.show(ctx, &link, &mut page));
            let d = out.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            std::thread::sleep(d.min(Duration::from_millis(500)));
            d
        };
        // frames go on until `done` says so
        let mut until =
            |pages: &mut crate::pages::Pages,
             what: &str,
             done: &dyn Fn(&crate::pages::Pages, Duration) -> bool| {
                let t = Instant::now();
                loop {
                    let d = frame(pages);
                    if done(pages, d) {
                        break;
                    }
                    assert!(t.elapsed() < HANG, "never {what}: {d:?}");
                }
            };
        // digital silence: the page finds the microphone silent, and with
        // nothing to move it looks four times a second
        until(&mut pages, "silent", &|p, _| p.live.silent(&s));
        until(&mut pages, "at rest", &|_, d| d == REST_LOOK);
        for _ in 0..4 {
            until(&mut pages, "at rest", &|_, d| d == REST_LOOK);
            assert!(!pages.live.moving());
            assert!(pages.live.silent(&s));
        }
        // speech: the next looks hear it
        loud.store(true, Ordering::Relaxed);
        until(&mut pages, "heard", &|p, _| !p.live.silent(&s));
        // silence again, then the app corks its stream: a look a second
        loud.store(false, Ordering::Relaxed);
        corked.store(true, Ordering::Relaxed);
        let t = Instant::now();
        while !cork_seen.load(Ordering::Relaxed) {
            assert!(t.elapsed() < HANG, "the feeder stalled");
            std::thread::sleep(Duration::from_millis(10));
        }
        until(&mut pages, "idle", &|_, d| d >= Duration::from_millis(900));
        // it sends again: four looks a second
        corked.store(false, Ordering::Relaxed);
        until(&mut pages, "flowing again", &|_, d| d == REST_LOOK);
        stop.store(true, Ordering::Relaxed);
        feeder.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn running_again_shows_for_four_seconds_after_a_recovery_on_live() {
        crate::i18n::pin_english();
        let ctx = egui::Context::default();
        let mut pages = crate::pages::Pages::new(&ctx, crate::pages::Feed::Demo);
        pages.no_launch();
        let at = |recovered: u64| {
            let mut s = state(Scenario::OnInCall);
            s.engine.recovered = recovered;
            crate::window::Link::recording(s)
        };
        let frame =
            |pages: &mut crate::pages::Pages, link: &crate::window::Link, page: Page, t: f64| {
                let mut p = page;
                let input = egui::RawInput {
                    time: Some(t),
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(960.0, 640.0),
                    )),
                    ..Default::default()
                };
                let _ = ctx.run(input, |ctx| pages.show(ctx, link, &mut p));
            };
        let title = |pages: &crate::pages::Pages, t: f64| {
            let s = state(Scenario::OnInCall);
            pages.live.head_now(&ctx, &s, t)
        };
        let (zero, one, two) = (at(0), at(1), at(2));
        for t in [1.0, 1.1, 1.2] {
            frame(&mut pages, &zero, Page::Live, t);
        }
        assert_eq!(title(&pages, 1.2), "Removing background noise.");
        // the daemon recovered while Live is up
        frame(&mut pages, &one, Page::Live, 1.3);
        assert_eq!(title(&pages, 1.3), "Running again.");
        assert_eq!(title(&pages, 5.2), "Running again.");
        assert_eq!(title(&pages, 5.4), "Removing background noise.");
        // muted, the header keeps saying so
        let mut muted = state(Scenario::Muted);
        muted.engine.recovered = 1;
        assert_eq!(pages.live.head_now(&ctx, &muted, 1.5), head_copy(&muted));
        // a recovery while another page is up: the daemon said it there
        frame(&mut pages, &one, Page::Settings, 6.0);
        frame(&mut pages, &two, Page::Settings, 6.1);
        frame(&mut pages, &two, Page::Live, 6.2);
        assert_eq!(title(&pages, 6.2), "Removing background noise.");
    }

    #[test]
    fn the_app_label_fits_its_room() {
        crate::i18n::pin_english();
        let ctx = egui::Context::default();
        crate::ui::install(&ctx, crate::ui::Theme::Dark);
        let using = |names: &[&str]| {
            let mut s = state(Scenario::OnInCall);
            s.apps.using = names
                .iter()
                .map(|n| crate::appstate::AppRef {
                    id: n.to_lowercase(),
                    name: Some(n.to_string()),
                })
                .collect();
            s
        };
        // the room a caption leaves a name at the default size
        let max = 0.22 * 710.0;
        let mut got = Vec::new();
        let _ = ctx.run(Default::default(), |ctx| {
            for names in [
                &["Zoom"][..],
                &["Zoom", "Discord"],
                &["Microsoft Teams (work or school)", "OBS Studio"],
                &["Zoom", "OBS Studio", "Discord"],
                &["Zoom", "OBS Studio", "Discord", "Firefox"],
            ] {
                got.push(app_label(ctx, &using(names), max).map(|((t, _), all)| (t, all)));
            }
        });
        let want = |t: &str, all| Some((t.to_string(), all));
        // a long first name gives way to the count
        let (long, all) = got.remove(2).unwrap();
        assert!(
            long.starts_with("Microsoft Teams") && long.ends_with("… +1") && !all,
            "{long}"
        );
        // never "Microsoft Teams (… +1" (the minimum size's cut): a cut
        // takes its bracket or separator with it, at every width
        let pair = using(&["Microsoft Teams (work or school)", "OBS Studio"]);
        let mut cuts = Vec::new();
        let _ = ctx.run(Default::default(), |ctx| {
            for max in 100..=200 {
                cuts.push(app_label(ctx, &pair, max as f32).unwrap().0 .0);
            }
        });
        assert!(cuts.iter().any(|c| c == "Microsoft Teams… +1"), "{cuts:?}");
        for c in &cuts {
            let before = c.trim_end_matches("… +1");
            assert!(
                !before.ends_with(|c: char| c.is_whitespace() || "([{-–,:/".contains(c)),
                "{c}"
            );
        }
        assert_eq!(
            got,
            [
                want("Zoom", true),
                want("Zoom, Discord", true),
                want("Zoom +2", false),
                want("Zoom +3", false),
            ]
        );
        // what the label leaves out, the picture's name and the tooltip say
        let four = using(&["Zoom", "OBS Studio", "Discord", "Firefox"]);
        assert_eq!(
            apps_all(&four),
            "Using HushMic: Zoom, OBS Studio, Discord, Firefox"
        );
        assert!(
            picture_name(&four).ends_with(" Using HushMic: Zoom, OBS Studio, Discord, Firefox."),
            "{}",
            picture_name(&four)
        );
        // two: the header line names both already
        assert!(picture_name(&using(&["Zoom", "Discord"]))
            .ends_with(" Zoom and Discord are using HushMic."));
    }

    #[test]
    fn running_again_never_hides_what_apps_hear() {
        let mut unfiltered = state(Scenario::OnInCall);
        unfiltered.engine.tier = Some(EngineTier::Passthrough);
        for (s, shows) in [
            (state(Scenario::OnInCall), true),
            (state(Scenario::Muted), false),
            (state(Scenario::Bypass), false),
            (unfiltered, false),
        ] {
            let title = head_copy(&s);
            let w = testing::with_setup(
                crate::window::Link::recording(s),
                Page::Live,
                egui::vec2(960.0, 640.0),
                |ps| ps.live.running_again(0.0),
            );
            assert_eq!(
                w.harness.query_by_label("Running again.").is_some(),
                shows,
                "{title}"
            );
            assert_eq!(
                w.harness.query_by_label(&title).is_some(),
                !shows,
                "{title}"
            );
        }
    }

    #[test]
    fn a_restarting_microphone_says_so_under_the_header() {
        let mut s = state(Scenario::OnInCall);
        let w = testing::window(s.clone(), Page::Live, egui::vec2(960.0, 640.0));
        assert!(w
            .harness
            .query_by_label_contains("keeps restarting")
            .is_none());
        s.engine.flapping = true;
        let mut w = testing::window(s, Page::Live, egui::vec2(960.0, 640.0));
        assert!(w
            .harness
            .query_by_label_contains("HushMic keeps restarting its microphone.")
            .is_some());
        w.harness.get_by_label("Copy diagnostics").click();
        w.harness.run_steps(2);
        assert!(
            w.harness.query_by_label("Copy diagnostics").is_none(),
            "copying"
        );
    }

    #[test]
    fn an_unplugged_mic_reads_as_unplugged_not_as_no_sound() {
        // UX panel round 2, A34: the chosen mic unplugged sends no sound,
        // the daemon's look or the window's own (3 s of silence); it is
        // still "unplugged", in the title and the notice alike
        crate::i18n::pin_english();
        let base = state(Scenario::MicUnplugged);
        let chosen = mic_name(&base);
        for standin in [true, false] {
            for (daemon, window) in [(true, false), (false, true), (true, true)] {
                let mut s = base.clone();
                s.mic.fallback = standin;
                s.mic.silent = daemon;
                let case = format!("stand-in {standin}, silent {daemon}/{window}");
                let (named, _) = degraded_titles(&s, window).expect(&case);
                assert!(named.contains("is unplugged."), "{case}: {named}");
                let p = problem(&s, window, None).expect(&case);
                assert_eq!(p.lead, format!("{chosen} is unplugged."), "{case}");
                assert!(!p.body.contains("on."), "{case}: {}", p.body);
            }
        }
    }

    #[test]
    fn a_degraded_state_takes_the_title() {
        crate::i18n::pin_english();
        // the chosen mic unplugged, a stand-in in use
        let s = state(Scenario::MicUnplugged);
        let chosen = mic_name(&s);
        let standin = s
            .description_of(s.mic.active.as_deref().unwrap())
            .unwrap()
            .to_owned();
        assert!(s.mic.fallback);
        assert_eq!(
            head_copy(&s),
            format!("Using {standin}. {chosen} is unplugged.")
        );
        // unplugged with nothing to stand in
        let mut s = state(Scenario::MicUnplugged);
        s.mic.fallback = false;
        assert_eq!(head_copy(&s), format!("{chosen} is unplugged."));
        // UX panel round 2, A15: the only microphone unplugged, the
        // daemon on a fallback that names nobody (or the chosen one)
        for active in [None, s.mic.selected.clone(), Some("gone".to_string())] {
            let mut s = state(Scenario::MicUnplugged);
            s.mic
                .devices
                .retain(|d| Some(&d.name) == s.mic.selected.as_ref());
            s.mic.active = active.clone();
            assert!(s.mic.fallback);
            assert_eq!(
                head_copy(&s),
                format!("{chosen} is unplugged."),
                "{active:?}"
            );
            let p = problem(&s, false, None).expect("the notice says so");
            assert_eq!(p.body, "Plug it back in, or choose another microphone.");
            let (named, plain) = degraded_titles(&s, false).unwrap();
            assert!(!named.contains("Using") && !plain.contains("another microphone."));
        }
        // stuck, rerouted, silent
        let mut s = state(Scenario::OnInCall);
        let mic = mic_name(&s);
        s.mic.silent = true;
        assert_eq!(head_copy(&s), format!("No sound from {mic}."));
        assert_eq!(
            head_for(&state(Scenario::OnInCall), true),
            format!("No sound from {mic}.")
        );
        s.mic.rerouted = true;
        assert_eq!(head_copy(&s), "Another app keeps taking this microphone.");
        s.mic.stuck = true;
        assert_eq!(head_copy(&s), format!("{mic} isn’t connecting."));
        // muted or suppression off: the user's state keeps the title
        s.mode = Mode::Mute;
        assert_eq!(head_copy(&s), "Muted.");
        // and the window shows the title: whole where it fits (960),
        // else without the names (720, SF-B8a)
        let w = testing::scenario(Scenario::MicUnplugged, Page::Live);
        assert_eq!(
            shown_title(&w),
            (format!("Using {standin}. {chosen} is unplugged."), true)
        );
        let w = testing::compact(Scenario::MicUnplugged, Page::Live);
        assert_eq!(shown_title(&w), ("Using another microphone.".into(), true));
        // rerouted has a shorter form too
        let mut s = state(Scenario::OnInCall);
        s.mic.rerouted = true;
        let w = testing::window(s, Page::Live, egui::vec2(960.0, 640.0));
        assert!(shown_title(&w).1, "{:?}", shown_title(&w));
    }

    fn heads(sc: Scenario) -> String {
        crate::i18n::pin_english();
        head_copy(&state(sc))
    }

    #[test]
    fn the_header_says_hushmics_state_alone() {
        // round 3b: the title, no second line; a silent microphone or a
        // problem is the notice's to say
        for (sc, title) in [
            (Scenario::OnInCall, "Removing background noise."),
            (Scenario::OnNoApp, "Ready."),
            (Scenario::Bypass, "Noise suppression is off."),
            (Scenario::Muted, "Muted."),
            (Scenario::LightForNow, "Removing background noise."),
            (
                Scenario::UnfilteredForNow,
                "Noise suppression paused for now.",
            ),
            (Scenario::Starting, "Starting."),
            (Scenario::Off, "HushMic is off."),
            (Scenario::MicSilent, "Removing background noise."),
            (Scenario::DirectMic, "Ready."),
            (
                Scenario::PipewireUnreachable,
                "Noise suppression is stopped.",
            ),
        ] {
            assert_eq!(heads(sc), title, "{sc:?}");
        }
        // who gets the sound, by name (the picture's name, Quit's question)
        let apps = |names: &[Option<&str>]| {
            let mut s = state(Scenario::OnInCall);
            s.apps.using = names
                .iter()
                .map(|n| crate::appstate::AppRef {
                    id: n.unwrap_or("x").to_lowercase(),
                    name: n.map(str::to_owned),
                })
                .collect();
            s
        };
        assert_eq!(apps_line(&apps(&[None])), "An app is using HushMic.");
        assert_eq!(
            apps_line(&apps(&[Some("Zoom"), Some("OBS Studio")])),
            "Zoom and OBS Studio are using HushMic."
        );
        assert_eq!(
            apps_line(&apps(&[Some("Zoom"), None])),
            "2 apps are using HushMic."
        );
        assert_eq!(
            apps_line(&apps(&[Some("Zoom"), Some("OBS"), Some("Discord")])),
            "3 apps are using HushMic."
        );
        // the picture says what it shows, then who gets the sound
        assert_eq!(
            picture_name(&state(Scenario::OnNoApp)),
            "Your microphone through HushMic, live. No app is using HushMic right now."
        );
        assert_eq!(
            picture_name(&state(Scenario::Bypass)),
            "Noise suppression is off: your raw microphone, live. Firefox is using HushMic."
        );
    }

    /// The header's one control: Mute at the height of the controls
    /// under the picture (32), Turn on the page's 40 px primary in its
    /// place while HushMic is off; both end the header's line and are
    /// centred on it. Nothing else stands beside the title.
    #[test]
    fn mute_is_the_headers_one_control() {
        let w = testing::scenario(Scenario::OnInCall, Page::Live);
        let mute = w.harness.get_by_label("Mute").rect();
        assert!((mute.height() - replay::ROW_H).abs() < 0.5, "{mute:?}");
        assert!(w
            .harness
            .query_by_role_and_label(Role::CheckBox, "Noise suppression")
            .is_none());
        let fig = fig_of(&w);
        let beside = w
            .harness
            .query_all_by_role(Role::Button)
            .filter(|n| n.rect().bottom() <= fig.top() && n.rect().left() >= fig.left())
            .count();
        assert_eq!(beside, 1, "Mute alone");
        let w = testing::scenario(Scenario::Off, Page::Live);
        let on = w.harness.get_by_label("Turn on HushMic").rect();
        assert!((on.height() - 40.0).abs() < 0.5, "{on:?}");
        assert!((on.center().y - mute.center().y).abs() < 0.5, "one line");
        assert!((on.right() - mute.right()).abs() < 0.5, "one end");
    }

    /// UX panel D1: Live's figure is a card like Settings' groups: the
    /// card fill under it, in both themes.
    #[test]
    fn the_figure_is_a_filled_card() {
        for theme in [crate::ui::Theme::Dark, crate::ui::Theme::Light] {
            let w = testing::with_setup(
                crate::window::Link::recording(state(Scenario::OnInCall)),
                Page::Live,
                egui::vec2(960.0, 640.0),
                move |p| p.pin_theme(theme),
            );
            let fig = w
                .harness
                .ctx
                .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("hm-live-fig")))
                .unwrap();
            let fill = crate::ui::tokens::Palette::of(theme).card_fill();
            // the fill may come with the card's shadow, as one shape
            fn rects(s: &egui::Shape, out: &mut Vec<egui::epaint::RectShape>) {
                match s {
                    egui::Shape::Rect(r) => out.push(r.clone()),
                    egui::Shape::Vec(v) => v.iter().for_each(|s| rects(s, out)),
                    _ => {}
                }
            }
            let mut all = Vec::new();
            for c in &w.harness.output().shapes {
                rects(&c.shape, &mut all);
            }
            let filled = all.iter().any(|r| {
                r.fill == fill
                    && (r.rect.min - fig.min).length() < 0.5
                    && (r.rect.max - fig.max).length() < 0.5
            });
            assert!(filled, "{theme:?}");
        }
    }

    #[test]
    fn screen_readers_get_named_controls_and_a_live_notice() {
        crate::i18n::pin_english();
        use egui::accesskit::Live;
        let w = full(state(Scenario::OnInCall), None, egui::vec2(960.0, 640.0));
        // no headings in the card: its controls carry their names
        testing::by_role(&w.harness, "Microphone", Role::ComboBox);
        testing::by_role(&w.harness, "Noise suppression", Role::RadioGroup);
        assert!(w.harness.query_by_label("Level Sounds good").is_some());
        let headings = w
            .harness
            .query_all_by_role(Role::Heading)
            .filter(|n| n.accesskit_node().data().level() == Some(2))
            .count();
        assert_eq!(headings, 0);
        let w = testing::scenario(Scenario::MicUnplugged, Page::Live);
        let problem = w.harness.get_by_label_contains("until it’s back");
        assert_eq!(problem.accesskit_node().live(), Live::Polite);
        // Settings' sections, and first run's status line
        let w = testing::scenario(Scenario::OnInCall, Page::Settings);
        let apps = testing::by_role(&w.harness, "Apps", Role::Heading);
        assert_eq!(apps.accesskit_node().data().level(), Some(2));
    }

    /// One header rule on every page (round 3b): a 40 px block, the card
    /// 20 px under it (88 px from the top at 960, 80 at 720); the actions
    /// beside the title while it keeps 320 px, else under it; a long title
    /// is cut with its full text on hover. Live's Mute stands beside the
    /// title at every size.
    #[test]
    fn every_page_has_the_one_header_rule() {
        crate::i18n::pin_english();
        // the title is a heading, read out when it changes (UX panel B16)
        let title_top = |w: &testing::Window, label: &str| {
            let t = testing::by_role(&w.harness, label, Role::Heading);
            assert_eq!(t.accesskit_node().data().level(), Some(1), "{label}");
            assert_eq!(
                t.accesskit_node().live(),
                egui::accesskit::Live::Polite,
                "{label}"
            );
            t.rect().top()
        };
        for (size, top) in [
            (egui::vec2(960.0, 640.0), 28.0),
            (egui::vec2(720.0, 520.0), 20.0),
        ] {
            for (page, label) in [(Page::Files, "Clean a file."), (Page::Settings, "Settings")] {
                let w = testing::window(state(Scenario::OnInCall), page, size);
                assert_eq!(title_top(&w, label), top + 6.0, "{page:?} {size:?}");
            }
        }
        // Live: Mute beside the title at 960 and at 720
        let w = testing::window(
            state(Scenario::OnInCall),
            Page::Live,
            egui::vec2(960.0, 640.0),
        );
        let t = w.harness.get_by_label("Removing background noise.").rect();
        let m = w.harness.get_by_label("Mute").rect();
        assert_eq!(t.top(), 34.0);
        assert!((m.center().y - t.center().y).abs() <= 0.5, "{m:?} {t:?}");
        let fig = w
            .harness
            .ctx
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("hm-live-fig")));
        assert_eq!(fig.expect("figure").top(), 88.0);
        let w = testing::window(
            state(Scenario::OnInCall),
            Page::Live,
            egui::vec2(720.0, 520.0),
        );
        let t = w.harness.get_by_label("Removing background noise.").rect();
        let m = w.harness.get_by_label("Mute").rect();
        assert!((m.center().y - t.center().y).abs() <= 0.5, "{m:?} {t:?}");
        // stopped: the control stays (disabled), so the card stays at 80
        // as in every other state
        let w = testing::window(
            state(Scenario::PipewireUnreachable),
            Page::Live,
            egui::vec2(720.0, 520.0),
        );
        let fig = w
            .harness
            .ctx
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("hm-live-fig")));
        assert_eq!(fig.expect("figure").top(), 80.0);
    }

    #[test]
    fn mute_off_and_turn_on_send_their_actions() {
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        w.harness.get_by_label("Mute").click();
        w.harness.run_steps(2);
        testing::by_role(&w.harness, "Off", Role::RadioButton).click();
        w.harness.run_steps(2);
        // explicit targets from the state drawn, never toggles
        assert_eq!(
            w.link.sent(),
            vec![Action::SetMode(Mode::Mute), Action::SetMode(Mode::Bypass)]
        );

        // a double click on one snapshot mutes twice, never unmutes
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        w.harness.get_by_label("Mute").click();
        w.harness.run_steps(1);
        w.harness.get_by_label("Mute").click();
        w.harness.run_steps(1);
        assert_eq!(
            w.link.sent(),
            vec![Action::SetMode(Mode::Mute), Action::SetMode(Mode::Mute)]
        );

        let mut w = testing::scenario(Scenario::Off, Page::Live);
        assert!(w.harness.query_by_label("Mute").is_none());
        w.harness.get_by_label("Turn on HushMic").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SetMode(Mode::Suppress)]);
    }

    /// UX panel round 2, A14: a double-click on Turn on turns HushMic on
    /// and leaves it on; its second click lands on Mute, which took the
    /// button's place, and does nothing.
    #[test]
    fn a_double_click_on_turn_on_never_turns_it_off_again() {
        let mut w = testing::at_60hz_with(
            crate::window::Link::mock(state(Scenario::Off), || {}),
            Page::Live,
            egui::vec2(960.0, 640.0),
            |_| {},
        );
        w.harness.run_steps(2);
        let at = w.harness.get_by_label("Turn on HushMic").rect().center();
        let click = |w: &mut testing::Window, at: egui::Pos2| {
            let ev = &mut w.harness.input_mut().events;
            ev.push(egui::Event::PointerMoved(at));
            for pressed in [true, false] {
                ev.push(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        };
        click(&mut w, at);
        w.harness.run_steps(3);
        assert_eq!(w.link.state().unwrap().mode, Mode::Suppress);
        // the second click, 50 ms on, where Turn on was: Mute is there
        let mute = w.harness.get_by_label("Mute").rect();
        assert!(mute.contains(at), "{mute:?} under {at:?}");
        click(&mut w, at);
        w.harness.run_steps(1);
        assert_eq!(w.link.state().unwrap().mode, Mode::Suppress);
        // half a second on, a click counts again
        w.harness.run_steps(30);
        click(&mut w, at);
        w.harness.run_steps(3);
        assert_eq!(w.link.state().unwrap().mode, Mode::Mute);
    }

    #[test]
    fn muted_offers_unmute_and_the_group_sets_where_unmute_returns() {
        let mut w = testing::scenario(Scenario::Muted, Page::Live);
        let back = state(Scenario::Muted).unmute_to;
        // "Muted", pressed (UX panel round 2, A7), at Mute's width
        let mute = w.harness.get_by_label("Muted");
        assert_eq!(
            mute.accesskit_node().toggled(),
            Some(egui::accesskit::Toggled::True)
        );
        assert!(w.harness.query_by_label("Unmute").is_none());
        assert!(w.harness.query_by_label("Mute").is_none());
        let muted_w = mute.rect().width();
        let on = testing::scenario(Scenario::OnInCall, Page::Live);
        let plain = on.harness.get_by_label("Mute");
        assert_eq!(
            plain.accesskit_node().toggled(),
            Some(egui::accesskit::Toggled::False)
        );
        assert_eq!(plain.rect().width(), muted_w, "one width");
        mute.click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SetMode(back)]);
        assert!(w.harness.query_by_label("Muted.").is_some());

        // Off and the strengths set where Unmute returns and never end
        // Mute; the group shows it
        let checked = |w: &testing::Window, step: &str| {
            testing::by_role(&w.harness, step, Role::RadioButton)
                .accesskit_node()
                .toggled()
                == Some(egui::accesskit::Toggled::True)
        };
        for (from, step, sent) in [
            (
                Mode::Suppress,
                "Off",
                vec![Action::SetUnmuteTo(Mode::Bypass)],
            ),
            (
                Mode::Bypass,
                "Maximum",
                vec![Action::SetUnmuteTo(Mode::Suppress)],
            ),
            (
                Mode::Bypass,
                "Gentle",
                vec![
                    Action::SetUnmuteTo(Mode::Suppress),
                    Action::SetStrength(Strength::Gentle),
                ],
            ),
        ] {
            let mut s = state(Scenario::Muted);
            assert_eq!(s.sound.current.strength, Strength::Maximum);
            s.unmute_to = from;
            let mut w = testing::window(s, Page::Live, egui::vec2(960.0, 640.0));
            assert_eq!(checked(&w, "Off"), from == Mode::Bypass, "{from:?}");
            assert_eq!(checked(&w, "Maximum"), from == Mode::Suppress, "{from:?}");
            testing::by_role(&w.harness, step, Role::RadioButton).click();
            w.harness.run_steps(2);
            assert_eq!(w.link.sent(), sent, "{from:?} {step}");
        }
        // on the mock daemon it stays muted and the group follows
        let mut w =
            testing::mock_window(state(Scenario::Muted), Page::Live, egui::vec2(960.0, 640.0));
        testing::by_role(&w.harness, "Off", Role::RadioButton).click();
        w.harness.run_steps(3);
        let st = w.link.state().expect("a state");
        assert_eq!(st.mode, Mode::Mute);
        assert_eq!(st.unmute_to, Mode::Bypass);
        assert!(checked(&w, "Off"));
        assert_eq!(
            w.harness.get_by_label("Muted").accesskit_node().toggled(),
            Some(egui::accesskit::Toggled::True)
        );
    }

    /// Noise suppression is one radio group: Off, then the strengths. The
    /// checked step is the state, and the line screen readers get says
    /// what it does. While HushMic is off the group waits, disabled, on
    /// the strength that applies when it is on (UX panel round 2, A6).
    #[test]
    fn noise_suppression_is_off_or_one_of_its_strengths() {
        crate::i18n::pin_english();
        let hint = "Applies when noise suppression is on.";
        let raw = "Apps hear your raw microphone.";
        let maximum = "Removes as much noise as it can. The pauses between words become silent.";
        let steps = ["Off", "Gentle", "Medium", "Strong", "Maximum"];
        for (scenario, step, says, enabled) in [
            (Scenario::OnInCall, "Maximum", maximum, true),
            (Scenario::Bypass, "Off", raw, true),
            (Scenario::Muted, "Maximum", maximum, true),
            (Scenario::UnfilteredForNow, "Maximum", maximum, true),
            (Scenario::Off, "Maximum", hint, false),
            (Scenario::PipewireUnreachable, "Maximum", maximum, false),
        ] {
            let w = testing::scenario(scenario, Page::Live);
            testing::by_role(&w.harness, "Noise suppression", Role::RadioGroup);
            let fig = fig_of(&w);
            let mut last = f32::MIN;
            for name in steps {
                let n = testing::by_role(&w.harness, name, Role::RadioButton);
                let on = n.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True);
                assert_eq!(on, name == step, "{scenario:?}: {name}");
                assert_eq!(
                    n.accesskit_node().is_disabled(),
                    !enabled,
                    "{scenario:?}: {name}"
                );
                // in this order, in one row in the card
                assert!(fig.contains_rect(n.rect()), "{scenario:?}: {name}");
                assert!(n.rect().left() >= last, "{scenario:?}: {name}");
                last = n.rect().right();
            }
            for words in [hint, raw, maximum] {
                assert_eq!(
                    w.harness.query_by_label(words).is_some(),
                    words == says,
                    "{scenario:?}: {words}"
                );
            }
        }
        // Off is bypass, a strength picked while it is off turns
        // suppression on at that strength in one step, and the strength
        // the microphone has comes back without being sent
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        testing::by_role(&w.harness, "Off", Role::RadioButton).click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SetMode(Mode::Bypass)]);
        let s = state(Scenario::Bypass);
        assert_eq!(s.sound.current.strength, Strength::Maximum);
        let mut w = testing::window(s.clone(), Page::Live, egui::vec2(960.0, 640.0));
        testing::by_role(&w.harness, "Strong", Role::RadioButton).click();
        w.harness.run_steps(2);
        assert_eq!(
            w.link.sent(),
            vec![
                Action::SetMode(Mode::Suppress),
                Action::SetStrength(Strength::Strong)
            ]
        );
        let mut w = testing::window(s, Page::Live, egui::vec2(960.0, 640.0));
        testing::by_role(&w.harness, "Maximum", Role::RadioButton).click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SetMode(Mode::Suppress)]);
        // on the mock daemon Off leaves the microphone's strength alone
        let mut w = testing::mock_window(
            state(Scenario::OnInCall),
            Page::Live,
            egui::vec2(960.0, 640.0),
        );
        let before = w.link.state().unwrap().sound.current.clone();
        testing::by_role(&w.harness, "Off", Role::RadioButton).click();
        w.harness.run_steps(3);
        let st = w.link.state().unwrap();
        assert_eq!(st.mode, Mode::Bypass);
        assert_eq!(st.sound.current, before);
        // while HushMic is off a click on a step does nothing
        let mut w = testing::scenario(Scenario::Off, Page::Live);
        testing::by_role(&w.harness, "Gentle", Role::RadioButton).click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![]);
    }

    /// One Maximum sentence on every page (the same engine setting), and
    /// no strength moves the card at either size.
    #[test]
    fn maximum_says_the_pauses_go_silent_and_no_strength_moves_the_card() {
        crate::i18n::pin_english();
        let maximum = "Removes as much noise as it can. The pauses between words become silent.";
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let mut heights = Vec::new();
            for (st, attn) in Strength::PRESETS {
                let mut s = state(Scenario::OnInCall);
                let model = s.sound.current.model_id.clone();
                s.sound.current = crate::appstate::Sound::new(&model, attn);
                let w = testing::window(s, Page::Live, size);
                if st == Strength::Maximum {
                    assert!(w.harness.query_by_label(maximum).is_some(), "{size:?}");
                }
                let fig = w
                    .harness
                    .ctx
                    .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("hm-live-fig")))
                    .unwrap();
                heights.push((st, fig.height()));
            }
            for (st, h) in &heights {
                assert!(
                    (h - heights[0].1).abs() < 0.5,
                    "{st:?} at {size:?}: {heights:?}"
                );
            }
        }
    }

    #[test]
    fn strength_sends_its_action() {
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        w.harness.get_by_label("Gentle").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SetStrength(Strength::Gentle)]);
    }

    #[test]
    fn the_header_controls_keep_the_keyboard_when_the_state_changes() {
        let mut w = testing::mock_window(
            state(Scenario::OnInCall),
            Page::Live,
            egui::vec2(960.0, 640.0),
        );
        let step = |w: &testing::Window, name: &str| {
            testing::by_role(&w.harness, name, Role::RadioButton).is_focused()
        };
        // Mute both ways
        w.harness.get_by_label("Mute").focus();
        w.harness.run_steps(2);
        w.harness.key_press(egui::Key::Enter);
        w.harness.run_steps(3);
        assert_eq!(w.link.state().unwrap().mode, Mode::Mute);
        assert!(w.harness.get_by_label("Muted").is_focused());
        w.harness.key_press(egui::Key::Enter);
        w.harness.run_steps(3);
        assert_eq!(w.link.state().unwrap().mode, Mode::Suppress);
        assert!(w.harness.get_by_label("Mute").is_focused());
        // Off and back with the arrows: the focus goes with the choice
        testing::by_role(&w.harness, "Maximum", Role::RadioButton).focus();
        w.harness.run_steps(2);
        for (key, want, name) in [
            (egui::Key::ArrowRight, Mode::Bypass, "Off"),
            (egui::Key::ArrowLeft, Mode::Suppress, "Maximum"),
        ] {
            w.harness.key_press(key);
            w.harness.run_steps(3);
            assert_eq!(w.link.state().unwrap().mode, want);
            assert!(step(&w, name), "{want:?}");
        }
        // Turn on hands the focus to Mute, which replaces it
        let mut w =
            testing::mock_window(state(Scenario::Off), Page::Live, egui::vec2(960.0, 640.0));
        w.harness.get_by_label("Turn on HushMic").focus();
        w.harness.run_steps(2);
        w.harness.key_press(egui::Key::Enter);
        w.harness.run_steps(3);
        assert_eq!(w.link.state().unwrap().mode, Mode::Suppress);
        assert!(w.harness.get_by_label("Mute").is_focused());
    }

    #[test]
    fn holding_space_or_enter_on_mute_flips_it_exactly_once() {
        // UX panel A31: the keyboard's auto-repeat is no press. A 1.3 s
        // hold repeats at 30 Hz, well past the header's 0.5 s click guard.
        for key in [egui::Key::Space, egui::Key::Enter] {
            let mut w = testing::at_60hz_with(
                crate::window::Link::mock(state(Scenario::OnInCall), || {}),
                Page::Live,
                egui::vec2(960.0, 640.0),
                |_| {},
            );
            w.harness.get_by_label("Mute").focus();
            w.harness.run_steps(2);
            w.harness.key_down(key);
            w.harness.run_steps(1);
            for _ in 0..40 {
                w.harness.event(egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: true,
                    modifiers: egui::Modifiers::NONE,
                    physical_key: None,
                });
                w.harness.run_steps(2);
            }
            w.harness.key_up(key);
            w.harness.run_steps(3);
            assert_eq!(w.link.state().unwrap().mode, Mode::Mute, "{key:?}");
            let modes = w
                .link
                .sent()
                .into_iter()
                .filter(|a| matches!(a, Action::SetMode(_)))
                .count();
            assert_eq!(modes, 1, "{key:?}: {:?}", w.link.sent());
            assert!(w.harness.get_by_label("Muted").is_focused(), "{key:?}");
        }
    }

    #[test]
    fn the_model_is_chosen_in_settings_and_the_step_down_shows_on_live() {
        let w = testing::scenario(Scenario::OnInCall, Page::Live);
        assert!(w.harness.query_by_label("Model").is_none());
        assert!(w.harness.query_by_label("Quality").is_none());
        // a busy computer: the note says so and its action sets Light
        for (sc, act) in [
            (Scenario::LightForNow, "Always use Light"),
            (Scenario::UnfilteredForNow, "Always use Light"),
        ] {
            let mut w = testing::window(state(sc), Page::Live, egui::vec2(960.0, 900.0));
            testing::by_role(&w.harness, act, Role::Link).click();
            w.harness.run_steps(2);
            assert_eq!(
                w.link.sent(),
                vec![Action::SetModel(Model::Light)],
                "{sc:?}"
            );
        }
        // too busy even for Light: the note stays, with nothing to offer
        let mut s = state(Scenario::UnfilteredForNow);
        s.sound.current.model = Model::Light;
        let w = testing::window(s, Page::Live, egui::vec2(960.0, 900.0));
        assert!(w
            .harness
            .query_by_label_contains("too busy for noise suppression")
            .is_some());
        assert!(w.harness.query_by_label("Always use Light").is_none());
    }

    #[test]
    fn always_use_light_from_the_keyboard_leaves_it_on_the_strength() {
        for sc in [Scenario::LightForNow, Scenario::UnfilteredForNow] {
            let mut w = testing::mock_window(state(sc), Page::Live, egui::vec2(960.0, 900.0));
            testing::by_role(&w.harness, "Always use Light", Role::Link).focus();
            w.harness.run_steps(2);
            w.harness.key_press(egui::Key::Enter);
            w.harness.run_steps(3);
            assert!(
                w.harness.query_by_label("Always use Light").is_none(),
                "{sc:?}"
            );
            let strength = testing::by_role(&w.harness, "Noise suppression", Role::RadioGroup);
            assert!(
                strength.query_by(|n| n.is_focused()).is_some(),
                "{sc:?}: the keyboard is on the strength"
            );
        }
    }

    #[test]
    fn a_strength_the_window_did_not_pick_is_read_out_with_its_microphone() {
        // UX panel round 2, A34: the chosen mic unplugged, its stand-in
        // runs at its own Maximum; the caption tells screen readers so
        crate::i18n::pin_english();
        let mut a = state(Scenario::OnInCall);
        a.sound.current.strength = Strength::Medium;
        let b = state(Scenario::MicUnplugged);
        assert_eq!(b.sound.current.strength, Strength::Maximum);
        let standin = b
            .description_of(b.sound.owner.as_deref().unwrap())
            .unwrap()
            .to_owned();
        let mut w = testing::switched(vec![a, b], Page::Live, egui::vec2(960.0, 900.0));
        w.harness.run_steps(2);
        let caption = |w: &testing::Switched, words: &str| {
            w.harness
                .query_by_label_contains(words)
                .map(|n| n.accesskit_node().live())
        };
        assert_eq!(caption(&w, "Strength Medium"), None, "nothing to tell yet");
        w.at.set(1);
        w.harness.run_steps(3);
        assert_eq!(
            caption(
                &w,
                &format!(
                    "Strength Maximum, the setting for {standin}. \
                     Removes as much noise as it can. The pauses between words become silent."
                )
            ),
            Some(egui::accesskit::Live::Polite)
        );
        // a pick in the window is read out by the control itself: the
        // caption says only what the step does
        let mut w = testing::mock_window(
            state(Scenario::OnInCall),
            Page::Live,
            egui::vec2(960.0, 900.0),
        );
        w.harness.get_by_label("Gentle").click();
        w.harness.run_steps(4);
        assert_eq!(
            w.link.state().unwrap().sound.current.strength,
            Strength::Gentle
        );
        assert!(w
            .harness
            .query_by_label_contains("Strength Gentle")
            .is_none());
        assert_eq!(
            w.harness
                .get_by_label("Takes the edge off. Most of the room sound stays.")
                .accesskit_node()
                .live(),
            egui::accesskit::Live::Polite
        );
    }

    #[test]
    fn a_step_down_that_ends_under_the_keyboard_leaves_it_on_the_strength() {
        let mut w = testing::switched(
            vec![state(Scenario::LightForNow), state(Scenario::OnInCall)],
            Page::Live,
            egui::vec2(960.0, 900.0),
        );
        testing::by_role(&w.harness, "Always use Light", Role::Link).focus();
        w.harness.run_steps(2);
        // the computer quiets down: Quality again, the note goes
        w.at.set(1);
        w.harness.run_steps(3);
        assert!(w.harness.query_by_label("Always use Light").is_none());
        let strength = testing::by_role(&w.harness, "Noise suppression", Role::RadioGroup);
        assert!(strength.query_by(|n| n.is_focused()).is_some());
        // the keyboard left the link before (and then the window lost
        // it): the note's end hands nothing over
        let mut w = testing::switched(
            vec![state(Scenario::LightForNow), state(Scenario::OnInCall)],
            Page::Live,
            egui::vec2(960.0, 900.0),
        );
        testing::by_role(&w.harness, "Always use Light", Role::Link).focus();
        w.harness.run_steps(2);
        w.harness.ctx.memory_mut(|m| {
            if let Some(f) = m.focused() {
                m.surrender_focus(f)
            }
        });
        w.harness.run_steps(2);
        w.at.set(1);
        w.harness.run_steps(3);
        assert!(w.harness.query_by(|n| n.is_focused()).is_none());
    }

    #[test]
    fn choose_another_opens_the_microphone_list() {
        let mut s = state(Scenario::MicUnplugged);
        s.mic.fallback = false;
        let mut w = testing::window(s, Page::Live, egui::vec2(960.0, 900.0));
        assert!(w.harness.query_by_label("USB Headset").is_none());
        // one line in the narrow card (UX panel round 2, A16)
        let link = w.harness.get_by_label("Choose a microphone").rect();
        assert!(link.height() <= Type::LINK.line + 1.0, "{link:?}");
        w.harness.get_by_label("Choose a microphone").click();
        w.harness.run_steps(3);
        assert!(w.harness.query_by_label("USB Headset").is_some());
    }

    #[test]
    fn a_custom_strength_checks_no_step_and_any_step_replaces_it() {
        let mut s = state(Scenario::OnInCall);
        s.sound.current = crate::appstate::Sound::new(&s.sound.current.model_id, 33.0);
        assert_eq!(s.sound.current.strength, Strength::Custom);
        let mut w = testing::window(s, Page::Live, egui::vec2(960.0, 640.0));
        for step in ["Gentle", "Medium", "Strong", "Maximum"] {
            let n = testing::by_role(&w.harness, step, Role::RadioButton);
            assert_eq!(
                n.accesskit_node().toggled(),
                Some(egui::accesskit::Toggled::False),
                "{step}"
            );
        }
        w.harness.get_by_label("Maximum").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SetStrength(Strength::Maximum)]);
    }

    #[test]
    fn the_mic_list_picks_a_device_or_the_system_default() {
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        // the dropdown: a combo box named for what it picks, its choice
        // where Orca reads it (UX panel B16)
        let combo = w
            .harness
            .get_by_role_and_label(Role::ComboBox, "Microphone");
        let shown = combo.accesskit_node().value().expect("its choice");
        // with what its tooltip says after it
        assert_eq!(
            combo.accesskit_node().description(),
            Some(format!(
                "{shown}. Strength is remembered for each microphone."
            ))
        );
        combo.click();
        w.harness.run_steps(2);
        // an option's second line is its description (UX panel round 2,
        // A34: Orca read "System default" without "Now: …")
        let default = w
            .harness
            .get_by_role_and_label(Role::ListBoxOption, "System default");
        assert!(
            default
                .accesskit_node()
                .description()
                .is_some_and(|d| d.starts_with("Now: ")),
            "{:?}",
            default.accesskit_node().description()
        );
        w.harness.get_by_label("USB Headset").click();
        w.harness.run_steps(2);
        let s = state(Scenario::OnInCall);
        let headset = s
            .mic
            .devices
            .iter()
            .find(|d| d.description == "USB Headset")
            .unwrap()
            .name
            .clone();
        assert_eq!(w.link.sent(), vec![Action::SelectMic(Some(headset))]);
    }

    #[test]
    fn an_app_on_the_raw_microphone_brings_no_warning() {
        // round 3b: the direct-mic warning, its links and its "Don't show
        // again" went from the window (while muted it says one line:
        // muted_says_which_app_still_hears_you)
        for sc in [
            Scenario::DirectMic,
            Scenario::DirectMicFollowsDefault,
            Scenario::DirectMicNoName,
        ] {
            let w = testing::scenario(sc, Page::Live);
            for words in ["directly", "Don’t show again", "hears the noise"] {
                assert!(
                    w.harness.query_by_label_contains(words).is_none(),
                    "{sc:?}: {words}"
                );
            }
        }
    }

    #[test]
    fn the_meters_recent_past_judges_the_level_at_once() {
        // Live opens mid-call on 10 s of the meter's history: a voice
        // between pauses, kept by the engine; the level speaks at once
        let col = |i: u64, raw: f32, clean: f32| MeterColumn {
            index: i,
            input_peak: raw * 1.4,
            output_peak: clean * 1.4,
            input_rms: raw,
            output_rms: clean,
            mode: EngineMode::Filtering,
            complete: true,
        };
        let mut check = LevelCheck::new();
        for i in 0..200 {
            let c = if i % 3 == 0 {
                col(i, 0.003, 0.0001) // the room, removed
            } else {
                col(i, 0.08, 0.075) // the voice, kept
            };
            judge_column(&mut check, &c);
        }
        assert_eq!(check.level().verdict, crate::miccheck::LevelVerdict::Good);
    }

    #[test]
    fn muted_says_which_app_still_hears_you() {
        crate::i18n::pin_english();
        let body = |s: &AppState| problem(s, false, None).map(|p| p.body);
        // conflict ruling 2: only while muted
        let mut s = state(Scenario::DirectMic);
        assert_eq!(body(&s), None, "not muted: no word of it");
        s.mode = Mode::Mute;
        let p = problem(&s, false, None).unwrap();
        assert_eq!(
            p.body,
            "Zoom uses your microphone directly and still hears you."
        );
        assert_eq!(p.tone, Tone::Warn);
        s.apps.direct.push(s.apps.direct[0].clone());
        s.apps.direct[1].app.name = None;
        assert_eq!(
            body(&s).unwrap(),
            "Zoom and An app use your microphone directly and still hear you."
        );
        // nobody on the raw microphone: muted as ever
        s.apps.direct.clear();
        assert_eq!(body(&s).unwrap(), "Apps using HushMic hear silence.");
        // and the window shows it
        let mut s = state(Scenario::DirectMic);
        s.mode = Mode::Mute;
        let w = testing::window(s, Page::Live, egui::vec2(960.0, 640.0));
        assert!(w
            .harness
            .query_by_label_contains("Zoom uses your microphone directly")
            .is_some());
    }

    #[test]
    fn the_page_says_the_one_thing_that_matters_most() {
        crate::i18n::pin_english();
        let at = |sc: Scenario, silent: bool| problem(&state(sc), silent, None);
        let pw = at(Scenario::PipewireUnreachable, true).unwrap();
        assert_eq!(
            (pw.tone, pw.fixes.clone()),
            (Tone::Error, vec![Fix::Retry, Fix::Diagnostics])
        );
        let down = at(Scenario::NeedsAttention, false).unwrap();
        assert_eq!(down.tone, Tone::Error);
        // a silent microphone outranks the step-down and Bypass
        let mut s = state(Scenario::LightForNow);
        let silent = problem(&s, true, None).unwrap();
        assert_eq!(silent.lead, "No sound from RODE NT-USB.");
        assert_eq!(silent.fixes, vec![Fix::ChooseMic]);
        let light = problem(&s, false, None).unwrap();
        assert_eq!(
            (light.tone, light.fixes),
            (Tone::Info, vec![Fix::AlwaysLight])
        );
        s.sound.current.model = Model::Light;
        assert!(problem(&s, false, None).unwrap().fixes.is_empty());
        assert_eq!(at(Scenario::MicUnplugged, false).unwrap().tone, Tone::Warn);
        // the level's advice
        let quiet = LevelShow::Note {
            word: "Very quiet".into(),
            lead: "Very quiet.".into(),
            body: "Raise the input volume in your sound settings.".into(),
        };
        let p = problem(&state(Scenario::OnInCall), false, Some(&quiet)).unwrap();
        assert_eq!(
            p.words(),
            "Very quiet. Raise the input volume in your sound settings."
        );
        assert_eq!(
            at(Scenario::Bypass, false).unwrap().words(),
            "Apps hear your raw microphone."
        );
        assert_eq!(at(Scenario::Muted, false).unwrap().tone, Tone::Info);
        // muted names no app, even the one that records HushMic
        let muted = state(Scenario::Muted);
        assert_eq!(muted.apps.using.len(), 1);
        assert_eq!(
            at(Scenario::Muted, false).unwrap().words(),
            "Apps using HushMic hear silence."
        );
        assert_eq!(at(Scenario::Off, false).unwrap().tone, Tone::Info);
        assert_eq!(at(Scenario::Starting, false).unwrap().tone, Tone::Info);
        // nothing wrong: nothing said
        assert!(at(Scenario::OnInCall, false).is_none());
        assert!(at(Scenario::OnNoApp, false).is_none());
    }

    /// `sc` with the chosen microphone called `desc`.
    fn named(sc: Scenario, desc: &str) -> AppState {
        let mut s = state(sc);
        let chosen = s.mic.selected.clone();
        for d in s.mic.devices.iter_mut() {
            if Some(&d.name) == chosen.as_ref() {
                d.description = desc.into();
            }
        }
        s
    }

    /// The title of the header this window shows, and whether its one
    /// line holds it whole.
    fn shown_title(w: &testing::Window) -> (String, bool) {
        let h = w
            .harness
            .query_all_by_role(Role::Heading)
            .find(|n| n.accesskit_node().data().level() == Some(1))
            .expect("a title");
        let label = h.accesskit_node().label().unwrap_or_default();
        let whole = text::width(&w.harness.ctx, &label, Type::H) <= h.rect().width() + 0.5;
        (label, whole)
    }

    /// UX panel SF-B8a: a degraded title never truncates; it names the
    /// microphone only when the whole sentence fits on its line.
    #[test]
    fn a_degraded_title_is_never_cut() {
        crate::i18n::pin_english();
        let laptop = "Built-in Audio Digital Microphone";
        let jabra = "Jabra Evolve2 30 USB Headset Mono";
        let absent = |desc: &str| {
            let mut s = named(Scenario::MicUnplugged, desc);
            s.mic.fallback = false;
            s
        };
        let silent = |desc: &str| {
            let mut s = named(Scenario::OnInCall, desc);
            s.mic.silent = true;
            s
        };
        let unplugged = |desc: &str| named(Scenario::MicUnplugged, desc);
        let mut plain_seen = 0;
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            for (s, plain) in [
                (absent(laptop), "Your microphone is unplugged."),
                (silent(jabra), "No sound from your microphone."),
                (unplugged(jabra), "Using another microphone."),
                (absent(laptop), "Your microphone is unplugged."),
            ] {
                let named_title = head_copy(&s);
                let w = testing::window(s, Page::Live, size);
                let (title, whole) = shown_title(&w);
                assert!(whole, "{size:?}: {title} is cut");
                assert!(title == named_title || title == plain, "{size:?}: {title}");
                if title == plain {
                    plain_seen += 1;
                }
            }
            // a short name keeps its sentence
            let w = testing::window(absent("RODE NT-USB"), Page::Live, size);
            assert_eq!(shown_title(&w), ("RODE NT-USB is unplugged.".into(), true));
        }
        assert!(
            plain_seen > 0,
            "the long names take the plain form somewhere"
        );
        // at 720 every long-name title is the plain one
        for s in [absent(laptop), silent(jabra), unplugged(jabra)] {
            let w = testing::window(s.clone(), Page::Live, egui::vec2(720.0, 520.0));
            assert_ne!(shown_title(&w).0, head_copy(&s), "720 names no long mic");
        }
    }

    /// A notice that takes several lines (a long name, its advice and a
    /// link) shows whole at the card's top: the lanes give it its room,
    /// and neither the card nor the controls move.
    #[test]
    fn a_long_notice_takes_its_room_from_the_lanes() {
        crate::i18n::pin_english();
        let size = egui::vec2(960.0, 640.0);
        let mut s = named(Scenario::OnInCall, "Built-in Audio Digital Microphone");
        let calm = testing::window(s.clone(), Page::Live, size);
        s.mic.silent = true;
        let noted = testing::window(s.clone(), Page::Live, size);
        let words = problem(&s, false, None).unwrap().words();
        let notice = noted.harness.get_by_label(&words).rect();
        let (a, b) = (fig_of(&calm), fig_of(&noted));
        assert!((a.top() - b.top()).abs() < 0.5 && (a.bottom() - b.bottom()).abs() < 0.5);
        assert!((controls_of(&calm) - controls_of(&noted)).abs() < 0.5);
        assert!(show_of(&noted).lane < show_of(&calm).lane);
        // its link is under its words, over the picture
        let link = testing::by_role(&noted.harness, "Choose a microphone", Role::Link).rect();
        let picture = testing::by_role(&noted.harness, &picture_name(&s), Role::Image).rect();
        assert!(link.top() >= notice.bottom() && link.bottom() <= picture.top() + 0.5);
        assert!(b.contains_rect(notice) && b.contains_rect(link));
    }

    #[test]
    fn a_real_device_name_is_not_cut_short() {
        crate::i18n::pin_english();
        let jabra = "Jabra Evolve2 30 USB Headset Mono";
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            for sc in [Scenario::MicUnplugged, Scenario::OnInCall] {
                let w = testing::window(named(sc, jabra), Page::Live, size);
                // the caption's choice names it whole
                let field = w
                    .harness
                    .get_by_role_and_label(Role::ComboBox, "Microphone");
                let chosen = field.accesskit_node().value().unwrap_or_default();
                assert!(chosen.contains(jabra), "{size:?}: {chosen}");
                let whole = crate::ui::widgets::select::quiet_width(&w.harness.ctx, &chosen);
                assert!(
                    field.rect().width() >= whole - 0.5,
                    "{size:?}: {:?} for {whole}",
                    field.rect()
                );
            }
        }
    }

    /// UX panel round 2, A13: picking a microphone with a long name moves
    /// nothing: the caption stays one row, the name is shortened there and
    /// whole in the choice's tooltip.
    #[test]
    fn a_long_name_picked_never_moves_the_card() {
        crate::i18n::pin_english();
        let long = "Jabra Evolve2 30 USB Headset Mono, Rear Port, the second of two of them";
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let mut s = state(Scenario::OnInCall);
            let other = s
                .mic
                .devices
                .iter()
                .position(|d| Some(&d.name) != s.mic.selected.as_ref() && d.present)
                .expect("another microphone");
            s.mic.devices[other].description = long.into();
            let fig = |s: &AppState| {
                let w = testing::window(s.clone(), Page::Live, size);
                let f = w
                    .harness
                    .ctx
                    .data(|d| d.get_temp::<egui::Rect>(egui::Id::new("hm-live-fig")))
                    .unwrap();
                let field = w
                    .harness
                    .get_by_role_and_label(Role::ComboBox, "Microphone")
                    .rect();
                let level = w.harness.get_by_label("Level Sounds good").rect();
                (f.height(), field, level)
            };
            let (short_h, short_field, _) = fig(&s);
            let mut picked = s.clone();
            picked.mic.selected = Some(picked.mic.devices[other].name.clone());
            picked.mic.active = picked.mic.selected.clone();
            let (long_h, long_field, level) = fig(&picked);
            assert_eq!(long_field.height(), short_field.height(), "{size:?}");
            assert!(long_field.width() > short_field.width(), "{size:?}");
            // it ends before the level
            assert!(
                long_field.right() < level.left(),
                "{size:?}: {long_field:?}"
            );
            assert!(
                (short_h - long_h).abs() < 0.5,
                "{size:?}: {short_h} {long_h}"
            );
        }
    }

    /// No state moves the card or the controls: a notice takes its room
    /// from the lanes.
    #[test]
    fn no_state_moves_the_controls() {
        crate::i18n::pin_english();
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let mut places = Vec::new();
            for s in [
                state(Scenario::OnInCall),
                state(Scenario::OnNoApp),
                state(Scenario::Bypass),
                state(Scenario::Muted),
                state(Scenario::LightForNow),
                state(Scenario::UnfilteredForNow),
                state(Scenario::MicUnplugged),
                state(Scenario::PipewireUnreachable),
                state(Scenario::NeedsAttention),
                state(Scenario::Off),
            ] {
                let w = testing::window(s.clone(), Page::Live, size);
                let (f, c) = (fig_of(&w), controls_of(&w));
                assert!(
                    (f.bottom() - (c + CARD_PAD + 1.0)).abs() < 0.5,
                    "{size:?}: {f:?}, controls to {c}"
                );
                places.push((f.top(), c));
            }
            let first = places[0];
            assert!(
                places
                    .iter()
                    .all(|p| (p.0 - first.0).abs() < 0.5 && (p.1 - first.1).abs() < 0.5),
                "{size:?}: {places:?}"
            );
        }
        // an absurdly long device name: its notice reads whole all the same
        let yeti = "Blue Yeti X Professional USB Condenser Microphone with Studio \
                    Monitoring and Zero-Latency Headphone Output (Analog Stereo, Rear Panel) \
                    on the Second USB Hub Behind the Monitor Stand, Port Three";
        let long = named(Scenario::MicUnplugged, yeti);
        let w = testing::window(long, Page::Live, egui::vec2(960.0, 640.0));
        let notice = w.harness.get_by_label_contains("until it’s back").rect();
        assert!(notice.height() > 2.0 * Type::CAP.line, "{notice:?}");
        assert!(fig_of(&w).contains_rect(notice));
    }

    #[test]
    fn pipewire_unreachable_is_the_pages_problem() {
        crate::i18n::pin_english();
        let mut w = testing::scenario(Scenario::PipewireUnreachable, Page::Live);
        // round 3b: the page stays, its title says HushMic is stopped and
        // the notice at the card's top says why, with its way out
        assert!(w
            .harness
            .query_by_label("Noise suppression is stopped.")
            .is_some());
        assert!(w
            .harness
            .query_by_label("HushMic can’t reach PipeWire, the sound system it works with.")
            .is_some());
        // the header's control stays where it is, disabled, and so is
        // the noise suppression group: nothing looks at work (UX panel
        // round 2, A16)
        let mute = w.harness.get_by_label("Mute");
        assert!(mute.accesskit_node().is_disabled());
        for step in ["Off", "Maximum"] {
            let n = testing::by_role(&w.harness, step, Role::RadioButton);
            assert!(n.accesskit_node().is_disabled(), "{step}");
        }
        assert!(w
            .harness
            .query_by_label("Replay the last 10 seconds")
            .is_some());
        w.harness.get_by_label("Try again").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::Retry]);
        // N-S1: the chooser still names the chosen microphone (no device
        // is listed), as the picture does
        let (names, _, idx) = mic_items(&state(Scenario::PipewireUnreachable));
        assert_eq!(names[idx], "RODE NT-USB");
        w.harness.get_by_label("Copy diagnostics").click();
        for _ in 0..50 {
            w.harness.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(5));
            if w.harness.query_by_label("Copied").is_some() {
                break;
            }
        }
        assert!(w.harness.query_by_label("Copied").is_some());
    }

    /// SF-D2: Live claims, for the daemon's notifications, only the
    /// problem its notice shows; it says so once, and again when the
    /// pick changes.
    #[test]
    fn live_tells_the_daemon_which_problem_it_shows() {
        crate::i18n::pin_english();
        let mut w = testing::with_setup(
            crate::window::Link::mock(state(Scenario::MicUnplugged), || {}),
            Page::Live,
            egui::vec2(960.0, 640.0),
            |_| {},
        );
        w.harness.run_steps(5);
        assert_eq!(
            w.link.told(),
            vec![Some(crate::notify::Problem::MicFallback)]
        );
        // the fallback ends: nothing is left that the daemon notifies
        w.link.send(Action::SelectMic(None));
        w.harness.run_steps(5);
        assert_eq!(
            w.link.told(),
            vec![Some(crate::notify::Problem::MicFallback), None]
        );
        // a rerouted capture outranks an unplugged microphone: only it is
        // claimed, so the fallback still reaches the desktop
        let mut s = state(Scenario::MicUnplugged);
        s.mic.rerouted = true;
        let w = testing::window(s, Page::Live, egui::vec2(960.0, 640.0));
        assert_eq!(w.link.told(), vec![Some(crate::notify::Problem::Rerouted)]);
        let w = testing::window(
            state(Scenario::PipewireUnreachable),
            Page::Live,
            egui::vec2(960.0, 640.0),
        );
        assert_eq!(w.link.told(), vec![Some(crate::notify::Problem::Failure)]);
    }

    #[test]
    fn the_header_keeps_its_height_in_every_state() {
        // the card's top is the same in every state, at both sizes and
        // the widths between
        let widths = [720.0, 840.0, 848.0, 856.0, 864.0, 872.0, 900.0, 960.0];
        for width in widths {
            let compact = width < 840.0;
            let mut tops = Vec::new();
            for sc in [
                Scenario::OnInCall,
                Scenario::OnNoApp,
                Scenario::Bypass,
                Scenario::Muted,
                Scenario::Starting,
                Scenario::Off,
                Scenario::UnfilteredForNow,
                Scenario::PipewireUnreachable,
            ] {
                let mut st = state(sc);
                // a turn-on, not a launch (Launch B would cover the page)
                st.engine.start_reason = st
                    .engine
                    .start_reason
                    .map(|_| crate::appstate::StartReason::TurnOn);
                let w = testing::window(st, Page::Live, egui::vec2(width, 640.0));
                tops.push(fig_of(&w).top());
            }
            assert!(
                tops.windows(2).all(|t| (t[0] - t[1]).abs() < 0.5),
                "{width}: {tops:?}"
            );
            if width == 720.0 || width == 960.0 {
                // round 3b: 20 + 40 + 20 at 720, 28 + 40 + 20 at 960
                let expect = if compact { 80.0 } else { 88.0 };
                assert!((tops[0] - expect).abs() < 0.5, "{width}: {tops:?}");
            }
        }
    }
}
