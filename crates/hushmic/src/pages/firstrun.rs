//! First run, three steps in the whole window (no rail): 1 · which
//! microphone HushMic cleans (with a live level of the chosen one), 2 ·
//! hear it work (Live's picture: the microphone's lane above, what apps
//! hear below), 3 · use it in your apps (the default-microphone
//! and login switches, and a line that says when an app uses HushMic).
//!
//! Never forces the state: in Bypass, Muted or Off step 2 says so and
//! offers the one control that changes it.

use eframe::egui::{self, pos2, vec2, Rect, Ui};

use super::util::{self, child, rect, text_as, text_at, Speak};
use super::{Cx, Feed};
use crate::appstate::{Action, AppState, Mode, Page, Phase};
use crate::diagnostics::EngineTier;
use crate::miccheck::probe::{LevelProbe, ProbeState};
use crate::miccheck::types::{Command, EngineInfo, Phase as CheckPhase, Setup};
use crate::miccheck::MicCheck;
use crate::tr;
use crate::ui::fonts::Type;
use crate::ui::icons::{self, Icon};
use crate::ui::paint::live::LiveState;
use crate::ui::paint::spectro::{Sizes, Spectro};
use crate::ui::text;
use crate::ui::tokens::{radius, Palette};
use crate::ui::widgets::{self, Button, PickBy, PickChoice, StepState, Switch};

const MAX_W: f32 = 600.0;
/// Step 2's lanes: their height, in a wide window and in a compact one.
const LANE: (f32, f32) = (112.0, 72.0);
/// While waiting for an app, ask the daemon to look again this often.
const REFRESH_SECS: f64 = 1.0;

pub struct FirstRunPage {
    feed: Feed,
    /// Step 2's picture, as Live's: the two lanes of what the Mic check
    /// backend hears.
    spectro: Spectro,
    step_shown: Option<u8>,
    last_refresh: f64,
    /// A screenshot scene: the end state at once, the columns stay put.
    scene: bool,
    level: f32,
    /// Step 1: the chosen microphone's level.
    probe: Option<LevelProbe>,
    /// Step 2: the Mic check backend (the chain runs while it listens,
    /// even before an app uses HushMic). It holds the picture and the take
    /// "Play the last 10 seconds" plays.
    check: Option<Box<dyn super::replay::Source>>,
    check_info: Option<EngineInfo>,
    /// Step 1: a microphone reached with the arrow keys, sent once they
    /// have been still for [`ARROW_SETTLE`] (each change restarts the
    /// chain).
    arrowed: Option<(Option<String>, f64)>,
    /// Step 2: the control used while it had the focus (its id). The
    /// control that replaces it takes the focus, or Continue when none
    /// does, unless something else took it meanwhile.
    handover: Option<egui::Id>,
    /// Step 2 drew a control this pass.
    control_drawn: bool,
}

/// The arrow keys rest this long before the microphone they reached is
/// chosen.
const ARROW_SETTLE: f64 = 0.4;
/// Step 1's microphone list (the kit's pick group).
const MIC_PICKS: &str = "fr-mics";

impl FirstRunPage {
    pub fn new(feed: Feed) -> FirstRunPage {
        FirstRunPage {
            feed,
            spectro: Spectro::new(),
            step_shown: None,
            last_refresh: f64::NEG_INFINITY,
            scene: false,
            level: 0.0,
            probe: None,
            check: None,
            check_info: None,
            arrowed: None,
            handover: None,
            control_drawn: false,
        }
    }

    /// Tests: whether step 2's picture holds anything.
    #[cfg(test)]
    fn has_picture(&self) -> bool {
        !self.spectro.forgotten()
    }

    pub fn prefill(&mut self, cols: usize) {
        self.scene = cols > 0;
    }

    pub fn hide(&mut self) {
        self.spectro.forget();
        self.step_shown = None;
        self.drop_sources();
    }

    /// Leaving step 1: the microphone the arrows reached is chosen now.
    fn flush_arrowed(&mut self, ctx: &egui::Context, cx: &mut Cx<'_>) {
        if let Some((name, _)) = self.arrowed.take() {
            choose_arrowed(ctx, cx, name);
        }
    }

    /// The window is minimised: let go of the microphone, keep the step.
    pub fn let_go(&mut self) {
        self.drop_sources();
    }

    /// Let go of the microphone (the level probe, the Mic check backend).
    fn drop_sources(&mut self) {
        self.probe = None;
        if let Some(mut c) = self.check.take() {
            c.send(Command::Visible(false));
        }
        self.check_info = None;
    }

    pub fn ui(&mut self, ui: &mut Ui, full: Rect, cx: &mut Cx<'_>) {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let s = cx.state;
        let step = s.ui.first_run_step.unwrap_or(1).clamp(1, 3);
        if self.step_shown != Some(step) {
            self.drop_sources();
            self.step_shown = Some(step);
            self.spectro.forget();
            self.handover = None;
        }
        self.control_drawn = false;
        if step == 3 && super::status::filtering(s) {
            cx.light.static_edge = true;
        }

        // ---- top: the steps and Skip
        let top_h = 16.0 + 32.0 + 16.0;
        let labels = [tr!("fr-step-mic"), tr!("fr-step-hear"), tr!("fr-step-apps")];
        let steps: Vec<(&str, StepState)> = labels
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let n = i as u8 + 1;
                let st = if n < step {
                    StepState::Done
                } else if n == step {
                    StepState::Current
                } else {
                    StepState::Ahead
                };
                (l.as_str(), st)
            })
            .collect();
        let mut c = child(
            ui,
            rect(
                full.left() + 24.0,
                full.top() + 16.0 + 6.0,
                full.width() - 48.0,
                20.0,
            ),
            "fr-steps",
        );
        let current = labels[usize::from(step - 1)].clone();
        let steps_name = tr!(
            "fr-steps-name",
            n = step,
            total = labels.len(),
            step = current
        );
        widgets::step_dots(&mut c, &steps_name, &steps, cx.compact);
        // Skip ends the setup from steps 1 and 2 (UX panel B20); the last
        // step has Done instead
        if step < 3 {
            let skip = tr!("fr-skip");
            let sb = Button::ghost(&skip).sm();
            let sw = sb.width(&ctx);
            if util::put(
                ui,
                rect(full.right() - 24.0 - sw, full.top() + 16.0, sw, 32.0),
                "fr-skip",
                sb,
            )
            .clicked()
            {
                self.flush_arrowed(&ctx, cx);
                finish(cx);
            }
        }

        // ---- the footer's place
        let foot_h = 16.0 + 40.0 + 16.0 + 1.0;
        let foot = Rect::from_min_max(pos2(full.left(), full.bottom() - foot_h), full.max);

        // ---- the step, centred, scrolling when the window is small
        let body = Rect::from_min_max(
            pos2(full.left(), full.top() + top_h),
            pos2(full.right(), foot.top()),
        );
        let mut bu = child(ui, body, ("fr-body", step));
        bu.set_clip_rect(body);
        let out = egui::ScrollArea::vertical()
            .scroll_source(egui::scroll_area::ScrollSource {
                drag: false,
                ..egui::scroll_area::ScrollSource::ALL
            })
            .id_salt(("fr-scroll", step))
            .auto_shrink([false, false])
            .show(&mut bu, |ui| {
                let area = ui.max_rect();
                let (pt, px, pb) = if cx.compact {
                    (8.0, 24.0, 16.0)
                } else {
                    (16.0, 32.0, 24.0)
                };
                let w = (area.width() - 2.0 * px).min(MAX_W);
                let x = area.left() + ((area.width() - w) / 2.0).max(px);
                let y = area.top() + pt;
                let bottom = match step {
                    1 => self.step_mic(ui, cx, x, y, w),
                    2 => self.step_hear(ui, cx, x, y, w),
                    _ => self.step_apps(ui, cx, x, y, w),
                };
                util::reach(ui, area.min, area.width(), bottom + pb);
            });
        crate::ui::widgets::scroll::bar(&bu, &out, &labels[usize::from(step - 1)]);

        // ---- bottom: Back and Continue / Done (after the step: the
        // reading and Tab order is the step, then its buttons)
        ui.painter().rect_filled(
            Rect::from_min_size(foot.min, vec2(foot.width(), 1.0)),
            0.0,
            pal.line,
        );
        if step > 1 {
            let back = tr!("fr-back");
            let b = Button::ghost(&back);
            let bw = b.width(&ctx);
            let r = util::put(
                ui,
                rect(foot.left() + 24.0, foot.top() + 17.0, bw, 40.0),
                "fr-back",
                b,
            );
            tell_step(&ctx, &r, &steps_name);
            if r.clicked() {
                cx.send(Action::UiFirstRun(Some(step - 1)));
            }
        }
        let next = if step == 3 {
            tr!("fr-done")
        } else {
            tr!("fr-continue")
        };
        let b = Button::primary(&next);
        let bw = b.width(&ctx);
        // step 1, where Back sits later: someone here only for their
        // recordings goes straight to Files (F14); the question goes when
        // the window is too narrow for it
        if step == 1 {
            let room = foot.width() - 48.0 - bw - 24.0;
            let go = tr!("fr-files-go");
            let ask = tr!("fr-files-only");
            let lw = text::layout(&ctx, &go, Type::LINK, pal.ink_muted, f32::INFINITY)
                .size()
                .x;
            let q = text::layout(&ctx, &ask, Type::CAP, pal.ink_subtle, f32::INFINITY);
            let y = foot.top() + 17.0 + 11.0;
            let mut x = foot.left() + 24.0;
            if q.size().x + 6.0 + lw <= room {
                let qw = q.size().x;
                text_at(ui, q, pos2(x, y));
                x += qw + 6.0;
            }
            let mut c = child(ui, rect(x, y, lw.min(room), 18.0), "fr-files-go");
            if widgets::link(&mut c, &go, false, true).clicked() {
                self.flush_arrowed(&ctx, cx);
                cx.send(Action::UiFirstRun(None));
                cx.goto = Some(Page::Files);
            }
        }
        let r = util::put(
            ui,
            rect(foot.right() - 24.0 - bw, foot.top() + 17.0, bw, 40.0),
            "fr-next",
            b,
        );
        tell_step(&ctx, &r, &steps_name);
        // step 2's control went (filtering now): its focus comes here
        if !self.control_drawn {
            if let Some(old) = self.handover.take() {
                util::take_focus(&r, old);
            }
        }
        if r.clicked() {
            self.flush_arrowed(&ctx, cx);
            if step == 3 {
                finish(cx);
            } else {
                cx.send(Action::UiFirstRun(Some(step + 1)));
            }
        }
    }

    // ---------------------------------------------------------------- 1 · microphone

    fn step_mic(&mut self, ui: &mut Ui, cx: &mut Cx<'_>, x: f32, y: f32, w: f32) -> f32 {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let s = cx.state;
        let mut y = text_as(
            ui,
            text::layout(&ctx, &tr!("fr-mic-title"), Type::H, pal.ink, w),
            pos2(x, y),
            Speak::LiveHeading(1),
        )
        .bottom()
            + 6.0;
        y = text_at(
            ui,
            text::layout(&ctx, &tr!("fr-mic-sub"), Type::SUB, pal.ink_muted, w),
            pos2(x, y),
        )
        .bottom()
            + 20.0;
        let default_now = s
            .mic
            .system_default
            .as_deref()
            .and_then(|d| s.description_of(d))
            .unwrap_or("")
            .to_owned();
        let mut options: Vec<(Option<String>, String, String)> = vec![(
            None,
            tr!("live-system-default"),
            tr!("fr-mic-default-note", device = default_now),
        )];
        for d in s.mic.devices.iter().filter(|d| d.present) {
            options.push((
                Some(d.name.clone()),
                d.description.clone(),
                device_note(&d.name, &d.description),
            ));
        }
        let unsupported = self.tick_level(&ctx, s);
        let live_capture = s.pipewire.live_capture != Some(false) && !unsupported;
        // the picks are one radio group ("1 of 4", one Tab stop, the arrows
        // move the highlight at once); the level follows it
        let mut group = crate::ui::widgets::a11y_children(
            ui,
            rect(x, y, w, 4000.0),
            egui::accesskit::Role::RadioGroup,
            &tr!("live-microphone"),
        );
        let selected = options
            .iter()
            .position(|o| o.0 == s.mic.selected)
            .unwrap_or(0);
        // each pick is its microphone (None: System default), so the
        // highlight follows the microphone when the list changes
        let keys: Vec<Option<String>> = options.iter().map(|o| o.0.clone()).collect();
        let mut picks =
            widgets::PickGroup::begin(&group, egui::Id::new(MIC_PICKS), &keys, selected);
        for (i, (_, label, note)) in options.iter().enumerate() {
            let ui = &mut group;
            let mut c = child(ui, rect(x, y, w, 200.0), ("fr-mic", i));
            picks.pick(&mut c, i, label, note);
            y = c.min_rect().bottom() + 8.0;
        }
        // a click, Space or Enter chooses at once; the arrows once they rest
        match picks.end(&group) {
            Some(PickChoice {
                index,
                by: PickBy::Activate,
            }) => {
                self.arrowed = None;
                if index != selected {
                    cx.send(Action::SelectMic(options[index].0.clone()));
                }
            }
            Some(PickChoice {
                index,
                by: PickBy::Arrow,
            }) => self.arrowed = Some((options[index].0.clone(), cx.now)),
            None => {}
        }
        if let Some((name, at)) = self.arrowed.clone() {
            let left = at + ARROW_SETTLE - cx.now;
            if left <= 0.0 {
                self.arrowed = None;
                choose_arrowed(&ctx, cx, name);
            } else {
                crate::ui::motion::repaint_in(&ctx, std::time::Duration::from_secs_f64(left));
            }
        }
        // the chosen microphone's live level in one fixed slot under the
        // list, so picking another never moves the page (UX panel D6)
        y += 4.0;
        if live_capture {
            let mut c = child(ui, rect(x, y, w, 14.0), "fr-level");
            widgets::level_meter(&mut c, &tr!("fr-level"), self.level, 32, false);
            y += 14.0 + 6.0;
            y = text_at(
                ui,
                text::layout(&ctx, &tr!("fr-level-cap"), Type::CAP, pal.ink_subtle, w),
                pos2(x, y),
            )
            .bottom();
        } else {
            y = text_at(
                ui,
                text::layout(&ctx, &tr!("fr-level-old"), Type::CAP, pal.ink_subtle, w),
                pos2(x, y),
            )
            .bottom();
        }
        y += 20.0;
        if super::status::stopped(s) {
            // HushMic can't work: what is wrong and what to do, in place
            // of the engine line
            return super::live::attention(ui, cx, x, y + 8.0, w);
        }
        let (line, dot) = match (s.mode, s.engine.phase) {
            (Mode::Off, _) => (tr!("fr-engine-off"), pal.line_strong),
            (_, Phase::Ready) => (tr!("fr-engine-ready"), pal.accent),
            _ => (tr!("fr-engine-starting"), pal.line_strong),
        };
        ui.painter().circle_filled(pos2(x + 3.0, y + 9.0), 3.0, dot);
        text_at(
            ui,
            text::layout(&ctx, &line, Type::CAP, pal.ink_subtle, w - 14.0),
            pos2(x + 14.0, y),
        )
        .bottom()
    }

    /// The chosen microphone's level (first-run step 1). True when this
    /// PipeWire cannot record for it (the old-PipeWire note instead).
    fn tick_level(&mut self, ctx: &egui::Context, s: &AppState) -> bool {
        match self.feed {
            Feed::Demo => {
                // the board's still level
                self.level = 0.62;
                false
            }
            Feed::Meter => {
                let node = s.mic.selected.clone();
                if self
                    .probe
                    .as_ref()
                    .is_none_or(|p| p.node() != node.as_deref())
                {
                    self.probe = Some(LevelProbe::start(node, util::wake(ctx)));
                }
                let probe = self.probe.as_ref().expect("started above");
                self.level = probe.level().unwrap_or(0.0);
                probe.state() == ProbeState::Unsupported
            }
        }
    }

    // ---------------------------------------------------------------- 2 · hear it work

    fn step_hear(&mut self, ui: &mut Ui, cx: &mut Cx<'_>, x: f32, y: f32, w: f32) -> f32 {
        if super::status::stopped(cx.state) {
            // nothing to hear: what is wrong and what to do, and the
            // microphone stays closed
            self.drop_sources();
            self.spectro.forget();
            return super::live::attention(ui, cx, x, y, w);
        }
        let ctx = ui.ctx().clone();
        let kit = crate::ui::kit(&ctx);
        let pal = kit.pal;
        let s = cx.state;
        let target = hear_state(s);
        let sounding = matches!(
            target,
            LiveState::On | LiveState::Bypass | LiveState::Muted | LiveState::Unfiltered
        );
        // the window's lit top edge while HushMic filters, as on Live
        if super::status::filtering(s) {
            cx.light.static_edge = true;
        }
        // The backend listens (the chain runs for it) and holds the
        // picture; a scene opens on the end state, its take there.
        let take = if self.scene { SCENE_TAKE } else { 0 };
        let check = listen(
            &mut self.check,
            &mut self.check_info,
            self.feed,
            take,
            &ctx,
            s,
        );
        let v = check.view(cx.now);
        let moving = match check.picture() {
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
            crate::ui::motion::moment_repaint(&ctx);
        } else if self.feed == Feed::Demo && sounding && !self.scene {
            // the demo's ten seconds go on with the frames
            crate::ui::motion::live_repaint(&ctx);
        }
        // a card with the two lanes in it, as Live's
        let lane = if cx.compact { LANE.1 } else { LANE.0 };
        let pad = super::live::CARD_PAD + 1.0;
        let fig = rect(x, y, w, Sizes::of(cx.compact, lane).height() + 2.0 * pad);
        let p = ui.painter().clone();
        p.add(widgets::card_fill_shape(fig, &pal));
        widgets::card_edge(&p, fig, &pal);
        let pic = fig.shrink(pad);
        let name = match target {
            LiveState::On => tr!("fr-hear-pic"),
            // outside filtering the picture says what Live's says
            _ => super::live::state_name(s),
        };
        let r = ui.interact(pic, ui.id().with("fr-picture"), egui::Sense::hover());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, &name));
        super::live::describe_mic(&ctx, r.id, s);
        let silent = v.level.verdict == crate::miccheck::LevelVerdict::NoSound;
        let (mut show, _) = super::live::lanes(&ctx, s, &v, silent, 0.0, pic.width(), cx.compact);
        show.lane = lane;
        // the microphone was chosen a step ago: here its name is a name
        show.mic.select = None;
        self.spectro.paint(&ctx, &p, pic, &show);
        let mut y = fig.bottom() + 20.0;

        // the words under it, and the one control that changes the state
        let (title, sub, control) = hear_copy(s);
        let control_w = control.as_ref().map_or(0.0, |c| c.width(&ctx));
        let side = control.is_some() && w - control_w - 24.0 >= 280.0;
        let tw = if side { w - control_w - 24.0 } else { w };
        let top = y;
        y = text_as(
            ui,
            text::layout(&ctx, &title, Type::H, pal.ink, tw),
            pos2(x, y),
            Speak::LiveHeading(1),
        )
        .bottom()
            + 6.0;
        let sb = text::layout(&ctx, &sub, Type::SUB, pal.ink_muted, tw);
        let sh = sb.size().y.max(40.0);
        text_at(ui, sb, pos2(x, y));
        y += sh;
        if let Some(c) = control {
            let (cxp, cyp) = if side {
                (x + w - control_w, top)
            } else {
                (x, y + 12.0)
            };
            let (used, r) = c.show(ui, rect(cxp, cyp, control_w, 40.0), s);
            self.control_drawn = true;
            // another control replaced the one used (Unmute, then the
            // switch while bypassed): the focus moves with it
            if let Some(old) = self.handover.filter(|old| *old != r.id) {
                util::take_focus(&r, old);
                self.handover = None;
            }
            if used {
                if r.has_focus() {
                    self.handover = Some(r.id);
                }
                cx.send(c.action(s));
            }
            if !side {
                y = cyp + 40.0;
            }
        }
        if sounding {
            y = self.play_button(ui, cx, x, y + 20.0);
        }
        y += 20.0;
        text_at(
            ui,
            text::layout(
                &ctx,
                &tr!("fr-hear-later-live"),
                Type::CAP,
                pal.ink_subtle,
                w,
            ),
            pos2(x, y),
        )
        .bottom()
    }

    /// "Play the last 10 seconds" (UX panel A5): the take, cleaned,
    /// through the speakers, until Stop, as Live's Replay. Disabled until
    /// there is a take. Returns its bottom.
    fn play_button(&mut self, ui: &mut Ui, cx: &mut Cx<'_>, x: f32, y: f32) -> f32 {
        let ctx = ui.ctx().clone();
        let s = cx.state;
        // a scene shows the end state: the take is there
        let take = if self.scene { SCENE_TAKE } else { 0 };
        let v = listen(
            &mut self.check,
            &mut self.check_info,
            self.feed,
            take,
            &ctx,
            s,
        )
        .view(cx.now);
        let playing = v.phase == CheckPhase::Replaying;
        let label = if playing {
            tr!("mic-stop")
        } else {
            tr!("fr-hear-play")
        };
        let b = Button::secondary(&label)
            .icon(if playing { Icon::Stop } else { Icon::Replay })
            .enabled(playing || v.can_replay);
        let bw = b.width(&ctx);
        let mut r = util::put(ui, rect(x, y, bw, 40.0), "fr-hear-play", b);
        if !playing && !v.can_replay {
            r = r.on_hover_text(tr!("mic-st-collecting"));
        }
        if r.clicked() {
            if let Some(c) = self.check.as_mut() {
                c.send(if playing {
                    Command::Stop
                } else {
                    Command::Replay
                });
            }
            ctx.request_repaint();
        }
        if playing {
            crate::ui::motion::live_repaint(&ctx);
        }
        y + 40.0
    }

    // ---------------------------------------------------------------- 3 · use it in your apps

    fn step_apps(&mut self, ui: &mut Ui, cx: &mut Cx<'_>, x: f32, y: f32, w: f32) -> f32 {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let s = cx.state;
        let mut y = text_as(
            ui,
            text::layout(&ctx, &tr!("fr-apps-title"), Type::H, pal.ink, w),
            pos2(x, y),
            Speak::LiveHeading(1),
        )
        .bottom()
            + 6.0;
        y = text_at(
            ui,
            text::layout(&ctx, &tr!("fr-apps-sub"), Type::SUB, pal.ink_muted, w),
            pos2(x, y),
        )
        .bottom()
            + 20.0;
        y = example(ui, x, y, &pal) + 20.0;
        let mut c = child(ui, rect(x, y, w, 400.0), "fr-switches");
        widgets::group(&mut c, |g| {
            if s.apps.can_set_default {
                switch_row(
                    g,
                    &tr!("set-default-mic"),
                    &tr!("fr-apps-default-cap"),
                    s.apps.set_default,
                    |v| cx.send(Action::SetDefaultMic(v)),
                );
            }
            switch_row(g, &tr!("set-autostart"), "", s.prefs.autostart, |v| {
                cx.send(Action::SetAutostart(v))
            });
        });
        y = c.min_rect().bottom() + 20.0;
        // the status line; while waiting, the daemon looks again each second
        let (line, dot, color) = match s.apps.using.first() {
            Some(app) => (
                match &app.name {
                    Some(n) => tr!("fr-apps-using", app = n.clone()),
                    None => tr!("fr-apps-using-anon"),
                },
                pal.accent,
                pal.ink,
            ),
            None => {
                if cx.now - self.last_refresh >= REFRESH_SECS {
                    self.last_refresh = cx.now;
                    cx.send(Action::Refresh);
                }
                crate::ui::motion::repaint_in(
                    &ctx,
                    std::time::Duration::from_secs_f64(REFRESH_SECS),
                );
                (tr!("fr-apps-waiting"), pal.line_strong, pal.ink_muted)
            }
        };
        ui.painter()
            .circle_filled(pos2(x + 4.0, y + 10.0), 4.0, dot);
        // read out when an app starts using HushMic (UX panel B16)
        text_as(
            ui,
            text::layout(&ctx, &line, Type::SUB, color, w - 16.0),
            pos2(x + 16.0, y),
            Speak::Live,
        )
        .bottom()
    }
}

/// Back and Continue (or Done) carry the step ("Step 2 of 3: Hear it
/// work") as their description: they keep the focus across a step change,
/// and a screen reader's word on the focused button cut off the step
/// title's own announcement (UX panel round 2, A34: step 2 to 3 said only
/// "Done").
fn tell_step(ctx: &egui::Context, r: &egui::Response, steps_name: &str) {
    let d = steps_name.to_owned();
    crate::ui::widgets::a11y(ctx, r.id, |n| n.set_description(d));
}

/// A scene's demo take: columns enough for its replay to be ready.
const SCENE_TAKE: usize = 180;

/// The backend that keeps the chain listening on first-run step 2 and
/// holds the last 10 seconds (the demo take on boards, `take` columns in).
fn listen<'a>(
    check: &'a mut Option<Box<dyn super::replay::Source>>,
    check_info: &mut Option<EngineInfo>,
    feed: Feed,
    take: usize,
    ctx: &egui::Context,
    s: &AppState,
) -> &'a mut Box<dyn super::replay::Source> {
    let info = super::replay::engine_info(s);
    let check = check.get_or_insert_with(|| match feed {
        Feed::Demo => Box::new(super::replay::DemoCheck::new(info.clone(), take, None).once()),
        Feed::Meter => Box::new(MicCheck::start(
            Setup {
                info: info.clone(),
                columns: true,
                // "Play the last 10 seconds" plays them once (UX panel
                // round 2, A3)
                once: true,
            },
            util::wake(ctx),
        )),
    });
    if check_info.as_ref() != Some(&info) {
        if check_info.is_some() {
            check.send(Command::Engine(info.clone()));
        }
        *check_info = Some(info);
    }
    check
}

/// Done, or Skip on steps 1 and 2: first run is over, Live opens.
fn finish(cx: &mut Cx<'_>) {
    cx.send(Action::UiFirstRun(None));
    cx.goto = Some(Page::Live);
}

/// The microphone the arrows reached, once they rest: chosen if it is
/// still there (it may have been unplugged in the meantime), else the
/// highlight goes back to the chosen one.
fn choose_arrowed(ctx: &egui::Context, cx: &mut Cx<'_>, name: Option<String>) {
    let s = cx.state;
    if name == s.mic.selected {
        return;
    }
    let present = name
        .as_deref()
        .is_none_or(|n| s.mic.devices.iter().any(|d| d.name == n && d.present));
    if present {
        cx.send(Action::SelectMic(name));
    } else {
        crate::ui::widgets::PickGroup::snap(ctx, egui::Id::new(MIC_PICKS));
    }
}

/// What step 2's picture shows.
fn hear_state(s: &AppState) -> LiveState {
    if s.mode == Mode::Off {
        return LiveState::Off;
    }
    match s.engine.phase {
        Phase::Ready => {}
        _ => return LiveState::Starting,
    }
    match s.mode {
        Mode::Bypass => LiveState::Bypass,
        Mode::Mute => LiveState::Muted,
        // the adaptive engine stepped down to passthrough: nothing filters
        // (as Live; Light for now still filters)
        _ if s.engine.tier == Some(EngineTier::Passthrough) => LiveState::Unfiltered,
        _ => LiveState::On,
    }
}

/// The one control step 2 offers outside filtering.
enum Control {
    Switch(String),
    Unmute(String),
    TurnOn(String),
}

impl Control {
    fn width(&self, ctx: &egui::Context) -> f32 {
        match self {
            Control::Switch(l) => {
                crate::ui::widgets::switch::TRACK.x + 12.0 + text::width(ctx, l, Type::SWITCH)
            }
            Control::Unmute(l) => Button::secondary(l).width(ctx),
            Control::TurnOn(l) => Button::primary(l).width(ctx),
        }
    }

    /// Draw; true when used.
    fn show(&self, ui: &mut Ui, r: Rect, _s: &AppState) -> (bool, egui::Response) {
        match self {
            Control::Switch(l) => {
                let mut on = false;
                let r = util::put(
                    ui,
                    // top-aligned with the title, as the board
                    Rect::from_min_size(r.min, vec2(r.width(), 24.0)),
                    "fr-switch",
                    Switch::new(&mut on).label(l),
                );
                (r.changed(), r)
            }
            Control::Unmute(l) => {
                let r = util::put(ui, r, "fr-unmute", Button::secondary(l));
                (r.clicked(), r)
            }
            Control::TurnOn(l) => {
                let r = util::put(ui, r, "fr-turn-on", Button::primary(l));
                (r.clicked(), r)
            }
        }
    }

    fn action(&self, s: &AppState) -> Action {
        match self {
            Control::Switch(_) => Action::SetMode(Mode::Suppress),
            Control::Unmute(_) => Action::SetMode(s.unmute_to),
            Control::TurnOn(_) => Action::SetMode(Mode::Suppress),
        }
    }
}

fn hear_copy(s: &AppState) -> (String, String, Option<Control>) {
    if s.mode == Mode::Off {
        return (
            tr!("live-title-off"),
            tr!("fr-hear-off-sub"),
            Some(Control::TurnOn(tr!("live-turn-on"))),
        );
    }
    if s.engine.phase != Phase::Ready {
        return (tr!("live-title-starting"), tr!("live-sub-starting"), None);
    }
    match s.mode {
        Mode::Bypass => (
            tr!("live-title-bypass"),
            tr!("fr-hear-bypass-sub"),
            Some(Control::Switch(tr!("live-switch"))),
        ),
        Mode::Mute => (
            tr!("fr-hear-muted-title"),
            tr!("fr-hear-muted-sub"),
            Some(Control::Unmute(tr!("live-unmute"))),
        ),
        _ if s.engine.tier == Some(EngineTier::Passthrough) => (
            tr!("live-title-unfiltered"),
            tr!("fr-hear-unfiltered-sub"),
            None,
        ),
        _ => (tr!("fr-hear-title"), tr!("fr-hear-sub"), None),
    }
}

/// What kind of microphone a device is, from its PipeWire name.
pub fn device_note(node: &str, description: &str) -> String {
    let n = node.to_lowercase();
    let d = description.to_lowercase();
    if n.starts_with("bluez") {
        tr!("fr-mic-bluetooth")
    } else if d.contains("headset") || n.contains("headset") {
        tr!("fr-mic-headset")
    } else if n.contains(".usb-") || n.contains("usb") {
        tr!("fr-mic-usb")
    } else if n.contains(".pci-") || n.contains("platform-") || d.contains("built-in") {
        tr!("fr-mic-builtin")
    } else {
        tr!("fr-mic-other")
    }
}

/// Step 3's picture of a call app's microphone setting (UX panel B4): a
/// flat, small illustration with its caption, "In Zoom it looks like
/// this", drawn so that nothing in it looks like a control (no field
/// border, a quiet chevron), and read as one picture.
fn example(ui: &mut Ui, x: f32, y: f32, pal: &Palette) -> f32 {
    let ctx = ui.ctx().clone();
    let p = ui.painter().clone();
    let inner = 220.0;
    let w = inner + 2.0 * 14.0;
    let e = text::layout(
        &ctx,
        &tr!("fr-apps-example"),
        Type::EYEBROW,
        pal.ink_subtle,
        inner,
    );
    let m = text::layout(
        &ctx,
        &tr!("live-microphone"),
        Type::CAP,
        pal.ink_muted,
        inner,
    );
    let v = text::layout(&ctx, "HushMic", Type::CAP.medium(), pal.ink, inner);
    let h = 12.0 + e.size().y + 6.0 + m.size().y + 6.0 + 28.0 + 12.0;
    let r = rect(x, y, w, h);
    p.rect_filled(r, radius::MD, pal.surface_raised);
    let mut yy = y + 12.0;
    e.paint(&p, pos2(x + 14.0, yy));
    yy += e.size().y + 6.0;
    m.paint(&p, pos2(x + 14.0, yy));
    yy += m.size().y + 6.0;
    let field = rect(x + 14.0, yy, inner, 28.0);
    p.rect_filled(field, radius::SM, pal.surface_hover);
    v.paint_centered_y(&p, field.left() + 10.0, field.center().y);
    icons::paint(
        &p,
        Icon::Chevron,
        pos2(field.right() - 14.0, field.center().y),
        14.0,
        pal.ink_subtle,
    );
    let cap = text::layout(
        &ctx,
        &tr!("fr-apps-example-cap"),
        Type::CAP,
        pal.ink_subtle,
        2.0 * w,
    );
    let cap_y = r.bottom() + 6.0;
    let cap_bottom = cap_y + cap.size().y;
    cap.paint(&p, pos2(x, cap_y));
    // one picture for screen readers: its caption and what it shows
    let all = Rect::from_min_max(r.min, pos2(r.right().max(x + cap.size().x), cap_bottom));
    let name = tr!("fr-apps-example-name");
    let resp = ui.interact(all, ui.id().with("fr-apps-example"), egui::Sense::hover());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, &name));
    cap_bottom
}

fn switch_row(
    g: &mut widgets::GroupUi<'_>,
    label: &str,
    caption: &str,
    on: bool,
    send: impl FnOnce(bool),
) {
    let mut v = on;
    let mut changed = false;
    g.row(|ui| {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let w = ui.available_width();
        let tw = w - 44.0 - 12.0;
        let lb = text::layout(&ctx, label, Type::LABEL, pal.ink, tw);
        let cb = (!caption.is_empty())
            .then(|| text::layout(&ctx, caption, Type::CAP, pal.ink_subtle, tw));
        let th = lb.size().y + cb.as_ref().map_or(0.0, |c| 2.0 + c.size().y);
        let o = ui.cursor().min;
        // the row keeps the switch's 29 px box; the track is centred
        let rh = th.max(29.0);
        let r = util::paint_at(ui, lb, pos2(o.x, o.y + (rh - th) / 2.0));
        // the switch (named by the label) before the caption that explains
        // it, in the reading order
        changed = util::put(
            ui,
            rect(o.x + w - 44.0, o.y + (rh - 24.0) / 2.0, 44.0, 24.0),
            ("fr-sw", label),
            Switch::new(&mut v).name(label),
        )
        .changed();
        if let Some(cb) = cb {
            text_at(ui, cb, pos2(o.x, r.bottom() + 2.0));
        }
        ui.advance_cursor_after_rect(Rect::from_min_size(o, vec2(w, rh)));
    });
    if changed {
        send(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock::{state, Scenario};
    use crate::pages::testing;
    use egui_kittest::kittest::{NodeT, Queryable};

    #[test]
    fn the_steps_move_with_continue_back_skip_and_done() {
        let mut w = testing::scenario(Scenario::FirstRun1, Page::FirstRun);
        assert!(w
            .harness
            .query_by_label("Which microphone should HushMic clean?")
            .is_some());
        w.harness.get_by_label("Continue").click();
        w.harness.run_steps(2);
        // Skip on step 2 ends the setup (UX panel B20), as on step 1
        w.harness.get_by_label("Skip").click();
        w.harness.run_steps(2);
        let steps: Vec<Action> = w
            .link
            .sent()
            .into_iter()
            .filter(|a| matches!(a, Action::UiFirstRun(_)))
            .collect();
        assert_eq!(
            steps,
            vec![Action::UiFirstRun(Some(2)), Action::UiFirstRun(None)]
        );
        let mut w = testing::scenario(Scenario::FirstRun1, Page::FirstRun);
        w.harness.get_by_label("Skip").click();
        w.harness.run_steps(2);
        assert!(w.link.sent().starts_with(&[Action::UiFirstRun(None)]));

        // F14: here only for recordings, step 1 goes straight to Files
        let mut w = testing::scenario(Scenario::FirstRun1, Page::FirstRun);
        assert!(w
            .harness
            .query_by_label("Only here to clean recordings?")
            .is_some());
        w.harness.get_by_label("Go to Files").click();
        w.harness.run_steps(2);
        assert!(w.link.sent().starts_with(&[Action::UiFirstRun(None)]));
        assert_eq!(*w.page.borrow(), Page::Files);

        let mut w = testing::scenario(Scenario::FirstRun3, Page::FirstRun);
        assert!(w.harness.query_by_label("Skip").is_none(), "Done ends it");
        assert!(
            w.harness.query_by_label("Go to Files").is_none(),
            "step 1 only"
        );
        w.harness.get_by_label("Back").click();
        w.harness.run_steps(2);
        w.harness.get_by_label("Done").click();
        w.harness.run_steps(2);
        let sent: Vec<Action> = w
            .link
            .sent()
            .into_iter()
            .filter(|a| *a != Action::Refresh)
            .collect();
        assert_eq!(sent[0], Action::UiFirstRun(Some(2)));
        assert!(sent.contains(&Action::UiFirstRun(None)));
        assert_eq!(*w.page.borrow(), Page::Live);
    }

    #[test]
    fn step_one_picks_the_microphone() {
        let mut w = testing::scenario(Scenario::FirstRun1, Page::FirstRun);
        w.harness.get_by_label("RODE NT-USB").click();
        w.harness.run_steps(2);
        let rode = state(Scenario::FirstRun1)
            .mic
            .devices
            .iter()
            .find(|d| d.description == "RODE NT-USB")
            .unwrap()
            .name
            .clone();
        assert_eq!(w.link.sent(), vec![Action::SelectMic(Some(rode))]);
        // old PipeWire: no level, and says why
        let w = testing::scenario(Scenario::FirstRunOldPipewire, Page::FirstRun);
        assert!(w
            .harness
            .query_by_label("The level shows on newer PipeWire versions. HushMic works either way.")
            .is_some());
    }

    #[test]
    fn step_two_plays_the_last_ten_seconds() {
        crate::i18n::pin_english();
        let mut w = testing::window(
            state(Scenario::FirstRun2),
            Page::FirstRun,
            egui::vec2(960.0, 640.0),
        );
        w.harness.run_steps(2);
        let play = "Play the last 10 seconds";
        assert!(
            w.harness.get_by_label(play).accesskit_node().is_disabled(),
            "nothing to play yet"
        );
        // the take fills (2 s of the demo clip)
        w.harness.run_steps(180);
        assert!(!w.harness.get_by_label(play).accesskit_node().is_disabled());
        w.harness.get_by_label(play).click();
        w.harness.run_steps(3);
        assert!(w.harness.query_by_label(play).is_none());
        w.harness.get_by_label("Stop").click();
        w.harness.run_steps(3);
        assert!(w.harness.query_by_label(play).is_some(), "stopped");
        // it plays the 10 seconds once, with no loop (UX panel round 2,
        // A3)
        w.harness.get_by_label(play).click();
        w.harness.run_steps(3);
        assert!(w.harness.query_by_label(play).is_none());
        // 11 s at the harness's 250 ms frames
        w.harness.run_steps(44);
        assert!(w.harness.query_by_label(play).is_some(), "played once");
        // outside filtering there is no button: step 2 offers the control
        // that changes the state
        let mut s = state(Scenario::FirstRun2);
        s.mode = Mode::Off;
        let w = testing::window(s, Page::FirstRun, egui::vec2(960.0, 640.0));
        assert!(w.harness.query_by_label(play).is_none());
    }

    #[test]
    fn a_step_two_scene_shows_play_ready() {
        // the screenshots open on step 2's end state: the take is there
        // (UX panel A5)
        crate::i18n::pin_english();
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let mut w = testing::with_setup(
                crate::window::Link::recording(state(Scenario::FirstRun2)),
                Page::FirstRun,
                size,
                |p| p.prefill(180),
            );
            w.harness.run_steps(2);
            let play = w.harness.get_by_label("Play the last 10 seconds");
            assert!(!play.accesskit_node().is_disabled(), "{size:?}");
        }
    }

    /// Step 2 shows Live's picture, the two lanes of what the backend
    /// hears, and lets go of it with the step.
    #[test]
    fn step_two_holds_lives_picture_and_lets_go_of_it() {
        crate::i18n::pin_english();
        let ctx = egui::Context::default();
        let mut pages = crate::pages::Pages::new(&ctx, crate::pages::Feed::Demo);
        pages.no_launch();
        pages.prefill(180);
        let link = crate::window::Link::recording(state(Scenario::FirstRun2));
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
        assert!(!pages.first_run.has_picture());
        frame(&mut pages, Page::FirstRun, 1.0);
        assert!(pages.first_run.has_picture());
        frame(&mut pages, Page::Settings, 1.1);
        assert!(!pages.first_run.has_picture());
        assert!(pages.first_run.check.is_none(), "the microphone is let go");
    }

    #[test]
    fn step_two_never_forces_the_state() {
        crate::i18n::pin_english();
        let mut s = state(Scenario::FirstRun2);
        s.mode = Mode::Bypass;
        let mut w = testing::window(s, Page::FirstRun, egui::vec2(960.0, 640.0));
        w.harness.run_steps(10);
        // nothing was sent by itself
        assert!(!w
            .link
            .sent()
            .iter()
            .any(|a| matches!(a, Action::SetMode(_))));
        w.harness.get_by_label("Noise suppression").click();
        w.harness.run_steps(2);
        assert!(w.link.sent().contains(&Action::SetMode(Mode::Suppress)));

        let mut s = state(Scenario::FirstRun2);
        s.mode = Mode::Mute;
        let back = s.unmute_to;
        let mut w = testing::window(s, Page::FirstRun, egui::vec2(960.0, 640.0));
        w.harness.get_by_label("Unmute").click();
        w.harness.run_steps(2);
        assert!(w.link.sent().contains(&Action::SetMode(back)));
    }

    #[test]
    fn step_two_tells_screen_readers_the_mic_its_picture_names() {
        let w = testing::scenario(Scenario::FirstRun2, Page::FirstRun);
        let name = tr!("fr-hear-pic");
        let pic = testing::by_role(&w.harness, &name, testing::Role::Image);
        let mic = super::super::live::picture_mic(&state(Scenario::FirstRun2));
        assert_eq!(
            pic.accesskit_node().description().as_deref(),
            Some(tr!("live-pic-mic", mic = mic).as_str())
        );
    }

    #[test]
    fn the_arrows_choose_a_microphone_once_they_rest() {
        let st = state(Scenario::FirstRun1);
        let present: Vec<String> = st
            .mic
            .devices
            .iter()
            .filter(|d| d.present)
            .map(|d| d.name.clone())
            .collect();
        assert!(st.mic.selected.is_none() && present.len() >= 3);
        let mut w = testing::at_60hz(st, Page::FirstRun, egui::vec2(960.0, 900.0));
        w.harness.get_by_label("System default").focus();
        w.harness.run_steps(2);
        // three quick Downs: the highlight runs ahead, nothing is sent yet
        for _ in 0..3 {
            w.harness.key_press(egui::Key::ArrowDown);
            w.harness.run_steps(2);
        }
        assert!(w.link.sent().is_empty(), "{:?}", w.link.sent());
        // once they rest (0.4 s): one change, to the third microphone
        w.harness.run_steps(40);
        assert_eq!(
            w.link.sent(),
            vec![Action::SelectMic(Some(present[2].clone()))]
        );
    }

    #[test]
    fn the_keyboard_follows_step_two_from_unmute_to_continue() {
        let mut s = state(Scenario::FirstRun2);
        s.mode = Mode::Mute;
        s.unmute_to = Mode::Bypass;
        let mut w = testing::mock_window(s, Page::FirstRun, egui::vec2(960.0, 900.0));
        w.harness.get_by_label("Unmute").focus();
        w.harness.run_steps(2);
        // Unmute returns to bypass: the switch replaces it and takes the keyboard
        w.harness.key_press(egui::Key::Enter);
        w.harness.run_steps(3);
        assert_eq!(w.link.state().unwrap().mode, Mode::Bypass);
        let switch = testing::by_role(&w.harness, "Noise suppression", testing::Role::CheckBox);
        assert!(switch.is_focused());
        // the switch turns filtering on and goes: Continue is next
        w.harness.key_press(egui::Key::Space);
        w.harness.run_steps(3);
        assert_eq!(w.link.state().unwrap().mode, Mode::Suppress);
        assert!(w.harness.get_by_label("Continue").is_focused());
    }

    #[test]
    fn the_highlight_stays_on_its_microphone_when_one_above_it_goes() {
        let st = state(Scenario::FirstRun1);
        let present: Vec<String> = st
            .mic
            .devices
            .iter()
            .filter(|d| d.present)
            .map(|d| d.name.clone())
            .collect();
        assert!(st.mic.selected.is_none() && present.len() >= 3);
        // the first microphone leaves: every one below it moves up a place
        let mut gone = st.clone();
        gone.mic.devices.retain(|d| d.name != present[0]);
        let mut w = testing::switched(vec![st, gone], Page::FirstRun, egui::vec2(960.0, 900.0));
        w.harness.get_by_label("System default").focus();
        w.harness.run_steps(2);
        for _ in 0..2 {
            w.harness.key_press(egui::Key::ArrowDown);
            w.harness.run_steps(2);
        }
        w.at.set(1);
        w.harness.run_steps(2);
        // Space before the arrows rest: the highlighted pick is still the
        // second microphone, not the one now in its place
        w.harness.key_press(egui::Key::Space);
        w.harness.run_steps(2);
        assert!(w.links[0].sent().is_empty(), "{:?}", w.links[0].sent());
        assert_eq!(
            w.links[1].sent(),
            vec![Action::SelectMic(Some(present[1].clone()))]
        );
    }

    #[test]
    fn an_arrowed_microphone_unplugged_before_it_settles_is_not_chosen() {
        let st = state(Scenario::FirstRun1);
        let present = st
            .mic
            .devices
            .iter()
            .find(|d| d.present)
            .unwrap()
            .name
            .clone();
        for (name, sent) in [
            ("alsa_input.unplugged".to_string(), false),
            (present.clone(), true),
        ] {
            let n = name.clone();
            let mut w = testing::with_setup(
                crate::window::Link::recording(st.clone()),
                Page::FirstRun,
                egui::vec2(960.0, 900.0),
                move |ps| ps.first_run.arrowed = Some((Some(n.clone()), 0.0)),
            );
            w.harness.run_steps(6);
            let want = if sent {
                vec![Action::SelectMic(Some(name))]
            } else {
                vec![]
            };
            assert_eq!(w.link.sent(), want);
        }
    }

    #[test]
    fn a_click_chooses_a_microphone_at_once() {
        let st = state(Scenario::FirstRun1);
        let first = st.mic.devices.iter().find(|d| d.present).unwrap().clone();
        let mut w = testing::window(st, Page::FirstRun, egui::vec2(960.0, 900.0));
        w.harness.get_by_label(&first.description).click();
        w.harness.run_steps(1);
        assert_eq!(w.link.sent(), vec![Action::SelectMic(Some(first.name))]);
    }

    #[test]
    fn the_microphone_picks_are_one_radio_group() {
        use egui_kittest::kittest::NodeT;
        let w = testing::scenario(Scenario::FirstRun1, Page::FirstRun);
        let group = testing::by_role(&w.harness, "Microphone", testing::Role::RadioGroup);
        let radios = group
            .accesskit_node()
            .children()
            .flat_map(|c| std::iter::once(c).chain(c.children()))
            .filter(|c| c.role() == testing::Role::RadioButton)
            .count();
        let present = state(Scenario::FirstRun1)
            .mic
            .devices
            .iter()
            .filter(|d| d.present)
            .count();
        assert_eq!(radios, present + 1, "System default and each microphone");
        // each card's second line is its description (UX panel round 2,
        // A34)
        let default = testing::by_role(&w.harness, "System default", testing::Role::RadioButton);
        assert!(
            default
                .accesskit_node()
                .description()
                .is_some_and(|d| d.contains("Now: ")),
            "{:?}",
            default.accesskit_node().description()
        );
    }

    #[test]
    fn the_focused_footer_button_carries_the_step() {
        // UX panel round 2, A34: Continue becomes Done in the same frame,
        // and the screen reader's word on the focus cut the step's
        // announcement off; the button that keeps the focus says the step
        let mut w = testing::mock_window(
            state(Scenario::FirstRun2),
            Page::FirstRun,
            egui::vec2(960.0, 900.0),
        );
        w.harness.run_steps(4);
        let described = |w: &testing::Window, label: &str| {
            w.harness.get_by_label(label).accesskit_node().description()
        };
        assert_eq!(
            described(&w, "Continue").as_deref(),
            Some("Step 2 of 3: Hear it work")
        );
        assert_eq!(
            described(&w, "Back").as_deref(),
            Some("Step 2 of 3: Hear it work")
        );
        w.harness.get_by_label("Continue").focus();
        w.harness.run_steps(1);
        w.harness.key_press(egui::Key::Enter);
        w.harness.run_steps(2);
        assert_eq!(w.link.state().unwrap().ui.first_run_step, Some(3));
        assert!(w.harness.get_by_label("Done").is_focused());
        assert_eq!(
            described(&w, "Done").as_deref(),
            Some("Step 3 of 3: Use it in your apps")
        );
    }

    #[test]
    fn step_two_over_a_passthrough_engine_says_unfiltered() {
        let mut s = state(Scenario::FirstRun2);
        s.engine.tier = Some(EngineTier::Passthrough);
        assert_eq!(hear_state(&s), LiveState::Unfiltered);
        let mut w = testing::window(s.clone(), Page::FirstRun, egui::vec2(960.0, 900.0));
        for _ in 0..10 {
            w.harness.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(w
            .harness
            .query_by_label("Noise suppression paused for now.")
            .is_some());
        assert!(w
            .harness
            .query_by_label("Talk, then type or tap the desk.")
            .is_none());
        let name = super::super::live::state_name(&s);
        testing::by_role(&w.harness, &name, testing::Role::Image);
        // Light for now still filters
        s.engine.tier = Some(EngineTier::Light);
        assert_eq!(hear_state(&s), LiveState::On);
    }

    #[test]
    fn stopped_says_what_is_wrong_in_the_steps() {
        for sc in [Scenario::NeedsAttention, Scenario::PipewireUnreachable] {
            for step in [1, 2] {
                let mut s = state(sc);
                s.ui.first_run_step = Some(step);
                let mut w = testing::window(s, Page::FirstRun, egui::vec2(960.0, 900.0));
                assert!(
                    w.harness.query_by_label("Try again").is_some(),
                    "{sc:?} step {step}"
                );
                w.harness.get_by_label("Try again").click();
                w.harness.run_steps(2);
                assert!(w.link.sent().contains(&Action::Retry), "{sc:?} step {step}");
            }
        }
    }

    #[test]
    fn picking_a_microphone_never_moves_step_one() {
        crate::i18n::pin_english();
        let rect_of = |w: &testing::Window, label: &str| w.harness.get_by_label(label).rect();
        let mut w = testing::scenario(Scenario::FirstRun1, Page::FirstRun);
        w.harness.run_steps(2);
        let level = rect_of(&w, "Level");
        let picks: Vec<String> =
            testing::by_role(&w.harness, "Microphone", testing::Role::RadioGroup)
                .accesskit_node()
                .children()
                .flat_map(|c| std::iter::once(c).chain(c.children()))
                .filter(|c| c.role() == testing::Role::RadioButton)
                .filter_map(|c| c.label())
                .collect();
        assert!(picks.len() >= 2, "{picks:?}");
        let last = picks.last().unwrap().clone();
        let at = rect_of(&w, &last);
        w.harness.get_by_label(&last).click();
        w.harness.run_steps(3);
        assert_eq!(rect_of(&w, "Level"), level, "the level stays in its slot");
        assert_eq!(rect_of(&w, &last), at, "the list stays put");
    }

    #[test]
    fn step_three_shows_zoom_as_a_picture_and_names_the_call_apps() {
        crate::i18n::pin_english();
        let w = testing::scenario(Scenario::FirstRun3, Page::FirstRun);
        let pic = w
            .harness
            .get_by_label("In Zoom it looks like this: Microphone, HushMic.");
        assert_eq!(pic.accesskit_node().role(), egui::accesskit::Role::Image);
        assert!(w
            .harness
            .query_by_label_contains("Zoom, Google Meet, Teams")
            .is_some());
        assert!(w
            .harness
            .query_by_label_contains("Turn this on if you’re not sure. Zoom, Meet and Teams")
            .is_some());
        // nothing in the picture is a control
        assert!(w.harness.query_by_label("HushMic").is_none());
    }

    #[test]
    fn step_three_switches_and_waits_for_an_app() {
        let mut w = testing::scenario(Scenario::FirstRun3, Page::FirstRun);
        // read out when it changes (UX panel B16), the step's title too
        let waiting = w.harness.get_by_label("Waiting for an app to use HushMic…");
        assert_eq!(
            waiting.accesskit_node().live(),
            egui::accesskit::Live::Polite
        );
        assert_eq!(
            w.harness
                .get_by_role(egui::accesskit::Role::Heading)
                .accesskit_node()
                .live(),
            egui::accesskit::Live::Polite
        );
        assert!(w.link.sent().contains(&Action::Refresh));
        w.harness
            .get_by_label("Use HushMic as the default microphone")
            .click();
        w.harness.run_steps(2);
        w.harness
            .get_by_label("Start HushMic when you log in")
            .click();
        w.harness.run_steps(2);
        let sent = w.link.sent();
        assert!(sent.contains(&Action::SetDefaultMic(true)));
        assert!(sent.contains(&Action::SetAutostart(true)));
        let w = testing::scenario(Scenario::FirstRun3Connected, Page::FirstRun);
        assert!(w
            .harness
            .query_by_label("Firefox is using HushMic.")
            .is_some());
    }

    #[test]
    fn device_kinds_read_from_the_node_name() {
        crate::i18n::pin_english();
        assert_eq!(
            device_note(
                "alsa_input.usb-RODE_Microphones_RODE_NT-USB-00.analog-stereo",
                "RODE NT-USB"
            ),
            "USB microphone"
        );
        assert_eq!(
            device_note(
                "alsa_input.pci-0000_00_1f.3.analog-stereo",
                "Built-in Audio"
            ),
            "Your computer’s microphone"
        );
        assert_eq!(
            device_note(
                "alsa_input.usb-Generic_USB_Headset-00.mono-fallback",
                "USB Headset"
            ),
            "Headset microphone"
        );
        assert_eq!(
            device_note("bluez_input.AA_BB", "WH-1000XM4"),
            "Bluetooth microphone"
        );
    }
}
