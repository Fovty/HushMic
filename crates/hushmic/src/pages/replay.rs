//! Live's last 10 seconds (the replay row under the picture): "Replay the
//! last 10 seconds" (Stop while it plays) and "Hold to hear the original"
//! ([`super::hold`]: a hold plays the original at the same moment while
//! held, a tap latches it, and Space held anywhere on the page holds too).
//! One line joins them when there is something to say: the measured
//! result after a replay, a playback that failed, a take whose sides
//! can't switch at the same moment. It stands right of the buttons, or
//! under them where that is too narrow. Why Replay
//! is off, and that nothing is saved, are the buttons' tooltips. While a
//! replay plays, Live's picture is the take, its playhead on the
//! playback's own clock. The level verdict goes to Live's controls
//! ([`LevelShow`]).
//!
//! While Live shows, the window keeps its own capture of the microphone
//! (the Mic check backend, `crate::miccheck`): it holds the last 10
//! seconds, and it runs the engine, so the picture moves with no app.
//! Leaving Live or minimising the window lets go of the microphone.

use std::sync::Arc;

use eframe::egui::{self, pos2, Ui};

use super::hold::{Hold, Kind};
use super::util::{self, rect};
use super::{Cx, Feed};
use crate::appstate::{AppState, Mode, Phase as EnginePhase};
use crate::miccheck::demo;
use crate::miccheck::spectrum::Picture;
use crate::miccheck::types::{
    Blocker, Column, Command, Engine, EngineInfo, LevelVerdict, Listen, Notice, Phase,
    PlaybackKind, Quieter, Setup, TakeResult, View, Voice, Warning, COLUMNS,
};
use crate::tr;
use crate::ui::fonts::Type;
use crate::ui::icons::Icon;
use crate::ui::paint::clips;
use crate::ui::text;
use crate::ui::widgets::Button;

/// A warning stays this long.
const WARNING_SECS: f64 = 4.0;
/// The row's padding above it, the gap between it and a line under it,
/// and how far a line beside the buttons keeps from them and from what
/// leads the row.
pub const PAD_TOP: f32 = 10.0;
pub const PAD_X: f32 = 16.0;
pub const STATUS_GAP: f32 = 6.0;
const STATUS_BESIDE: f32 = 16.0;
/// The row's controls are the small size; 8 px between the buttons, and
/// between the row's two lines where it wraps.
pub const ROW_H: f32 = 32.0;
const GAP_X: f32 = 8.0;
pub const GAP_Y: f32 = 8.0;

/// Where the row's [`View`] comes from.
pub trait Source {
    fn send(&mut self, c: Command);
    /// The newest view (a demo source advances to `now`).
    fn view(&mut self, now: f64) -> View;
    /// The picture of what the view's timeline covers: two lanes, the
    /// microphone and what apps hear. None: a source that keeps none.
    fn picture(&mut self) -> Option<Arc<Picture>> {
        None
    }
}

/// The boards' replay and level states, for screenshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Replaying,
    Holding,
    /// The original latched by a tap, during a replay.
    Latched,
    Result,
    /// A take that can't switch at the same moment, after its replay.
    Sequential,
    Quiet,
    Loud,
    /// Exact digital silence: nothing to replay.
    Silent,
    /// The settings changed: the last 10 seconds start over.
    Changed,
    /// No tap, and this PipeWire can't record to a pipe.
    Unavailable,
}

impl Preset {
    pub fn parse(word: &str) -> Option<Preset> {
        Some(match word {
            "replaying" => Preset::Replaying,
            "holding" => Preset::Holding,
            "latched" => Preset::Latched,
            "result" => Preset::Result,
            "sequential" => Preset::Sequential,
            "quiet" => Preset::Quiet,
            "loud" => Preset::Loud,
            "silent" => Preset::Silent,
            "changed" => Preset::Changed,
            "unavailable" => Preset::Unavailable,
            _ => return None,
        })
    }
}

/// The level verdict among Live's controls: a dot and a word ("Sounds
/// good"), or a warn word ("Very quiet") with its advice as the page's
/// notice.
#[derive(Clone, Debug, PartialEq)]
pub enum LevelShow {
    Line {
        word: String,
        waiting: bool,
    },
    Note {
        word: String,
        lead: String,
        body: String,
    },
}

/// The row and the backend behind it.
pub struct Replay {
    feed: Feed,
    source: Option<Box<dyn Source>>,
    info: Option<EngineInfo>,
    /// The one compare control: the hold button and page Space.
    pub(crate) hold: Hold,
    /// What the backend was last told: a hold (`Command::Hold`) and the
    /// latch (`Command::Listen`, true: the original).
    hold_sent: bool,
    latch_sent: bool,
    /// The window is minimised: the backend was told it is hidden (it lets
    /// go of the microphone and keeps the result).
    paused: bool,
    warning: Option<(Warning, f64)>,
    last_warning: Option<Warning>,
    prefill: usize,
    /// A mock source's microphone sends silence.
    silent: bool,
    /// Screenshots: open on this board state.
    pub preset: Option<Preset>,
    /// Replay or Hold had the keyboard last frame (when the take goes and
    /// they turn disabled, the keyboard moves on).
    row_kb: Option<egui::Id>,
    /// The Replay/Stop button, as last drawn.
    replay_id: Option<egui::Id>,
}

impl Replay {
    pub fn new(feed: Feed) -> Replay {
        Replay {
            feed,
            source: None,
            info: None,
            hold: Hold::default(),
            hold_sent: false,
            latch_sent: false,
            paused: false,
            warning: None,
            last_warning: None,
            prefill: 0,
            silent: false,
            preset: None,
            row_kb: None,
            replay_id: None,
        }
    }

    pub fn prefill(&mut self, cols: usize) {
        self.prefill = cols;
    }

    /// A mock source's microphone sends silence (screenshots).
    pub fn silent(&mut self) {
        self.silent = true;
    }

    /// The picture of the last 10 seconds, or of the take a replay plays
    /// (None while there is no backend).
    pub fn picture(&mut self) -> Option<Arc<Picture>> {
        self.source.as_mut().and_then(|s| s.picture())
    }

    /// The backend is running and listening (the microphone may be open).
    pub fn listening(&self) -> bool {
        self.source.is_some() && !self.paused
    }

    /// The window is minimised while Live is up: the backend lets go of
    /// the microphone and keeps the take's result; the next frame of Live
    /// tells it it shows again. A press ends; the latch stays, as the
    /// backend keeps it (and the result) while hidden.
    pub fn pause(&mut self) {
        if self.paused {
            return;
        }
        if let Some(s) = self.source.as_mut() {
            if self.hold_sent {
                s.send(Command::Hold(false));
            }
            s.send(Command::Visible(false));
            self.paused = true;
        }
        self.end_presses();
    }

    /// Nothing held or latched (the backend is gone).
    fn let_go(&mut self) {
        self.hold.clear();
        self.latch_sent = false;
        self.end_presses();
    }

    /// No press, page Space or key on the hold button (the latch aside).
    fn end_presses(&mut self) {
        self.hold.end_presses();
        self.hold_sent = false;
    }

    /// Live was left (or HushMic can't work): the backend lets go of the
    /// microphone and goes.
    pub fn hide(&mut self) {
        if let Some(mut s) = self.source.take() {
            s.send(Command::Visible(false));
        }
        self.info = None;
        self.let_go();
        self.paused = false;
        self.warning = None;
        self.last_warning = None;
    }

    fn source(&mut self, ctx: &egui::Context, s: &AppState) -> &mut Box<dyn Source> {
        let info = engine_info(s);
        let (feed, prefill, preset) = (self.feed, self.prefill, self.preset);
        let silent = self.silent;
        let unfiltered = s.engine.tier == Some(crate::diagnostics::EngineTier::Passthrough);
        self.source.get_or_insert_with(|| -> Box<dyn Source> {
            match feed {
                Feed::Demo => {
                    let demo = DemoCheck::with(info, prefill, preset, unfiltered);
                    Box::new(if silent { demo.silent() } else { demo })
                }
                Feed::Meter => {
                    let setup = Setup {
                        info,
                        columns: false,
                        once: false,
                    };
                    Box::new(crate::miccheck::MicCheck::start(setup, util::wake(ctx)))
                }
            }
        })
    }

    /// Before anything is drawn: the backend's facts, its newest view, and
    /// the compare control's keys (a page Space tap is Replay/Stop).
    pub fn begin(&mut self, ctx: &egui::Context, cx: &Cx<'_>, layer: egui::LayerId) -> View {
        let s = cx.state;
        let now = cx.now;
        if std::mem::take(&mut self.paused) {
            // back from a minimise: listening again
            if let Some(src) = self.source.as_mut() {
                src.send(Command::Visible(true));
            }
        }
        let info = engine_info(s);
        if self.info.as_ref() != Some(&info) {
            let fresh = self.info.is_none();
            // only a new take clears the latch: the backend drops its take
            // for a new chain (Off included); a minimise, Stop, the end of
            // a replay and a replay with no result (muted or too short)
            // keep it
            if self.info.as_ref().is_some_and(|old| drops_take(old, &info)) {
                self.hold.clear();
            }
            self.info = Some(info.clone());
            let preset = self.preset;
            let src = self.source(ctx, s);
            if fresh {
                src.send(Command::Visible(true));
                if preset == Some(Preset::Latched) {
                    self.hold.compare.latched = true;
                    self.hold.compare.on = true;
                    self.latch_sent = true;
                }
            } else {
                src.send(Command::Engine(info));
            }
        }
        let v = self.source(ctx, s).view(now);
        if let Some(w) = v.warning {
            if self.last_warning != Some(w) {
                self.warning = Some((w, now));
            }
        }
        self.last_warning = v.warning;
        if self.warning.is_some_and(|(_, t)| now - t > WARNING_SECS) {
            self.warning = None;
        }
        if self.warning.is_some() {
            crate::ui::motion::repaint_in(ctx, std::time::Duration::from_millis(500));
        }
        // Replay/Stop switches once per key press: a held key's repeats
        // would turn the replay on and off (ruling 3)
        if self
            .replay_id
            .is_some_and(|id| ctx.memory(|m| m.has_focus(id)))
        {
            ctx.input_mut(|i| {
                i.events.retain(|e| {
                    !matches!(
                        e,
                        egui::Event::Key {
                            key: egui::Key::Space | egui::Key::Enter,
                            repeat: true,
                            ..
                        }
                    )
                })
            });
        }
        // Space compares while a replay plays, and only then: it never
        // starts one (a Space by accident would loop the last 10 seconds
        // through the speakers) and never stops one (ruling 3). A take
        // that can't switch at the same moment switches sides on a tap.
        let playing = v.phase == Phase::Replaying;
        let kind = hold_kind(&v);
        self.hold.space_off = !playing;
        if self.hold.keys(ctx, layer, &[], has_take(&v), kind, now) && kind == Kind::Toggle {
            self.hold.compare.click();
        }
        if v.phase == Phase::Replaying {
            // the playhead moves with what is heard, every frame
            crate::ui::motion::moment_repaint(ctx);
        } else if !matches!(v.phase, Phase::Blocked(_)) {
            // the backend wakes the page for a new state, not for each
            // column; a look a second shows it someone is looking (else
            // it lets go of the microphone after `LOOK_TIMEOUT`)
            crate::ui::motion::repaint_in(ctx, std::time::Duration::from_secs(1));
        }
        v
    }

    /// The original plays now (here or as the backend reports it).
    pub fn original(&self, v: &View) -> bool {
        v.phase == Phase::Replaying
            && (v.holding || v.listen == Listen::Original || self.hold.compare.on)
    }

    /// Replay or Stop. The latch stays: Replay plays the side it chose.
    fn replay_or_stop(&mut self, playing: bool) {
        self.send(if playing {
            Command::Stop
        } else {
            Command::Replay
        });
    }

    /// Tell the backend what changed: the latch first (it ends a hold
    /// there), then the hold. A sequential take never holds.
    fn sync(&mut self, sequential: bool) {
        let latch = self.hold.compare.latched;
        if latch != self.latch_sent {
            self.latch_sent = latch;
            self.hold_sent = false;
            let side = if latch {
                Listen::Original
            } else {
                Listen::HushMic
            };
            self.send(Command::Listen(side));
        }
        let held = self.hold.compare.held() && !sequential;
        if held != self.hold_sent {
            self.hold_sent = held;
            self.send(Command::Hold(held));
        }
    }

    fn send(&mut self, c: Command) {
        if let Some(s) = self.source.as_mut() {
            s.send(c);
        }
    }

    /// Where the row's two buttons go in `w` (inside the figure's border)
    /// beside what leads the row (Live's strength, `leads` wide: as it
    /// likes to be, and at its tightest): see [`row_plan`]. Replay's
    /// short form is its short label.
    fn plan(&self, ctx: &egui::Context, v: &View, w: f32, leads: (f32, f32)) -> RowPlan {
        let room = w - 2.0 * PAD_X;
        let hold_label = hold_label(v);
        let hw = self
            .hold
            .look(&hold_label, has_take(v), v.holding)
            .sm()
            .width(ctx);
        let widths = (replay_width(ctx, false), replay_width(ctx, true));
        row_plan(room, leads, widths, hw)
    }

    /// Draw the row's buttons and its line from `y` down, `w` wide from
    /// `x` (inside the figure's border); `leads`: see [`Replay::plan`].
    /// Returns its bottom.
    #[allow(clippy::too_many_arguments)]
    pub fn row(
        &mut self,
        ui: &mut Ui,
        cx: &mut Cx<'_>,
        v: &View,
        x: f32,
        y: f32,
        w: f32,
        leads: (f32, f32),
    ) -> f32 {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let now = cx.now;
        let has_take = has_take(v);
        let playing = v.phase == Phase::Replaying;
        let sequential = v.playback == PlaybackKind::Sequential;
        let plan = self.plan(&ctx, v, w, leads);
        let iy = y + PAD_TOP + if plan.wrap { ROW_H + GAP_Y } else { 0.0 };
        // Replay or Hold had the keyboard and is drawn disabled now (the
        // take went: a minimise lets go of the audio): the page's rail
        // row takes it, so the keyboard is never lost
        let had = ctx.memory(|m| m.focused());
        let mut lost = false;
        let mut row = [None, None];
        {
            let (at, rw) = plan.first;
            let ix = x + PAD_X + at;
            let long = tr!("mic-replay");
            let label = if playing {
                tr!("mic-stop")
            } else if plan.short {
                tr!("live-replay-short")
            } else {
                long.clone()
            };
            let b = Button::secondary(&label)
                .sm()
                .icon(if playing { Icon::Stop } else { Icon::Replay })
                .min_width(rw)
                .enabled(has_take);
            if collecting(cx.state, v) {
                // the take so far, under the disabled button (UX panel
                // B1)
                let fill = rect(ix, iy, rw * v.collected, ROW_H);
                ui.painter()
                    .rect_filled(fill, crate::ui::tokens::radius::SM, pal.surface_hover);
            }
            let r = util::put(ui, rect(ix, iy, rw, ROW_H), "live-replay", b);
            // what it keeps and that nothing is saved; off, why. Under
            // its short label the whole sentence is its name and leads
            // its tooltip
            let tip = Self::replay_tip(cx.state, v);
            let r = if plan.short && !playing {
                ctx.accesskit_node_builder(r.id, |n| {
                    n.set_label(long.clone());
                    n.set_description(tip.clone());
                });
                r.on_hover_text(format!("{long}. {tip}"))
            } else {
                util::tip(r, &tip)
            };
            lost |= had == Some(r.id) && !has_take;
            row[0] = Some(r.id);
            self.replay_id = Some(r.id);
            if r.clicked() {
                self.replay_or_stop(playing);
            }
        }
        {
            let (at, hw) = plan.second;
            let ix = x + PAD_X + at;
            // what a press does, and that Space does the same from
            // anywhere on the page
            let desc = if sequential {
                tr!("mic-hold-seq-desc")
            } else {
                format!("{} {}", tr!("mic-hold-desc"), tr!("mic-hold-space"))
            };
            let label = hold_label(v);
            let look = self
                .hold
                .look(&label, has_take, v.holding)
                .sm()
                .min_width(hw);
            let (r, latched) = self.hold.button(
                ui,
                rect(ix, iy, hw, ROW_H),
                "live-hold",
                look,
                desc.clone(),
                has_take,
                hold_kind(v),
                now,
            );
            if has_take {
                r.clone().on_hover_text(desc);
            } else {
                r.clone().on_hover_text(Self::no_take_tip(cx.state, v));
            }
            lost |= had == Some(r.id) && !has_take;
            row[1] = Some(r.id);
            if latched && !playing {
                // it plays, latched, as a tap's press does
                self.sync(false);
                self.send(Command::Replay);
            }
        }
        let f = ctx.memory(|m| m.focused());
        let had_row = self.row_kb.take();
        self.row_kb = f.filter(|f| row.contains(&Some(*f)));
        if lost || had_row.is_some() && f.is_none() && !has_take {
            crate::ui::widgets::rail::take_focus(&ctx);
        }
        let sent = (self.latch_sent, self.hold_sent);
        self.sync(sequential);
        if sent != (self.latch_sent, self.hold_sent) {
            ctx.request_repaint();
        }
        // the line: beside the buttons where it fits, else under the row
        if let Some(line) = self.line(&ctx, cx.state, v, w, &plan) {
            let size = line.size();
            let at = if line.under {
                pos2(x + PAD_X, iy + ROW_H + STATUS_GAP)
            } else {
                // at the right end of its room, centred on the buttons
                pos2(
                    x + PAD_X + plan.free.1 - size.x,
                    iy + (ROW_H - size.y) / 2.0,
                )
            };
            // one named node, read out when it appears or changes
            let sr = egui::Rect::from_min_size(at, size);
            let resp = ui.interact(sr, ui.id().with("live-replay-status"), egui::Sense::hover());
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &line.words)
            });
            util::tell(&ctx, resp.id, util::Speak::Live);
            let mut ty = at.y;
            for block in &line.blocks {
                let bx = if line.under {
                    at.x
                } else {
                    at.x + size.x - block.size().x
                };
                block.paint(ui.painter(), pos2(bx, ty));
                ty += block.size().y;
            }
        }
        let (buttons, under) = self.height(&ctx, cx.state, v, w, leads);
        y + buttons + under
    }

    /// The line the row says ([`Replay::status`]), placed: in the room
    /// the row has free beside the buttons where that holds it (in one
    /// line, or in two that stand within the buttons' height), else on a
    /// line of its own under the row, which is there only while it has
    /// something to say.
    fn line(
        &self,
        ctx: &egui::Context,
        s: &AppState,
        v: &View,
        w: f32,
        plan: &RowPlan,
    ) -> Option<Line> {
        let (lead, body) = self.status(s, v)?;
        let words = [lead.as_str(), body.as_str()]
            .iter()
            .filter(|t| !t.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        let room = w - 2.0 * PAD_X;
        let beside = plan.free.1 - plan.free.0;
        let fits = |blocks: &[text::TextBlock]| {
            blocks.iter().all(|b| b.size().x <= beside)
                && blocks.iter().map(|b| b.size().y).sum::<f32>() <= ROW_H + 4.0
        };
        let mut tries = vec![vec![status_block(ctx, &lead, &body, f32::INFINITY)]];
        if beside > 0.0 {
            tries.push(vec![status_block(ctx, &lead, &body, beside)]);
        }
        let (blocks, under) = match tries.into_iter().find(|t| fits(t)) {
            Some(blocks) => (blocks, false),
            None => (vec![status_block(ctx, &lead, &body, room)], true),
        };
        Some(Line {
            words,
            blocks,
            under,
        })
    }

    /// The row's height at `w`, as [`Replay::row`] draws it: its one or
    /// two lines of controls with the padding above them, and what a line
    /// under them adds (0 while there is none, or while it stands beside
    /// the buttons).
    pub fn height(
        &self,
        ctx: &egui::Context,
        s: &AppState,
        v: &View,
        w: f32,
        leads: (f32, f32),
    ) -> (f32, f32) {
        let plan = self.plan(ctx, v, w, leads);
        let under = match self.line(ctx, s, v, w, &plan) {
            Some(line) if line.under => STATUS_GAP + line.size().y,
            _ => 0.0,
        };
        let lines = if plan.wrap {
            2.0 * ROW_H + GAP_Y
        } else {
            ROW_H
        };
        (PAD_TOP + lines, under)
    }

    /// The width what leads the row has: one of `leads` beside the
    /// buttons, or the row's whole width where they stand under it.
    pub fn lead(&self, ctx: &egui::Context, v: &View, w: f32, leads: (f32, f32)) -> f32 {
        self.plan(ctx, v, w, leads).lead
    }

    /// Replay's tooltip: while it plays, that it repeats; with a take,
    /// what Live keeps and that nothing is saved; else why it is off.
    fn replay_tip(s: &AppState, v: &View) -> String {
        if v.phase == Phase::Replaying {
            tr!("mic-title-replaying")
        } else if has_take(v) {
            format!(
                "{} {}",
                tr!("mic-st-listening-lead"),
                tr!("mic-st-listening-body")
            )
        } else {
            Self::no_take_tip(s, v)
        }
    }

    /// Why Replay and Hold are off: what replay waits for, that the
    /// settings changed, else that there is no take yet (their tooltip).
    fn no_take_tip(s: &AppState, v: &View) -> String {
        let join = |lead: String, body: String| {
            [lead, body]
                .into_iter()
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        match waiting_line(s, v) {
            Some((lead, body)) => join(lead, body),
            None if v.notice == Some(Notice::SettingsChanged) => {
                join(tr!("mic-note-changed-lead"), tr!("mic-note-changed-body"))
            }
            None => tr!("mic-no-take-tip"),
        }
    }

    /// The line under the buttons, (lead, in ink and medium; body), when
    /// there is something to say: a playback or capture that failed, a
    /// take whose sides can't switch at the same moment. Otherwise the
    /// picture and the buttons say it all: a replay that ended leaves no
    /// line (the level beside the microphone already says how it sounds,
    /// and a line that comes and goes moves the page).
    pub fn status(&self, s: &AppState, v: &View) -> Option<(String, String)> {
        let latched = self.hold.compare.latched;
        let sequential = v.playback == PlaybackKind::Sequential;
        let playing = v.phase == Phase::Replaying;
        // while replay waits for what it needs, its tooltip says why
        if waiting_line(s, v).is_some() || collecting(s, v) {
            return None;
        }
        if let Some((w, _)) = self.warning {
            let text = match w {
                Warning::PlaybackFailed => tr!("mic-warn-playback"),
                Warning::CaptureStopped => tr!("mic-warn-capture"),
            };
            return Some((String::new(), text));
        }
        if playing {
            return None;
        }
        if sequential && has_take(v) {
            return Some(if latched {
                (tr!("mic-st-seq-chosen"), tr!("mic-sub-latched"))
            } else {
                (
                    tr!("mic-note-sequential-lead"),
                    tr!("mic-note-sequential-body"),
                )
            });
        }
        None
    }
}

/// Replay is off only because the take is still too short: the button
/// fills as the take grows. Not while something else holds it back (a
/// notice: muted, settings changed).
fn collecting(s: &AppState, v: &View) -> bool {
    !has_take(v)
        && v.phase == Phase::Listening
        && v.notice.is_none()
        && waiting_line(s, v).is_none()
}

/// What replay waits for (HushMic can't work, it starts, an old PipeWire,
/// a silent take), as the disabled buttons' tooltip says it. None: it
/// doesn't wait.
fn waiting_line(s: &AppState, v: &View) -> Option<(String, String)> {
    let none = String::new;
    if !s.pipewire.reachable {
        return Some((none(), tr!("mic-st-pipewire")));
    }
    if s.mode != Mode::Off && s.engine.phase == EnginePhase::Down {
        return Some((none(), tr!("mic-st-down")));
    }
    if s.mode == Mode::Off || v.phase == Phase::Blocked(Blocker::Off) {
        return Some((none(), tr!("mic-st-off")));
    }
    if matches!(
        s.engine.phase,
        EnginePhase::Starting | EnginePhase::Reconnecting
    ) || matches!(
        v.phase,
        Phase::Connecting | Phase::Blocked(Blocker::NoMicrophone | Blocker::NoPipewire)
    ) {
        return Some((none(), tr!("mic-sub-connecting")));
    }
    if v.phase == Phase::Blocked(Blocker::NeedsNewerPipewire) {
        return Some((tr!("mic-st-old-pipewire"), tr!("mic-sub-old-pipewire")));
    }
    if v.phase == Phase::MicProblem {
        return Some((tr!("mic-take-empty"), none()));
    }
    None
}

/// The row's line as it is drawn: its words (its name for screen
/// readers), its lines of text, and whether it stands under the buttons
/// (else right of them, each line right-aligned).
struct Line {
    words: String,
    blocks: Vec<text::TextBlock>,
    under: bool,
}

impl Line {
    fn size(&self) -> egui::Vec2 {
        self.blocks.iter().fold(egui::Vec2::ZERO, |size, b| {
            egui::vec2(size.x.max(b.size().x), size.y + b.size().y)
        })
    }
}

/// Where a row of controls under a picture puts its parts
/// ([`row_plan`]): whether its two buttons are on a line of their own,
/// whether the first has its short form, the width of what leads the
/// row, each button's place from the row's left and its width, and the
/// room the row has between the two ends (from, to).
pub struct RowPlan {
    pub wrap: bool,
    pub short: bool,
    pub lead: f32,
    pub first: (f32, f32),
    pub second: (f32, f32),
    pub free: (f32, f32),
}

/// One row `room` wide under a picture, on Live and on Files: steps lead
/// it at the left (`leads`: their width with 10 px and with 6 px a side),
/// two buttons end it at the right (`first`: its width in its long and
/// in its short form; `second`). What gives way, in this order: the
/// steps' padding, the first button's long form; then the buttons go on
/// a line of their own under the steps, which take the row's width, and
/// share that line equally.
pub fn row_plan(room: f32, leads: (f32, f32), first: (f32, f32), second: f32) -> RowPlan {
    let tries = [
        (leads.0, first.0, false),
        (leads.1, first.0, false),
        (leads.0, first.1, true),
        (leads.1, first.1, true),
    ];
    let fits = |lead: f32, fw: f32| lead + STATUS_BESIDE + fw + GAP_X + second <= room;
    match tries.into_iter().find(|(lead, fw, _)| fits(*lead, *fw)) {
        Some((lead, fw, short)) => {
            let at = room - (fw + GAP_X + second);
            RowPlan {
                wrap: false,
                short,
                lead,
                first: (at, fw),
                second: (at + fw + GAP_X, second),
                free: (lead + STATUS_BESIDE, at - STATUS_BESIDE),
            }
        }
        None => {
            let half = (room - GAP_X) / 2.0;
            RowPlan {
                wrap: true,
                short: false,
                lead: room,
                first: (0.0, half),
                second: (half + GAP_X, half),
                free: (room, room),
            }
        }
    }
}

/// Replay and Stop share one width: the wider label, at least 232 px.
/// Replay's width, under its long or its short label: Stop is as wide.
fn replay_width(ctx: &egui::Context, short: bool) -> f32 {
    let replay = if short {
        tr!("live-replay-short")
    } else {
        tr!("mic-replay")
    };
    let stop = tr!("mic-stop");
    let a = Button::secondary(&replay)
        .sm()
        .icon(Icon::Replay)
        .width(ctx);
    let b = Button::secondary(&stop).sm().icon(Icon::Stop).width(ctx);
    a.max(b)
}

fn hold_label(v: &View) -> String {
    if v.playback == PlaybackKind::Sequential {
        tr!("mic-hold-seq")
    } else {
        tr!("mic-hold")
    }
}

/// `<strong>lead</strong> body`: 13/18, the lead ink and medium, the body
/// ink-muted.
fn status_block(ctx: &egui::Context, lead: &str, body: &str, w: f32) -> text::TextBlock {
    let pal = crate::ui::kit(ctx).pal;
    let mut r = text::Rich::new();
    if !lead.is_empty() {
        r = r.span(lead, Type::CAP.medium(), pal.ink);
        if !body.is_empty() {
            r = r.span(" ", Type::CAP, pal.ink_muted);
        }
    }
    if !body.is_empty() {
        r = r.span(body, Type::CAP, pal.ink_muted);
    }
    r.layout(ctx, w)
}

/// There is something to replay (the hold button and Space are live from
/// then on; until then Space reaches the focused control).
pub fn has_take(v: &View) -> bool {
    (v.can_replay && !matches!(v.phase, Phase::Blocked(_))) || v.phase == Phase::Replaying
}

/// The level verdict for Live's controls: none while there is nothing to
/// judge (off, starting, a silent microphone), and none while what apps
/// hear isn't the cleaned microphone: muted, noise suppression off, the
/// chosen microphone unplugged (UX panel B1: no "Sounds good" then; the
/// picture and the header say the state).
///
/// `meter` is the verdict from the engine meter (Live's, warm from the
/// recent past at once): it speaks while the take's own is still waiting,
/// so "Say a few words" never shows while the meter hears a voice.
pub fn level_show(s: &AppState, v: &View, meter: LevelVerdict) -> Option<LevelShow> {
    let unplugged = s.mic.fallback
        || s.mic
            .selected
            .as_deref()
            .is_some_and(|m| s.mic.devices.iter().any(|d| d.name == m && !d.present));
    if matches!(s.mode, Mode::Off | Mode::Mute | Mode::Bypass)
        || unplugged
        || s.engine.phase != EnginePhase::Ready
        || matches!(
            v.phase,
            Phase::Connecting | Phase::MicProblem | Phase::Blocked(_)
        )
    {
        return None;
    }
    let verdict = match v.level.verdict {
        LevelVerdict::Waiting => meter,
        own => own,
    };
    Some(match verdict {
        LevelVerdict::Waiting => LevelShow::Line {
            word: tr!("miccheck-level-waiting"),
            waiting: true,
        },
        LevelVerdict::Good => LevelShow::Line {
            word: tr!("miccheck-level-good"),
            waiting: false,
        },
        LevelVerdict::VeryQuiet => LevelShow::Note {
            word: tr!("miccheck-level-quiet"),
            lead: tr!("mic-level-quiet-note"),
            body: tr!("mic-level-quiet-advice"),
        },
        LevelVerdict::TooLoud => LevelShow::Note {
            word: tr!("miccheck-level-loud"),
            lead: tr!("mic-level-loud-note"),
            body: tr!("mic-level-loud-advice"),
        },
        LevelVerdict::NoSound => return None,
    })
}

/// A take whose two sides can't switch at the same moment: the button
/// only picks the side Replay plays.
fn hold_kind(v: &View) -> Kind {
    if v.playback == PlaybackKind::Sequential {
        Kind::Toggle
    } else {
        Kind::Hold
    }
}

/// The daemon's facts for the backend.
pub fn engine_info(s: &AppState) -> EngineInfo {
    let engine = if s.mode == Mode::Off || s.engine.phase == EnginePhase::Down {
        Engine::Off
    } else {
        match s.mode {
            Mode::Mute => Engine::Muted,
            Mode::Bypass => Engine::Bypass,
            _ => Engine::Filtering,
        }
    };
    let ready = s.engine.phase == EnginePhase::Ready;
    EngineInfo {
        engine,
        meter: if ready {
            s.engine.meter.as_ref().map(std::path::PathBuf::from)
        } else {
            None
        },
        model: s.sound.current.model_id.clone(),
        strength_db: s.sound.current.attn_limit,
        mic_node: s.mic.active.clone(),
        mic_name: super::live::mic_name(s),
        pipewire: s.pipewire.reachable,
    }
}

/// The backend drops its take on this change (miccheck/state.rs
/// `on_engine`): another chain (a new meter, or none while off),
/// microphone or model. A strength goes to the running chain: the take
/// stays.
fn drops_take(old: &EngineInfo, new: &EngineInfo) -> bool {
    old.meter != new.meter || old.mic_node != new.mic_node || old.model != new.model
}

// ---------------------------------------------------------------- sources

/// The backend: it asks for frames itself when its view changes.
impl Source for crate::miccheck::MicCheck {
    fn send(&mut self, c: Command) {
        crate::miccheck::MicCheck::send(self, c);
    }
    fn view(&mut self, _now: f64) -> View {
        crate::miccheck::MicCheck::view(self)
    }
    fn picture(&mut self) -> Option<Arc<Picture>> {
        Some(crate::miccheck::MicCheck::picture(self))
    }
}

fn blocked(info: &EngineInfo) -> Option<Phase> {
    if !info.pipewire {
        Some(Phase::Blocked(Blocker::NoPipewire))
    } else if info.engine == Engine::Off {
        Some(Phase::Blocked(Blocker::Off))
    } else if info.mic_node.is_none() {
        Some(Phase::Blocked(Blocker::NoMicrophone))
    } else {
        None
    }
}

/// The last 10 seconds without a microphone, for mock windows and
/// screenshots: the timeline's levels from the boards' keyboard clip, the
/// picture from the made-up ten seconds of [`demo`]. Listening fills the
/// take; a replay plays the frozen 10 s in a loop (the playhead moves)
/// until Stop, the hold flips the side, and Stop shows the keyboard
/// clip's measured result (25 to 27 dB, `docs/comparison.md`).
pub struct DemoCheck {
    info: EngineInfo,
    /// The picture's lanes.
    lanes: demo::Lanes,
    /// Started with a take (a screenshot scene, a test): it stays as it
    /// is, whatever the clock does, until the page hides and drops it.
    still: bool,
    /// The microphone sends silence: nothing goes into the lanes.
    silent: bool,
    /// The engine passes the microphone on as it is (too busy to filter).
    unfiltered: bool,
    view: View,
    clip_i: usize,
    last: Option<f64>,
    replay_t0: Option<f64>,
    frozen: bool,
    /// A replay plays once (first run), as [`Setup::once`].
    once: bool,
    /// The replay was started by a hold: its release ends it.
    held_replay: bool,
}

/// A board state picked on its own plays its replay from the start; the
/// board's shot is taken this far in (`board-shot.mjs --at 3000`).
const PRESET_AT: f32 = 0.37;

impl DemoCheck {
    pub fn new(info: EngineInfo, prefill: usize, preset: Option<Preset>) -> DemoCheck {
        DemoCheck::with(info, prefill, preset, false)
    }

    /// As [`DemoCheck::new`]; `unfiltered`: the engine is too busy to
    /// filter, so what apps hear is the microphone as it is.
    pub fn with(
        info: EngineInfo,
        prefill: usize,
        preset: Option<Preset>,
        unfiltered: bool,
    ) -> DemoCheck {
        let mut d = DemoCheck {
            info: info.clone(),
            lanes: demo::Lanes::new(),
            still: prefill > 0,
            silent: false,
            unfiltered,
            view: View::default(),
            clip_i: 0,
            last: None,
            replay_t0: None,
            frozen: false,
            once: false,
            held_replay: false,
        };
        d.view.mic_name = info.mic_name.clone();
        d.view.phase = Phase::Listening;
        for _ in 0..prefill {
            d.step();
        }
        d.apply_engine();
        if let Some(p) = preset {
            d.preset(p);
        }
        d
    }

    /// A replay plays once, as first run's ([`Setup::once`]).
    pub fn once(mut self) -> DemoCheck {
        self.once = true;
        self
    }

    /// The microphone sends exact silence: the lanes stay empty.
    pub fn silent(mut self) -> DemoCheck {
        self.silent = true;
        self.lanes.clear();
        self
    }

    /// How the lanes' next column is heard.
    fn heard(&self) -> Engine {
        match self.info.engine {
            Engine::Filtering if self.unfiltered => Engine::Bypass,
            e => e,
        }
    }

    /// The strength the lanes are heard at.
    fn db(&self) -> f32 {
        self.info.strength_db
    }

    fn apply_engine(&mut self) {
        self.view.notice = match self.info.engine {
            Engine::Bypass => Some(Notice::Bypass),
            Engine::Muted => Some(Notice::Muted),
            _ => None,
        };
        if let Some(b) = blocked(&self.info) {
            self.view.phase = b;
            self.view.can_replay = false;
            self.lanes.clear();
        } else if matches!(self.view.phase, Phase::Blocked(_)) {
            self.view.phase = Phase::Listening;
        }
        self.view.serial += 1;
    }

    fn preset(&mut self, p: Preset) {
        self.frozen = true;
        let db = self.db();
        let mut replay = |v: &mut View| {
            v.phase = Phase::Replaying;
            v.timeline.live = false;
            v.timeline.position = Some(PRESET_AT);
            self.lanes.freeze(db);
            self.lanes.show_frozen(true);
        };
        match p {
            Preset::Replaying => replay(&mut self.view),
            Preset::Holding => {
                replay(&mut self.view);
                self.view.holding = true;
            }
            Preset::Latched => {
                replay(&mut self.view);
                self.view.listen = Listen::Original;
            }
            Preset::Result => {
                self.view.phase = Phase::Result;
                self.view.result = Some(KEYBOARD_RESULT);
            }
            Preset::Sequential => {
                self.view.phase = Phase::Result;
                self.view.result = Some(KEYBOARD_RESULT);
                self.view.playback = PlaybackKind::Sequential;
            }
            Preset::Quiet => self.view.level.verdict = LevelVerdict::VeryQuiet,
            Preset::Loud => self.view.level.verdict = LevelVerdict::TooLoud,
            Preset::Silent => {
                self.lanes.clear();
                self.view.phase = Phase::MicProblem;
                self.view.timeline.columns.clear();
                self.view.can_replay = false;
                self.view.level.verdict = LevelVerdict::NoSound;
            }
            Preset::Changed => {
                self.lanes.clear();
                self.view.notice = Some(Notice::SettingsChanged);
                self.view.level.verdict = LevelVerdict::Waiting;
                self.view.timeline.columns.clear();
                self.view.can_replay = false;
            }
            Preset::Unavailable => {
                self.lanes.clear();
                self.view.phase = Phase::Blocked(Blocker::NeedsNewerPipewire);
                self.view.can_replay = false;
            }
        }
        self.view.serial += 1;
    }

    fn step(&mut self) {
        if !self.silent && self.info.engine != Engine::Off {
            self.lanes.step(self.heard(), self.db());
        }
        let (raw, clean) = clips::KEYBOARD.at(self.clip_i);
        self.clip_i += 1;
        let cols = &mut self.view.timeline.columns;
        cols.push(Column { raw, clean });
        if cols.len() > COLUMNS {
            cols.remove(0);
        }
        self.view.can_replay = cols.len() >= 40;
        self.view.collected = (cols.len() as f32 / 40.0).min(1.0);
        self.view.level.verdict = if cols.len() >= 20 {
            LevelVerdict::Good
        } else {
            LevelVerdict::Waiting
        };
    }

    /// A replay on the latch's side (`view.listen`).
    fn start_replay(&mut self, now: f64) {
        self.view.phase = Phase::Replaying;
        self.view.timeline.live = false;
        self.view.timeline.position = Some(0.0);
        self.replay_t0 = Some(now);
        self.lanes.freeze(self.db());
        self.lanes.show_frozen(true);
    }

    fn end_replay(&mut self) {
        self.view.phase = Phase::Result;
        self.view.result = Some(KEYBOARD_RESULT);
        self.view.timeline.live = true;
        self.view.timeline.position = None;
        self.view.holding = false;
        self.replay_t0 = None;
        self.held_replay = false;
        self.lanes.show_frozen(false);
    }
}

/// The keyboard demo clip's measured range is 25 to 27 dB.
const KEYBOARD_RESULT: TakeResult = TakeResult {
    between_words: Quieter::Much,
    between_words_db: Some(26.0),
    voice: Voice::Kept,
    voice_db: Some(-1.0),
};

impl Source for DemoCheck {
    fn send(&mut self, c: Command) {
        let now = self.last.unwrap_or(0.0);
        match c {
            // as the backend: hidden drops the last 10 seconds
            Command::Visible(false) if !self.frozen => {
                if self.replay_t0.is_some() {
                    self.end_replay();
                }
                self.view.timeline.columns.clear();
                self.view.can_replay = false;
                self.lanes.clear();
                // what it started with is gone: it listens from here on
                self.still = false;
            }
            Command::Visible(_) => {}
            Command::Replay if self.view.can_replay => self.start_replay(now),
            Command::Replay => {}
            Command::Stop => self.end_replay(),
            // as the backend (miccheck/state.rs): the latch (`Listen`, kept
            // across replays) and a hold over it (the original while held);
            // a hold before there is anything to replay does nothing
            Command::Hold(true) if self.view.holding => {}
            Command::Hold(true) if self.view.phase == Phase::Replaying => self.view.holding = true,
            Command::Hold(true) if self.view.can_replay => {
                self.start_replay(now);
                self.view.holding = true;
                self.held_replay = true;
            }
            Command::Hold(true) => {}
            // a hold that started the replay ends it (UX panel round 2, A3)
            Command::Hold(false) if self.view.holding && self.held_replay => self.end_replay(),
            Command::Hold(false) => self.view.holding = false,
            Command::Listen(l) => {
                self.view.holding = false;
                self.held_replay = false;
                self.view.listen = l;
            }
            Command::Seek(p) => {
                if let Some(t0) = self.replay_t0.as_mut() {
                    *t0 = now - p as f64 * 10.0;
                }
                self.view.timeline.position = Some(p);
            }
            Command::Engine(info) => {
                // as the backend: a strength goes to the frozen take too
                if info.strength_db != self.info.strength_db {
                    self.lanes.restrength(info.strength_db);
                }
                self.info = info;
                self.apply_engine();
            }
        }
        self.view.serial += 1;
    }

    fn picture(&mut self) -> Option<Arc<Picture>> {
        Some(self.lanes.picture())
    }

    fn view(&mut self, now: f64) -> View {
        if self.frozen {
            self.last = Some(now);
            return self.view.clone();
        }
        let last = *self.last.get_or_insert(now);
        // as the backend: the replay loops until Stop, or plays once
        if let Some(t0) = self.replay_t0 {
            if self.once && now - t0 >= 10.0 {
                self.end_replay();
            } else {
                let p = ((now - t0) / 10.0).max(0.0).fract() as f32;
                self.view.timeline.position = Some(p);
            }
        }
        let listening = matches!(self.view.phase, Phase::Listening | Phase::Result);
        if self.view.timeline.live && listening && !self.still {
            let n = (((now - last) / 0.05) as usize).min(40);
            for _ in 0..n {
                self.step();
            }
            if n > 0 {
                self.last = Some(last + n as f64 * 0.05);
                self.view.serial += 1;
            }
        } else {
            self.last = Some(now);
        }
        self.view.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock::{state, Scenario};
    use crate::appstate::{Action, Page};
    use crate::miccheck::types::Timeline;
    use crate::pages::testing;
    use egui_kittest::kittest::{NodeT, Queryable};

    /// A row gives way in one order: the steps' padding, the first
    /// button's long form, then the buttons go under the steps.
    #[test]
    fn a_row_too_narrow_gives_way_in_one_order() {
        let (leads, first, second) = ((300.0, 260.0), (200.0, 100.0), 150.0);
        let buttons = |fw: f32| fw + GAP_X + second;
        let need = |lead: f32, fw: f32| lead + STATUS_BESIDE + buttons(fw);
        for (room, lead, fw, short) in [
            (900.0, leads.0, first.0, false),
            (need(leads.0, first.0), leads.0, first.0, false),
            (need(leads.0, first.0) - 1.0, leads.1, first.0, false),
            (need(leads.1, first.0) - 1.0, leads.0, first.1, true),
            (need(leads.0, first.1) - 1.0, leads.1, first.1, true),
        ] {
            let p = row_plan(room, leads, first, second);
            assert!(!p.wrap, "{room}");
            assert_eq!((p.lead, p.first.1, p.short), (lead, fw, short), "{room}");
            // the buttons end the row, the room between is what is left
            assert_eq!(p.second.0 + p.second.1, room, "{room}");
            assert_eq!(p.second.0 - (p.first.0 + p.first.1), GAP_X, "{room}");
            assert_eq!(p.free, (lead + STATUS_BESIDE, p.first.0 - STATUS_BESIDE));
        }
        let room = need(leads.1, first.1) - 1.0;
        let p = row_plan(room, leads, first, second);
        assert!(p.wrap && !p.short);
        assert_eq!(p.lead, room);
        assert_eq!(p.first.1, p.second.1);
        assert_eq!(p.first.0, 0.0);
        assert_eq!(p.second.0 + p.second.1, room);
    }

    fn view_with(phase: Phase) -> View {
        View {
            phase,
            mic_name: "RODE NT-USB".into(),
            ..View::default()
        }
    }

    #[test]
    fn the_line_under_the_buttons_says_only_what_they_do_not() {
        crate::i18n::pin_english();
        let s = state(Scenario::OnNoApp);
        let mut r = Replay::new(Feed::Demo);
        let line = |r: &Replay, s: &AppState, v: &View| {
            r.status(s, v).map(|(lead, body)| format!("{lead}|{body}"))
        };
        // collecting, at rest, playing, holding: the buttons and the
        // picture say it; Replay's tooltip has the rest
        assert_eq!(line(&r, &s, &view_with(Phase::Listening)), None);
        assert_eq!(
            Replay::replay_tip(&s, &view_with(Phase::Listening)),
            "Nothing to replay yet. Live keeps the last 10 seconds while it’s open."
        );
        let take = View {
            can_replay: true,
            ..view_with(Phase::Listening)
        };
        assert_eq!(line(&r, &s, &take), None);
        assert_eq!(
            Replay::replay_tip(&s, &take),
            "HushMic keeps the last 10 seconds while Live is open. Nothing is saved."
        );
        let mut v = view_with(Phase::Replaying);
        assert_eq!(line(&r, &s, &v), None);
        assert_eq!(
            Replay::replay_tip(&s, &v),
            "Playing on repeat. Stop ends it."
        );
        v.holding = true;
        assert_eq!(line(&r, &s, &v), None);
        r.hold.compare.latched = true;
        assert_eq!(line(&r, &s, &v), None);
        r.hold.compare.latched = false;
        // after a replay: no line (the level says how it sounds)
        let mut v = view_with(Phase::Result);
        v.result = Some(KEYBOARD_RESULT);
        assert_eq!(line(&r, &s, &v), None);
        // not suppressing now: the result's claim goes (round 2, A6)
        for (mode, engine) in [(Mode::Bypass, Notice::Bypass), (Mode::Mute, Notice::Muted)] {
            let mut s = state(Scenario::OnNoApp);
            s.mode = mode;
            v.notice = Some(engine);
            assert_eq!(line(&r, &s, &v), None, "{mode:?}");
            v.notice = None;
            assert_eq!(line(&r, &s, &v), None, "{mode:?}, no notice yet");
        }
        // what replay waits for is its buttons' tooltip. A silent
        // microphone (3 s of zeros): the last 10 seconds may still hold
        // speech, so it never says there was none (UX panel round 2, A17)
        let waits = |s: &AppState, v: &View| {
            assert_eq!(line(&r, s, v), None);
            Replay::replay_tip(s, v)
        };
        assert_eq!(
            waits(&s, &view_with(Phase::MicProblem)),
            "Replay waits for sound from your microphone."
        );
        let mut v = view_with(Phase::Listening);
        v.notice = Some(Notice::SettingsChanged);
        assert_eq!(
            waits(&s, &v),
            "Your settings changed, so the last 10 seconds start over."
        );
        assert_eq!(
            waits(&s, &view_with(Phase::Blocked(Blocker::NeedsNewerPipewire))),
            "Replay needs a newer PipeWire. Noise suppression works as usual."
        );
        assert_eq!(
            waits(&s, &view_with(Phase::Connecting)),
            "The last 10 seconds start once HushMic hears your microphone."
        );
        assert_eq!(
            waits(
                &state(Scenario::Off),
                &view_with(Phase::Blocked(Blocker::Off))
            ),
            "Replay works while HushMic is on."
        );
        // a take that can't switch at the same moment says so, and which
        // side the button chose
        let seq = |phase, latched: bool| {
            let mut r = Replay::new(Feed::Demo);
            r.hold.compare.latched = latched;
            let v = View {
                playback: PlaybackKind::Sequential,
                can_replay: true,
                ..view_with(phase)
            };
            line(&r, &s, &v)
        };
        assert_eq!(
            seq(Phase::Listening, false).as_deref(),
            Some("Same-moment switching isn’t possible for this take.|Each side plays from the start.")
        );
        assert_eq!(seq(Phase::Replaying, false), None);
        assert_eq!(seq(Phase::Replaying, true), None);
        assert_eq!(
            seq(Phase::Result, true).as_deref(),
            Some("Replay plays your original microphone.|Press again to hear HushMic.")
        );
    }

    #[test]
    fn the_level_goes_to_lives_controls() {
        crate::i18n::pin_english();
        let s = state(Scenario::OnNoApp);
        let mut v = view_with(Phase::Listening);
        let waiting = LevelVerdict::Waiting;
        v.level.verdict = LevelVerdict::Good;
        assert_eq!(
            level_show(&s, &v, waiting),
            Some(LevelShow::Line {
                word: "Sounds good".into(),
                waiting: false
            })
        );
        v.level.verdict = LevelVerdict::VeryQuiet;
        assert_eq!(
            level_show(&s, &v, waiting),
            Some(LevelShow::Note {
                word: "Very quiet".into(),
                lead: "Very quiet.".into(),
                body: "Raise the input volume in your sound settings.".into()
            })
        );
        v.level.verdict = LevelVerdict::NoSound;
        assert_eq!(level_show(&s, &v, waiting), None, "Live's own note says it");
        v.level.verdict = LevelVerdict::Good;
        assert_eq!(level_show(&state(Scenario::Off), &v, waiting), None);
        assert_eq!(level_show(&state(Scenario::Starting), &v, waiting), None);
    }

    #[test]
    fn the_meter_speaks_while_the_take_waits_and_no_state_sounds_good() {
        crate::i18n::pin_english();
        let good = LevelVerdict::Good;
        let word = |s: &AppState, v: &View, meter| match level_show(s, v, meter) {
            Some(LevelShow::Line { word, .. }) | Some(LevelShow::Note { word, .. }) => word,
            None => "-".into(),
        };
        let s = state(Scenario::OnInCall);
        let mut v = view_with(Phase::Listening);
        v.level.verdict = LevelVerdict::Waiting;
        // the take still forms; the meter, warm from the recent past,
        // hears a voice: no "Say a few words"
        assert_eq!(word(&s, &v, good), "Sounds good");
        assert_eq!(word(&s, &v, LevelVerdict::Waiting), "Say a few words");
        // the take's own verdict, once it has one, wins
        v.level.verdict = LevelVerdict::VeryQuiet;
        assert_eq!(word(&s, &v, good), "Very quiet");
        // no "Sounds good" while what apps hear isn't the cleaned mic
        v.level.verdict = LevelVerdict::Good;
        for sc in [
            Scenario::Muted,
            Scenario::Bypass,
            Scenario::Off,
            Scenario::MicUnplugged,
        ] {
            assert_eq!(word(&state(sc), &v, good), "-", "{sc:?}");
        }
        let mut s = state(Scenario::OnInCall);
        s.mic.fallback = true;
        assert_eq!(word(&s, &v, good), "-", "on a stand-in");
    }

    /// A strength picked while a take stands still re-mixes what apps
    /// hear in the picture, as the backend re-mixes the take; the
    /// microphone's lane stays as it was.
    #[test]
    fn a_new_strength_remixes_the_frozen_lane() {
        use crate::miccheck::spectrum::{Cursor, ROWS};
        let mut info = engine_info(&state(Scenario::OnInCall));
        info.strength_db = 100.0;
        let mut d = DemoCheck::new(info.clone(), 200, Some(Preset::Replaying));
        let lanes = |d: &mut DemoCheck| {
            let (mut mic, mut apps) = (0u64, 0u64);
            let picture = d.picture().expect("the demo's lanes");
            let shown = picture.read(&mut Cursor::default(), |_, levels, _| {
                mic += levels[..ROWS].iter().map(|b| u64::from(*b)).sum::<u64>();
                apps += levels[ROWS..].iter().map(|b| u64::from(*b)).sum::<u64>();
            });
            assert!(shown.frozen && shown.reset);
            (mic, apps)
        };
        let maximum = lanes(&mut d);
        assert!(maximum.1 < maximum.0, "{maximum:?}");
        info.strength_db = 12.0;
        d.send(Command::Engine(info));
        let gentle = lanes(&mut d);
        assert_eq!(gentle.0, maximum.0, "the microphone is the same");
        assert!(
            gentle.1 > maximum.1 && gentle.1 < gentle.0,
            "more of the room stays: {gentle:?} after {maximum:?}"
        );
    }

    #[test]
    fn a_disabled_replay_says_it_collects() {
        crate::i18n::pin_english();
        let r = Replay::new(Feed::Demo);
        let s = state(Scenario::OnInCall);
        let mut v = view_with(Phase::Listening);
        v.collected = 0.4;
        assert!(collecting(&s, &v));
        // the button fills; no line says it
        assert_eq!(r.status(&s, &v), None);
        // a take: the usual line
        v.can_replay = true;
        assert!(!collecting(&s, &v));
        // a notice keeps its line; what replay waits for keeps its own
        v.can_replay = false;
        v.notice = Some(Notice::SettingsChanged);
        assert!(!collecting(&s, &v));
        v.notice = None;
        assert!(!collecting(&state(Scenario::Off), &v));
        assert!(!collecting(&s, &view_with(Phase::Connecting)));
    }

    #[test]
    fn replay_latch_and_stop_drive_the_backend() {
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        // the demo source needs two seconds of sound before a replay
        for _ in 0..60 {
            w.harness.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(40));
            if w.harness
                .query_by_label("Replay the last 10 seconds")
                .is_some_and(|n| !n.accesskit_node().is_disabled())
            {
                break;
            }
        }
        w.harness.get_by_label("Replay the last 10 seconds").click();
        w.harness.run_steps(2);
        assert!(w.harness.query_by_label(CLEAN).is_some());
        // a click with no press (an assistive technology) latches it
        w.harness
            .get_by_label("Hold to hear the original")
            .click_accesskit();
        w.harness.run_steps(2);
        assert!(w.harness.query_by_label(ORIGINAL).is_some());
        w.harness.get_by_label("Stop").click();
        w.harness.run_steps(2);
        // it ended: the buttons are back, and no line came for it
        assert!(w
            .harness
            .query_by_label("Replay the last 10 seconds")
            .is_some());
        assert!(!shows(&w, "Pauses 26 dB quieter · Voice preserved"));
    }

    /// The demo backend, with what the page sends it.
    struct Logged(DemoCheck, Log);

    impl Source for Logged {
        fn send(&mut self, c: Command) {
            self.1.borrow_mut().push(c.clone());
            self.0.send(c);
        }
        fn view(&mut self, now: f64) -> View {
            self.0.view(now)
        }
        fn picture(&mut self) -> Option<Arc<Picture>> {
            self.0.picture()
        }
    }

    type Log = std::rc::Rc<std::cell::RefCell<Vec<Command>>>;

    /// Live on 60 Hz frames with a replayable demo take, and the
    /// commands it sends from then on.
    fn ready60() -> (testing::Window, Log) {
        let log: Log = Default::default();
        let l = log.clone();
        let s = state(Scenario::OnInCall);
        let info = engine_info(&s);
        let mut w = testing::at_60hz_with(
            crate::window::Link::recording(s),
            Page::Live,
            egui::vec2(960.0, 640.0),
            move |ps| {
                let demo = DemoCheck::new(info.clone(), 200, None);
                ps.live.replay.source = Some(Box::new(Logged(demo, l.clone())));
            },
        );
        w.harness.run_steps(3);
        log.borrow_mut().clear();
        (w, log)
    }

    const HOLD: &str = "Hold to hear the original";

    /// A replay loops until Stop: past its 10 s it starts over (the
    /// playhead wraps, the picture stays the replay's), a hold carries
    /// across the lap, and Stop shows the result after a replay.
    #[test]
    fn a_replay_loops_until_stop() {
        let s = state(Scenario::OnInCall);
        let mut d = DemoCheck::new(engine_info(&s), 200, None);
        let _ = d.view(0.0);
        d.send(Command::Replay);
        d.send(Command::Hold(true));
        let v = d.view(12.5);
        assert_eq!(v.phase, Phase::Replaying, "past 10 s, still playing");
        assert!(!v.timeline.live, "the replay's picture");
        let at = v.timeline.position.unwrap();
        assert!((at - 0.25).abs() < 1e-3, "wrapped: {at}");
        assert!(v.holding, "the hold carries across the lap");
        let v = d.view(31.0);
        assert_eq!(v.phase, Phase::Replaying);
        assert!((v.timeline.position.unwrap() - 0.1).abs() < 1e-3);
        d.send(Command::Stop);
        let v = d.view(31.1);
        assert_eq!(v.phase, Phase::Result);
        assert_eq!(v.result, Some(KEYBOARD_RESULT));
    }

    /// First run's demo replay plays once, as its backend does (UX panel
    /// round 2, A3), and a hold that started a replay ends it.
    #[test]
    fn the_demo_plays_once_when_asked_and_a_hold_ends_its_own_replay() {
        let s = state(Scenario::OnInCall);
        let mut d = DemoCheck::new(engine_info(&s), 200, None).once();
        let _ = d.view(0.0);
        d.send(Command::Replay);
        assert_eq!(d.view(9.9).phase, Phase::Replaying);
        assert_eq!(d.view(10.1).phase, Phase::Result, "once, no lap");
        let mut d = DemoCheck::new(engine_info(&s), 200, None);
        let _ = d.view(0.0);
        d.send(Command::Hold(true));
        assert_eq!(d.view(1.0).phase, Phase::Replaying);
        d.send(Command::Hold(false));
        assert_eq!(d.view(1.1).phase, Phase::Result);
        // a tap's latch keeps the replay it started
        d.send(Command::Hold(true));
        d.send(Command::Listen(Listen::Original));
        d.send(Command::Hold(false));
        assert_eq!(d.view(1.2).phase, Phase::Replaying);
    }

    /// The pointer goes down (or up) on the hold button.
    fn pointer(w: &mut testing::Window, pressed: bool) {
        let at = w.harness.get_by_label(HOLD).rect().center();
        let events = &mut w.harness.input_mut().events;
        events.push(egui::Event::PointerMoved(at));
        events.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }

    /// A control or the status line says it.
    fn shows(w: &testing::Window, text: &str) -> bool {
        w.harness.query_by_label_contains(text).is_some()
    }

    /// The hold button's pressed look (`aria-pressed`).
    fn pressed(w: &testing::Window) -> bool {
        w.harness.get_by_label(HOLD).accesskit_node().toggled()
            == Some(egui::accesskit::Toggled::True)
    }

    fn played(log: &Log) -> bool {
        log.borrow()
            .iter()
            .any(|c| matches!(c, Command::Replay | Command::Stop))
    }

    #[test]
    fn a_tap_latches_the_original_and_the_next_tap_returns() {
        let (mut w, log) = ready60();
        pointer(&mut w, true);
        w.harness.run_steps(1);
        // any press plays the original at once
        assert_eq!(log.borrow().last(), Some(&Command::Hold(true)));
        // released within 180 ms: a tap, and the original stays on
        pointer(&mut w, false);
        w.harness.run_steps(3);
        assert!(hearing_original(&w));
        assert!(latched(&w));
        assert!(pressed(&w), "the pressed look stays while latched");
        // the next tap: HushMic again
        pointer(&mut w, true);
        w.harness.run_steps(1);
        pointer(&mut w, false);
        w.harness.run_steps(3);
        assert!(hearing_hushmic(&w));
        assert!(!pressed(&w));
        // the backend's latch ends each hold: a tap is press, then latch
        assert_eq!(
            *log.borrow(),
            vec![
                Command::Hold(true),
                Command::Listen(Listen::Original),
                Command::Hold(true),
                Command::Listen(Listen::HushMic)
            ]
        );
        assert!(!played(&log));
    }

    #[test]
    fn a_hold_plays_the_original_until_released_and_clears_the_latch() {
        let (mut w, log) = ready60();
        pointer(&mut w, true);
        w.harness.run_steps(20);
        assert!(hearing_original(&w));
        assert!(hearing_original(&w));
        pointer(&mut w, false);
        w.harness.run_steps(3);
        // the hold started the replay: letting go ends it (UX panel round
        // 2, A3), nothing loops on
        assert!(!replaying(&w));
        assert!(w.harness.query_by_label("Stop").is_none());
        assert!(!pressed(&w));
        // latched, then held: the release returns and clears the latch
        pointer(&mut w, true);
        w.harness.run_steps(1);
        pointer(&mut w, false);
        w.harness.run_steps(3);
        assert!(latched(&w));
        log.borrow_mut().clear();
        pointer(&mut w, true);
        w.harness.run_steps(20);
        pointer(&mut w, false);
        w.harness.run_steps(3);
        assert!(hearing_hushmic(&w));
        assert!(!pressed(&w));
        // the backend's latch goes too
        assert_eq!(
            *log.borrow(),
            vec![Command::Hold(true), Command::Listen(Listen::HushMic)]
        );
        assert!(!played(&log));
    }

    #[test]
    fn a_long_hold_lasts_until_the_button_lets_go() {
        // egui stops counting a press as a click after 0.8 s or a few
        // pixels of travel; the hold is the button's down, not the click's
        let (mut w, _log) = ready60();
        pointer(&mut w, true);
        w.harness.run_steps(90);
        let at = w.harness.get_by_label(HOLD).rect().center() + egui::vec2(30.0, 12.0);
        w.harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(at));
        w.harness.run_steps(10);
        assert!(hearing_original(&w));
        assert!(hearing_original(&w));
        w.harness
            .input_mut()
            .events
            .push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
        w.harness.run_steps(3);
        assert!(!replaying(&w), "the hold's replay ended");
        assert!(w.harness.query_by_label("Stop").is_none());
        assert!(!pressed(&w));
    }

    #[test]
    fn an_assistive_click_toggles_the_latch_and_says_how() {
        let (mut w, log) = ready60();
        let hold = w.harness.get_by_label(HOLD);
        assert_eq!(
            hold.accesskit_node().description().as_deref(),
            Some(
                "Press to switch, hold to compare. Space does the same from anywhere on the page."
            )
        );
        hold.click_accesskit();
        w.harness.run_steps(3);
        assert!(latched(&w));
        assert!(pressed(&w));
        w.harness.get_by_label(HOLD).click_accesskit();
        w.harness.run_steps(3);
        assert!(hearing_hushmic(&w));
        assert!(!pressed(&w));
        assert_eq!(
            *log.borrow(),
            // latched, it plays from the start; the next click returns
            vec![
                Command::Listen(Listen::Original),
                Command::Replay,
                Command::Listen(Listen::HushMic)
            ]
        );
    }

    #[test]
    fn a_press_the_window_loses_counts_for_nothing() {
        let (mut w, log) = ready60();
        pointer(&mut w, true);
        w.harness.run_steps(1);
        // the window loses the keyboard mid-press, and gets it back with
        // the button still down: that press is over
        w.harness.input_mut().focused = false;
        w.harness.run_steps(2);
        w.harness.input_mut().focused = true;
        w.harness.run_steps(2);
        // a quick release now neither latches nor clicks
        pointer(&mut w, false);
        w.harness.run_steps(3);
        assert!(!pressed(&w));
        assert_eq!(
            *log.borrow(),
            vec![Command::Hold(true), Command::Hold(false)]
        );
    }

    #[test]
    fn a_latched_original_outlives_the_replay_and_a_minimise() {
        let log: Log = Default::default();
        let l = log.clone();
        let s = state(Scenario::OnInCall);
        let info = engine_info(&s);
        let mut w = testing::with_setup(
            crate::window::Link::recording(s),
            Page::Live,
            egui::vec2(960.0, 640.0),
            move |ps| {
                let demo = DemoCheck::new(info.clone(), 200, None);
                ps.live.replay.source = Some(Box::new(Logged(demo, l.clone())));
            },
        );
        w.harness.run_steps(3);
        // latched: the replay plays the original, past its 10 s (it
        // loops) until Stop
        w.harness.get_by_label(HOLD).click_accesskit();
        w.harness.run_steps(2);
        assert!(latched(&w));
        w.harness.run_steps(44);
        assert!(latched(&w), "still playing");
        w.harness.get_by_label("Stop").click();
        w.harness.run_steps(2);
        assert!(
            w.harness
                .query_by_label("Replay the last 10 seconds")
                .is_some(),
            "it ended"
        );
        assert!(pressed(&w), "still latched");
        // Replay again: still the original
        w.harness.get_by_label("Replay the last 10 seconds").click();
        w.harness.run_steps(2);
        assert!(latched(&w));
        // minimised and back: the backend dropped the last 10 seconds and
        // kept the result; the latch stays, and the next replay plays the
        // original
        for on in [true, false] {
            w.harness
                .input_mut()
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .minimized = Some(on);
            w.harness.run_steps(3);
        }
        assert!(pressed(&w), "latched after the minimise");
        w.harness.run_steps(12);
        w.harness.get_by_label("Replay the last 10 seconds").click();
        w.harness.run_steps(2);
        assert!(latched(&w));
        assert!(pressed(&w));
        assert!(!log.borrow().contains(&Command::Listen(Listen::HushMic)));
    }

    /// A backend the test changes while the page runs.
    struct Shared(std::rc::Rc<std::cell::RefCell<View>>, Log);

    impl Source for Shared {
        fn send(&mut self, c: Command) {
            self.1.borrow_mut().push(c);
        }
        fn view(&mut self, _: f64) -> View {
            self.0.borrow().clone()
        }
    }

    #[test]
    fn a_replay_without_a_result_keeps_the_latch() {
        let view = std::rc::Rc::new(std::cell::RefCell::new(View {
            phase: Phase::Result,
            result: Some(KEYBOARD_RESULT),
            can_replay: true,
            serial: 1,
            ..View::default()
        }));
        let log: Log = Default::default();
        let (v, l) = (view.clone(), log.clone());
        let mut w = testing::with_setup(
            crate::window::Link::recording(state(Scenario::OnInCall)),
            Page::Live,
            egui::vec2(960.0, 640.0),
            move |ps| ps.live.replay.source = Some(Box::new(Shared(v.clone(), l.clone()))),
        );
        w.harness.run_steps(2);
        w.harness.get_by_label(HOLD).click_accesskit();
        w.harness.run_steps(2);
        assert!(pressed(&w));
        let change = |w: &mut testing::Window, f: &dyn Fn(&mut View)| {
            f(&mut view.borrow_mut());
            view.borrow_mut().serial += 1;
            w.harness.run_steps(3);
        };
        // the last 10 seconds start over (a minimise, a blip): kept
        change(&mut w, &|v| v.can_replay = false);
        change(&mut w, &|v| v.can_replay = true);
        // a replay of a take with no result (muted, or too short): kept
        change(&mut w, &|v| {
            v.phase = Phase::Replaying;
            v.result = None;
        });
        change(&mut w, &|v| v.phase = Phase::Result);
        assert!(pressed(&w));
        assert!(!log.borrow().contains(&Command::Listen(Listen::HushMic)));
    }

    /// The real backend's state machine (no audio): the test plays its
    /// events.
    struct Real(
        std::rc::Rc<std::cell::RefCell<crate::miccheck::state::CheckState>>,
        Log,
    );

    impl Source for Real {
        fn send(&mut self, c: Command) {
            self.1.borrow_mut().push(c.clone());
            self.0.borrow_mut().on_command(&c);
        }
        fn view(&mut self, _: f64) -> View {
            self.0.borrow().view(Vec::new())
        }
    }

    #[test]
    fn off_and_down_drop_the_take_and_the_latch_a_new_strength_keeps_them() {
        use crate::miccheck::state::{CheckState, Event};
        // a running chain has its meter; one that went down has none
        let mut on = state(Scenario::OnInCall);
        on.engine.meter = Some("/run/hushmic/meter-1".into());
        let mut stronger = on.clone();
        stronger.sound.current.attn_limit += 6.0;
        let mut down = on.clone();
        down.engine.phase = EnginePhase::Down;
        let backend = std::rc::Rc::new(std::cell::RefCell::new(CheckState::new(&Setup {
            info: engine_info(&on),
            columns: false,
            once: false,
        })));
        let log: Log = Default::default();
        let (b, l) = (backend.clone(), log.clone());
        let mut w = testing::switched_with(
            vec![
                on.clone(),
                state(Scenario::Off),
                on.clone(),
                stronger.clone(),
                stronger,
                down,
            ],
            Page::Live,
            egui::vec2(960.0, 640.0),
            move |ps| ps.live.replay.source = Some(Box::new(Real(b.clone(), l.clone()))),
        );
        let event = |w: &mut testing::Switched, e: Event| {
            backend.borrow_mut().on_event(e);
            w.harness.run_steps(2);
        };
        // a take, latched and replayed to its end
        let take = |w: &mut testing::Switched| {
            event(w, Event::Ready(crate::miccheck::types::Source::Tap));
            event(w, Event::Buffered(10.0));
            w.harness.get_by_label(HOLD).click_accesskit();
            w.harness.run_steps(2);
            event(
                w,
                Event::Frozen {
                    result: Some(KEYBOARD_RESULT),
                    playback: PlaybackKind::Seamless,
                },
            );
            event(w, Event::PlaybackEnded);
            assert!(hold_pressed(w), "latched");
            assert_eq!(backend.borrow().view(Vec::new()).listen, Listen::Original);
            log.borrow_mut().clear();
        };
        take(&mut w);
        // Off: the backend drops the take, and the latch goes with it
        w.at.set(1);
        w.harness.run_steps(3);
        assert!(log.borrow().contains(&Command::Listen(Listen::HushMic)));
        assert_eq!(backend.borrow().view(Vec::new()).listen, Listen::HushMic);
        // on again, a new take; another strength goes to the running
        // chain: the take and the latch stay
        w.at.set(2);
        w.harness.run_steps(3);
        take(&mut w);
        w.at.set(3);
        w.harness.run_steps(3);
        assert!(hold_pressed(&w));
        assert!(!log.borrow().contains(&Command::Listen(Listen::HushMic)));
        assert_eq!(backend.borrow().view(Vec::new()).listen, Listen::Original);
        assert_eq!(backend.borrow().result(), Some(KEYBOARD_RESULT));
        // (a tap lets go of the latch for the next take)
        w.harness.get_by_label(HOLD).click_accesskit();
        w.harness.run_steps(2);
        assert!(!hold_pressed(&w));
        // on again, a new take; the engine down (it failed or stopped):
        // the page stays (round 3b, the problem in its notice), the
        // backend hears the engine is off and drops its take, and the
        // latch goes with it
        w.at.set(4);
        w.harness.run_steps(3);
        take(&mut w);
        w.at.set(5);
        w.harness.run_steps(3);
        assert!(log
            .borrow()
            .iter()
            .any(|c| matches!(c, Command::Engine(i) if i.engine == Engine::Off)));
        assert!(!hold_pressed(&w));
        assert_eq!(backend.borrow().view(Vec::new()).listen, Listen::HushMic);
    }

    /// R-D3: a hold that began on a take the backend still called seamless,
    /// whose replay then froze as sequential, ends with nothing (no latch),
    /// and the button works after it.
    #[test]
    fn a_hold_cut_short_by_a_sequential_freeze_counts_for_nothing() {
        use crate::miccheck::state::{CheckState, Event};
        let on = state(Scenario::OnInCall);
        for by_key in [false, true] {
            let backend = std::rc::Rc::new(std::cell::RefCell::new(CheckState::new(&Setup {
                info: engine_info(&on),
                columns: false,
                once: false,
            })));
            let log: Log = Default::default();
            let (b, l) = (backend.clone(), log.clone());
            let mut w = testing::switched_with(
                vec![on.clone()],
                Page::Live,
                egui::vec2(960.0, 640.0),
                move |ps| ps.live.replay.source = Some(Box::new(Real(b.clone(), l.clone()))),
            );
            let event = |w: &mut testing::Switched, e: Event| {
                backend.borrow_mut().on_event(e);
                w.harness.run_steps(2);
            };
            event(&mut w, Event::Ready(crate::miccheck::types::Source::Tap));
            event(&mut w, Event::Buffered(10.0));
            let at = w.harness.get_by_label(HOLD).rect().center();
            let press = |w: &mut testing::Switched, down: bool| {
                if by_key {
                    if down {
                        w.harness.key_down(egui::Key::Enter);
                    } else {
                        w.harness.key_up(egui::Key::Enter);
                    }
                } else {
                    let events = &mut w.harness.input_mut().events;
                    events.push(egui::Event::PointerMoved(at));
                    events.push(egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed: down,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
            };
            if by_key {
                w.harness.get_by_label(HOLD).focus();
                w.harness.run_steps(2);
            }
            log.borrow_mut().clear();
            // held past the tap: the original while held
            press(&mut w, true);
            w.harness.run_steps(20);
            assert!(log.borrow().contains(&Command::Hold(true)), "{by_key}");
            // the replay freezes as sequential, then the release
            event(
                &mut w,
                Event::Frozen {
                    result: None,
                    playback: PlaybackKind::Sequential,
                },
            );
            press(&mut w, false);
            w.harness.run_steps(3);
            assert!(
                !log.borrow().contains(&Command::Listen(Listen::Original)),
                "{by_key}: {:?}",
                log.borrow()
            );
            assert_eq!(backend.borrow().view(Vec::new()).listen, Listen::HushMic);
            assert!(!hold_pressed_seq(&w), "{by_key}");
            // the button works: the next Enter picks the original once
            log.borrow_mut().clear();
            w.harness.get_by_label("Hear the original").focus();
            w.harness.run_steps(2);
            w.harness.key_down(egui::Key::Enter);
            w.harness.run_steps(1);
            w.harness.key_up(egui::Key::Enter);
            w.harness.run_steps(3);
            let picks = log
                .borrow()
                .iter()
                .filter(|c| **c == Command::Listen(Listen::Original))
                .count();
            assert_eq!(picks, 1, "{by_key}: {:?}", log.borrow());
            assert!(hold_pressed_seq(&w), "{by_key}");
        }
    }

    fn hold_pressed_seq(w: &testing::Switched) -> bool {
        w.harness
            .get_by_label("Hear the original")
            .accesskit_node()
            .toggled()
            == Some(egui::accesskit::Toggled::True)
    }

    fn hold_pressed(w: &testing::Switched) -> bool {
        w.harness.get_by_label(HOLD).accesskit_node().toggled()
            == Some(egui::accesskit::Toggled::True)
    }

    #[test]
    fn enter_and_space_on_the_hold_button_follow_the_rule_and_never_replay() {
        let (mut w, log) = ready60();
        w.harness.get_by_label(HOLD).focus();
        w.harness.run_steps(2);
        // Enter tapped: the latch
        w.harness.key_down(egui::Key::Enter);
        w.harness.run_steps(1);
        w.harness.key_up(egui::Key::Enter);
        w.harness.run_steps(3);
        assert!(latched(&w));
        // Space tapped on the button: back to HushMic
        w.harness.key_down(egui::Key::Space);
        w.harness.run_steps(1);
        w.harness.key_up(egui::Key::Space);
        w.harness.run_steps(3);
        assert!(hearing_hushmic(&w));
        // Enter held: the original while held
        w.harness.key_down(egui::Key::Enter);
        w.harness.run_steps(20);
        assert!(hearing_original(&w));
        w.harness.key_up(egui::Key::Enter);
        w.harness.run_steps(3);
        assert!(hearing_hushmic(&w));
        assert!(!played(&log), "{:?}", log.borrow());
    }

    #[test]
    fn space_on_focused_replay_replays_once() {
        let (mut w, log) = ready60();
        w.harness.get_by_label("Replay the last 10 seconds").focus();
        w.harness.run_steps(2);
        w.harness.key_down(egui::Key::Space);
        w.harness.run_steps(2);
        w.harness.key_up(egui::Key::Space);
        w.harness.run_steps(3);
        let replays = log
            .borrow()
            .iter()
            .filter(|c| **c == Command::Replay)
            .count();
        assert_eq!(replays, 1, "{:?}", log.borrow());
        assert!(!log.borrow().contains(&Command::Hold(true)));
    }

    /// Ruling 3: page Space never starts a replay (a Space by accident
    /// would loop the last 10 seconds through the speakers) and never
    /// stops one; it compares while one plays.
    #[test]
    fn page_space_compares_only_while_a_replay_plays() {
        let (mut w, log) = ready60();
        let tap = |w: &mut testing::Window| {
            w.harness.key_down(egui::Key::Space);
            w.harness.run_steps(2);
            w.harness.key_up(egui::Key::Space);
            w.harness.run_steps(3);
        };
        // no replay: a tap and a hold do nothing
        tap(&mut w);
        w.harness.key_down(egui::Key::Space);
        w.harness.run_steps(20);
        w.harness.key_up(egui::Key::Space);
        w.harness.run_steps(3);
        assert!(log.borrow().is_empty(), "{:?}", log.borrow());
        assert!(!replaying(&w));
        // Replay by its button
        w.harness.get_by_label("Replay the last 10 seconds").click();
        w.harness.run_steps(3);
        assert!(log.borrow().contains(&Command::Replay));
        assert!(hearing_hushmic(&w));
        // a tap while it plays: no Stop, no flash of the original
        log.borrow_mut().clear();
        tap(&mut w);
        assert!(log.borrow().is_empty(), "{:?}", log.borrow());
        assert!(hearing_hushmic(&w));
        // held 180 ms: the original, until released
        w.harness.key_down(egui::Key::Space);
        w.harness.run_steps(6);
        assert!(log.borrow().is_empty(), "not before 180 ms");
        w.harness.run_steps(10);
        assert_eq!(*log.borrow(), vec![Command::Hold(true)]);
        assert!(hearing_original(&w));
        // Tab meanwhile: the release still lets go
        w.harness.key_press(egui::Key::Tab);
        w.harness.run_steps(2);
        w.harness.key_up(egui::Key::Space);
        w.harness.run_steps(3);
        assert_eq!(
            *log.borrow(),
            vec![Command::Hold(true), Command::Hold(false)]
        );
        assert!(hearing_hushmic(&w));
    }

    /// Ruling 3: a key held on a focused Replay/Stop switches it once; its
    /// repeats don't turn the replay on and off.
    #[test]
    fn a_key_held_on_replay_switches_it_once() {
        for key in [egui::Key::Space, egui::Key::Enter] {
            let (mut w, log) = ready60();
            w.harness.get_by_label("Replay the last 10 seconds").focus();
            w.harness.run_steps(2);
            let push = |w: &mut testing::Window, pressed: bool, repeat: bool| {
                w.harness.input_mut().events.push(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat,
                    modifiers: egui::Modifiers::NONE,
                });
                w.harness.step();
            };
            push(&mut w, true, false);
            for _ in 0..12 {
                push(&mut w, true, true);
            }
            push(&mut w, false, false);
            w.harness.run_steps(3);
            let switches = log
                .borrow()
                .iter()
                .filter(|c| matches!(c, Command::Replay | Command::Stop))
                .count();
            assert_eq!(switches, 1, "{key:?}: {:?}", log.borrow());
        }
    }

    /// A Live window whose demo take is replayable.
    fn ready(sc: Scenario) -> testing::Window {
        let mut w = testing::scenario(sc, Page::Live);
        for _ in 0..60 {
            w.harness.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(40));
            if w.harness
                .query_by_label("Replay the last 10 seconds")
                .is_some_and(|n| !n.accesskit_node().is_disabled())
            {
                break;
            }
        }
        w
    }

    #[test]
    fn while_a_replay_plays_the_picture_is_the_replay() {
        crate::i18n::pin_english();
        let mut w = ready(Scenario::OnInCall);
        let live = "Your microphone through HushMic, live.";
        assert!(w.harness.query_by_label_contains(live).is_some());
        w.harness.get_by_label("Replay the last 10 seconds").click();
        w.harness.run_steps(3);
        assert!(w
            .harness
            .query_by_label("The last 10 seconds, replaying through HushMic.")
            .is_some());
        // the original, held: the picture says which side plays
        pointer(&mut w, true);
        w.harness.run_steps(2);
        assert!(w
            .harness
            .query_by_label("The last 10 seconds, replaying your original microphone.")
            .is_some());
        pointer(&mut w, false);
        w.harness.run_steps(3);
        // stopped: now again
        w.harness.get_by_label("Stop").click();
        w.harness.run_steps(3);
        assert!(w.harness.query_by_label_contains(live).is_some());
    }

    /// The picture's name while a replay plays: the side that is heard.
    const CLEAN: &str = "The last 10 seconds, replaying through HushMic.";
    const ORIGINAL: &str = "The last 10 seconds, replaying your original microphone.";

    fn hearing_original(w: &testing::Window) -> bool {
        w.harness.query_by_label(ORIGINAL).is_some()
    }

    fn hearing_hushmic(w: &testing::Window) -> bool {
        w.harness.query_by_label(CLEAN).is_some()
    }

    fn replaying(w: &testing::Window) -> bool {
        hearing_hushmic(w) || hearing_original(w)
    }

    /// A tap left the original on: it plays, and the button keeps its
    /// pressed look.
    fn latched(w: &testing::Window) -> bool {
        hearing_original(w) && pressed(w)
    }

    #[test]
    fn a_space_tap_within_one_frame_does_not_stick() {
        // egui with one pass per frame: the press and the release arrive
        // in the same input
        let ctx = egui::Context::default();
        ctx.options_mut(|o| o.max_passes = std::num::NonZeroUsize::MIN);
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut page = Replay::new(crate::pages::Feed::Demo);
        page.source = Some(Box::new(Recorder(log.clone())));
        let space = |pressed| egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let input = egui::RawInput {
            events: vec![space(true), space(false)],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            // as `begin` reads them
            if page
                .hold
                .keys(ctx, egui::LayerId::background(), &[], true, Kind::Hold, 0.0)
            {
                page.replay_or_stop(false);
            }
        });
        // a tap: Replay, and never a flash of the original
        assert!(!page.hold.compare.space_is_down());
        assert!(!page.hold.compare.on);
        assert!(!log.borrow().contains(&Command::Hold(true)));
        assert!(log.borrow().contains(&Command::Replay));
    }

    #[test]
    fn muted_offers_unmute_to_where_it_returns() {
        let mut w = ready(Scenario::Muted);
        let back = state(Scenario::Muted).unmute_to;
        w.harness.get_by_label("Muted").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SetMode(back)]);
    }

    #[test]
    fn a_replay_that_ended_leaves_no_line() {
        crate::i18n::pin_english();
        let r = Replay::new(Feed::Demo);
        let mut v = view_with(Phase::Result);
        assert_eq!(r.status(&state(Scenario::OnNoApp), &v), None);
        v.result = Some(KEYBOARD_RESULT);
        assert_eq!(r.status(&state(Scenario::OnNoApp), &v), None);
    }

    #[test]
    fn stopped_keeps_the_page_and_the_microphone_closed() {
        crate::i18n::pin_english();
        for (sc, line) in [
            (
                Scenario::NeedsAttention,
                "Replay works again once the HushMic microphone is back.",
            ),
            (
                Scenario::PipewireUnreachable,
                "Replay works again once PipeWire is back.",
            ),
        ] {
            let w = testing::scenario(sc, Page::Live);
            assert!(w.harness.query_by_label("Try again").is_some(), "{sc:?}");
            assert!(
                w.harness.query_by_label("Copy diagnostics").is_some(),
                "{sc:?}"
            );
            // the row stays, with nothing to replay
            let replay = w.harness.get_by_label("Replay the last 10 seconds");
            assert!(replay.accesskit_node().is_disabled(), "{sc:?}");
            // why, in its tooltip
            assert_eq!(
                replay.accesskit_node().description().as_deref(),
                Some(line),
                "{sc:?}"
            );
            // the backend opens no microphone: the engine is off, or
            // PipeWire is gone
            let info = engine_info(&state(sc));
            assert!(info.engine == Engine::Off || !info.pipewire, "{sc:?}");
        }
    }

    /// Records what the page tells its backend.
    struct Recorder(std::rc::Rc<std::cell::RefCell<Vec<Command>>>);

    impl Source for Recorder {
        fn send(&mut self, c: Command) {
            self.0.borrow_mut().push(c);
        }
        fn view(&mut self, _: f64) -> View {
            View::default()
        }
    }

    #[test]
    fn a_minimise_keeps_the_backend_and_its_result() {
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let l = log.clone();
        let mut w = testing::with_setup(
            crate::window::Link::recording(state(Scenario::OnInCall)),
            Page::Live,
            egui::vec2(960.0, 640.0),
            move |ps| ps.live.replay.source = Some(Box::new(Recorder(l.clone()))),
        );
        let minimise = |w: &mut testing::Window, on: bool| {
            w.harness
                .input_mut()
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .minimized = Some(on);
            w.harness.run_steps(3);
        };
        minimise(&mut w, true);
        minimise(&mut w, false);
        // told hidden once, then shown again: the same backend, not a new one
        let vis: Vec<Command> = log
            .borrow()
            .iter()
            .filter(|c| matches!(c, Command::Visible(_)))
            .cloned()
            .collect();
        assert_eq!(
            vis,
            vec![
                Command::Visible(true),
                Command::Visible(false),
                Command::Visible(true)
            ]
        );
    }

    #[test]
    fn leaving_the_page_lets_go_of_the_microphone() {
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut page = Replay::new(crate::pages::Feed::Demo);
        page.source = Some(Box::new(Recorder(log.clone())));
        page.hide();
        assert!(page.source.is_none());
        assert_eq!(*log.borrow(), vec![Command::Visible(false)]);
    }

    #[test]
    fn off_offers_turning_hushmic_on() {
        let mut w = testing::scenario(Scenario::Off, Page::Live);
        assert_eq!(
            w.harness
                .get_by_label("Replay the last 10 seconds")
                .accesskit_node()
                .description()
                .as_deref(),
            Some("Replay works while HushMic is on.")
        );
        assert!(w
            .harness
            .get_by_label("Replay the last 10 seconds")
            .accesskit_node()
            .is_disabled());
        w.harness.get_by_label("Turn on HushMic").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SetMode(Mode::Suppress)]);
    }

    #[test]
    fn engine_facts_follow_the_state() {
        let i = engine_info(&state(Scenario::Muted));
        assert_eq!(i.engine, Engine::Muted);
        assert_eq!(i.mic_name, "RODE NT-USB");
        assert_eq!(engine_info(&state(Scenario::Off)).engine, Engine::Off);
        assert!(!engine_info(&state(Scenario::PipewireUnreachable)).pipewire);
    }

    /// A backend that shows one view and records what it is told.
    struct Fixed(View, std::rc::Rc<std::cell::RefCell<Vec<Command>>>);

    impl Source for Fixed {
        fn send(&mut self, c: Command) {
            self.1.borrow_mut().push(c);
        }
        fn view(&mut self, _: f64) -> View {
            self.0.clone()
        }
    }

    /// Replay and Hold off with no take yet say why on hover; while
    /// replay waits for HushMic, the reason the row's line gives.
    #[test]
    fn replay_and_hold_off_say_why() {
        crate::i18n::pin_english();
        let tip = |sc: Scenario, button: &str| {
            // listening with nothing kept yet (Live just opened again)
            let v = View {
                can_replay: false,
                serial: 1,
                ..view_with(Phase::Listening)
            };
            let log: std::rc::Rc<std::cell::RefCell<Vec<Command>>> = Default::default();
            let mut w = testing::with_setup(
                crate::window::Link::recording(state(sc)),
                Page::Live,
                egui::vec2(960.0, 640.0),
                move |ps| {
                    ps.no_launch();
                    ps.live.replay.source = Some(Box::new(Fixed(v.clone(), log.clone())));
                },
            );
            let b = w.harness.get_by_label(button);
            assert!(b.accesskit_node().is_disabled(), "{sc:?} {button}");
            b.hover();
            w.harness.run_steps(6);
            let found = |t: &str| w.harness.query_all_by_label_contains(t).count();
            (
                found("Nothing to replay yet."),
                found("The last 10 seconds start once HushMic hears your microphone."),
            )
        };
        assert_eq!(
            tip(Scenario::OnInCall, "Replay the last 10 seconds"),
            (1, 0)
        );
        assert_eq!(tip(Scenario::OnInCall, "Hold to hear the original"), (1, 0));
        // replay waits for HushMic to start: the row's own reason
        assert_eq!(
            tip(Scenario::Starting, "Replay the last 10 seconds"),
            (0, 1)
        );
    }

    /// A backend that lets go of its take when the page is hidden (a
    /// minimise): Replay and Hold come back disabled.
    struct Drops(View);

    impl Source for Drops {
        fn send(&mut self, c: Command) {
            if c == Command::Visible(false) {
                self.0.can_replay = false;
                self.0.timeline.columns.clear();
                self.0.serial += 1;
            }
        }
        fn view(&mut self, _: f64) -> View {
            self.0.clone()
        }
    }

    /// A backend whose engine is off while the flag is set.
    struct Turns(View, std::rc::Rc<std::cell::Cell<bool>>);

    impl Source for Turns {
        fn send(&mut self, _: Command) {}
        fn view(&mut self, _: f64) -> View {
            let mut v = self.0.clone();
            if self.1.get() {
                v.phase = Phase::Blocked(Blocker::Off);
                v.serial += 1;
            }
            v
        }
    }

    #[test]
    fn turned_off_under_the_keyboard_the_rail_takes_it() {
        let v = View {
            phase: Phase::Listening,
            timeline: Timeline {
                columns: vec![
                    Column {
                        raw: 0.5,
                        clean: 0.2
                    };
                    200
                ],
                live: true,
                position: None,
            },
            can_replay: true,
            serial: 1,
            ..View::default()
        };
        let off = std::rc::Rc::new(std::cell::Cell::new(false));
        let o = off.clone();
        let mut w = testing::with_setup(
            crate::window::Link::recording(state(Scenario::OnInCall)),
            Page::Live,
            egui::vec2(960.0, 640.0),
            move |ps| ps.live.replay.source = Some(Box::new(Turns(v.clone(), o.clone()))),
        );
        w.harness.get_by_label("Hold to hear the original").focus();
        w.harness.run_steps(2);
        // turned off from the tray: the row turns disabled, and the
        // keyboard goes to the page's rail row (Live's header has Turn on)
        off.set(true);
        w.harness.run_steps(3);
        assert!(w
            .harness
            .get_by_label("Hold to hear the original")
            .accesskit_node()
            .is_disabled());
        assert_eq!(focused(&w).as_deref(), Some("Live"));
        // on again: the row is back
        off.set(false);
        w.harness.run_steps(3);
        assert!(!w
            .harness
            .get_by_label("Replay the last 10 seconds")
            .accesskit_node()
            .is_disabled());
    }

    /// The name of the node with the keyboard.
    fn focused(w: &testing::Window) -> Option<String> {
        w.harness
            .query_by(|n| n.is_focused())
            .and_then(|n| n.accesskit_node().label())
    }

    #[test]
    fn a_control_disabled_under_the_keyboard_hands_it_to_the_rail() {
        let v = View {
            phase: Phase::Listening,
            timeline: Timeline {
                columns: vec![
                    Column {
                        raw: 0.5,
                        clean: 0.2
                    };
                    200
                ],
                live: true,
                position: None,
            },
            can_replay: true,
            serial: 1,
            ..View::default()
        };
        for control in ["Replay the last 10 seconds", "Hold to hear the original"] {
            let v = v.clone();
            let mut w = testing::with_setup(
                crate::window::Link::recording(state(Scenario::OnInCall)),
                Page::Live,
                egui::vec2(960.0, 640.0),
                move |ps| ps.live.replay.source = Some(Box::new(Drops(v.clone()))),
            );
            w.harness.get_by_label(control).focus();
            w.harness.run_steps(2);
            assert!(w.harness.get_by_label(control).is_focused());
            for on in [true, false] {
                w.harness
                    .input_mut()
                    .viewports
                    .entry(egui::ViewportId::ROOT)
                    .or_default()
                    .minimized = Some(on);
                w.harness.run_steps(3);
            }
            assert!(w
                .harness
                .get_by_label(control)
                .accesskit_node()
                .is_disabled());
            assert_eq!(focused(&w).as_deref(), Some("Live"), "{control}");
        }
    }

    #[test]
    fn a_sequential_take_picks_its_side_with_the_button() {
        // switching at the same moment isn't possible: the button is a
        // latch that picks the side Replay plays
        let v = View {
            phase: Phase::Listening,
            timeline: Timeline {
                columns: vec![
                    Column {
                        raw: 0.5,
                        clean: 0.2
                    };
                    200
                ],
                live: true,
                position: None,
            },
            can_replay: true,
            playback: PlaybackKind::Sequential,
            serial: 1,
            ..View::default()
        };
        let window = |v: &View| {
            let log: Log = Default::default();
            let (v, l) = (v.clone(), log.clone());
            let w = testing::with_setup(
                crate::window::Link::recording(state(Scenario::OnInCall)),
                Page::Live,
                egui::vec2(960.0, 640.0),
                move |ps| ps.live.replay.source = Some(Box::new(Fixed(v.clone(), l.clone()))),
            );
            log.borrow_mut().clear();
            (w, log)
        };
        const SEQ: &str = "Hear the original";
        let (mut w, log) = window(&v);
        assert!(!shows(&w, HOLD) && !shows(&w, "or hold"), "no holding here");
        assert!(w
            .harness
            .query_by_label_contains("Each side plays from the start.")
            .is_some());
        let button = w.harness.get_by_label(SEQ);
        assert_eq!(
            button.accesskit_node().description().as_deref(),
            Some("Press to switch sides. Each side plays from the start.")
        );
        button.click_accesskit();
        w.harness.run_steps(2);
        let chosen = |w: &testing::Window| {
            w.harness.get_by_label(SEQ).accesskit_node().toggled()
                == Some(egui::accesskit::Toggled::True)
        };
        assert!(chosen(&w), "pressed while the original is chosen");
        w.harness.get_by_label("Replay the last 10 seconds").click();
        w.harness.run_steps(2);
        assert_eq!(
            *log.borrow(),
            vec![Command::Listen(Listen::Original), Command::Replay]
        );
        // Enter held on the focused button: one switch, key repeats aside
        log.borrow_mut().clear();
        w.harness.get_by_label(SEQ).focus();
        w.harness.run_steps(2);
        let key = |pressed, repeat| egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed,
            repeat,
            modifiers: egui::Modifiers::NONE,
        };
        w.harness.input_mut().events.push(key(true, false));
        w.harness.run_steps(1);
        for _ in 0..6 {
            w.harness.input_mut().events.push(key(true, true));
            w.harness.run_steps(1);
        }
        assert!(log.borrow().is_empty(), "it switches on the release");
        w.harness.input_mut().events.push(key(false, false));
        w.harness.run_steps(2);
        assert_eq!(*log.borrow(), vec![Command::Listen(Listen::HushMic)]);
        assert!(!chosen(&w));
        // page Space is Replay/Stop, once per press however long it's held
        log.borrow_mut().clear();
        w.harness.get_by_label("Replay the last 10 seconds").focus();
        w.harness.run_steps(2);
        w.harness.key_down(egui::Key::Space);
        w.harness.run_steps(4);
        w.harness.key_up(egui::Key::Space);
        w.harness.run_steps(2);
        assert_eq!(*log.borrow(), vec![Command::Replay]);
        // while it plays the picture says which side; the button picks
        // the other
        let (mut w, _) = window(&View {
            phase: Phase::Replaying,
            ..v.clone()
        });
        assert!(hearing_hushmic(&w));
        w.harness.get_by_label(SEQ).click_accesskit();
        w.harness.run_steps(2);
        assert!(hearing_original(&w) && chosen(&w));
    }

    #[test]
    fn the_hold_waits_for_a_take_the_backend_can_replay() {
        // columns already, but not enough to replay: the backend would
        // ignore a hold
        let v = View {
            phase: Phase::Listening,
            timeline: Timeline {
                columns: vec![
                    Column {
                        raw: 0.5,
                        clean: 0.2
                    };
                    10
                ],
                live: true,
                position: None,
            },
            can_replay: false,
            serial: 1,
            ..View::default()
        };
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let l = log.clone();
        let mut w = testing::with_setup(
            crate::window::Link::recording(state(Scenario::OnInCall)),
            Page::Live,
            egui::vec2(960.0, 640.0),
            move |ps| ps.live.replay.source = Some(Box::new(Fixed(v.clone(), l.clone()))),
        );
        assert!(w
            .harness
            .get_by_label("Hold to hear the original")
            .accesskit_node()
            .is_disabled());
        w.harness.key_down(egui::Key::Space);
        w.harness.run_steps(2);
        w.harness.key_up(egui::Key::Space);
        w.harness.run_steps(2);
        assert!(
            !log.borrow().contains(&Command::Hold(true)),
            "{:?}",
            log.borrow()
        );
    }

    #[test]
    fn space_on_the_quit_question_answers_it_and_holds_nothing() {
        let v = View {
            phase: Phase::Listening,
            timeline: Timeline {
                columns: vec![
                    Column {
                        raw: 0.5,
                        clean: 0.2
                    };
                    200
                ],
                live: true,
                position: None,
            },
            can_replay: true,
            serial: 1,
            ..View::default()
        };
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let l = log.clone();
        let mut w = testing::with_setup(
            crate::window::Link::recording(state(Scenario::OnInCall)),
            Page::Live,
            egui::vec2(960.0, 640.0),
            move |ps| ps.live.replay.source = Some(Box::new(Fixed(v.clone(), l.clone()))),
        );
        // an app uses HushMic: Ctrl+Q asks, the keyboard on Cancel
        w.harness
            .key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Q);
        w.harness.run_steps(2);
        assert!(w.harness.get_by_label("Cancel").is_focused());
        w.harness.key_press(egui::Key::Space);
        w.harness.run_steps(3);
        assert!(
            !log.borrow().contains(&Command::Hold(true)),
            "{:?}",
            log.borrow()
        );
        // Space pressed Cancel: the question is gone, nothing quit
        assert!(w.harness.query_by_label("Cancel").is_none());
        assert!(w.link.sent().is_empty());
    }
}
