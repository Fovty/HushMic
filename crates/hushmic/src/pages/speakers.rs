//! Speakers: the speakers cleaner (HushMic Speakers, an output whose sound
//! reaches a real output with the noise removed). Built from Live's rules,
//! without a picture: the header (the state, and the switch), then one
//! card: a row with what it does, how to use it and "Hold to hear the
//! original", over three cards: Output (Play to), Cleaning (the strength)
//! and Status (who plays here, or the one problem that matters).
//!
//! The page never decides: it draws the snapshot's `speakers` block and
//! sends what the user asked for. The hold is momentary: the page sends it
//! on while held and off at the release, and off when it leaves, is
//! minimised or the cleaner stops (the daemon also ends it when the
//! window goes away).

use eframe::egui::{self, pos2, vec2, Rect, Ui};

use super::hold::{Hold, Kind};
use super::live::{
    card_head, frame_light, problem_block, BAR_IN, CARD_GAP, CARD_PAD, LINK_GAP, PAD, PAD_COMPACT,
    SWAP_SETTLE, SWITCH_HALF,
};
use super::util::{self, child, rect};
use super::Cx;
use crate::appstate::{Action, AppState, Mode, Phase, SpeakersState, Strength};
use crate::diagnostics::EngineTier;
use crate::tr;
use crate::ui::fonts::Type;
use crate::ui::text::{self, TextBlock};
use crate::ui::tokens::Palette;
use crate::ui::widgets::{self, Button, SelectItem, Switch};
use crate::window::Link;

/// The top row's padding and its hold button's height (Live's replay row).
const ROW_PAD_TOP: f32 = 12.0;
const ROW_PAD_X: f32 = 16.0;
const ROW_PAD_BOTTOM: f32 = 12.0;
const ROW_H: f32 = 40.0;
/// With less than this for the words beside it, the hold button goes
/// under them.
const ROW_TEXT_MIN: f32 = 240.0;
/// While held or latched the page renews the hold this often (seconds);
/// the daemon ends a hold not renewed for about 2.5 s, so a window that
/// died cannot leave the original playing.
const HOLD_RENEW: f64 = 1.0;
/// The Status card makes room for a problem of this many lines and two
/// links, so no problem moves the card's bottom.
const STATUS_ROWS: usize = 4;

#[derive(Default)]
pub struct SpeakersPage {
    hold: Hold,
    /// What the page last asked the daemon for the hold.
    sent: bool,
    /// When the page last sent `speakers hold on` (the hold is a lease the
    /// page renews every [`HOLD_RENEW`] while it holds).
    sent_at: f64,
    /// The daemon has said `held` since the page last sent on: its `held`
    /// going false then means the daemon ended the hold.
    confirmed: bool,
    select_id: Option<egui::Id>,
    /// The header showed Turn on (HushMic off) last frame.
    header_off: Option<bool>,
    swapped_at: Option<f64>,
    focus_switch: Option<egui::Id>,
}

/// How a problem reads: the colour of its 2 px bar (Live's tones).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tone {
    Error,
    Warn,
    Info,
}

/// A problem's way out, a link under its words.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fix {
    Retry,
    Diagnostics,
    ChooseOutput,
}

#[derive(Clone, Debug, PartialEq)]
struct Problem {
    tone: Tone,
    lead: String,
    body: String,
    fixes: Vec<Fix>,
    /// More lines of the same kind, each its own lead and body (one per
    /// app that left HushMic Speakers).
    more: Vec<(String, String)>,
}

impl Problem {
    /// Its words laid out `w` wide: each line's lead in medium ink, the
    /// body muted, the lines one under another.
    fn block(&self, ctx: &egui::Context, w: f32, pal: &Palette) -> TextBlock {
        if self.more.is_empty() {
            return problem_block(ctx, &self.lead, &self.body, w, pal);
        }
        let mut r = text::Rich::new();
        let lines =
            std::iter::once((&self.lead, &self.body)).chain(self.more.iter().map(|(l, b)| (l, b)));
        for (i, (lead, body)) in lines.enumerate() {
            if i > 0 {
                r = r.span("\n", Type::CAP, pal.ink_muted);
            }
            r = r
                .span(lead, Type::CAP.medium(), pal.ink)
                .span(" ", Type::CAP, pal.ink_muted)
                .span(body, Type::CAP, pal.ink_muted);
        }
        r.layout(ctx, w)
    }

    /// Its words in one string (its name for screen readers).
    fn words(&self) -> String {
        std::iter::once((&self.lead, &self.body))
            .chain(self.more.iter().map(|(l, b)| (l, b)))
            .map(|(l, b)| {
                [l.as_str(), b.as_str()]
                    .iter()
                    .filter(|t| !t.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// "Hold to hear the original" in this window: not at all, held down
/// (a press or page Space), or latched by a tap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    No,
    Pressed,
    Latched,
}

/// The cleaner runs and HushMic with it: what plays here is cleaned (or
/// held to the original).
pub fn runs(s: &AppState) -> bool {
    s.pipewire.reachable
        && s.mode != Mode::Off
        && s.speakers.enabled
        && s.speakers.phase == Phase::Ready
}

/// The output's display name; None = System default.
fn output_name(sp: &SpeakersState, name: Option<&str>) -> Option<String> {
    name.map(|n| sp.description_of(n).to_owned())
}

/// Where the sound plays while the chosen output is unplugged.
fn standin(sp: &SpeakersState) -> String {
    sp.system_default
        .as_deref()
        .or(sp.playing_on.as_deref())
        .map(|d| sp.description_of(d).to_owned())
        .unwrap_or_else(|| tr!("live-system-default"))
}

/// The header (Live's rule): the state alone. HushMic's own state first
/// (PipeWire gone, off), then the switch, the chain, the hold, then what
/// the cleaner does.
pub fn title(s: &AppState) -> String {
    title_in(s, Held::No, None)
}

/// [`title`] that falls back to the plain words when the output's name
/// would not fit `room` on the title's line.
fn title_in(s: &AppState, held: Held, fit: Option<(&egui::Context, f32)>) -> String {
    let sp = &s.speakers;
    if !s.pipewire.reachable {
        return tr!("speakers-title-stopped");
    }
    if s.mode == Mode::Off {
        return tr!("live-title-off");
    }
    if !sp.enabled {
        return tr!("speakers-title-off");
    }
    match sp.phase {
        Phase::Off | Phase::Starting => return tr!("live-title-starting"),
        Phase::Reconnecting => return tr!("live-title-reconnecting"),
        Phase::Down => return tr!("speakers-title-stopped"),
        Phase::Ready => {}
    }
    // the original plays, here or as the daemon says: said plainly, so
    // nobody leaves the cleaning off without noticing
    if sp.held || held != Held::No {
        return tr!("speakers-title-holding");
    }
    if sp.output_absent() {
        let output = output_name(sp, sp.output.as_deref()).unwrap_or_default();
        let t = tr!("speakers-title-unplugged", output = output);
        return match fit {
            Some((ctx, room)) if text::width(ctx, &t, Type::H) > room => {
                tr!("speakers-title-unplugged-plain")
            }
            _ => t,
        };
    }
    if sp.tier == Some(EngineTier::Passthrough) {
        return tr!("speakers-title-paused");
    }
    if sp.playing.is_empty() {
        tr!("speakers-title-ready")
    } else {
        tr!("speakers-title-on")
    }
}

/// The one problem the Status card shows, the most important first:
/// HushMic can't reach PipeWire, the cleaner can't start, the chosen
/// output is unplugged, the computer is too busy, the light model for now,
/// HushMic Speakers made the system output. None when nothing is wrong (or
/// the cleaner is off: the header says so).
fn problem(s: &AppState, held: Held) -> Option<Problem> {
    let sp = &s.speakers;
    let p = |tone, lead: String, body: String, fixes: &[Fix]| Problem {
        tone,
        lead,
        body,
        fixes: fixes.to_vec(),
        more: Vec::new(),
    };
    if !s.pipewire.reachable {
        return Some(p(
            Tone::Error,
            tr!("live-status-pipewire-lead"),
            tr!("live-status-pipewire-body"),
            &[Fix::Diagnostics],
        ));
    }
    if s.mode == Mode::Off || !sp.enabled {
        return None;
    }
    if sp.phase == Phase::Down {
        return Some(p(
            Tone::Error,
            tr!("speakers-down-lead"),
            tr!("live-status-down-body"),
            &[Fix::Retry, Fix::Diagnostics],
        ));
    }
    if sp.phase != Phase::Ready {
        return None;
    }
    // the original plays: how to get the cleaning back
    if sp.held || held != Held::No {
        let body = if held == Held::Pressed {
            tr!("mic-sub-holding")
        } else {
            tr!("mic-sub-latched")
        };
        return Some(p(Tone::Warn, tr!("speakers-holding-lead"), body, &[]));
    }
    // apps a restart left on another output, uncleaned: each by name, and
    // what to do
    if !sp.left.is_empty() {
        let anon = tr!("live-app-anon");
        let names = util::numbered(
            sp.left
                .iter()
                .map(|a| a.name.as_deref().unwrap_or(anon.as_str())),
        );
        let mut lines = names.into_iter().map(|app| {
            (
                tr!("speakers-left-lead", app = app),
                tr!("speakers-left-body"),
            )
        });
        let (lead, body) = lines.next().unwrap_or_default();
        let mut pr = p(Tone::Warn, lead, body, &[]);
        pr.more = lines.collect();
        return Some(pr);
    }
    if sp.output_absent() {
        let output = output_name(sp, sp.output.as_deref()).unwrap_or_default();
        return Some(p(
            Tone::Warn,
            tr!("speakers-unplugged-lead", output = output),
            tr!("speakers-unplugged-body", device = standin(sp)),
            &[Fix::ChooseOutput],
        ));
    }
    match sp.tier {
        Some(EngineTier::Passthrough) => {
            return Some(p(
                Tone::Warn,
                tr!("speakers-busy-lead"),
                tr!("speakers-busy-body"),
                &[],
            ))
        }
        Some(EngineTier::Light) => {
            return Some(p(
                Tone::Info,
                tr!("live-light-now-title"),
                tr!("live-light-now-body"),
                &[],
            ))
        }
        _ => {}
    }
    if sp.is_default {
        return Some(p(
            Tone::Info,
            tr!("speakers-default-lead"),
            tr!("speakers-default-body"),
            &[],
        ));
    }
    None
}

/// Who plays here, in a sentence: "Discord is playing here.", "Discord and
/// Firefox are playing here.", "3 apps are playing here.", or "No app is
/// playing here yet.".
pub fn playing_line(sp: &SpeakersState) -> String {
    let names = app_names(sp);
    match (names.as_slice(), sp.playing.as_slice()) {
        ([], _) => tr!("speakers-playing-none"),
        ([_], [a]) if a.name.is_none() => tr!("speakers-playing-anon"),
        ([one], _) => tr!("speakers-playing-one", app = one.clone()),
        ([a, b], _) => tr!("speakers-playing-two", app1 = a.clone(), app2 = b.clone()),
        (all, _) => tr!("speakers-playing-many", count = all.len()),
    }
}

/// The names of the apps playing here ("An app" for one with none), two
/// of one name numbered.
fn app_names(sp: &SpeakersState) -> Vec<String> {
    let anon = tr!("live-app-anon");
    util::numbered(
        sp.playing
            .iter()
            .map(|a| a.name.as_deref().unwrap_or(anon.as_str())),
    )
}

/// The Play to list: System default (now: the device), then the outputs
/// (an unplugged one says so). The chosen output not listed (PipeWire
/// gone) still shows, last, by its name.
pub fn output_items(sp: &SpeakersState) -> (Vec<String>, Vec<String>, usize) {
    let mut names = vec![tr!("live-system-default")];
    let mut notes = vec![sp
        .system_default
        .as_deref()
        // HushMic Speakers made the system output: it plays on the real
        // output that was the system output, and says why
        .map(|d| {
            let device = sp.description_of(d).to_owned();
            if sp.is_default {
                tr!("speakers-system-default-self", device = device)
            } else {
                tr!("live-system-default-now", device = device)
            }
        })
        .unwrap_or_default()];
    let mut idx = 0;
    for (i, d) in sp.outputs.iter().enumerate() {
        names.push(d.description.clone());
        let chosen = sp.output.as_deref() == Some(d.name.as_str());
        notes.push(if d.present {
            String::new()
        } else if chosen {
            tr!("speakers-unplugged-note", device = standin(sp))
        } else {
            tr!("live-mic-unplugged")
        });
        if chosen {
            idx = i + 1;
        }
    }
    if let Some(o) = sp.output.as_deref().filter(|_| idx == 0) {
        names.push(sp.description_of(o).to_owned());
        notes.push(String::new());
        idx = names.len() - 1;
    }
    (names, notes, idx)
}

fn tone_color(t: Tone, pal: &Palette) -> egui::Color32 {
    match t {
        Tone::Error => pal.danger,
        Tone::Warn => pal.warn,
        Tone::Info => pal.ink_subtle,
    }
}

impl SpeakersPage {
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let s = cx.state;
        let ctx = ui.ctx().clone();
        if super::status::filtering(s) {
            cx.light.static_edge = true;
        }
        // the hold's keys before anything is drawn (page Space holds the
        // original while the cleaner runs)
        let active = runs(s);
        // the daemon ended the hold (a lease that lapsed, the command
        // line): the page lets go too, at once, so it never says the
        // original plays while cleaned sound does
        if self.sent && s.speakers.held {
            self.confirmed = true;
        }
        if self.sent && self.confirmed && !s.speakers.held && active {
            self.hold.let_go();
            self.sent = false;
            self.confirmed = false;
        }
        self.hold.space_off = !active;
        self.hold
            .keys(&ctx, ui.layer_id(), &[], active, Kind::Hold, cx.now);
        let (pt, _, pb) = if cx.compact { PAD_COMPACT } else { PAD };
        let origin = ui.max_rect().min;
        let width = ui.max_rect().width();
        let (x, w) = util::column(ui.max_rect(), cx.compact);
        let y = origin.y + pt;
        let y = self.header(ui, cx, x, y, w) + util::HEAD_GAP;
        let y = self.figure(ui, cx, x, y, w);
        util::reach(ui, origin, width, y + pb);
        // the hold reaches the daemon while held, and ends with the
        // cleaner
        if !active {
            self.hold.end_presses();
            self.hold.clear();
        }
        let want = active && self.hold.compare.on;
        if want != self.sent || want && cx.now - self.sent_at >= HOLD_RENEW {
            if want {
                self.sent_at = cx.now;
            }
            if want != self.sent {
                self.confirmed = false;
            }
            self.sent = want;
            cx.send(Action::SpeakersHold(want));
        }
        if want {
            let next = (self.sent_at + HOLD_RENEW - cx.now).max(0.0);
            crate::ui::motion::repaint_in(&ctx, std::time::Duration::from_secs_f64(next));
        }
    }

    /// The hold in this window now (only while the cleaner runs: it
    /// ends with it).
    fn held(&self) -> Held {
        if self.hold.compare.held() {
            Held::Pressed
        } else if self.hold.compare.on {
            Held::Latched
        } else {
            Held::No
        }
    }

    /// The page went away (another page, a minimise): no hold goes on.
    pub fn hide(&mut self, link: &Link) {
        self.hold.end_presses();
        self.hold.clear();
        self.confirmed = false;
        if std::mem::take(&mut self.sent) {
            link.send(Action::SpeakersHold(false));
        }
    }

    fn header(&mut self, ui: &mut Ui, cx: &mut Cx<'_>, x: f32, y: f32, w: f32) -> f32 {
        let s = cx.state;
        let ctx = ui.ctx().clone();
        let off = s.mode == Mode::Off;
        let works = s.pipewire.reachable;
        let switch_label = tr!("speakers-switch");
        let turn_on_label = tr!("live-turn-on");
        let turn_on = Button::primary(&turn_on_label);
        let switch_w = crate::ui::widgets::switch::TRACK.x
            + 12.0
            + text::width(&ctx, &switch_label, Type::SWITCH);
        // the title is cut beside the wider of the two, so a state change
        // never moves the header
        let widest = turn_on.width(&ctx).max(switch_w);
        let title = title_in(s, self.held(), Some((&ctx, util::head_room(w, widest))));
        let head = util::head(ui, &title, x, y, w, widest);
        let ctl_w = if off { turn_on.width(&ctx) } else { switch_w };
        let ctl_x = if head.stacked {
            head.acts.left()
        } else {
            head.acts.right() - ctl_w
        };
        let ctl_y = head.acts.top();
        // Turn on swapped for the switch (or back): for a moment a click on
        // the new one does nothing (Live's double-click rule)
        if self.header_off.replace(off).is_some_and(|was| was != off) {
            self.swapped_at = Some(cx.now);
        }
        let settled = self.swapped_at.is_none_or(|t| cx.now - t >= SWAP_SETTLE);
        if off {
            let r = util::put(
                ui,
                rect(ctl_x, ctl_y, ctl_w, 40.0),
                "speakers-turn-on",
                turn_on.enabled(works),
            );
            if r.clicked() && settled {
                self.focus_switch = r.has_focus().then_some(r.id);
                cx.send(Action::SetMode(Mode::Suppress));
            }
        } else {
            let mut on = s.speakers.enabled;
            let r = util::put(
                ui,
                rect(ctl_x, ctl_y + 20.0 - SWITCH_HALF, switch_w, 24.0),
                "speakers-switch",
                Switch::new(&mut on).label(&switch_label).enabled(works),
            );
            let desc = tr!("speakers-switch-desc");
            ctx.accesskit_node_builder(r.id, |n| n.set_description(desc));
            if let Some(button) = self.focus_switch.take() {
                util::take_focus(&r, button);
            }
            if r.changed() && settled {
                cx.send(Action::SpeakersOn(on));
            }
        }
        head.bottom
    }

    fn figure(&mut self, ui: &mut Ui, cx: &mut Cx<'_>, x: f32, y: f32, w: f32) -> f32 {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let s = cx.state;
        let card_at = ui.painter().add(egui::Shape::Noop);
        let page_fill = widgets::backdrop(ui);
        widgets::set_backdrop(ui, pal.card_fill());
        let row_bottom = self.top_row(ui, cx, x + 1.0, y + 1.0, w - 2.0);
        // the cards: Output, Cleaning, Status side by side; at the compact
        // size Output | Status, Cleaning across under them (Live's rule)
        let top = row_bottom + 1.0;
        let full = w - 2.0;
        let (out_col, clean_col, status_col) = if cx.compact {
            let half = ((full - 1.0) / 2.0).floor();
            (
                (x + 1.0, half),
                (x + 1.0, full),
                (x + 1.0 + half + 1.0, full - half - 1.0),
            )
        } else {
            let unit = full / 3.6;
            (
                (x + 1.0, unit),
                (x + 1.0 + unit + 1.0, 1.6 * unit - 1.0),
                (x + 1.0 + 2.6 * unit + 1.0, unit - 1.0),
            )
        };
        let card = |ui: &mut Ui, (cl, cw): (f32, f32), t: f32, salt: usize| {
            let inner = rect(cl + CARD_PAD, t + CARD_PAD, cw - 2.0 * CARD_PAD, 2000.0);
            let mut c = child(ui, inner, ("speakers-card", salt));
            c.set_width(inner.width());
            c
        };
        let mut c = card(ui, out_col, top, 0);
        self.output_card(&mut c, cx);
        let out_h = c.min_rect().bottom() - top + CARD_PAD;
        // the Status card's height is known before it is drawn (it is
        // drawn last, for the reading order), and never less than its room
        // for the longest problem
        let status_h = status_need(&ctx, cx, self.held(), status_col.1 - 2.0 * CARD_PAD, &pal)
            .max(status_room());
        let (row_h, clean_top) = if cx.compact {
            let h = out_h.max(status_h);
            (h, top + h + 1.0)
        } else {
            (0.0, top)
        };
        let mut c = card(ui, clean_col, clean_top, 1);
        cleaning_card(&mut c, cx);
        let clean_h = c.min_rect().bottom() - clean_top + CARD_PAD;
        let row_h = if cx.compact {
            row_h
        } else {
            out_h.max(clean_h).max(status_h)
        };
        let mut c = card(ui, status_col, top, 2);
        self.status_card(&mut c, cx);
        let fig_bottom = if cx.compact {
            clean_top + clean_h + 1.0
        } else {
            top + row_h + 1.0
        };
        let fig = Rect::from_min_max(pos2(x, y), pos2(x + w, fig_bottom));
        #[cfg(test)]
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("hm-speakers-fig"), fig));
        let p = ui.painter().clone();
        p.set(card_at, widgets::card_fill_shape(fig, &pal));
        widgets::card_edge(&p, fig, &pal);
        // the frame's light: cleaning what plays now
        if runs(s) && !s.speakers.held && s.speakers.tier != Some(EngineTier::Passthrough) {
            frame_light(&p, &pal, fig);
        }
        p.rect_filled(
            Rect::from_min_size(pos2(x + 1.0, row_bottom), vec2(w - 2.0, 1.0)),
            0.0,
            pal.line,
        );
        let vline = |cl: f32, bottom: f32| {
            p.rect_filled(
                Rect::from_min_max(pos2(cl - 1.0, top), pos2(cl, bottom)),
                0.0,
                pal.line,
            );
        };
        if cx.compact {
            vline(status_col.0, top + row_h);
            p.rect_filled(
                Rect::from_min_size(pos2(x + 1.0, clean_top - 1.0), vec2(full, 1.0)),
                0.0,
                pal.line,
            );
        } else {
            vline(clean_col.0, fig_bottom - 1.0);
            vline(status_col.0, fig_bottom - 1.0);
        }
        widgets::set_backdrop(ui, page_fill);
        fig_bottom
    }

    /// The top row: what the cleaner does and how to use it, and "Hold to
    /// hear the original" (beside the words, or under them when narrow).
    /// Returns its bottom (the hairline's top).
    fn top_row(&mut self, ui: &mut Ui, cx: &mut Cx<'_>, x: f32, y: f32, w: f32) -> f32 {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let s = cx.state;
        let active = runs(s);
        let label = tr!("mic-hold");
        let look = self.hold.look(&label, active, s.speakers.held);
        let hw = look.width(&ctx);
        let inner = w - 2.0 * ROW_PAD_X;
        let beside = inner - hw - 16.0 >= ROW_TEXT_MIN;
        let tw = if beside { inner - hw - 16.0 } else { inner };
        let block = about_block(&ctx, tw, &pal);
        let top = y + ROW_PAD_TOP;
        let (text_y, hold_at, bottom) = if beside {
            let h = block.size().y.max(ROW_H);
            (
                top + (h - block.size().y) / 2.0,
                pos2(x + ROW_PAD_X + inner - hw, top + (h - ROW_H) / 2.0),
                top + h + ROW_PAD_BOTTOM,
            )
        } else {
            let ty = top;
            let hy = ty + block.size().y + 12.0;
            (ty, pos2(x + ROW_PAD_X, hy), hy + ROW_H + ROW_PAD_BOTTOM)
        };
        util::text_at(ui, block, pos2(x + ROW_PAD_X, text_y));
        let had = ctx.memory(|m| m.focused());
        let (r, _) = self.hold.button(
            ui,
            Rect::from_min_size(hold_at, vec2(hw, ROW_H)),
            "speakers-hold",
            look,
            tr!("speakers-hold-desc"),
            active,
            Kind::Hold,
            cx.now,
        );
        if !active {
            let r = r.clone().on_hover_text(tr!("speakers-hold-off-tip"));
            // the button went disabled under the keyboard: the rail takes
            // it, so the keyboard is never lost
            if had == Some(r.id) {
                crate::ui::widgets::rail::take_focus(&ctx);
            }
        }
        bottom
    }

    /// Output: where the cleaned sound plays, and the delay it adds.
    fn output_card(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let sp = &cx.state.speakers;
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        card_head(ui, &tr!("speakers-output"), &tr!("speakers-play-to"), &pal);
        let (names, notes, mut idx) = output_items(sp);
        let items: Vec<SelectItem<'_>> = names
            .iter()
            .zip(&notes)
            .map(|(n, t)| SelectItem { label: n, note: t })
            .collect();
        let before = idx;
        let sw = ui.available_width();
        let fh = widgets::select::field_height(&ctx, items.get(idx).map_or("", |i| i.label), sw);
        // the tallest name's room from the start: a pick never moves the
        // cards
        let reserve = items
            .iter()
            .map(|i| widgets::select::field_height(&ctx, i.label, sw))
            .fold(fh, f32::max)
            - fh;
        let mut c = child(
            ui,
            Rect::from_min_size(ui.cursor().min, vec2(sw, fh)),
            "speakers-output",
        );
        let r = widgets::select(&mut c, &tr!("speakers-play-to"), &mut idx, &items);
        // (its description is the choice shown, where Orca reads it)
        self.select_id = Some(r.id);
        ui.advance_cursor_after_rect(Rect::from_min_size(ui.cursor().min, vec2(sw, fh)));
        if idx != before {
            let pick = if idx == 0 {
                None
            } else {
                sp.outputs
                    .get(idx - 1)
                    .map(|d| d.name.clone())
                    .or_else(|| sp.output.clone())
            };
            cx.send(Action::SpeakersOutput(pick));
        }
        ui.add_space(CARD_GAP);
        let hint = text::label(ui, &tr!("speakers-delay"), Type::CAP, pal.ink_subtle);
        if reserve > 0.0 {
            ui.expand_to_include_rect(Rect::from_min_size(
                hint.rect.left_bottom(),
                vec2(1.0, reserve),
            ));
        }
    }

    /// Status: who plays here while the cleaner runs, then the one problem
    /// that matters (a 2 px bar in its tone, its links), or the quiet line.
    fn status_card(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let s = cx.state;
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        card_head(ui, &tr!("live-status"), "", &pal);
        let w = ui.available_width();
        if runs(s) {
            // "● Discord and Firefox are playing here.": the accent dot
            // while an app plays, hollow while none does; a live region
            let sp = &s.speakers;
            let line = playing_line(sp);
            let (block, any) = playing_block(&ctx, sp, w, &pal);
            let at = ui.cursor().min;
            let dot = pos2(at.x + 4.0, at.y + Type::CAP.line / 2.0);
            if any {
                ui.painter().circle_filled(dot, 4.0, pal.accent);
            } else {
                ui.painter()
                    .circle_stroke(dot, 3.5, egui::Stroke::new(1.0_f32, pal.ink_subtle));
            }
            // painted: the node below names it (all the apps from three on)
            let r = util::paint_at(ui, block, pos2(at.x + 14.0, at.y));
            let resp = ui.interact(r, ui.id().with("speakers-playing"), egui::Sense::hover());
            let all = tr!("speakers-playing-all", apps = app_names(sp).join(", "));
            let name = if sp.playing.len() > 2 {
                all.clone()
            } else {
                line
            };
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &name));
            util::tell(&ctx, resp.id, util::Speak::Live);
            if sp.playing.len() > 2 {
                resp.on_hover_text(all);
            }
            ui.advance_cursor_after_rect(Rect::from_min_size(at, vec2(w, r.height())));
            ui.add_space(CARD_GAP);
        }
        let Some(p) = problem(s, self.held()) else {
            let r = text::label(ui, &tr!("live-status-ok"), Type::CAP, pal.ink_subtle);
            util::tell(&ctx, r.id, util::Speak::Live);
            return;
        };
        let tw = (w - BAR_IN).max(40.0);
        let block = p.block(&ctx, tw, &pal);
        let top = ui.cursor().min;
        let tr_ = Rect::from_min_size(pos2(top.x + BAR_IN, top.y), block.size());
        block.paint(ui.painter(), tr_.min);
        let full = p.words();
        let resp = ui.interact(tr_, ui.id().with("speakers-problem"), egui::Sense::hover());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &full));
        util::tell(&ctx, resp.id, util::Speak::Live);
        ui.advance_cursor_after_rect(Rect::from_min_size(top, vec2(w, block.size().y)));
        for fix in &p.fixes {
            let label = fix_label(cx, *fix);
            ui.add_space(LINK_GAP);
            let lh = link_height(&ctx, &label, tw, &pal);
            let mut row = child(
                ui,
                Rect::from_min_size(ui.cursor().min + vec2(BAR_IN, 0.0), vec2(tw, lh)),
                ("speakers-fix", *fix as usize),
            );
            let l = widgets::link(&mut row, &label, false, false);
            ui.advance_cursor_after_rect(Rect::from_min_size(ui.cursor().min, vec2(w, lh)));
            if l.clicked() {
                match fix {
                    Fix::Retry => cx.send(Action::SpeakersRetry),
                    Fix::Diagnostics => cx.diag.start(),
                    Fix::ChooseOutput => {
                        if let Some(id) = self.select_id {
                            widgets::select::open_select(&ctx, id);
                            ctx.request_repaint();
                        }
                    }
                }
            }
        }
        let bar_bottom = ui.cursor().min.y.max(tr_.bottom());
        ui.painter().rect_filled(
            Rect::from_min_max(top, pos2(top.x + 2.0, bar_bottom)),
            0.0,
            tone_color(p.tone, &pal),
        );
    }
}

/// The Status card's height for what it shows now in a card `cw` wide
/// (its padding included).
fn status_need(ctx: &egui::Context, cx: &Cx<'_>, held: Held, cw: f32, pal: &Palette) -> f32 {
    let s = cx.state;
    let mut h = CARD_PAD + Type::EYEBROW.line + CARD_GAP;
    if runs(s) {
        let (block, _) = playing_block(ctx, &s.speakers, cw, pal);
        h += block.size().y + CARD_GAP;
    }
    let tw = (cw - BAR_IN).max(40.0);
    match problem(s, held) {
        None => h += Type::CAP.line,
        Some(p) => {
            h += p.block(ctx, tw, pal).size().y;
            for fix in &p.fixes {
                h += LINK_GAP + link_height(ctx, &fix_label(cx, *fix), tw, pal);
            }
        }
    }
    h + CARD_PAD
}

fn fix_label(cx: &Cx<'_>, fix: Fix) -> String {
    match fix {
        Fix::Retry => tr!("live-try-again"),
        Fix::Diagnostics => cx.diag.label(),
        Fix::ChooseOutput => tr!("speakers-choose-output"),
    }
}

/// A link may wrap in the narrow card: its room is what it takes.
fn link_height(ctx: &egui::Context, label: &str, tw: f32, pal: &Palette) -> f32 {
    text::layout(ctx, label, Type::LINK, pal.ink, tw)
        .size()
        .y
        .max(Type::LINK.line)
}

/// The playing line laid out beside its dot, in a card `w` wide, and
/// whether an app plays.
fn playing_block(
    ctx: &egui::Context,
    sp: &SpeakersState,
    w: f32,
    pal: &Palette,
) -> (TextBlock, bool) {
    let any = !sp.playing.is_empty();
    let (ty, color) = if any {
        (Type::CAP.medium(), pal.ink)
    } else {
        (Type::CAP, pal.ink_subtle)
    };
    (
        text::layout(ctx, &playing_line(sp), ty, color, (w - 14.0).max(40.0)),
        any,
    )
}

/// The top row's words: what the cleaner does (medium ink), how to use it.
fn about_block(ctx: &egui::Context, w: f32, pal: &Palette) -> TextBlock {
    text::Rich::new()
        .span(&tr!("speakers-about"), Type::CAP.medium(), pal.ink)
        .span(" ", Type::CAP, pal.ink_muted)
        .span(&tr!("speakers-route"), Type::CAP, pal.ink_muted)
        .layout(ctx, w)
}

/// The Status card's least height: the eyebrow, the playing line (two
/// lines), a problem of [`STATUS_ROWS`] lines and two links, the padding.
fn status_room() -> f32 {
    CARD_PAD
        + Type::EYEBROW.line
        + CARD_GAP
        + 2.0 * Type::CAP.line
        + CARD_GAP
        + STATUS_ROWS as f32 * Type::CAP.line
        + 2.0 * (LINK_GAP + Type::LINK.line)
        + CARD_PAD
}

/// The strength caption while the cleaner does not run: why, and when the
/// step applies.
fn strength_waits(s: &AppState) -> String {
    let sp = &s.speakers;
    if !sp.enabled {
        tr!("speakers-strength-applies")
    } else if s.mode == Mode::Off {
        tr!("speakers-strength-hushmic-off")
    } else if !s.pipewire.reachable || sp.phase == Phase::Down {
        tr!("speakers-strength-down")
    } else {
        tr!("speakers-strength-starting")
    }
}

/// Cleaning: the strength (the microphone's four steps), changed live.
fn cleaning_card(ui: &mut Ui, cx: &mut Cx<'_>) {
    let s = cx.state;
    let sp = &s.speakers;
    let ctx = ui.ctx().clone();
    let pal = crate::ui::kit(&ctx).pal;
    card_head(ui, &tr!("speakers-cleaning"), &tr!("live-strength"), &pal);
    let labels = [
        tr!("strength-gentle"),
        tr!("strength-medium"),
        tr!("strength-strong"),
        tr!("strength-maximum"),
    ];
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let mut idx = super::live::strength_index(sp.strength).unwrap_or(usize::MAX);
    let before = idx;
    let name = tr!("live-strength");
    widgets::Strength::new(&name, &mut idx, &refs).show(ui);
    if idx != before && idx < 4 {
        cx.send(Action::SpeakersStrength(Strength::PRESETS[idx].0));
    }
    ui.add_space(CARD_GAP);
    let cap = match sp.strength {
        _ if !runs(s) => strength_waits(s),
        Strength::Gentle => tr!("live-strength-gentle"),
        Strength::Medium => tr!("live-strength-medium"),
        Strength::Strong => tr!("live-strength-strong"),
        Strength::Maximum => tr!("live-strength-maximum"),
        Strength::Custom => tr!("live-strength-custom"),
    };
    let top = ui.cursor().min.y;
    let r = text::label(ui, &cap, Type::CAP, pal.ink_subtle);
    util::tell(&ctx, r.id, util::Speak::Live);
    // the caption keeps the room of the longest one (two lines at least),
    // so the card's height stays when the strength changes
    let w = ui.available_width();
    let room = [
        tr!("speakers-strength-applies"),
        tr!("speakers-strength-hushmic-off"),
        tr!("speakers-strength-starting"),
        tr!("speakers-strength-down"),
        tr!("live-strength-gentle"),
        tr!("live-strength-medium"),
        tr!("live-strength-strong"),
        tr!("live-strength-maximum"),
        tr!("live-strength-custom"),
    ]
    .iter()
    .map(|t| text::layout(&ctx, t, Type::CAP, pal.ink_subtle, w).size().y)
    .fold(36.0_f32, f32::max);
    let used = ui.cursor().min.y - top;
    if used < room {
        ui.add_space(room - used);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock::{self, Scenario, SpeakersScene};
    use crate::appstate::Page;
    use crate::pages::testing::{self, Role};
    use egui::accesskit::{Live, Toggled};
    use egui_kittest::kittest::{NodeT, Queryable};

    const HOLD: &str = "Hold to hear the original";
    const BIG: egui::Vec2 = egui::vec2(960.0, 640.0);
    const SMALL: egui::Vec2 = egui::vec2(720.0, 520.0);

    fn st(sc: SpeakersScene) -> AppState {
        mock::with_speakers(mock::state(Scenario::OnInCall), sc)
    }

    fn win(sc: SpeakersScene) -> testing::Window {
        testing::window(st(sc), Page::Speakers, BIG)
    }

    fn fig(w: &testing::Window) -> Rect {
        w.harness
            .ctx
            .data(|d| d.get_temp::<Rect>(egui::Id::new("hm-speakers-fig")))
            .expect("the card was drawn")
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

    /// The hold lines sent, a renewal of the same word folded into it.
    fn holds(w: &testing::Window) -> Vec<bool> {
        fold(hold_lines(&w.link.sent()))
    }

    fn hold_lines(sent: &[Action]) -> Vec<bool> {
        sent.iter()
            .filter_map(|a| match a {
                Action::SpeakersHold(on) => Some(*on),
                _ => None,
            })
            .collect()
    }

    fn fold(mut v: Vec<bool>) -> Vec<bool> {
        v.dedup();
        v
    }

    /// The pointer goes down (or up) on the hold button of a switched
    /// window.
    fn pointer_at(h: &mut egui_kittest::Harness<'static>, pressed: bool) {
        let at = h.get_by_label(HOLD).rect().center();
        let events = &mut h.input_mut().events;
        events.push(egui::Event::PointerMoved(at));
        events.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }

    #[test]
    fn a_hold_is_a_lease_the_page_renews_every_second() {
        let mut w = win(SpeakersScene::Playing);
        pointer(&mut w, true);
        // 2.5 s held (kittest steps 250 ms): on, then about every second
        w.harness.run_steps(10);
        let on = hold_lines(&w.link.sent());
        assert!(on.iter().all(|b| *b), "{on:?}");
        assert!((2..=4).contains(&on.len()), "{on:?}");
        pointer(&mut w, false);
        w.harness.run_steps(8);
        // the release ends it, and nothing is renewed after
        let all = hold_lines(&w.link.sent());
        assert_eq!(all.last(), Some(&false));
        assert_eq!(all.len(), on.len() + 1, "{all:?}");
    }

    #[test]
    fn the_page_lets_go_when_the_daemon_ends_the_hold() {
        let held = |on: bool| {
            let mut s = st(SpeakersScene::Playing);
            s.speakers.held = on;
            s
        };
        let heading = |h: &egui_kittest::Harness<'static>, t: &str| {
            h.query_by_role_and_label(Role::Heading, t).is_some()
        };
        let pressed = |h: &egui_kittest::Harness<'static>| {
            h.get_by_label(HOLD).accesskit_node().toggled() == Some(Toggled::True)
        };
        let sent = |w: &testing::Switched| {
            w.links
                .iter()
                .flat_map(|l| hold_lines(&l.sent()))
                .collect::<Vec<_>>()
        };
        // latched; the daemon holds it; then it ends it (a lease lapsed)
        let mut w = testing::switched(
            vec![held(false), held(true), held(false)],
            Page::Speakers,
            BIG,
        );
        pointer_at(&mut w.harness, true);
        w.harness.run_steps(1);
        pointer_at(&mut w.harness, false);
        w.harness.run_steps(3);
        assert!(heading(&w.harness, "Playing the original."));
        w.at.set(1);
        w.harness.run_steps(3);
        assert!(pressed(&w.harness));
        w.at.set(2);
        w.harness.run_steps(2);
        // the page says what plays: cleaned
        assert!(heading(&w.harness, "Removing noise from what you hear."));
        assert!(!pressed(&w.harness));
        assert!(w
            .harness
            .query_by_label_contains("Playing the original")
            .is_none());
        assert_eq!(fold(sent(&w)), [true], "no off: the daemon ended it");
        // one tap brings the original back
        pointer_at(&mut w.harness, true);
        w.harness.run_steps(1);
        pointer_at(&mut w.harness, false);
        w.harness.run_steps(3);
        assert_eq!(fold(sent(&w)), [true]);
        assert!(heading(&w.harness, "Playing the original."));
        assert_eq!(hold_lines(&w.links[2].sent()).first(), Some(&true));

        // held down when the daemon ends it: the press counts for nothing
        // more; the next press holds again
        let mut w = testing::switched(
            vec![held(false), held(true), held(false)],
            Page::Speakers,
            BIG,
        );
        pointer_at(&mut w.harness, true);
        w.harness.run_steps(2);
        w.at.set(1);
        w.harness.run_steps(2);
        w.at.set(2);
        w.harness.run_steps(30);
        assert!(heading(&w.harness, "Removing noise from what you hear."));
        assert!(hold_lines(&w.links[2].sent()).is_empty());
        pointer_at(&mut w.harness, false);
        w.harness.run_steps(3);
        assert!(hold_lines(&w.links[2].sent()).is_empty());
        pointer_at(&mut w.harness, true);
        w.harness.run_steps(2);
        assert_eq!(hold_lines(&w.links[2].sent()), [true]);
    }

    #[test]
    fn the_rail_has_four_pages_and_opens_speakers_after_live() {
        let mut w = testing::scenario(Scenario::OnInCall, Page::Live);
        let tabs: Vec<String> = w
            .harness
            .query_all_by_role(Role::Tab)
            .filter_map(|n| n.accesskit_node().label())
            .collect();
        assert_eq!(tabs, ["Live", "Speakers", "Files", "Settings"]);
        testing::by_role(&w.harness, "Speakers", Role::Tab).click();
        w.harness.run_steps(2);
        assert_eq!(*w.page.borrow(), Page::Speakers);
        // the switch starts off, and the page says so
        testing::by_role(&w.harness, "Speaker cleaning is off.", Role::Heading);
        let switch = testing::by_role(&w.harness, "Speaker cleaning", Role::CheckBox);
        assert_eq!(switch.accesskit_node().toggled(), Some(Toggled::False));
        // the compact rail too
        let w = testing::compact(Scenario::OnInCall, Page::Speakers);
        assert_eq!(w.harness.query_all_by_role(Role::Tab).count(), 4);
    }

    #[test]
    fn the_header_says_the_cleaners_state_alone() {
        let off = |mut s: AppState| {
            s.mode = Mode::Off;
            s
        };
        let cases = [
            (st(SpeakersScene::Off), "Speaker cleaning is off."),
            (st(SpeakersScene::Starting), "Starting."),
            (st(SpeakersScene::Idle), "Ready."),
            (
                st(SpeakersScene::Playing),
                "Removing noise from what you hear.",
            ),
            (st(SpeakersScene::Held), "Playing the original."),
            (st(SpeakersScene::Unplugged), "USB Headset is unplugged."),
            (st(SpeakersScene::Down), "Speaker cleaning is stopped."),
            (
                st(SpeakersScene::Light),
                "Removing noise from what you hear.",
            ),
            (
                st(SpeakersScene::IsDefault),
                "Removing noise from what you hear.",
            ),
            (
                mock::with_speakers(mock::state(Scenario::Off), SpeakersScene::Playing),
                "HushMic is off.",
            ),
            (off(st(SpeakersScene::Idle)), "HushMic is off."),
            (
                mock::with_speakers(
                    mock::state(Scenario::PipewireUnreachable),
                    SpeakersScene::Playing,
                ),
                "Speaker cleaning is stopped.",
            ),
        ];
        for (s, want) in cases {
            crate::i18n::pin_english();
            assert_eq!(title(&s), want);
            let w = testing::window(s, Page::Speakers, BIG);
            testing::by_role(&w.harness, want, Role::Heading);
        }
        // a name too long for the title's line: the plain words
        let mut s = st(SpeakersScene::Unplugged);
        for d in s.speakers.outputs.iter_mut() {
            d.description = "Jabra Evolve2 85 Wireless Stereo Headset Analog Output".into();
        }
        let w = testing::window(s, Page::Speakers, SMALL);
        testing::by_role(&w.harness, "Your output is unplugged.", Role::Heading);
    }

    #[test]
    fn the_switch_sends_on_and_off_and_describes_itself() {
        let mut w = win(SpeakersScene::Off);
        let switch = testing::by_role(&w.harness, "Speaker cleaning", Role::CheckBox);
        assert_eq!(
            switch.accesskit_node().description().as_deref(),
            Some("Adds HushMic Speakers to your outputs while it is on.")
        );
        switch.click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SpeakersOn(true)]);
        let mut w = win(SpeakersScene::Playing);
        testing::by_role(&w.harness, "Speaker cleaning", Role::CheckBox).click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SpeakersOn(false)]);
        // on a mock daemon the switch starts the cleaner
        let mut w = testing::mock_window(mock::state(Scenario::OnInCall), Page::Speakers, BIG);
        testing::by_role(&w.harness, "Speaker cleaning", Role::CheckBox).click();
        w.harness.run_steps(2);
        testing::by_role(&w.harness, "Starting.", Role::Heading);
    }

    #[test]
    fn hushmic_off_offers_turn_on_where_the_switch_was() {
        let mut w = testing::window(
            mock::with_speakers(mock::state(Scenario::Off), SpeakersScene::Idle),
            Page::Speakers,
            BIG,
        );
        assert!(w
            .harness
            .query_by_role_and_label(Role::CheckBox, "Speaker cleaning")
            .is_none());
        w.harness.get_by_label("Turn on HushMic").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SetMode(Mode::Suppress)]);
    }

    #[test]
    fn a_hold_plays_the_original_until_its_release() {
        let mut w = win(SpeakersScene::Playing);
        pointer(&mut w, true);
        w.harness.run_steps(1);
        assert_eq!(holds(&w), [true]);
        // held across frames: one activation for the press
        w.harness.run_steps(4);
        assert_eq!(holds(&w), [true]);
        pointer(&mut w, false);
        w.harness.run_steps(2);
        assert_eq!(holds(&w), [true, false]);
        // Enter held on the focused button: the same, key repeats aside
        w.harness.get_by_label(HOLD).focus();
        w.harness.run_steps(2);
        w.harness.key_down(egui::Key::Enter);
        w.harness.run_steps(1);
        w.harness.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: egui::Modifiers::NONE,
        });
        w.harness.run_steps(3);
        w.harness.key_up(egui::Key::Enter);
        w.harness.run_steps(2);
        assert_eq!(holds(&w), [true, false, true, false]);
    }

    #[test]
    fn the_hold_waits_while_the_cleaner_does_not_run() {
        for s in [
            st(SpeakersScene::Off),
            st(SpeakersScene::Starting),
            st(SpeakersScene::Down),
            mock::with_speakers(mock::state(Scenario::Off), SpeakersScene::Playing),
        ] {
            let mut w = testing::window(s, Page::Speakers, BIG);
            assert!(w.harness.get_by_label(HOLD).accesskit_node().is_disabled());
            pointer(&mut w, true);
            w.harness.run_steps(2);
            pointer(&mut w, false);
            w.harness.run_steps(2);
            assert!(holds(&w).is_empty());
        }
        // the daemon holds it (the command line): the button shows it
        let w = win(SpeakersScene::Held);
        assert_eq!(
            w.harness.get_by_label(HOLD).accesskit_node().toggled(),
            Some(Toggled::True)
        );
    }

    #[test]
    fn the_original_playing_is_said_in_the_header_and_status() {
        let heading = |w: &testing::Window, t: &str| {
            w.harness
                .query_by_role_and_label(Role::Heading, t)
                .is_some()
        };
        let mut w = testing::at_60hz(st(SpeakersScene::Playing), Page::Speakers, BIG);
        // held down: at once, before the daemon answers
        pointer(&mut w, true);
        w.harness.run_steps(20);
        assert!(heading(&w, "Playing the original."));
        assert!(w
            .harness
            .query_by_label_contains("Let go to hear HushMic again.")
            .is_some());
        pointer(&mut w, false);
        w.harness.run_steps(3);
        assert!(heading(&w, "Removing noise from what you hear."));
        // a tap latches it: it stays said until the next tap
        pointer(&mut w, true);
        w.harness.run_steps(1);
        pointer(&mut w, false);
        w.harness.run_steps(30);
        assert!(heading(&w, "Playing the original."));
        let line = w
            .harness
            .get_by_label_contains("Playing the original, without cleaning.");
        let label = line.accesskit_node().value();
        assert!(
            label
                .as_deref()
                .is_some_and(|l| l.ends_with("Press again to hear HushMic.")),
            "{label:?}"
        );
        assert_eq!(line.accesskit_node().live(), Live::Polite);
        // held from elsewhere (the daemon says so): the same
        let w = win(SpeakersScene::Held);
        assert!(w
            .harness
            .query_by_label_contains("Playing the original, without cleaning.")
            .is_some());
    }

    #[test]
    fn the_strength_caption_says_why_it_waits() {
        crate::i18n::pin_english();
        let off = |mut s: AppState| {
            s.mode = Mode::Off;
            s
        };
        for (s, want) in [
            (
                st(SpeakersScene::Off),
                "Applies when speaker cleaning is on.",
            ),
            (
                off(st(SpeakersScene::Idle)),
                "HushMic is off. This applies when you turn it on.",
            ),
            (
                st(SpeakersScene::Starting),
                "Applies as soon as speaker cleaning has started.",
            ),
            (
                st(SpeakersScene::Down),
                "Applies once speaker cleaning runs again.",
            ),
        ] {
            assert_eq!(strength_waits(&s), want);
            let w = testing::window(s, Page::Speakers, BIG);
            assert!(w.harness.query_by_label(want).is_some(), "{want}");
        }
        // HushMic Speakers as the system output: System default names the
        // real output and why
        let (_, notes, _) = output_items(&st(SpeakersScene::IsDefault).speakers);
        assert_eq!(
            notes[0],
            "Plays on Built-in Audio Analog Stereo: HushMic Speakers is the system output"
        );
    }

    #[test]
    fn status_names_each_app_a_restart_left_behind() {
        crate::i18n::pin_english();
        let line = "Discord left HushMic Speakers when it restarted. \
                    Pick HushMic Speakers for it again.";
        let s = st(SpeakersScene::Left);
        let p = problem(&s, Held::No).expect("a problem");
        assert_eq!((p.tone, p.words().as_str()), (Tone::Warn, line));
        let w = testing::window(s.clone(), Page::Speakers, BIG);
        let shown = w.harness.get_by_label(line);
        assert_eq!(shown.accesskit_node().live(), Live::Polite);
        // one line per app, a nameless one as "An app"; two of one name
        // numbered
        let mut s2 = s.clone();
        s2.speakers.left.push(crate::appstate::AppRef {
            id: "x".into(),
            name: None,
        });
        s2.speakers.left.push(s.speakers.left[0].clone());
        let p = problem(&s2, Held::No).unwrap();
        assert_eq!(p.more.len(), 2);
        assert_eq!(
            p.more[0].0,
            "An app left HushMic Speakers when it restarted."
        );
        assert_eq!(
            p.more[1].0,
            "Discord (2) left HushMic Speakers when it restarted."
        );
        let w = testing::window(s2, Page::Speakers, SMALL);
        assert!(w
            .harness
            .query_by_label_contains("An app left HushMic Speakers")
            .is_some());
        // holding the original says so first; off, nothing is left
        assert_eq!(
            problem(&s, Held::Latched).map(|p| p.lead).as_deref(),
            Some("Playing the original, without cleaning.")
        );
        let mut s3 = s.clone();
        mock::apply(&mut s3, &Action::SpeakersOn(false));
        assert!(s3.speakers.left.is_empty());
    }

    #[test]
    fn maximum_says_the_pauses_go_silent() {
        let w = win(SpeakersScene::Idle);
        assert!(w
            .harness
            .query_by_label(
                "Removes as much noise as it can. The pauses between words become silent."
            )
            .is_some());
    }

    #[test]
    fn leaving_the_page_or_minimising_ends_a_latched_hold() {
        for minimise in [false, true] {
            let mut w = testing::at_60hz(st(SpeakersScene::Playing), Page::Speakers, BIG);
            // a tap latches the original
            pointer(&mut w, true);
            w.harness.run_steps(1);
            pointer(&mut w, false);
            w.harness.run_steps(3);
            assert_eq!(holds(&w), [true], "a tap latches");
            if minimise {
                w.harness
                    .input_mut()
                    .viewports
                    .entry(egui::ViewportId::ROOT)
                    .or_default()
                    .minimized = Some(true);
                w.harness.run_steps(2);
            } else {
                testing::by_role(&w.harness, "Live", Role::Tab).click();
                w.harness.run_steps(3);
            }
            assert_eq!(holds(&w), [true, false], "minimise {minimise}");
        }
    }

    #[test]
    fn play_to_lists_the_outputs_and_picks_one() {
        let mut w = win(SpeakersScene::Playing);
        let combo = w.harness.get_by_role_and_label(Role::ComboBox, "Play to");
        assert_eq!(
            combo.accesskit_node().value().as_deref(),
            Some("USB Headset")
        );
        combo.click();
        w.harness.run_steps(2);
        let default = w
            .harness
            .get_by_role_and_label(Role::ListBoxOption, "System default");
        assert_eq!(
            default.accesskit_node().description().as_deref(),
            Some("Now: Built-in Audio Analog Stereo")
        );
        w.harness.get_by_label("HDMI / DisplayPort 1").click();
        w.harness.run_steps(2);
        assert_eq!(
            w.link.sent(),
            vec![Action::SpeakersOutput(Some(
                "alsa_output.pci-0000_01_00.1.hdmi-stereo".into()
            ))]
        );
        // an unplugged pick says where the sound plays meanwhile
        let (names, notes, idx) = output_items(&st(SpeakersScene::Unplugged).speakers);
        assert_eq!(names[idx], "USB Headset");
        assert_eq!(
            notes[idx],
            "Unplugged · playing on Built-in Audio Analog Stereo"
        );
        // System default sends no name
        let mut w = win(SpeakersScene::Playing);
        w.harness
            .get_by_role_and_label(Role::ComboBox, "Play to")
            .click();
        w.harness.run_steps(2);
        w.harness
            .get_by_role_and_label(Role::ListBoxOption, "System default")
            .click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SpeakersOutput(None)]);
    }

    #[test]
    fn the_strength_sends_the_speakers_step() {
        let mut w = win(SpeakersScene::Idle);
        w.harness.get_by_label("Gentle").click();
        w.harness.run_steps(2);
        assert_eq!(
            w.link.sent(),
            vec![Action::SpeakersStrength(Strength::Gentle)]
        );
        // off, it says when it applies
        let w = win(SpeakersScene::Off);
        assert!(w
            .harness
            .query_by_label("Applies when speaker cleaning is on.")
            .is_some());
    }

    #[test]
    fn status_names_who_plays_and_the_one_problem() {
        crate::i18n::pin_english();
        let mut sp = st(SpeakersScene::Idle).speakers;
        assert_eq!(playing_line(&sp), "No app is playing here yet.");
        sp.playing = vec![crate::appstate::AppRef {
            id: "x".into(),
            name: None,
        }];
        assert_eq!(playing_line(&sp), "An app is playing here.");
        let sp = st(SpeakersScene::Playing).speakers;
        assert_eq!(playing_line(&sp), "Discord and Firefox are playing here.");
        let mut more = sp.clone();
        more.playing.push(more.playing[0].clone());
        assert_eq!(playing_line(&more), "3 apps are playing here.");
        assert_eq!(app_names(&more)[2], "Discord (2)");

        let lead = |sc| problem(&st(sc), Held::No).map(|p| (p.lead, p.fixes));
        assert_eq!(lead(SpeakersScene::Playing), None);
        assert_eq!(lead(SpeakersScene::Off), None);
        assert_eq!(
            lead(SpeakersScene::Down),
            Some((
                "HushMic Speakers can’t start.".into(),
                vec![Fix::Retry, Fix::Diagnostics]
            ))
        );
        assert_eq!(
            lead(SpeakersScene::Unplugged),
            Some(("USB Headset is unplugged.".into(), vec![Fix::ChooseOutput]))
        );
        assert_eq!(
            lead(SpeakersScene::Light).map(|l| l.0).as_deref(),
            Some("Using Light for now.")
        );
        assert_eq!(
            lead(SpeakersScene::IsDefault).map(|l| l.0).as_deref(),
            Some("HushMic Speakers is your system output.")
        );
        // Try again and Choose an output act
        let mut w = win(SpeakersScene::Down);
        w.harness.get_by_label("Try again").click();
        w.harness.run_steps(2);
        assert_eq!(w.link.sent(), vec![Action::SpeakersRetry]);
        let mut w = win(SpeakersScene::Unplugged);
        w.harness.get_by_label("Choose an output").click();
        w.harness.run_steps(2);
        assert!(w
            .harness
            .query_by_role_and_label(Role::ListBoxOption, "System default")
            .is_some());
    }

    #[test]
    fn the_card_fits_both_sizes_and_no_state_moves_it() {
        for size in [BIG, SMALL] {
            let mut heights = Vec::new();
            for sc in SpeakersScene::ALL {
                let w = testing::window(st(sc), Page::Speakers, size);
                let f = fig(&w);
                let area = Rect::from_min_max(
                    pos2(crate::ui::widgets::Rail::width(size.x < 960.0), 0.0),
                    pos2(size.x, size.y),
                );
                let (x, cw) = util::column(area, size.x < 960.0);
                assert!(
                    (f.left() - x).abs() < 0.5 && (f.right() - (x + cw)).abs() < 0.5,
                    "{sc:?} {size:?}: {f:?}"
                );
                heights.push((sc, f.height()));
            }
            let h0 = heights[0].1;
            for (sc, h) in &heights {
                assert!((h - h0).abs() < 0.5, "{sc:?} at {size:?}: {h} vs {h0}");
            }
        }
    }

    #[test]
    fn screen_readers_get_headings_descriptions_and_live_regions() {
        crate::i18n::pin_english();
        let w = win(SpeakersScene::Playing);
        for card in ["Output", "Cleaning", "Status"] {
            let h = testing::by_role(&w.harness, card, Role::Heading);
            assert_eq!(h.accesskit_node().data().level(), Some(2), "{card}");
        }
        let hold = w.harness.get_by_label(HOLD);
        assert_eq!(
            hold.accesskit_node().description().as_deref(),
            Some("Press to switch, hold to compare, on what plays now.")
        );
        let playing = w
            .harness
            .get_by_label("Discord and Firefox are playing here.");
        assert_eq!(playing.accesskit_node().live(), Live::Polite);
        let quiet = w.harness.get_by_label("Nothing needs your attention.");
        assert_eq!(quiet.accesskit_node().live(), Live::Polite);
        let w = win(SpeakersScene::Unplugged);
        let problem = w.harness.get_by_label_contains("until it’s back");
        assert_eq!(problem.accesskit_node().live(), Live::Polite);
        // the reading order: the switch, the hold, Play to, the strength
        let order: Vec<String> = w
            .harness
            .root()
            .query_all(egui_kittest::kittest::by().predicate(|n| {
                matches!(
                    n.role(),
                    Role::CheckBox | Role::Button | Role::ComboBox | Role::RadioButton
                )
            }))
            .filter_map(|n| n.accesskit_node().label())
            .filter(|l| ["Speaker cleaning", HOLD, "Play to", "Gentle"].contains(&l.as_str()))
            .collect();
        assert_eq!(order, ["Speaker cleaning", HOLD, "Play to", "Gentle"]);
    }
}
