//! Files: clean a recording (PLAN "Clean a file", App-Files). Adding a
//! file starts one background pass at full strength; the strength, the
//! hold and Save are mixes of that pass and the original, so none of them
//! cleans again. Nothing plays until Play.
//!
//! The header says what happens (a title and one reserved second line);
//! the figure holds the selected file's picture (two spectrograms on one
//! time axis, the whole file wide: the original above, the cleaned sound
//! below as far as it is cleaned, one playhead across both; a click or a
//! drag on either seeks; where the file is and how long it is stand in
//! its upper right corner, how far the cleaned copy is in the lower
//! lane's caption) and Live's row of controls: the file's strength ·
//! Play · Hold to hear the original. Under it (App-Files2) a toolbar
//! holds what is for every file (Add files…, Save all, Clear all), and
//! one row per file its state, its one action and its ×. The list has no
//! white button: its toolbar's are outlined, a row's are quiet icons
//! ([`quiet_icon`]). The card reaches the page's end: the list takes what
//! its rows need and the lanes the rest ([`share`]).

pub mod daemon;
pub mod demo;
pub(crate) mod desktop;
pub mod intake;
pub mod model;

use eframe::egui::{self, pos2, vec2, Color32, Rect, Response, Sense, Ui, WidgetInfo, WidgetType};

use self::model::{
    Cmd, Estimate, FileId, Kind, Pass, Place, Play, Reason, Row, Save, Source, View,
};
use super::hold::{Hold, Kind as HoldKind};
use super::replay;
use super::util::{self, child, rect, text_at};
use super::{Cx, Feed};
use crate::files::picture::Sheet;
use crate::tr;
use crate::ui::fonts::Type;
use crate::ui::icons::{self, Icon};
use crate::ui::paint::mix;
use crate::ui::paint::spectro::{self, FileShow, Lane, Sizes, Still};
use crate::ui::text;
use crate::ui::tokens::Theme;
use crate::ui::widgets::{self, Button};
use std::sync::Arc;

/// Under the row of controls under the picture (10 px above it:
/// [`replay::PAD_TOP`]).
const PLAYER_PAD: f32 = 12.0;
/// The empty page under the picture (App-Files3 `.s3-empty { gap: 10px;
/// padding: 4px 16px 20px }`): the button 4 px under it, the formats line
/// 10 px under the button, 20 px under the line.
const EMPTY_GAP: (f32, f32, f32) = (4.0, 10.0, 20.0);
/// What a file that never opened says under the picture: its gaps above
/// the words, between them and the buttons, and under those.
const FAILED_GAP: (f32, f32, f32) = (8.0, 10.0, 14.0);
/// The toolbar and the rows take what the rows need, up to this much of
/// the card's height (a longer list scrolls); the lanes take the rest.
const LIST_SHARE: f32 = 0.4;
/// The lanes are never lower than this: the list gives way to them, down
/// to this many rows in view.
const LANE_FLOOR: f32 = 56.0;
const LIST_ROWS_MIN: usize = 2;
/// The card's padding around the toolbar and the rows (`.hm-fx`): the
/// rows' boxes line up with the toolbar's edges, and the last row ends as
/// far above the card's border.
const LIST_PAD: f32 = 16.0;
pub struct FilesPage {
    feed: Feed,
    pub(crate) source: Option<Box<dyn Source>>,
    /// The one compare control: the hold button and page Space.
    hold: Hold,
    /// What the source was last told about the original.
    original_sent: bool,
    /// The hold button was pressed last frame (a new press while paused
    /// peeks, latched or not).
    pressed: bool,
    still: Still,
    /// The picture in the painter: whose, at which strength, in which
    /// theme.
    drawn: Option<(Arc<Sheet>, u32, Theme)>,
    levels: Vec<u8>,
    /// The selected file as last drawn.
    shown: Option<FileId>,
    /// Something is dragged over the window.
    drag: bool,
    /// Screenshots: the drag-over look without a drag.
    pin_drag: bool,
    /// Screenshots: open on this board state, `secs` into it.
    pub preset: Option<(String, f64)>,
    /// The toast after Clear all (or a row's ×): how many it took (and the
    /// name of one alone), and since when (−∞ once Undo was pressed: it
    /// doesn't come back for them).
    toast: Option<((usize, Option<String>), f64)>,
    /// The note shown last ([`View::note`]'s number), and since when.
    noted: Option<(u64, f64)>,
    /// "Choose files…" had the keyboard focus when it went (the first
    /// files came): "Add files…" takes it over (UX panel B21).
    focus_to_add: bool,
    /// The selected file whose row the list last brought into view.
    in_view: Option<FileId>,
}

/// A note about a drop shows this long (F4: about as long as the
/// toast, so a file that went is noticed).
const NOTE_FOR: f64 = 8.0;

/// The toast after Clear all shows this long (the daemon keeps the files
/// a little longer, [`crate::files::jobs::UNDO_FOR`]).
const TOAST_FOR: f64 = 8.0;

impl FilesPage {
    pub fn new(feed: Feed) -> FilesPage {
        FilesPage {
            feed,
            source: None,
            hold: Hold::default(),
            original_sent: false,
            pressed: false,
            still: Still::new(),
            drawn: None,
            levels: Vec::new(),
            shown: None,
            drag: false,
            pin_drag: false,
            preset: None,
            toast: None,
            noted: None,
            focus_to_add: false,
            in_view: None,
        }
    }

    fn source(&mut self, ctx: &egui::Context, now: f64) -> &mut Box<dyn Source> {
        let (feed, preset) = (self.feed, self.preset.clone());
        if self.source.is_none() {
            // the board's "Drag over" still: files held over the window
            self.pin_drag = preset.as_ref().is_some_and(|(n, _)| n == "drag-over");
        }
        self.source.get_or_insert_with(|| -> Box<dyn Source> {
            match (feed, preset) {
                (_, Some((name, secs))) => Box::new(demo::DemoFiles::preset(&name, now, secs)),
                // the daemon's jobs and the window's own player
                (Feed::Meter, None) => Box::new(daemon::DaemonFiles::new(util::wake(ctx))),
                (Feed::Demo, None) => Box::new(demo::DemoFiles::live()),
            }
        })
    }

    fn send(&mut self, c: Cmd) {
        if let Some(s) = self.source.as_mut() {
            s.send(c);
        }
    }

    /// The page was left: what plays pauses (a press in flight ends, the
    /// latch stays), and the picture's texture goes.
    pub fn hide(&mut self) {
        self.hold.end_presses();
        self.send(Cmd::Pause);
        self.still.forget();
        self.drawn = None;
        self.levels = Vec::new();
    }

    /// The window is minimised: playback goes on, as in a media player,
    /// and the source keeps up with it (the end of the file, what was
    /// heard, the daemon's jobs) without drawing.
    pub fn background(
        &mut self,
        state: &crate::appstate::AppState,
        link: &crate::window::Link,
        now: f64,
    ) {
        if let Some(src) = self.source.as_mut() {
            src.state(state, now);
            src.view(now);
            for a in src.actions() {
                link.send(a);
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx<'_>) {
        let ctx = ui.ctx().clone();
        let kit = crate::ui::kit(&ctx);
        let now = cx.now;
        let src = self.source(&ctx, now);
        src.state(cx.state, now);
        let v = src.view(now);
        let row = v.row().cloned();
        self.follow(row.as_ref());
        self.drops(&ctx);
        // the light at the top, as on every page while HushMic filters
        if super::status::filtering(cx.state) {
            cx.light.static_edge = true;
        }

        let (pt, _, pb) = if cx.compact {
            (20.0, 24.0, 24.0)
        } else {
            (28.0, 32.0, 32.0)
        };
        let page = ui.max_rect();
        let (x, w) = super::util::column(page, cx.compact);
        // no player on screen (a file that doesn't play): no page Space
        // either
        let playable = row.as_ref().is_some_and(playable);
        // page Space stays the page's on the time line and Play
        let keep = [ui.id().with("files-time"), ui.id().with("files-play")];
        if self
            .hold
            .keys(&ctx, ui.layer_id(), &keep, playable, HoldKind::Hold, now)
        {
            // a Space tap is the page's Play
            self.toggle_play(&v);
        }
        let mut y = page.top() + pt;
        let bottom = self.header(ui, &v, row.as_ref(), x, y, w);
        y = bottom + util::HEAD_GAP;
        y = self.figure(ui, cx, &v, row.as_ref(), x, y, w, page.bottom() - pb);
        util::reach(ui, page.min, page.width(), y + pb);
        self.toast(ui, cx, &v, x, w);
        self.sync(&v);
        if let Some(src) = self.source.as_mut() {
            for a in src.actions() {
                cx.link.send(a);
            }
        }

        let busy = self.source.as_ref().is_some_and(|s| s.busy());
        if v.play == Play::Playing {
            // the playhead moves with what is heard, every frame
            crate::ui::motion::moment_repaint(&ctx);
        } else if busy && v.play != Play::Paused {
            crate::ui::motion::live_repaint(&ctx);
        } else if busy {
            // only the pass runs: the cleaned lane grows a column at a time
            crate::ui::motion::repaint_in(&ctx, std::time::Duration::from_millis(100));
        }
        let _ = kit;
    }

    /// Keep the page's state in step with the source: a new file drops the
    /// latch.
    fn follow(&mut self, row: Option<&Row>) {
        let sel = row.map(|r| r.id);
        if sel != self.shown {
            self.shown = sel;
            self.hold.clear();
            self.hold.end_presses();
            // a new file plays HushMic (the source's side of Cmd::Select)
            self.original_sent = false;
        }
    }

    /// X11 drops (the drag-over look shows only where drops arrive).
    fn drops(&mut self, ctx: &egui::Context) {
        let (hovered, dropped) = ctx.input(|i| {
            (
                !i.raw.hovered_files.is_empty(),
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect::<Vec<_>>(),
            )
        });
        self.drag = hovered || self.pin_drag;
        if !dropped.is_empty() {
            self.send(Cmd::Add(dropped));
        }
    }

    fn toggle_play(&mut self, v: &View) {
        self.send(if v.play == Play::Paused || v.peek {
            Cmd::Play
        } else {
            Cmd::Pause
        });
    }

    /// Tell the source whether the original plays (the hold's rule is the
    /// page's). A new press while paused peeks, latched or not; a tap that
    /// latches during a peek keeps it playing.
    fn sync(&mut self, v: &View) {
        let pressed = self.hold.compare.held();
        let fresh_press = pressed && !self.pressed;
        self.pressed = pressed;
        let on = self.hold.compare.on;
        if on != self.original_sent || (fresh_press && v.play == Play::Paused) {
            self.original_sent = on;
            self.send(Cmd::Original(on));
        }
        if self.hold.compare.latched && !pressed && v.peek {
            self.send(Cmd::Play);
        }
    }

    // ---------------------------------------------------------------- header

    /// The header: the title alone (round 3b; Quit's question is the
    /// window's sheet). Returns where it ends.
    fn header(&mut self, ui: &mut Ui, v: &View, row: Option<&Row>, x: f32, y: f32, w: f32) -> f32 {
        let ctx = ui.ctx().clone();
        let title = copy(v, row, self.hold.compare.latched, self.drag);
        // "Save all and quit" waits: Cancel keeps HushMic (A104 SF-Q1)
        if v.quitting.is_some() {
            let label = tr!("files-quit-cancel");
            let b = Button::secondary(&label);
            let bw = b.width(&ctx);
            let head = util::head(ui, &title, x, y, w, bw);
            let bx = if head.stacked {
                head.acts.left()
            } else {
                head.acts.right() - bw
            };
            let r = util::put(
                ui,
                rect(bx, head.acts.top(), bw, 40.0),
                "files-quit-cancel",
                b,
            );
            if r.clicked() {
                self.send(Cmd::CancelQuit);
            }
            return head.bottom;
        }
        // the call's sentence is whole over two lines where one doesn't
        // hold it (A104 N-Y1): what to expect is its second half
        if title == paused_line(v) && !fits(&ctx, &title, w) {
            let pal = crate::ui::kit(&ctx).pal;
            let block = text::layout_rows(&ctx, &title, Type::H, pal.ink, w, 2);
            let h = block.size().y;
            util::text_as(ui, block, pos2(x, y), util::Speak::LiveHeading(1));
            return y + h.max(util::HEAD_H);
        }
        util::head(ui, &title, x, y, w, 0.0).bottom
    }

    // ---------------------------------------------------------------- figure

    #[allow(clippy::too_many_arguments)]
    fn figure(
        &mut self,
        ui: &mut Ui,
        cx: &mut Cx<'_>,
        v: &View,
        row: Option<&Row>,
        x: f32,
        y: f32,
        w: f32,
        bottom: f32,
    ) -> f32 {
        let ctx = ui.ctx().clone();
        let kit = crate::ui::kit(&ctx);
        let pal = kit.pal;
        let compact = cx.compact;
        let inner_x = x + 1.0;
        let inner_w = w - 2.0;
        let p = ui.painter().clone();
        // the card's fill, under everything in it (UX panel D1); its size
        // is known at the end
        let fill_at = p.add(egui::Shape::Noop);

        let file_row = row.filter(|r| r.length > 0.0);
        // a file that never opened and needs attention (F15)
        let failed_row = row.filter(|r| r.length <= 0.0 && r.attention().is_some());
        // under the picture: the row of controls, or what a file that
        // never opened says instead
        let plain = replay::PAD_TOP + replay::ROW_H + PLAYER_PAD;
        let controls = file_row.map(|r| self.controls(&ctx, v, r, inner_w));
        let player_h = match (failed_row, &controls) {
            (Some(r), _) => failed_height(&ctx, r, inner_w - 32.0).max(plain),
            (None, Some(c)) => c.h,
            (None, None) => plain,
        };
        // ---- the picture: its lanes as high as the page and the list
        // leave them
        let room = bottom - y - 2.0;
        let (lane, rows) = if v.files.is_empty() {
            let under = EMPTY_GAP.0 + 40.0 + EMPTY_GAP.1 + Type::CAP.line + EMPTY_GAP.2;
            let lane = Sizes::lane_for(compact, room - under).min(spectro::LANE_MAX);
            (lane, 0)
        } else {
            share(compact, room, player_h, v.files.len())
        };
        let pic = rect(inner_x, y + 1.0, inner_w, Sizes::of(compact, lane).height());
        let top_h = pic.height() + player_h;
        let lanes = self.picture(ui, cx, v, row, pic, lane);
        if let Some(r) = file_row {
            self.seek(ui, v, r, lanes);
        }

        let cy = pic.bottom();
        // the empty page: the button and what opens, in the middle of the
        // room the card has left (the whole card takes a drop)
        let empty_top = {
            let block = 40.0 + EMPTY_GAP.1 + Type::CAP.line;
            cy + ((bottom - 1.0 - cy - block) / 2.0).max(EMPTY_GAP.0)
        };
        // the figure's one white button: "Choose files…" on the empty
        // page (a file's next step is on its row: round 3)
        let cta = match row {
            None => Some((tr!("files-choose"), Cmd::Choose)),
            Some(_) => None,
        };
        match (file_row, cta) {
            (_, Some((label, cmd))) => {
                let b = Button::primary(&label);
                let bw = b.width(&ctx);
                let r = util::put(
                    ui,
                    rect(x + (w - bw) / 2.0, empty_top, bw, 40.0),
                    "files-cta",
                    b,
                );
                self.focus_to_add = r.has_focus();
                if r.clicked() {
                    self.send(cmd);
                }
            }
            (None, None) => {
                if let Some(r) = failed_row {
                    self.failed_card(ui, r, inner_x + 16.0, cy, inner_w - 32.0);
                }
            }
            (Some(r), None) => {
                if let Some(c) = controls {
                    self.player_row(ui, cx, v, r, inner_x, cy, c);
                }
            }
        }

        // ---- the files: the toolbar and the rows; the card ends after
        // them (the page is as tall as its content). With no file, no
        // toolbar and no strength yet (round 3): the one way in, "Choose
        // files…", and what opens under it
        let end = if v.files.is_empty() {
            let t = text::layout(
                &ctx,
                &tr!("files-formats"),
                Type::CAP,
                pal.ink_subtle,
                inner_w - 2.0 * LIST_PAD,
            );
            let tw = t.size().x;
            let ty = empty_top + 40.0 + EMPTY_GAP.1;
            let text = text_at(ui, t, pos2(x + (w - tw) / 2.0, ty)).bottom();
            text + (empty_top - cy).min(EMPTY_GAP.2)
        } else {
            self.files_area(ui, cx, v, inner_x, y + 1.0 + top_h, inner_w, rows)
        };
        // the card reaches the page's end at every size
        let end = end.max(bottom - 1.0);
        let fig = Rect::from_min_max(pos2(x, y), pos2(x + w, end + 1.0));
        p.set(fill_at, widgets::card_fill_shape(fig, &pal));
        // the frame: the card's edge; white when something is dragged over
        // (never cyan)
        if self.drag {
            p.rect_stroke(
                fig,
                crate::ui::tokens::radius::MD,
                egui::Stroke::new(1.0_f32, pal.ink),
                egui::StrokeKind::Inside,
            );
        } else {
            widgets::card_edge(&p, fig, &pal);
        }
        frame_light(&p, &pal, fig, self.drag, kit.dark());
        fig.bottom()
    }

    /// Under the picture, for a file that never opened: what happened and
    /// what to do, and its way off the page or to its folder (F15).
    fn failed_card(&mut self, ui: &mut Ui, r: &Row, x: f32, y: f32, w: f32) {
        let ctx = ui.ctx().clone();
        let pal = crate::ui::kit(&ctx).pal;
        let Some(a) = r.attention() else {
            return;
        };
        let help = attention(r, a).1;
        let t = text::layout(&ctx, &help, Type::CAP, pal.ink_muted, w);
        let s = t.size();
        let ty = text_at(ui, t, pos2(x + (w - s.x) / 2.0, y + FAILED_GAP.0)).bottom();
        let remove = tr!("files-remove");
        let show = tr!("files-show-in-folder");
        let (b1, b2) = (
            Button::secondary(&remove).sm(),
            Button::secondary(&show).sm().icon(Icon::Folder),
        );
        let (w1, w2) = (b1.width(&ctx), b2.width(&ctx));
        let bx = x + (w - (w1 + 8.0 + w2)) / 2.0;
        let by = ty + FAILED_GAP.1;
        let r1 = util::put(
            ui,
            rect(bx, by, w1, 32.0),
            ("files-failed-remove", r.id),
            b1,
        );
        let label = tr!("files-remove-name", name = r.name.clone());
        ctx.accesskit_node_builder(r1.id, |n| n.set_label(label));
        if r1.clicked() {
            self.send(Cmd::Remove(r.id));
        }
        let r2 = util::put(
            ui,
            rect(bx + w1 + 8.0, by, w2, 32.0),
            ("files-failed-show", r.id),
            b2,
        );
        if r2.clicked() {
            self.send(Cmd::ShowInFolder(r.id));
        }
    }

    /// The selected file's picture into `pic`: the original above, the
    /// cleaned sound below at the page's strength, the playhead where the
    /// file is; while it plays, the lane that is heard at full strength.
    /// Returns the two lanes.
    fn picture(
        &mut self,
        ui: &mut Ui,
        cx: &Cx<'_>,
        v: &View,
        row: Option<&Row>,
        pic: Rect,
        lane: f32,
    ) -> [Rect; 2] {
        let ctx = ui.ctx().clone();
        let kit = crate::ui::kit(&ctx);
        let file = row.filter(|r| r.length > 0.0);
        let sheet = file.and(self.source.as_deref()).and_then(|s| s.picture());
        match &sheet {
            Some(sheet) => {
                // the levels at this strength, when any of it changed
                let a = v.strength.alpha().to_bits();
                let same = self.drawn.as_ref().is_some_and(|(was, k, theme)| {
                    Arc::ptr_eq(was, sheet) && *k == a && *theme == kit.pal.theme
                });
                if !same {
                    sheet.levels(v.strength.alpha(), &mut self.levels);
                    self.still.set(&ctx, &self.levels);
                    self.drawn = Some((sheet.clone(), a, kit.pal.theme));
                }
            }
            None => self.drawn = None,
        }
        // the lane that is heard stands out; the sides cross over
        let side = ctx.animate_bool_with_time_and_easing(
            ui.id().with("files-pic-side"),
            v.original,
            if kit.reduced_motion { 0.0 } else { 0.18 },
            egui::emath::easing::cubic_in_out,
        );
        let covered = match (&sheet, file) {
            (Some(s), Some(r)) => (s.cols() as f64 * s.column_secs() / r.length) as f32,
            _ => 0.0,
        };
        // why a lane is empty, in it: a file that didn't open, one that
        // waits its turn
        let note = match row {
            Some(r) if file.is_none() => {
                r.attention().map(|a| (Some(Icon::Warn), attention(r, a).2))
            }
            Some(r) if sheet.is_none() && r.pass == Pass::Waiting => {
                Some((None, tr!("files-row-waiting")))
            }
            Some(_) => None,
            // no file yet: what will be here
            None => Some((None, tr!("files-lane-original-empty"))),
        };
        let glyph = match row.map(|r| r.kind) {
            Some(Kind::Video) => Icon::FileFilm,
            _ => Icon::FileSound,
        };
        // the lower lane's caption says how far the cleaned copy is: its
        // strength once it is cleaned, until then the pass's words (the
        // short ones where the lane is too narrow for the long)
        let progress = file.and_then(|r| cleaning_line(v, r));
        let room =
            pic.width() - 2.0 * (Sizes::of(cx.compact, lane).pad_x + spectro::INSET.0) - 24.0;
        let (cleaned, strength) = match (&progress, file) {
            (Some(pr), _) => {
                let fits = text::width(&ctx, &pr.one, Type::CAP) <= room;
                match &pr.two {
                    Some((short, _)) if !fits => (short.clone(), String::new()),
                    _ => (pr.one.clone(), String::new()),
                }
            }
            (None, Some(r)) if r.pass == Pass::Cleaned => {
                (tr!("files-lane-cleaned"), r.strength.word())
            }
            _ => (tr!("files-lane-cleaned"), String::new()),
        };
        let show = FileShow {
            original: Lane {
                glyph,
                label: tr!("files-lane-original"),
                name: row.map(|r| r.name.clone()).unwrap_or_default(),
                strong: true,
                select: None,
                tail: 0.0,
                sound: sheet.is_some(),
                note,
                shut: false,
            },
            cleaned: Lane {
                glyph,
                label: cleaned,
                name: strength,
                strong: true,
                select: None,
                tail: 0.0,
                sound: sheet.is_some(),
                note: row
                    .is_none()
                    .then(|| (None, tr!("files-lane-cleaned-empty"))),
                shut: false,
            },
            covered,
            // a rough guess at the length isn't shown until it is counted
            clock: file.map(|r| match clock_or_none(r) {
                total if total.is_empty() => clock(v.position),
                total => tr!("files-time", at = clock(v.position), total = total),
            }),
            // at rest at the start there is nothing to mark (the time
            // says 0:00)
            position: file
                .filter(|_| v.play != Play::Paused || v.position > 0.0)
                .map(|r| (v.position / r.length) as f32),
            heard: (v.play != Play::Paused).then_some(side),
            playing_label: tr!("live-lane-playing"),
            compact: cx.compact,
            lane,
        };
        let painted = self.still.paint(&ctx, ui.painter(), pic, &show);
        // a name cut short is whole on hover
        for (i, (at, cut)) in painted.names.into_iter().enumerate() {
            if cut {
                let name = [&show.original.name, &show.cleaned.name][i];
                ui.interact(at, ui.id().with(("files-pic-name", i)), Sense::hover())
                    .on_hover_text(name);
            }
        }
        painted.lanes
    }

    /// The picture is the file's time line: a click or a drag on either
    /// lane seeks, and so do the arrow keys.
    fn seek(&mut self, ui: &mut Ui, v: &View, r: &Row, lanes: [Rect; 2]) {
        let ctx = ui.ctx().clone();
        let kit = crate::ui::kit(&ctx);
        let area = lanes[0].union(lanes[1]);
        let id = ui.id().with("files-time");
        let resp = ui.interact(area, id, Sense::click_and_drag());
        let len = r.length;
        let at = clock(v.position);
        // a rough guess at the length isn't shown until it is counted
        let total = clock_or_none(r);
        let value = if total.is_empty() {
            at
        } else {
            tr!("files-position-value", at = at, total = total)
        };
        let label = tr!("files-position");
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Slider, true, &label));
        let about = tr!("files-picture", name = r.name.clone());
        ctx.accesskit_node_builder(id, |n| {
            n.set_description(about);
            n.set_value(value.clone());
            n.set_numeric_value(v.position.floor());
            n.set_min_numeric_value(0.0);
            n.set_max_numeric_value(len.round());
        });
        let to_secs =
            |px: f32| f64::from(((px - area.left()) / area.width()).clamp(0.0, 1.0)) * len;
        if let Some(pt) = resp.interact_pointer_pos() {
            if resp.drag_started() || resp.clicked() || resp.dragged() {
                if resp.drag_started() || resp.clicked() {
                    resp.request_focus();
                }
                let drag = resp.dragged() && !resp.drag_started();
                let to = to_secs(pt.x);
                if (to - v.position).abs() > 1e-3 {
                    self.send(Cmd::Seek { to, drag });
                }
            }
        }
        if resp.has_focus() {
            // a slider's keys: the arrows seek (5 s; Up and Down as Right
            // and Left), never move the focus
            let filter = egui::EventFilter {
                horizontal_arrows: true,
                vertical_arrows: true,
                ..Default::default()
            };
            ui.memory_mut(|m| m.set_focus_lock_filter(id, filter));
            let step = ctx.input_mut(|i| {
                let mut key = |k| i.consume_key(egui::Modifiers::NONE, k);
                use egui::Key;
                if key(Key::ArrowLeft) || key(Key::ArrowDown) {
                    Some(v.position - 5.0)
                } else if key(Key::ArrowRight) || key(Key::ArrowUp) {
                    Some(v.position + 5.0)
                } else if key(Key::PageDown) {
                    Some(v.position - 30.0)
                } else if key(Key::PageUp) {
                    Some(v.position + 30.0)
                } else if key(Key::Home) {
                    Some(0.0)
                } else if key(Key::End) {
                    Some(len)
                } else {
                    None
                }
            });
            if let Some(to) = step {
                self.send(Cmd::Seek {
                    to: to.clamp(0.0, len),
                    drag: false,
                });
            }
        }
        if widgets::shows_focus(&kit, &resp) {
            widgets::focus_ring(ui.painter(), area.expand(2.0), 8.0, kit.pal.focus_ring());
        }
    }

    /// The row of controls under a file's picture, measured in `w`
    /// (inside the card's border): Live's row ([`replay::row_plan`]) with
    /// the file's strength leading it and Play and Hold ending it; Play's
    /// short form is its icon alone. A recording with more than one
    /// track has a line under the row: the track that is cleaned, at the
    /// row's right end, or what Save can't keep, from its left.
    fn controls(&self, ctx: &egui::Context, v: &View, r: &Row, w: f32) -> Controls {
        let pal = crate::ui::kit(ctx).pal;
        let room = w - 2.0 * replay::PAD_X;
        let leads = (steps_width(ctx, 10.0), steps_width(ctx, 6.0));
        let hold = tr!("mic-hold");
        let hw = self
            .hold
            .look(&hold, playable(r), v.original)
            .sm()
            .width(ctx);
        let plan = replay::row_plan(room, leads, (play_width(ctx), replay::ROW_H), hw);
        let pad = if plan.lead + 0.5 < leads.0 { 6.0 } else { 10.0 };
        // all its tracks kept (F1): which one is cleaned, and a menu to
        // clean another, under its long label (A109) where the row is
        // wide enough for that
        let picker = (r.tracks > 1 && r.dropped == 0).then(|| {
            let full = cleans_track(r);
            let fw = Button::secondary(&full)
                .sm()
                .icon_after(Icon::Chevron)
                .width(ctx);
            if fw <= room {
                full
            } else {
                let n = (r.track + 1) as u64;
                tr!("files-track", n = n)
            }
        });
        let words = row_words(ctx, r, room, pal);
        let under = match (&picker, &words) {
            (Some(_), _) => replay::GAP_Y + replay::ROW_H,
            (None, Some(w)) => replay::STATUS_GAP + w.height(),
            (None, None) => 0.0,
        };
        let rows = if plan.wrap {
            2.0 * replay::ROW_H + replay::GAP_Y
        } else {
            replay::ROW_H
        };
        Controls {
            plan,
            pad,
            picker,
            words,
            h: replay::PAD_TOP + rows + under + PLAYER_PAD,
        }
    }

    /// The row of controls from `y` (the picture's bottom) down, from `x`
    /// (inside the card's border): the strength · Play · Hold to hear the
    /// original (that Space holds too is Hold's tooltip), and what
    /// [`FilesPage::controls`] found to say about the file's tracks.
    #[allow(clippy::too_many_arguments)]
    fn player_row(
        &mut self,
        ui: &mut Ui,
        cx: &mut Cx<'_>,
        v: &View,
        r: &Row,
        x: f32,
        y: f32,
        c: Controls,
    ) {
        let ctx = ui.ctx().clone();
        let Controls {
            plan, pad, picker, ..
        } = c;
        let ok = playable(r);
        let left = x + replay::PAD_X;
        let top = y + replay::PAD_TOP;
        const H: f32 = replay::ROW_H;
        // the strength: this file's, and the next ones'
        let labels = step_labels();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let name = tr!("files-strength-name");
        let before = v.strength.index();
        let mut idx = before;
        let mut steps = child(ui, rect(left, top, plan.lead, H), "files-strength");
        let group = widgets::Strength::new(&name, &mut idx, &refs)
            .dense()
            .fitted(pad, false)
            .show(&mut steps);
        // what the strength does is its tooltip (and the group's
        // description, for screen readers)
        let tip = strength_tip(v.strength, v.files.len() > 1);
        let said = tip.clone();
        ctx.accesskit_node_builder(group.id, |n| n.set_description(said));
        group.on_hover_text(tip);
        if idx != before && idx < 4 {
            self.send(Cmd::Strength(model::Strength::ALL[idx]));
        }
        // Play / Pause and the hold end the row, or stand under the
        // strength where the row is too narrow for all three
        let by = top + if plan.wrap { H + replay::GAP_Y } else { 0.0 };
        let playing = v.play != Play::Paused && !v.peek;
        let (play_label, icon) = if playing {
            (tr!("files-pause"), Icon::Pause)
        } else {
            (tr!("files-play"), Icon::Play)
        };
        let (at, pw) = plan.first;
        let b = Button::secondary(&play_label).sm().enabled(ok);
        let b = if plan.short {
            b.icon_only(icon)
        } else {
            b.icon(icon).min_width(pw)
        };
        let resp = util::put(ui, rect(left + at, by, pw, H), "files-play", b);
        let clicked = ok && resp.clicked();
        if plan.short && ok {
            resp.on_hover_text(play_label.as_str());
        }
        if clicked {
            self.toggle_play(v);
        }
        // the hold; what a press does, and that Space does the same, is
        // its tooltip and its description
        let label = tr!("mic-hold");
        let (at, hw) = plan.second;
        let look = self.hold.look(&label, ok, v.original).sm().min_width(hw);
        let desc = format!("{} {}", tr!("mic-hold-desc"), tr!("mic-hold-space"));
        let (hr, latched) = self.hold.button(
            ui,
            rect(left + at, by, hw, H),
            "files-hold",
            look,
            desc.clone(),
            ok,
            HoldKind::Hold,
            cx.now,
        );
        if ok {
            hr.on_hover_text(desc);
        }
        if latched && v.play == Play::Paused {
            // it plays, latched, as a tap's press does
            self.original_sent = true;
            self.send(Cmd::Original(true));
            self.send(Cmd::Play);
        }
        let end = left + plan.second.0 + plan.second.1;
        let under = by + H;
        if let Some(label) = picker {
            let b = Button::secondary(&label).sm().icon_after(Icon::Chevron);
            let bw = b.width(&ctx);
            let at = rect(end - bw, under + replay::GAP_Y, bw, H);
            let resp = util::put(ui, at, "files-track", b);
            let name = tr!("files-track-change");
            let value = cleans_track(r);
            ctx.accesskit_node_builder(resp.id, |n| {
                n.set_label(name);
                n.set_value(value);
            });
            let names: Vec<String> = (0..r.tracks).map(|k| track_name(r, k)).collect();
            let items: Vec<widgets::select::MenuItem<'_>> = names
                .iter()
                .enumerate()
                .map(|(k, n)| widgets::select::MenuItem {
                    label: n,
                    icon: (k == r.track).then_some(Icon::Tick),
                    separator: false,
                    hint: None,
                })
                .collect();
            if let Some(k) = widgets::select::menu(ui, &resp, &items) {
                if k != r.track {
                    self.send(Cmd::Track(r.id, k));
                }
            }
        }
        if let Some(words) = c.words {
            let mut y = under + replay::STATUS_GAP;
            for p in words.lines {
                let h = p.size().y;
                text_at(ui, p, pos2(left, y));
                y += h;
            }
        }
    }

    // ---------------------------------------------------------------- files

    /// Under the player, once there is a file: the toolbar (Add files…,
    /// Save all, Clear all) and one row per file (App-Files2). `top`: the
    /// area's border line; `rows`: how many rows the list shows at once
    /// ([`share`]; more scroll). Returns the area's end (the card's
    /// border goes under it).
    #[allow(clippy::too_many_arguments)]
    fn files_area(
        &mut self,
        ui: &mut Ui,
        cx: &mut Cx<'_>,
        v: &View,
        x: f32,
        top: f32,
        w: f32,
        rows: usize,
    ) -> f32 {
        let ctx = ui.ctx().clone();
        let kit = crate::ui::kit(&ctx);
        let pal = kit.pal;
        ui.painter()
            .rect_filled(rect(x, top, w, 1.0), 0.0, pal.line);
        // `.hm-fx`: padding 12 16 16
        let (bx, bw) = (x + LIST_PAD, w - 2.0 * LIST_PAD);
        let by = top + 1.0 + 12.0;
        let many = v.files.len() > 1;
        let b = Batch::of(v);

        // ---- the toolbar: the batch buttons
        let add = tr!("files-add");
        let save_all = tr!("files-save-all");
        let clear = tr!("files-clear-all");
        let mut acts: Vec<(Button<'_>, &str, Option<Cmd>)> = vec![(
            Button::secondary(&add).sm().icon(Icon::Plus),
            "files-add",
            Some(Cmd::Choose),
        )];
        // With two files or more, Save all and Clear all, in slots that
        // never move: Save all is off when there is nothing left to save,
        // never gone (a click meant for it never lands on Clear all).
        if many {
            // one look whatever is left to save: the list's buttons are
            // all the outlined kind (a white one came and went with the
            // number of files)
            let s = Button::secondary(&save_all);
            let cmd = b.save_all.then_some(Cmd::SaveAll);
            acts.push((s.sm().enabled(b.save_all), "files-save-all", cmd));
            // at once, no question: undoable for a moment (the toast)
            let c = Button::secondary(&clear).sm();
            acts.push((c, "files-clear", Some(Cmd::ClearAll)));
        }
        const BAR: f32 = replay::ROW_H;
        let bar_name = if many {
            tr!("files-bar-all")
        } else {
            tr!("files-files")
        };
        let bar = ui.interact(
            rect(bx, by, bw, BAR),
            ui.id().with("files-bar"),
            Sense::hover(),
        );
        bar.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, &bar_name));
        let mut ax = bx;
        let mut clicked = None;
        for (button, salt, cmd) in acts {
            let bw = button.width(&ctx);
            let r = util::put(ui, rect(ax, by, bw, BAR), salt, button);
            if salt == "files-add" && std::mem::take(&mut self.focus_to_add) {
                r.request_focus();
            }
            if r.clicked() {
                clicked = Some((salt, cmd));
            }
            ax += bw + 6.0;
        }
        if let Some((_, Some(cmd))) = clicked {
            self.send(cmd);
        }

        // ---- the list, clear of the toolbar (round 3b)
        let ly = by + BAR + LIST_GAP;
        let max_h = rows.max(1) as f32 * ROW_H;
        // the scroll bar in the card's right padding, not over the rows
        let h = self.list(ui, cx, v, bx, ly, bw, x + w - 4.0, max_h);
        ly + h + LIST_PAD
    }

    /// The toast after Clear all, over the page's foot: "3 files cleared
    /// · Undo", for [`TOAST_FOR`] seconds while the daemon can still
    /// bring them back. `x`, `w`: the page's column.
    fn toast(&mut self, ui: &mut Ui, cx: &Cx<'_>, v: &View, x: f32, w: f32) {
        let now = cx.now;
        let key = (v.cleared, v.cleared_name.clone());
        match &self.toast {
            _ if v.cleared == 0 => self.toast = None,
            Some((k, _)) if *k == key => {}
            _ => self.toast = Some((key, now)),
        }
        match (&v.note, self.noted) {
            (Some((_, k)), Some((seen, _))) if *k == seen => {}
            (Some((_, k)), _) => self.noted = Some((*k, now)),
            (None, _) => {}
        }
        // Clear all's (with Undo) first, else a drop's note
        let cleared = self
            .toast
            .clone()
            .filter(|(_, since)| now - since < TOAST_FOR)
            .map(|((n, name), since)| {
                let words = match name {
                    Some(name) => tr!("files-removed", name = name),
                    None => tr!("files-cleared", count = n),
                };
                (words, true, TOAST_FOR - (now - since))
            });
        let noted = || {
            let (text, _) = v.note.as_ref()?;
            let (_, since) = self.noted?;
            (now - since < NOTE_FOR).then(|| (text.clone(), false, NOTE_FOR - (now - since)))
        };
        let Some((words, undoable, left)) = cleared.or_else(noted) else {
            return;
        };
        let key = self.toast.as_ref().map(|t| t.0.clone());
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
        let kit = crate::ui::kit(&ctx);
        let pal = kit.pal;
        let undo = tr!("files-undo");
        let room = if undoable { w - 120.0 } else { w - 40.0 };
        let t = text::layout(&ctx, &words, Type::LIST, pal.ink, room);
        let b = Button::secondary(&undo).sm();
        let (tw, th) = (t.size().x, t.size().y);
        let bw = if undoable { b.width(&ctx) } else { 0.0 };
        let (pad, gap, h) = (14.0, 12.0, 44.0_f32.max(th + 16.0));
        let tw_all = if undoable {
            pad + tw + gap + bw + 8.0
        } else {
            pad + tw + pad
        };
        let bottom = ui.clip_rect().bottom().min(ctx.content_rect().bottom());
        let at = pos2(x + (w - tw_all) / 2.0, bottom - 24.0 - h);
        let area = rect(at.x, at.y, tw_all, h);
        egui::Area::new(ui.id().with("files-toast"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            .show(&ctx, |ui| {
                let p = ui.painter();
                p.rect(
                    area,
                    10.0,
                    pal.surface_raised,
                    egui::Stroke::new(1.0_f32, pal.line_strong),
                    egui::StrokeKind::Inside,
                );
                // a polite status for screen readers, named by its words,
                // Undo in it
                let mut c = widgets::a11y_children(ui, area, egui::accesskit::Role::Status, &words);
                widgets::a11y(ui.ctx(), c.unique_id(), |n| {
                    n.set_live(egui::accesskit::Live::Polite);
                });
                util::paint_at(&c, t, pos2(area.left() + pad, area.center().y - th / 2.0));
                let br = rect(area.right() - 8.0 - bw, area.center().y - 16.0, bw, 32.0);
                if undoable && util::put(&mut c, br, "files-undo", b).clicked() {
                    if let Some(k) = key.clone() {
                        self.toast = Some((k, f64::NEG_INFINITY));
                    }
                    self.send(Cmd::UndoClear);
                }
            });
    }

    /// One row per file ([`ROW_H`] high, a hairline between), `w` wide from
    /// `x`; past `max_h` (whole rows) it scrolls inside, its bar at
    /// `bar_right` (in the card's padding). Returns its height.
    #[allow(clippy::too_many_arguments)]
    fn list(
        &mut self,
        ui: &mut Ui,
        cx: &mut Cx<'_>,
        v: &View,
        x: f32,
        y: f32,
        w: f32,
        bar_right: f32,
        max_h: f32,
    ) -> f32 {
        let ctx = ui.ctx().clone();
        let kit = crate::ui::kit(&ctx);
        let pal = kit.pal;
        let many = v.files.len() > 1;
        let looks: Vec<Look> = v
            .files
            .iter()
            .map(|r| {
                let offer_next = many && v.next_up != Some(r.id);
                row_look(r, many, offer_next, &paused_line(v))
            })
            .collect();
        // one action column for every row, as wide as the widest action
        // shown; the names give way
        let act_w = looks
            .iter()
            .filter_map(|l| l.act.as_ref())
            .map(|a| act_width(&ctx, a))
            .fold(QUIET, f32::max);
        // and one state column, as wide as the widest state shown: the
        // names end where it starts, and take the room it leaves (F15)
        // (a wide list lets a long state be read whole: "Not saved: the
        // folder can't be written" was cut with half the row empty)
        let most = if cx.compact {
            STATUS_W_COMPACT
        } else {
            STATUS_W.max(0.4 * w)
        };
        let status_w = looks
            .iter()
            .map(|l| status_width(&ctx, l))
            .fold(STATUS_W_MIN, f32::max)
            .min(most);
        let content = v.files.len() as f32 * ROW_H;
        let h = content.min(max_h);
        // the rows' column, and the scroll area out to the bar
        let area = rect(x, y, w, h);
        let scroll = Rect::from_min_max(area.min, pos2(bar_right.max(area.right()), area.bottom()));
        let mut c = child(ui, scroll, "files-list");
        c.set_clip_rect(scroll.intersect(ui.clip_rect()));
        let mut out: Vec<(FileId, Option<Cmd>, bool)> = Vec::new();
        let mut ring = None;
        // another file is selected (or the first one): its row comes into
        // view where the list is longer than it shows
        let bring = std::mem::replace(&mut self.in_view, v.selected) != v.selected;
        // no drag-to-scroll: the list would be an unnamed Tab stop
        let out_list = egui::ScrollArea::vertical()
            .scroll_source(egui::scroll_area::ScrollSource {
                drag: false,
                ..egui::scroll_area::ScrollSource::ALL
            })
            .id_salt("files-list")
            // the area keeps its full width, so its floating bar sits in
            // the card's padding past the rows (shrunk to the rows, the
            // bar lay on their last 4 px)
            .auto_shrink([false, true])
            .max_height(area.height())
            .show(&mut c, |ui| {
                ui.set_max_width(w);
                ui.spacing_mut().item_spacing.y = 0.0;
                // the hairline above a row: not on the first, nor next to
                // a filled (selected or hovered) row
                let mut rule = false;
                for (r, look) in v.files.iter().zip(looks) {
                    let selected = v.selected == Some(r.id);
                    let o = file_row(ui, r, &look, selected, (status_w, act_w), rule, &kit);
                    rule = !o.filled;
                    if selected && bring {
                        o.pick
                            .scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
                    }
                    if o.pick.clicked() {
                        out.push((r.id, None, false));
                    }
                    if o.act.is_some_and(|a| a.clicked()) {
                        out.push((r.id, look.act.map(|a| a.cmd), false));
                    }
                    if o.remove.is_some_and(|x| x.clicked()) {
                        out.push((r.id, None, true));
                    }
                    if widgets::shows_focus(&kit, &o.pick) {
                        ring = Some(o.pick.rect);
                    }
                }
            });
        // the list's bar shows when rows are hidden: a named keyboard control
        widgets::scroll::bar(&c, &out_list, &tr!("files-files"));
        if let Some(r) = ring {
            // the kit's ring, inside the list's clip
            let p = ui
                .painter()
                .with_clip_rect(area.expand(4.0).intersect(ui.clip_rect()));
            widgets::focus_ring(&p, r.intersect(area.expand(2.0)), 6.0, pal.focus_ring());
        }
        for (id, cmd, remove) in out {
            match (cmd, remove) {
                (_, true) => self.send(Cmd::Remove(id)),
                (Some(c), _) => self.send(c),
                (None, _) if v.selected != Some(id) => self.send(Cmd::Select(id)),
                _ => {}
            }
        }
        h
    }
}

/// The files Quit would stop: those cleaning or waiting their turn, and
/// those saving.
pub(super) fn quit_count(s: &crate::appstate::AppState) -> usize {
    use crate::appstate::JobState;
    s.files
        .jobs
        .iter()
        .filter(|j| matches!(j.state, JobState::Waiting | JobState::Cleaning) || j.saving)
        .count()
}

/// The files Quit would lose: those it stops, and those cleaned but not
/// saved (the daemon's `Jobs::at_stake`).
pub(crate) fn at_stake(s: &crate::appstate::AppState) -> usize {
    quit_count(s) + s.files.jobs.iter().filter(|j| j.unsaved()).count()
}

/// The quit sheet's sentences about files: what Quit stops, what it
/// loses, and that they clean again next time ("1 file is still
/// cleaning. 2 cleaned files aren't saved. Files that aren't saved…");
/// with nothing left, that last sentence alone. And whether any still
/// cleans (the safe answer is then Keep cleaning).
pub(super) fn quit_line(s: &crate::appstate::AppState) -> (String, bool) {
    let busy = quit_count(s);
    let unsaved = s.files.jobs.iter().filter(|j| j.unsaved()).count();
    let mut line = Vec::new();
    if busy > 0 {
        line.push(tr!("quit-ask-files", count = busy));
    }
    if unsaved > 0 {
        line.push(tr!("files-quit-unsaved", count = unsaved));
    }
    line.push(tr!("files-sub-quit"));
    (line.join(" "), busy > 0)
}

/// Whether the file can play (it opened and it is where it was).
fn playable(r: &Row) -> bool {
    r.length > 0.0 && !r.attention().is_some_and(Reason::unplayable)
}

/// How far, as the row says it ("42%"): whole percents, 100 only when
/// done.
fn percent(f: f32) -> String {
    let n = (f.clamp(0.0, 1.0) * 100.0).floor() as i64;
    tr!("files-percent", n = n.to_string())
}

/// Time left in words: whole minutes (a minute or more left).
fn minutes(secs: f64) -> String {
    tr!(
        "files-about-minutes",
        n = ((secs / 60.0).round() as i64).max(1).to_string()
    )
}

/// The header: what happens (title) and what to do (the second line).
pub fn copy(v: &View, row: Option<&Row>, latched: bool, drag: bool) -> String {
    let title = headline(v, row, latched, drag);
    match row {
        Some(r) => no_stop_after(title, &[&r.name, &r.output, &r.saved_name]),
        None => title,
    }
}

/// A headline that ends in a file's name keeps no full stop after its
/// extension ("Cleaning talk.mp3", not "talk.mp3."; F17).
fn no_stop_after(title: String, names: &[&str]) -> String {
    let ext = |n: &str| {
        std::path::Path::new(n)
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
    };
    let ends = title
        .strip_suffix('.')
        .filter(|t| names.iter().filter_map(|n| ext(n)).any(|e| t.ends_with(&e)));
    match ends {
        Some(t) => t.to_string(),
        None => title,
    }
}

fn headline(v: &View, row: Option<&Row>, latched: bool, drag: bool) -> String {
    if let Some(n) = v.quitting {
        return tr!("files-title-quitting", count = n);
    }
    let Some(r) = row else {
        return if drag {
            tr!("files-title-drag")
        } else {
            tr!("files-title-empty")
        };
    };
    let playing = v.play != Play::Paused;
    let done = r.pass == Pass::Cleaned;
    if let Some(a) = r.attention() {
        return attention(r, a).0;
    }
    let name = r.name.clone();
    if v.original && playing {
        return tr!("files-title-original");
    }
    if latched && !playing {
        return tr!("files-title-original-on");
    }
    if v.play == Play::CaughtUp {
        // a pass that gets no CPU isn't ahead of anything
        return if r.stalled {
            tr!("files-title-caught-up-stalled")
        } else {
            tr!("files-title-caught-up")
        };
    }
    if playing {
        return tr!("files-title-playing");
    }
    // another file's Save failed: said up top, not only on its row
    let failed = v
        .files
        .iter()
        .find_map(|o| Some((o, o.attention().filter(|a| a.save_only())?)));
    if let Some((o, a)) = failed {
        return attention(o, a).0;
    }
    // the name the Save really wrote ("(cleaned 2)": it never overwrites)
    let saved_name = if r.saved_name.is_empty() {
        r.output.clone()
    } else {
        r.saved_name.clone()
    };
    match (&r.save, &r.pass) {
        (Save::Stale { was, .. }, _) => {
            tr!(
                "files-title-saved-at",
                name = saved_name,
                strength = was.word()
            )
        }
        (Save::Saved, _) => match &v.place {
            Place::NextToOriginal => tr!("files-title-saved", name = saved_name),
            Place::Folder(f) => tr!("files-title-saved-in", folder = f.clone()),
            Place::Instead(crate::appstate::FolderMiss::Gone) => {
                tr!("files-title-saved-folder-gone")
            }
            Place::Instead(crate::appstate::FolderMiss::NoAnswer) => {
                tr!("files-title-saved-folder-silent")
            }
        },
        _ if done => tr!("files-title-cleaned", name = name),
        // waiting for a call: never "Cleaning" over a paused row
        (_, Pass::Yielded) => paused_line(v),
        (_, Pass::Waiting) if for_a_call(v) => paused_line(v),
        // what does clean meanwhile, by its name
        (_, Pass::Waiting) => match v.files.iter().find(|o| o.pass == Pass::Cleaning) {
            Some(o) => tr!(
                "files-title-waiting-for",
                name = name,
                other = o.name.clone()
            ),
            None => tr!("files-title-waiting", name = name),
        },
        _ => tr!("files-title-cleaning", name = name),
    }
}

/// A file that needs attention: what happened (the title), what still
/// works or what to do (its line's tooltip), and its line in the list.
fn attention(r: &Row, a: &Reason) -> (String, String, String) {
    let name = r.name.clone();
    let first = clock(r.damaged.unwrap_or(0.0));
    let stopped = |again: bool| {
        (
            tr!("files-title-stopped"),
            if again {
                tr!("files-help-stopped-again")
            } else {
                tr!("files-help-stopped")
            },
            tr!("files-row-stopped"),
        )
    };
    match a {
        Reason::Unsupported { ext } => (
            tr!("files-title-format", ext = ext.clone()),
            tr!("files-help-format"),
            tr!("files-row-format"),
        ),
        Reason::Unreadable => (
            tr!("files-title-unreadable", name = name),
            tr!("files-help-unreadable"),
            tr!("files-row-unreadable"),
        ),
        Reason::Incomplete => (
            tr!("files-title-incomplete", name = name.clone()),
            tr!("files-help-incomplete"),
            tr!("files-row-incomplete"),
        ),
        // a recording's name, bytes that aren't one: damaged (F15)
        Reason::NotMedia if r.kind != Kind::Other => (
            tr!("files-title-broken"),
            tr!("files-help-broken"),
            tr!("files-row-broken"),
        ),
        Reason::NotMedia => (
            tr!("files-title-not-media", name = name.clone()),
            tr!("files-help-not-media"),
            tr!("files-row-not-media"),
        ),
        Reason::Empty => (
            tr!("files-title-empty-file", name = name.clone()),
            tr!("files-help-empty"),
            tr!("files-row-empty"),
        ),
        Reason::NoSoundTrack => (
            tr!("files-title-no-sound"),
            tr!("files-help-no-sound"),
            tr!("files-row-no-sound"),
        ),
        Reason::Silent => (
            tr!("files-title-silent"),
            tr!("files-help-silent"),
            tr!("files-row-silent"),
        ),
        Reason::Engine => (
            tr!("files-title-engine"),
            tr!("files-help-engine"),
            tr!("files-row-engine"),
        ),
        Reason::Cache { bytes: Some(b) } => {
            // GB with one decimal, at least 0.1
            let gb = format!("{:.1}", (*b as f64 / 1e9).max(0.1));
            (
                tr!("files-title-cache", gb = gb.clone()),
                tr!("files-help-cache"),
                tr!("files-row-cache", gb = gb),
            )
        }
        // the cache folder can't be written: the pass stopped
        Reason::Cache { bytes: None } => stopped(false),
        Reason::Failed { again } | Reason::Crashed { again } => stopped(*again),
        Reason::Damaged => (
            tr!("files-title-damaged", name = name),
            tr!("files-help-damaged", time = first.clone()),
            // how long it should be, when the file says (and it is longer)
            match r.stated.filter(|s| *s > r.damaged.unwrap_or(0.0) + 0.5) {
                Some(s) => tr!("files-row-damaged", time = first, length = clock(s)),
                None => tr!("files-row-damaged-at", time = first),
            },
        ),
        Reason::NoSpace => (
            tr!("files-title-no-space"),
            tr!("files-help-no-space"),
            tr!("files-row-no-space"),
        ),
        Reason::Moved { at_save: false } => (
            tr!("files-title-moved", name = name),
            tr!("files-help-moved"),
            tr!("files-row-moved"),
        ),
        Reason::Moved { at_save: true } => (
            tr!("files-title-moved", name = name),
            tr!("files-help-moved-save"),
            tr!("files-row-moved-save"),
        ),
        Reason::NoWrite { folder } => (
            tr!("files-title-no-write", folder = folder.clone()),
            tr!("files-help-no-write"),
            tr!("files-row-no-write"),
        ),
        // the encoder's own words in the tooltip: the true reason, and no
        // Save again (the same Save fails the same way)
        Reason::SaveEncode { format, why } => (
            tr!("files-title-save-encode", format = format.clone()),
            match why {
                Some(why) => tr!("files-help-save-encode-why", why = why.clone()),
                None => tr!("files-help-save-encode"),
            },
            tr!("files-row-save-encode", format = format.clone()),
        ),
        Reason::SaveRead => (
            tr!("files-title-save-read"),
            tr!("files-help-save-read", name = name),
            tr!("files-row-save-read"),
        ),
        // the system's own words only in the row's tooltip
        Reason::SaveFailed { why } => (
            tr!("files-title-save-failed", name = name.clone()),
            match why {
                Some(why) => tr!("files-help-save-failed", why = why.clone()),
                None => tr!("files-help-save-again"),
            },
            tr!("files-row-save-failed"),
        ),
    }
}

/// The row's button for a file whose pass didn't finish: a next step that
/// cleans again.
fn next_step(r: &Row) -> Option<(String, Cmd)> {
    match r.attention()? {
        Reason::Engine | Reason::Cache { .. } | Reason::Failed { .. } | Reason::Crashed { .. } => {
            Some((tr!("files-try-again"), Cmd::Retry(r.id)))
        }
        Reason::Damaged => Some((
            tr!("files-clean-first", time = clock(r.damaged.unwrap_or(0.0))),
            Cmd::CleanFirst(r.id),
        )),
        _ => None,
    }
}

/// A file's length as the page shows it: none for a file that couldn't be
/// read, or while only a rough guess is known; the file's own word as is.
fn clock_or_none(r: &Row) -> String {
    if r.length > 0.0 && r.estimate != Estimate::Rough {
        clock(r.length)
    } else {
        String::new()
    }
}

/// A row's height (`.hm-fr`, round 3b: no gap, a hairline between).
const ROW_H: f32 = 36.0;
/// The space between the toolbar and the first row.
const LIST_GAP: f32 = 12.0;

/// The list-wide facts the toolbar and the rows go by.
struct Batch {
    /// Save all works: a file is left to save (F4: also one alone among
    /// files that need attention; it shows with two files or more).
    save_all: bool,
}

impl Batch {
    fn of(v: &View) -> Batch {
        let ok = |r: &&Row| r.attention().is_none() && r.length > 0.0;
        let open: Vec<&Row> = v
            .files
            .iter()
            .filter(ok)
            .filter(|r| {
                !matches!(
                    r.save,
                    Save::Saved | Save::WhenCleaned | Save::Writing { .. }
                )
            })
            .collect();
        Batch {
            save_all: !open.is_empty(),
        }
    }
}

/// A row's one action: a real button. The usual ones (Save, Cancel while
/// it saves, Show in folder) are their icon alone, as quiet as the row's
/// ×, with the words as the tooltip; what is rare or needs saying keeps
/// its words, in a small outlined button.
struct Act {
    label: String,
    icon: Option<Icon>,
    /// Only the icon shows.
    icon_only: bool,
    cmd: Cmd,
    /// Its name for screen readers (it says which file).
    name: String,
    /// Its tooltip (Save: the name it saves as).
    tip: Option<String>,
}

/// What a row shows besides its name and length (`.hm-fr`).
struct Look {
    cap: String,
    /// The cap's colour: warn (needs attention), muted (saved), else subtle.
    att: bool,
    saved: bool,
    /// A check before it: a saved copy at the page's strength.
    check: bool,
    act: Option<Act>,
    /// What to do about a problem: the row's tooltip, and part of its
    /// description for screen readers.
    tip: Option<String>,
    /// The × (not while its save writes: Cancel first).
    remove: bool,
    /// How far its pass (or its save) is, after the state words
    /// ("Ready in about 6 min · 42%"); the player keeps the bar.
    pct: Option<f32>,
}

/// A row: its state, its one action and its line. `many`: more than one
/// file;
/// `yielded_tip`: why cleaning waits ([`paused_line`]).
fn row_look(r: &Row, many: bool, offer_next: bool, yielded_tip: &str) -> Look {
    let yielded_tip = yielded_tip.to_string();
    let frac = if r.length > 0.0 {
        (r.cleaned() / r.length).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let act = |label: String, icon: Option<Icon>, cmd: Cmd, name: String| {
        Some(Act {
            label,
            icon,
            icon_only: false,
            cmd,
            name,
            tip: None,
        })
    };
    // its icon alone, the words on its tooltip
    let iconic = |a: Act, tip: String| Act {
        icon_only: true,
        tip: Some(tip),
        ..a
    };
    let named = |label: &str| {
        tr!(
            "files-action-name",
            action = label.to_owned(),
            name = r.name.clone()
        )
    };
    let mut look = Look {
        cap: String::new(),
        att: false,
        saved: false,
        check: false,
        act: None,
        tip: None,
        remove: true,
        pct: Some(frac),
    };
    // F5: which file a Save wrote, on the row's tooltip
    let saved_as =
        (!r.saved_name.is_empty()).then(|| tr!("files-row-saved-as", name = r.saved_name.clone()));
    match (&r.pass, &r.save) {
        (Pass::Attention(a), _) => {
            let (_, help, cap) = attention(r, a);
            look.cap = cap;
            look.tip = Some(help);
            look.att = true;
            let to = |label: String, cmd: Cmd| {
                let name = named(&label);
                act(label, None, cmd, name)
            };
            look.act = match a {
                Reason::NoSpace => to(tr!("files-save-elsewhere"), Cmd::SaveElsewhere(r.id)),
                Reason::NoWrite { .. } => {
                    to(tr!("files-choose-another-folder"), Cmd::SaveElsewhere(r.id))
                }
                Reason::Moved { .. } | Reason::Unreadable => {
                    to(tr!("files-choose-again"), Cmd::ChooseAgain(r.id))
                }
                // the same Save would fail the same way: no Save again
                a if a.permanent() => None,
                a if a.save_only() => to(tr!("files-save-again"), Cmd::Save(r.id)),
                // the file's next step: Try again, Clean the first 3:12
                _ => next_step(r).and_then(|(label, cmd)| to(label, cmd)),
            };
            look.pct = None;
        }
        (_, Save::Writing { fraction, .. }) => {
            // "Saving · 39%": how far, in one reading
            look.cap = tr!("files-saving");
            look.act = act(
                tr!("files-cancel"),
                Some(Icon::Stop),
                Cmd::CancelSave(r.id),
                tr!("files-cancel-name", name = r.output.clone()),
            )
            .map(|a| iconic(a, tr!("files-cancel")));
            look.remove = false;
            look.pct = Some(*fraction);
        }
        (_, Save::Saved) => {
            look.cap = tr!("files-row-saved-strength", strength = r.strength.word());
            look.saved = true;
            look.check = true;
            let label = tr!("files-show-in-folder");
            let name = named(&label);
            look.act = act(
                label.clone(),
                Some(Icon::Folder),
                Cmd::ShowInFolder(r.id),
                name,
            )
            .map(|a| iconic(a, label));
            look.pct = None;
        }
        // saved at another strength than now: Save at this one
        (_, Save::Stale { was, now: now_at }) => {
            look.cap = tr!("files-row-saved-at", strength = was.word());
            look.saved = true;
            let label = tr!("files-save-at", strength = now_at.word());
            look.act = act(
                label,
                Some(Icon::Save),
                Cmd::Save(r.id),
                tr!("files-save-name", name = r.output.clone()),
            )
            .map(|a| {
                iconic(
                    a,
                    tr!(
                        "files-save-at-tip",
                        strength = now_at.word(),
                        name = r.output.clone()
                    ),
                )
            });
            look.pct = None;
        }
        (Pass::Cleaned, Save::WhenCleaned) => {
            look.cap = tr!("files-row-saves-now");
            look.pct = None;
        }
        (_, Save::WhenCleaned) => look.cap = tr!("files-saves-when-cleaned"),
        (Pass::Cleaned, _) => {
            look.cap = tr!("files-row-cleaned-at", strength = r.strength.word());
            look.act = act(
                tr!("files-save"),
                Some(Icon::Save),
                Cmd::Save(r.id),
                tr!("files-save-name", name = r.output.clone()),
            )
            .map(|a| iconic(a, tr!("files-save-tip", name = r.output.clone())));
            look.pct = None;
        }
        // the call comes first: said whole in its tooltip (and the title),
        // and the user may say otherwise
        (Pass::Yielded, _) => {
            look.cap = tr!("files-row-yielded");
            look.tip = Some(yielded_tip.clone());
            let label = tr!("files-clean-anyway");
            let name = named(&label);
            look.act = act(label, None, Cmd::CleanAnyway, name);
            // how far it got stays said (A114 SF-P1: a call can last an hour)
            if r.cleaned() <= 0.0 {
                look.pct = None;
            }
        }
        // waiting its turn: one stopped partway (another file is played,
        // or an app uses HushMic) keeps its bar
        (Pass::Waiting, _) => {
            if many {
                look.cap = tr!("files-row-waiting");
            }
            if r.cleaned() <= 0.0 {
                look.pct = None;
            }
            // F8: this one before the others that wait
            if offer_next {
                let label = tr!("files-clean-next");
                let name = named(&label);
                look.act = act(label, None, Cmd::CleanNext(r.id), name);
            }
        }
        _ => {
            look.cap = match r.time_left {
                Some(t) if t < 60.0 => tr!("files-ready-under-a-minute"),
                Some(t) => tr!("files-ready-in", time = minutes(t)),
                None => tr!("files-row-cleaning"),
            }
        }
    }
    // HE-AAC without ffmpeg, the tracks: said quietly, where nothing
    // else is
    if look.tip.is_none() {
        let said: Vec<String> = [
            look.saved.then_some(saved_as).flatten(),
            tracks_line(r),
            r.reduced.then(|| tr!("files-reduced-tip")),
        ]
        .into_iter()
        .flatten()
        .collect();
        look.tip = (!said.is_empty()).then(|| said.join(" "));
    }
    look
}

/// What a file's sound tracks come to: "Only the first of 2 sound tracks
/// is kept. Install ffmpeg…" when Save can't keep them all (the page also
/// says that under the player), "Cleans track 2 · Mic, of 2. The other
/// sound tracks are kept as they are." for an OBS recording (F1: only
/// the voice is cleaned); nothing for one track.
fn tracks_line(r: &Row) -> Option<String> {
    let all = r.tracks + r.dropped;
    if r.dropped > 0 {
        Some(tr!("files-tracks-dropped", count = all))
    } else if r.tracks > 1 {
        Some(format!(
            "{}. {}",
            cleans_track(r),
            tr!("files-tracks-copied")
        ))
    } else {
        None
    }
}

/// "Cleans track 2 · Mic, of 2" (without a title: "Cleans track 2 of 2").
fn cleans_track(r: &Row) -> String {
    let n = (r.track + 1) as u64;
    match r.titles.get(r.track).filter(|t| !t.is_empty()) {
        Some(t) => tr!(
            "files-cleans-track-titled",
            n = n,
            title = t.clone(),
            count = r.tracks
        ),
        None => tr!("files-cleans-track", n = n, count = r.tracks),
    }
}

/// How far a file not yet cleaned is, under the player (the row, which
/// says it too, may be scrolled away; A114 SF-P1: a part not yet cleaned
/// is empty in the picture's lower lane, and says how far): on one line
/// where it fits,
/// "Cleaning… 38% · ready in about 6 min"; else its two halves, each a
/// line ("Cleaning… 38%", "Ready in about 6 min", the row's words). A
/// file paused for a call: "Paused at 38% · continues after your call".
/// One stopped partway and waiting (another file was played, or an app
/// uses HushMic and one file cleans at a time): "Waiting at 38% · play it
/// to continue", since playing a file cleans it first (`Jobs::play`;
/// Clean next only puts it after the ones cleaning). While a call holds
/// every file, playing it wouldn't: "Waiting at 38%" alone ("Stopped"
/// is a failed pass).
#[derive(Clone, Debug, PartialEq)]
struct Progress {
    one: String,
    two: Option<(String, String)>,
}

/// A call holds the cleaning (a row says "Paused for your call").
fn for_a_call(v: &View) -> bool {
    v.files.iter().any(|o| o.pass == Pass::Yielded)
}

fn cleaning_line(v: &View, r: &Row) -> Option<Progress> {
    if r.length <= 0.0 {
        return None;
    }
    let pct = percent((r.cleaned() / r.length) as f32);
    match r.pass {
        Pass::Cleaning => {
            let head = tr!("files-cleaning-at", percent = pct.clone());
            Some(match r.time_left {
                Some(t) if t < 60.0 => Progress {
                    one: tr!("files-cleaning-soon", percent = pct),
                    two: Some((head, tr!("files-ready-under-a-minute"))),
                },
                Some(t) => Progress {
                    one: tr!("files-cleaning-left", percent = pct, time = minutes(t)),
                    two: Some((head, tr!("files-ready-in", time = minutes(t)))),
                },
                None => Progress {
                    one: head,
                    two: None,
                },
            })
        }
        _ if r.cleaned() <= 0.0 => None,
        Pass::Yielded => Some(Progress {
            one: tr!("files-paused-call", percent = pct.clone()),
            two: Some((
                tr!("files-paused-at", percent = pct),
                tr!("files-continues-after-call"),
            )),
        }),
        Pass::Waiting if for_a_call(v) => Some(Progress {
            one: tr!("files-waiting-at", percent = pct),
            two: None,
        }),
        Pass::Waiting => Some(Progress {
            one: tr!("files-waiting-play", percent = pct.clone()),
            two: Some((
                tr!("files-waiting-at", percent = pct),
                tr!("files-play-to-continue"),
            )),
        }),
        _ => None,
    }
}

/// The player row's words, laid out.
struct Words {
    lines: Vec<text::TextBlock>,
}

impl Words {
    fn height(&self) -> f32 {
        self.lines.iter().map(|p| p.size().y).sum()
    }
}

/// The row of controls under a file's picture, as
/// [`FilesPage::controls`] measured it: where its parts stand, the
/// padding in the strength's steps, what the line under the row holds
/// (the track picker's label, or the words about the tracks), and the
/// height of it all with its gaps.
struct Controls {
    plan: replay::RowPlan,
    pad: f32,
    picker: Option<String>,
    words: Option<Words>,
    h: f32,
}

/// The player row's words at `room`: a file whose tracks Save can't all
/// keep says so (whole: its second sentence is what to do), in two lines
/// at most; cut short where they don't fit. None with nothing to say or
/// no room.
fn row_words(
    ctx: &egui::Context,
    r: &Row,
    room: f32,
    pal: crate::ui::tokens::Palette,
) -> Option<Words> {
    if room <= 48.0 {
        return None;
    }
    let dropped = Some(r).filter(|r| r.dropped > 0).and_then(tracks_line)?;
    let rows = |b: &text::TextBlock| (b.size().y / Type::CAP.line).round() as usize;
    let all = text::layout(ctx, &dropped, Type::CAP, pal.ink_muted, room);
    if rows(&all) <= 2 {
        return Some(Words { lines: vec![all] });
    }
    Some(Words {
        lines: vec![text::layout_rows(
            ctx,
            &dropped,
            Type::CAP,
            pal.ink_muted,
            room,
            2,
        )],
    })
}

/// The words of the strength's four steps.
fn step_labels() -> [String; 4] {
    [
        tr!("strength-gentle"),
        tr!("strength-medium"),
        tr!("strength-strong"),
        tr!("strength-maximum"),
    ]
}

/// The strength's width with `pad` px a side in its steps.
fn steps_width(ctx: &egui::Context, pad: f32) -> f32 {
    let labels = step_labels();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    widgets::Strength::fitted_width(ctx, &refs, pad, false).ceil()
}

/// Play's width under its label: Pause takes as much, so the row never
/// moves when a file starts to play.
fn play_width(ctx: &egui::Context) -> f32 {
    [
        (tr!("files-play"), Icon::Play),
        (tr!("files-pause"), Icon::Pause),
    ]
    .iter()
    .map(|(l, i)| Button::secondary(l).sm().icon(*i).width(ctx))
    .fold(0.0, f32::max)
}

/// The height of what a file that never opened says under the picture
/// (`FilesPage::failed_card`), `w` wide.
fn failed_height(ctx: &egui::Context, r: &Row, w: f32) -> f32 {
    let help = r.attention().map(|a| attention(r, a).1).unwrap_or_default();
    let words = text::layout(ctx, &help, Type::CAP, Color32::WHITE, w)
        .size()
        .y;
    FAILED_GAP.0 + words + FAILED_GAP.1 + 32.0 + FAILED_GAP.2
}

/// The toolbar over the rows with the card's padding around both: the
/// border line, 12 px, the toolbar, the gap to the rows, the padding
/// under them.
const LIST_FRAME: f32 = 1.0 + 12.0 + replay::ROW_H + LIST_GAP + LIST_PAD;

/// How a card `room` high inside is shared once it has files: a lane's
/// height and the rows the list shows at once. The list takes what its
/// `files` rows need, with its toolbar up to [`LIST_SHARE`] of the card
/// (then it scrolls); the lanes take all the rest after the picture's
/// gaps and the `player_h` under it. Where that would leave a lane under
/// [`LANE_FLOOR`], the list gives way down to [`LIST_ROWS_MIN`] rows.
fn share(compact: bool, room: f32, player_h: f32, files: usize) -> (f32, usize) {
    let gaps = Sizes::of(compact, 0.0).height();
    let lane =
        |rows: usize| ((room - gaps - player_h - LIST_FRAME - rows as f32 * ROW_H) / 2.0).floor();
    let least = LIST_ROWS_MIN.min(files);
    let cap = ((LIST_SHARE * room - LIST_FRAME) / ROW_H).floor().max(0.0) as usize;
    let mut rows = files.min(cap.max(least));
    while rows > least && lane(rows) < LANE_FLOOR {
        rows -= 1;
    }
    (lane(rows).max(LANE_FLOOR), rows)
}

/// m:ss, or h:mm:ss from an hour on.
pub fn clock(s: f64) -> String {
    let s = (s + 1e-6).floor().max(0.0) as u64;
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// A track's name in the menu: "Track 2 · Mic/Aux", or "Track 2".
fn track_name(r: &Row, k: usize) -> String {
    let n = (k + 1) as u64;
    match r.titles.get(k).filter(|t| !t.is_empty()) {
        Some(t) => tr!("files-track-titled", n = n, title = t.clone()),
        None => tr!("files-track", n = n),
    }
}

/// Cleaning waits for a call: the whole sentence, with the app's name.
fn paused_line(v: &View) -> String {
    match &v.yield_app {
        Some(app) => tr!("files-paused-for", app = app.clone()),
        None => tr!("files-paused"),
    }
}

/// The strength's tooltip: what it does to a recording (F9: Maximum
/// silences the pauses), and with more files that it is every file's.
fn strength_tip(s: model::Strength, many: bool) -> String {
    let line = s.files_line();
    if many {
        format!("{line}\n{}", tr!("files-every-file"))
    } else {
        line
    }
}

/// A title fits on the header's one line `w` wide.
fn fits(ctx: &egui::Context, title: &str, w: f32) -> bool {
    text::layout(ctx, title, Type::H, Color32::WHITE, f32::INFINITY)
        .size()
        .x
        <= w
}

/// What a row hands back: the name (it picks the file), its one action,
/// the ×.
struct RowOut {
    pick: Response,
    act: Option<Response>,
    remove: Option<Response>,
    /// Selected or hovered: filled, so no hairline next to it.
    filled: bool,
}

/// The state column's width at most (its words are cut short past it;
/// the whole state is in the row's description), and at least.
const STATUS_W: f32 = 196.0;
const STATUS_W_COMPACT: f32 = 148.0;
const STATUS_W_MIN: f32 = 96.0;

/// How wide a row's state is drawn: its check, its words and how far
/// (as wide as "100%", so it never moves while it counts).
fn status_width(ctx: &egui::Context, look: &Look) -> f32 {
    let w = |t: &str, ty| {
        text::layout(ctx, t, ty, Color32::WHITE, f32::INFINITY)
            .size()
            .x
    };
    let mut x = if look.check { 18.0 } else { 0.0 };
    if !look.cap.is_empty() {
        x += w(&look.cap, Type::LIST_NOTE);
    }
    if look.pct.is_some() {
        if !look.cap.is_empty() {
            x += w(" · ", Type::LIST_NOTE);
        }
        x += w(&percent(1.0), Type::MONO);
    }
    x.ceil()
}

/// A row's tooltip, over the row rather than under it: it never covers
/// the next file (F15).
fn tip_above(resp: egui::Response, tip: &str) -> egui::Response {
    let mut t = egui::Tooltip::for_enabled(&resp);
    t.popup = t
        .popup
        .align(egui::RectAlign::TOP_START)
        .align_alternatives(&[egui::RectAlign::BOTTOM_START]);
    t.show(|ui| {
        ui.set_max_width(ui.spacing().tooltip_width);
        ui.add(egui::Label::new(tip));
    });
    resp
}

/// A quiet icon button's side: a row's × and its icon action. It is also
/// the action column's least width: Save, Cancel while it saves and Show
/// in folder take turns in it, so the usual states never move the columns;
/// a worded action (Try again, Clean anyway…) widens it while it shows.
const QUIET: f32 = 28.0;

/// The width a row's action takes.
fn act_width(ctx: &egui::Context, a: &Act) -> f32 {
    if a.icon_only && a.icon.is_some() {
        QUIET
    } else {
        act_button(a).width(ctx)
    }
}

/// A row's icon button: the icon alone, in `ink`; under the pointer it
/// gets its box and the full ink. No frame at rest: a framed box 32 tall
/// filled the 36 px row from edge to edge, and a white one changed with
/// the number of files.
fn quiet_icon(
    ui: &Ui,
    kit: &crate::ui::Kit,
    at: Rect,
    id: egui::Id,
    icon: Icon,
    ink: Color32,
    label: &str,
) -> Response {
    let (pal, p) = (&kit.pal, ui.painter());
    let resp = ui.interact(at, id, Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    let hover = resp.hovered();
    if hover {
        p.rect(
            at,
            8.0,
            pal.surface_hover,
            egui::Stroke::new(1.0_f32, pal.line_strong),
            egui::StrokeKind::Inside,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    icons::paint(
        p,
        icon,
        at.center(),
        16.0,
        if hover { pal.ink } else { ink },
    );
    if widgets::shows_focus(kit, &resp) {
        widgets::focus_ring(p, at, 8.0, pal.focus_ring());
    }
    resp
}

/// `name` cut in its middle to fit `w` ("podcast-epis…final-mix.mp3"):
/// its end (a take number, the extension) stays in sight.
fn middle(ctx: &egui::Context, name: &str, ty: Type, w: f32) -> String {
    if text::width(ctx, name, ty) <= w {
        return name.to_owned();
    }
    let chars: Vec<char> = name.chars().collect();
    let cut = |k: usize| {
        let tail = (k * 11).div_ceil(20).min(k);
        let head: String = chars[..k - tail].iter().collect();
        let end: String = chars[chars.len() - tail..].iter().collect();
        format!("{head}…{end}")
    };
    // the most characters that fit
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if text::width(ctx, &cut(mid), ty) <= w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut(lo)
}

/// A row's worded action as a small button.
fn act_button(a: &Act) -> Button<'_> {
    let b = Button::secondary(&a.label).xs();
    match a.icon {
        Some(i) if a.icon_only => b.icon_only(i),
        Some(i) => b.icon(i),
        None => b,
    }
}

/// One file: its kind, name and state, its length, its one action and the
/// × (`.hm-fr`, round 3b: `minmax(0, 1fr) status_w 44px act_w 28px`, 8
/// apart, 4 at the right, 36 px tall); `rule`: the hairline on its top
/// edge. `cols`: the state's and the action's widths.
fn file_row(
    ui: &mut Ui,
    r: &Row,
    look: &Look,
    selected: bool,
    cols: (f32, f32),
    rule: bool,
    kit: &crate::ui::Kit,
) -> RowOut {
    let (status_w, act_w) = cols;
    let ctx = ui.ctx().clone();
    let pal = &kit.pal;
    let w = ui.available_width();
    let (area, _) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::hover());
    let x_r = area.right() - 4.0;
    let x_col = rect(x_r - 28.0, area.top(), 28.0, ROW_H);
    let act_col = rect(x_col.left() - 8.0 - act_w, area.top(), act_w, ROW_H);
    let len_col = rect(act_col.left() - 8.0 - 44.0, area.top(), 44.0, ROW_H);
    let pick_rect = Rect::from_min_max(area.min, pos2(len_col.left() - 8.0, area.bottom()));
    // at the narrowest the state column gives way too: the name keeps 64
    let x0 = area.left() + 12.0;
    let nx = x0 + 24.0;
    let status_w = status_w.min(pick_rect.right() - (nx + 64.0 + 8.0)).max(0.0);
    let status_col = rect(pick_rect.right() - status_w, area.top(), status_w, ROW_H);
    let cy = area.center().y;

    let p = ui.painter().clone();
    // the selection's fill in its hairline, one model in both themes
    // (UX panel round 2, A18); hover is the lighter highlight
    let hovered = ui.rect_contains_pointer(area);
    let filled = selected || hovered;
    if selected {
        widgets::selected(&p, area, 6.0, pal);
    } else if hovered {
        widgets::highlight(&p, area, 6.0, pal, 1.0);
    } else if rule {
        p.rect_filled(
            Rect::from_min_size(area.min, vec2(area.width(), 1.0)),
            0.0,
            pal.line,
        );
    }

    // the name: it picks the file
    let mut pick = ui.interact(pick_rect, ui.id().with(("files-row", r.id)), Sense::click());
    let name = r.name.clone();
    pick.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, selected, &name));
    let len = clock_or_none(r);
    let about = [
        len.clone(),
        look.cap.clone(),
        look.pct.map(percent).unwrap_or_default(),
        look.tip.clone().unwrap_or_default(),
    ]
    .into_iter()
    .filter(|t| !t.is_empty())
    .collect::<Vec<_>>()
    .join(", ");
    if !about.is_empty() {
        ctx.accesskit_node_builder(pick.id, |n| n.set_description(about));
    }
    if let Some(tip) = &look.tip {
        pick = tip_above(pick, tip);
    }
    if pick.gained_focus() {
        // Tab onto a row below the list's fold brings it in
        pick.scroll_to_me(None);
    }
    if pick.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    // the content 12 px inside the row's box; the kind by the name's
    // extension until the file says
    let icon = match r.kind {
        Kind::Video => Icon::Film,
        Kind::Audio => Icon::Sound,
        Kind::Other => Icon::File,
    };
    icons::paint(&p, icon, pos2(x0 + 8.0, cy), 16.0, pal.ink_muted);
    // the name up to the state column, cut in its middle (its end tells
    // takes apart)
    let room = (status_col.left() - 8.0 - nx).max(0.0);
    let shown = middle(&ctx, &r.name, Type::LIST, room);
    let nb = text::layout(&ctx, &shown, Type::LIST, pal.ink, f32::INFINITY);
    nb.paint_centered_y(&p, nx, cy);
    let c = if look.att {
        pal.warn
    } else if look.saved {
        pal.ink_muted
    } else {
        pal.ink_subtle
    };
    // the state from the column's start: a check for a saved copy, the
    // words, then how far (mono figures: only a new digit moves); the
    // words give way first
    let mut sx = status_col.left();
    let end = status_col.right();
    if look.check && end - sx > 18.0 {
        icons::paint(&p, Icon::Tick, pos2(sx + 6.0, cy), 14.0, pal.accent);
        sx += 18.0;
    }
    let pct = look
        .pct
        .map(|f| text::layout(&ctx, &percent(f), Type::MONO, c, f32::INFINITY));
    let dot = text::layout(&ctx, " · ", Type::LIST_NOTE, c, f32::INFINITY);
    let tail = match &pct {
        Some(pb) if !look.cap.is_empty() => pb.size().x + dot.size().x,
        Some(pb) => pb.size().x,
        None => 0.0,
    };
    if !look.cap.is_empty() && end - sx - tail > 8.0 {
        let cb = text::layout_elided(&ctx, &look.cap, Type::LIST_NOTE, c, end - sx - tail);
        let cw = cb.size().x;
        cb.paint_centered_y(&p, sx, cy);
        sx += cw;
        if pct.is_some() {
            let dw = dot.size().x;
            dot.paint_centered_y(&p, sx, cy);
            sx += dw;
        }
    }
    if let Some(pb) = pct.filter(|pb| end - sx >= pb.size().x) {
        pb.paint_centered_y(&p, sx, cy);
    }
    // the length
    if !len.is_empty() {
        let lb = text::layout(&ctx, &len, Type::MONO, pal.ink_subtle, f32::INFINITY);
        let lw = lb.size().x;
        lb.paint_centered_y(&p, len_col.right() - lw, cy);
    }
    // the one action, at the column's end
    let act = look.act.as_ref().map(|a| {
        let bw = act_width(&ctx, a);
        let at = rect(act_col.right() - bw, cy - QUIET / 2.0, bw, QUIET);
        let resp = match a.icon.filter(|_| a.icon_only) {
            Some(icon) => {
                let id = ui.id().with(("files-act", r.id));
                quiet_icon(ui, kit, at, id, icon, pal.ink, &a.name)
            }
            None => {
                let resp = util::put(ui, at, ("files-act", r.id), act_button(a));
                let name = a.name.clone();
                ctx.accesskit_node_builder(resp.id, |n| n.set_label(name));
                resp
            }
        };
        match &a.tip {
            Some(tip) => tip_above(resp, tip),
            None => resp,
        }
    });
    // the ×
    let remove = look.remove.then(|| {
        let id = ui.id().with(("files-x", r.id));
        let at = Rect::from_center_size(x_col.center(), vec2(QUIET, QUIET));
        let label = tr!("files-remove-name", name = r.name.clone());
        quiet_icon(ui, kit, at, id, Icon::Close, pal.ink_muted, &label)
            .on_hover_text(tr!("files-remove"))
    });
    RowOut {
        pick,
        act,
        remove,
        filled,
    }
}

/// The frame light along the figure's top edge (12% to 88%), white from
/// 4% to 96% while something is dragged over.
fn frame_light(
    p: &egui::Painter,
    pal: &crate::ui::tokens::Palette,
    fig: Rect,
    drag: bool,
    dark: bool,
) {
    let inset = if drag { 0.04 } else { 0.12 };
    let (x0, x1) = (
        fig.left() + inset * fig.width(),
        fig.right() - inset * fig.width(),
    );
    let fl = if drag {
        if dark {
            Color32::WHITE
        } else {
            pal.ink
        }
    } else {
        pal.frame_light
    };
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
    let _ = mix;
}

#[cfg(test)]
mod tests {
    use super::demo::{DemoFiles, STATES};
    use super::model::Strength;
    use super::*;
    use crate::appstate::mock::{state, Scenario};
    use crate::appstate::Page;
    use crate::pages::testing;
    use egui_kittest::kittest::{NodeT, Queryable};

    /// What the boards' header says in each state (App-Files).
    const TITLES: [(&str, &str); 19] = [
        ("empty", "Clean a file."),
        ("drag-over", "Drop to clean them."),
        ("added", "Cleaning interview.m4a"),
        ("playing", "You are hearing HushMic."),
        ("holding", "You are hearing the original."),
        ("strength-change", "You are hearing HushMic."),
        ("paused", "Cleaning interview.m4a"),
        ("caught-up", "Cleaning ahead…"),
        ("seek-past-the-front", "Cleaning ahead…"),
        ("save-waiting", "You are hearing HushMic."),
        ("cleaned", "interview.m4a is cleaned."),
        ("saving", "lecture-03.mp4 is cleaned."),
        ("saved", "Saved interview (cleaned).m4a"),
        ("queue", "interview.m4a is cleaned."),
        // never "Cleaning" over a paused row (F2)
        (
            "yield",
            "Paused while Zoom uses your microphone. Your call comes first.",
        ),
        ("video", "You are hearing HushMic."),
        ("format", "HushMic doesn’t open .wma files."),
        ("no-space", "There isn’t enough space on the disk."),
        ("moved", "interview.m4a moved."),
    ];

    #[test]
    fn every_board_state_has_its_header() {
        crate::i18n::pin_english();
        for (name, title) in TITLES {
            assert!(STATES.contains(&name), "{name}");
            let v = DemoFiles::preset(name, 100.0, 3.3).view(100.0);
            let t = copy(&v, v.row(), false, name == "drag-over");
            assert_eq!(t, title, "{name}");
        }
        // the format is never a surprise: Save's tooltip names the output
        let v = DemoFiles::preset("cleaned", 100.0, 3.3).view(100.0);
        let save = row_look(v.row().expect("a file"), false, false, "")
            .act
            .expect("Save");
        assert_eq!(save.tip.as_deref(), Some("Save as interview (cleaned).m4a"));
        assert!(
            save.icon_only,
            "Save is its icon; the tooltip has the words"
        );
        for name in demo::LIST_STATES {
            assert!(
                !DemoFiles::preset(name, 100.0, 3.3)
                    .view(100.0)
                    .files
                    .is_empty(),
                "{name}"
            );
        }
        // a tap latched the original while paused
        let v = DemoFiles::preset("paused", 100.0, 3.3).view(100.0);
        assert_eq!(copy(&v, v.row(), true, false), "The original is on.");
    }

    /// The daemon's problems without a board (PLAN §3, appux's copy):
    /// title, what to do (the row's tooltip), the row's line.
    #[test]
    fn every_problem_has_its_words() {
        crate::i18n::pin_english();
        let words = [
            (
                "unreadable",
                "HushMic can’t read interview.m4a",
                "Check that it opens in another app, then choose it again.",
                "Can’t be read",
            ),
            (
                "no-sound",
                "This video has no sound track.",
                "There is nothing in it to clean.",
                "No sound track",
            ),
            (
                "silent",
                "There’s no sound in this file.",
                "It is silent all the way through.",
                "Silent",
            ),
            (
                "engine",
                "The cleaning engine didn’t start.",
                "Nothing was written. Try again in a moment.",
                "Didn’t start",
            ),
            (
                "stopped",
                "Cleaning stopped before the end.",
                "Nothing was written. Try again.",
                "Stopped before the end",
            ),
            (
                "stopped-again",
                "Cleaning stopped before the end.",
                "It stopped again. Restart HushMic, then try once more.",
                "Stopped before the end",
            ),
            (
                "cache",
                "Cleaning this file needs about 0.3 GB free.",
                "It is used while the file is open. Free some space, then try again.",
                "Needs 0.3 GB free",
            ),
            (
                "damaged",
                "Part of interview.m4a is damaged.",
                "HushMic can clean the first 3:12.",
                "Damaged after 3:12 of 12:40",
            ),
            (
                "no-write",
                "Can’t write to Cleaned.",
                "It wasn’t saved. Choose another folder, or allow writing to this one.",
                "Not saved: the folder can’t be written",
            ),
            (
                "save-encode",
                "Couldn’t write the M4A file.",
                "ffmpeg: Invalid audio stream. Exactly one MP3 audio stream is required. Nothing was saved.",
                "Not saved: the M4A encoder failed",
            ),
            (
                "save-read",
                "Couldn’t read the original.",
                "interview.m4a couldn’t be read again to save it. Check that it opens in another app.",
                "Not saved: the original couldn’t be read",
            ),
            (
                "save-failed",
                "Couldn’t save interview.m4a",
                "Input/output error. Nothing was saved. Try saving again.",
                "Not saved",
            ),
            (
                "moved-save",
                "interview.m4a moved.",
                "It moved before it was saved. Nothing was written.",
                "Moved before it was saved",
            ),
        ];
        assert_eq!(words.len(), demo::MORE_STATES.len());
        // the title in the header; what to do is the row's tooltip
        for (name, title, help, cap) in words {
            assert!(demo::MORE_STATES.contains(&name), "{name}");
            let v = DemoFiles::preset(name, 100.0, 3.3).view(100.0);
            let r = v.row().expect("a file");
            assert_eq!(copy(&v, Some(r), false, false), title, "{name}");
            let look = row_look(r, false, false, "");
            assert_eq!(look.cap, cap, "{name}");
            assert_eq!(look.tip.as_deref(), Some(help), "{name}");
        }
    }

    type Log = std::rc::Rc<std::cell::RefCell<Vec<Cmd>>>;

    /// The demo source, with what the page sends it.
    struct Logged(DemoFiles, Log);

    impl Source for Logged {
        fn send(&mut self, c: Cmd) {
            self.1.borrow_mut().push(c.clone());
            self.0.send(c);
        }
        fn view(&mut self, now: f64) -> View {
            self.0.view(now)
        }
        fn picture(&self) -> Option<Arc<Sheet>> {
            self.0.picture()
        }
        fn busy(&self) -> bool {
            self.0.busy()
        }
    }

    /// Files on 60 Hz frames, open on a board state, and the commands it
    /// sends from then on.
    fn files(name: &'static str) -> (testing::Window, Log) {
        files_at(name, egui::vec2(960.0, 640.0))
    }

    fn files_at(name: &'static str, size: egui::Vec2) -> (testing::Window, Log) {
        let log: Log = Default::default();
        let l = log.clone();
        let mut w = testing::at_60hz_with(
            crate::window::Link::recording(state(Scenario::OnInCall)),
            Page::Files,
            size,
            move |ps| {
                let demo = if name == "live" {
                    DemoFiles::live()
                } else {
                    DemoFiles::preset(name, 0.0, 3.3)
                };
                ps.files.source = Some(Box::new(Logged(demo, l.clone())));
            },
        );
        w.harness.run_steps(3);
        log.borrow_mut().clear();
        (w, log)
    }

    fn sent(log: &Log, c: &Cmd) -> bool {
        log.borrow().contains(c)
    }

    const HOLD: &str = "Hold to hear the original";

    fn pointer(w: &mut testing::Window, label: &str, pressed: bool) {
        let at = w.harness.get_by_label(label).rect().center();
        let events = &mut w.harness.input_mut().events;
        events.push(egui::Event::PointerMoved(at));
        events.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }

    #[test]
    fn space_plays_and_the_hold_plays_the_original() {
        let (mut w, log) = files("paused");
        // a Space tap is Play, with no flash of the original
        w.harness.key_down(egui::Key::Space);
        w.harness.run_steps(2);
        w.harness.key_up(egui::Key::Space);
        w.harness.run_steps(3);
        assert!(sent(&log, &Cmd::Play), "{:?}", log.borrow());
        assert!(!sent(&log, &Cmd::Original(true)));
        w.harness.get_by_label("Pause").click();
        w.harness.run_steps(3);
        assert!(sent(&log, &Cmd::Pause));
        log.borrow_mut().clear();
        // a hold while paused plays the original until it lets go
        pointer(&mut w, HOLD, true);
        w.harness.run_steps(20);
        assert_eq!(log.borrow().first(), Some(&Cmd::Original(true)));
        pointer(&mut w, HOLD, false);
        w.harness.run_steps(3);
        assert_eq!(log.borrow().last(), Some(&Cmd::Original(false)));
        log.borrow_mut().clear();
        // a click with no press (an assistive technology) latches it, and
        // it plays
        w.harness.get_by_label(HOLD).click_accesskit();
        w.harness.run_steps(3);
        assert!(sent(&log, &Cmd::Original(true)), "{:?}", log.borrow());
        assert!(sent(&log, &Cmd::Play));
        assert!(w
            .harness
            .query_by_label("You are hearing the original.")
            .is_some());
    }

    #[test]
    fn a_long_hold_lasts_until_the_button_lets_go() {
        // egui stops counting a press as a click after 0.8 s or a few
        // pixels of travel; the hold is the button's down, not the click's
        let (mut w, log) = files("playing");
        pointer(&mut w, HOLD, true);
        w.harness.run_steps(90);
        let at = w.harness.get_by_label(HOLD).rect().center() + egui::vec2(30.0, 12.0);
        w.harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(at));
        w.harness.run_steps(10);
        assert_eq!(*log.borrow(), [Cmd::Original(true)]);
        let up = egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        };
        w.harness.input_mut().events.push(up);
        w.harness.run_steps(3);
        assert_eq!(*log.borrow(), [Cmd::Original(true), Cmd::Original(false)]);
    }

    /// A demo whose selected file is this many seconds from cleaned.
    struct Soon(DemoFiles, f64);

    impl Source for Soon {
        fn send(&mut self, c: Cmd) {
            self.0.send(c);
        }
        fn view(&mut self, now: f64) -> View {
            let mut v = self.0.view(now);
            let sel = v.selected;
            for r in v.files.iter_mut().filter(|r| Some(r.id) == sel) {
                r.time_left = Some(self.1);
            }
            v
        }
        fn picture(&self) -> Option<Arc<Sheet>> {
            self.0.picture()
        }
        fn busy(&self) -> bool {
            self.0.busy()
        }
    }

    fn soon(size: egui::Vec2, secs: f64) -> testing::Window {
        let mut w = testing::at_60hz_with(
            crate::window::Link::recording(state(Scenario::OnInCall)),
            Page::Files,
            size,
            move |ps| {
                let demo = DemoFiles::preset("paused", 0.0, 3.3);
                ps.files.source = Some(Box::new(Soon(demo, secs)));
            },
        );
        w.harness.run_steps(3);
        w
    }

    /// The selected row's description: its length and its state.
    fn row_about(w: &testing::Window, name: &str) -> String {
        w.harness
            .get_by_label(name)
            .accesskit_node()
            .description()
            .unwrap_or_default()
    }

    #[test]
    fn the_wait_is_the_rows_state_in_short() {
        let w = soon(egui::vec2(960.0, 640.0), 20.0);
        assert_eq!(
            row_about(&w, "interview.m4a"),
            "12:40, Ready in under 1 min, 57%"
        );
        let w = soon(egui::vec2(720.0, 520.0), 99.0 * 60.0);
        assert_eq!(
            row_about(&w, "interview.m4a"),
            "12:40, Ready in about\u{a0}99\u{a0}min, 57%"
        );
        // no button waits with it: nothing to press until it is cleaned
        assert!(w.harness.query_by_label_contains("Save").is_none());
    }

    /// A save says how far it is, once: "Saving · 39%" (no time beside
    /// the percent to read as a third fact).
    #[test]
    fn a_save_says_how_far_it_is_once() {
        let (w, _) = files("saving");
        let about = row_about(&w, "lecture-03.mp4");
        assert!(about.starts_with("44:10, Saving, "), "{about}");
        assert!(
            about.ends_with('%') && !about.contains("\u{a0}%"),
            "{about}"
        );
        assert_eq!(about.matches(", ").count(), 2, "{about}");
    }

    #[test]
    fn leaving_the_page_pauses_what_plays() {
        let (mut w, log) = files("playing");
        w.harness.get_by_label("Live").click();
        w.harness.run_steps(3);
        assert_eq!(*w.page.borrow(), Page::Live);
        assert!(sent(&log, &Cmd::Pause), "{:?}", log.borrow());
    }

    #[test]
    fn a_failed_save_offers_its_way_forward() {
        // a folder that can't be written: choose another
        let (mut w, log) = files("no-write");
        w.harness
            .get_by_label("Choose another folder…: interview.m4a")
            .click();
        w.harness.run_steps(2);
        let id = log.borrow().iter().find_map(|c| match c {
            Cmd::SaveElsewhere(id) => Some(*id),
            _ => None,
        });
        assert!(id.is_some(), "{:?}", log.borrow());
        // a Save that may go next time: Save again
        let (mut w, log) = files("save-failed");
        w.harness.get_by_label("Save again: interview.m4a").click();
        w.harness.run_steps(2);
        assert!(
            log.borrow().iter().any(|c| matches!(c, Cmd::Save(_))),
            "{:?}",
            log.borrow()
        );
        // one that would fail the same way again (the encoder refuses the
        // format, the original can't be read): no Save again, the reason
        // on the row
        for name in ["save-encode", "save-read"] {
            let (w, _) = files(name);
            assert!(
                w.harness
                    .query_by_label("Save again: interview.m4a")
                    .is_none(),
                "{name}"
            );
        }
    }

    #[test]
    fn the_cards_send_their_commands() {
        let (mut w, log) = files("queue");
        w.harness.get_by_label("Medium").click();
        w.harness.run_steps(2);
        assert!(sent(&log, &Cmd::Strength(Strength::Medium)));
        let lecture = |log: &Log| {
            log.borrow().iter().find_map(|c| match c {
                Cmd::Select(id) => Some(*id),
                _ => None,
            })
        };
        w.harness.get_by_label("lecture-03.mp4").click();
        w.harness.run_steps(2);
        let id = lecture(&log).expect("selected");
        // the second file is cleaning: its × stops the pass
        w.harness.get_by_label("Remove lecture-03.mp4").click();
        w.harness.run_steps(2);
        assert!(sent(&log, &Cmd::Remove(id)), "{:?}", log.borrow());
        w.harness.get_by_label("Add files…").click();
        w.harness.run_steps(2);
        assert!(sent(&log, &Cmd::Choose));
        w.harness.get_by_label("Save all").click();
        w.harness.run_steps(2);
        assert!(sent(&log, &Cmd::SaveAll));
    }

    #[test]
    fn each_row_has_its_one_action() {
        let (mut w, log) = files("queue");
        w.harness
            .get_by_label("Save interview (cleaned).m4a")
            .click();
        w.harness.run_steps(2);
        assert!(log.borrow().iter().any(|c| matches!(c, Cmd::Save(_))));
        // a saved file: Show in folder (no Open)
        let (mut w, log) = files("all-saved");
        assert!(w.harness.query_by_label("Open").is_none());
        w.harness
            .get_by_label("Show in folder: voice-note.opus")
            .click();
        w.harness.run_steps(2);
        assert!(log
            .borrow()
            .iter()
            .any(|c| matches!(c, Cmd::ShowInFolder(_))));
        // a save that writes: Cancel, and no × until it is done
        let (w, _) = files("saving");
        assert!(w
            .harness
            .query_by_label("Cancel saving lecture-03 (cleaned).mp4")
            .is_some());
        assert!(w.harness.query_by_label("Remove lecture-03.mp4").is_none());
    }

    /// F4: one cleaned file left to save among files that need
    /// attention: Save all saves it (it was off).
    #[test]
    fn save_all_works_while_one_file_is_left_to_save() {
        let mut v = DemoFiles::preset("queue", 0.0, 3.3).view(0.0);
        assert!(v.files.len() >= 2);
        for f in v.files.iter_mut() {
            f.pass = Pass::Attention(Reason::Damaged);
            f.length = 10.0;
        }
        (v.files[0].pass, v.files[0].save) = (Pass::Cleaned, Save::No);
        let b = Batch::of(&v);
        assert!(b.save_all, "one file left to save");
        v.files[0].save = Save::Saved;
        assert!(!Batch::of(&v).save_all, "nothing left");
    }

    #[test]
    fn the_batch_buttons_show_when_they_help() {
        let has = |w: &testing::Window, l: &str| w.harness.query_by_label(l).is_some();
        // one file: as simple as before
        let (w, _) = files("cleaned");
        assert!(has(&w, "Add files…"));
        assert!(!has(&w, "Save all") && !has(&w, "Clear all"));
        // several left to save
        let (w, _) = files("queue");
        assert!(has(&w, "Save all") && has(&w, "Clear all"));
        let save_all = w.harness.get_by_label("Save all");
        assert!(!save_all.accesskit_node().is_disabled());
        let slots = (save_all.rect(), w.harness.get_by_label("Clear all").rect());
        // all saved: nothing left for Save all, which stays where it was,
        // off (A1: Clear all never slides under the pointer)
        let (w, _) = files("all-saved");
        let save_all = w.harness.get_by_label("Save all");
        assert!(save_all.accesskit_node().is_disabled());
        assert_eq!(
            (save_all.rect(), w.harness.get_by_label("Clear all").rect()),
            slots
        );
        // no file (round 3): no toolbar and no strength yet, only
        // "Choose files…", that files may be dropped, and the formats
        let (w, _) = files("live");
        assert!(!has(&w, "Add files…"));
        assert!(!has(&w, "Strength, for every file here"));
        assert!(has(&w, "Choose files…"));
        assert!(has(
            &w,
            "Or drop them here. Audio or video: WAV, MP3, M4A, FLAC, Opus, MP4, MKV and more."
        ));
    }

    /// The demo, its view changed by `edit` (states no board has).
    struct Edited(DemoFiles, Log, fn(&mut View));

    impl Source for Edited {
        fn send(&mut self, c: Cmd) {
            self.1.borrow_mut().push(c.clone());
            self.0.send(c);
        }
        fn view(&mut self, now: f64) -> View {
            let mut v = self.0.view(now);
            (self.2)(&mut v);
            v
        }
        fn picture(&self) -> Option<Arc<Sheet>> {
            self.0.picture()
        }
        fn busy(&self) -> bool {
            self.0.busy()
        }
    }

    #[test]
    fn a_long_files_action_keeps_clear_of_its_length() {
        // an hour-long recording damaged after an hour: the widest row
        // action there is; the action column grows for it
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let w = testing::at_60hz_with(
                crate::window::Link::recording(state(Scenario::OnInCall)),
                Page::Files,
                size,
                move |ps| {
                    let demo = DemoFiles::preset("damaged", 0.0, 3.3);
                    ps.files.source = Some(Box::new(Edited(demo, Default::default(), |v| {
                        if let Some(f) = v.files.get_mut(0) {
                            f.damaged = Some(3792.0);
                            f.length = 3792.0;
                            f.stated = Some(7960.0);
                        }
                    })));
                },
            );
            let pick = w.harness.get_by_label("interview.m4a").rect();
            let act = w
                .harness
                .get_by_label("Clean the first 1:03:12: interview.m4a")
                .rect();
            // the row: name and state, 8, the 44 px length, 8, the action
            let len_right = pick.right() + 8.0 + 44.0;
            assert!(
                act.left() >= len_right + 8.0 - 0.5,
                "{size:?} action {act:?} over the length (ends {len_right})"
            );
        }
    }

    #[test]
    fn clear_all_clears_at_once_and_can_be_undone() {
        // round 3: no question, even for a file whose cleaning isn't saved
        // (its Save hit a full disk); a toast can undo it (A1)
        let log: Log = Default::default();
        let l = log.clone();
        let mut w = testing::at_60hz_with(
            crate::window::Link::recording(state(Scenario::OnInCall)),
            Page::Files,
            egui::vec2(960.0, 640.0),
            move |ps| {
                let demo = DemoFiles::preset("all-saved", 0.0, 3.3);
                ps.files.source = Some(Box::new(Edited(demo, l.clone(), |v| {
                    v.files.truncate(2);
                    if let Some(f) = v.files.get_mut(1) {
                        f.pass = Pass::Attention(Reason::NoSpace);
                        f.save = Save::No;
                    }
                })));
            },
        );
        w.harness.run_steps(3);
        log.borrow_mut().clear();
        w.harness.get_by_label("Clear all").click();
        w.harness.run_steps(3);
        assert_eq!(*log.borrow(), vec![Cmd::ClearAll]);
        assert!(w.harness.query_by_label("Keep them").is_none());
        // "3 files cleared · Undo" (the demo clears its whole board)
        assert!(w.harness.query_by_label("interview.m4a").is_none());
        assert!(w.harness.query_by_label("3 files cleared").is_some());
        w.harness.get_by_label("Undo").click();
        w.harness.run_steps(3);
        assert_eq!(*log.borrow(), vec![Cmd::ClearAll, Cmd::UndoClear]);
        assert!(w.harness.query_by_label("Undo").is_none());
        assert!(w.harness.query_by_label("interview.m4a").is_some());
    }

    /// B10: the headline says what really happens: a file that waits
    /// names the one cleaning, and another file's failed Save is said up
    /// top.
    #[test]
    fn the_headline_names_what_really_happens() {
        crate::i18n::pin_english();
        let mut v = DemoFiles::preset("queue", 100.0, 3.3).view(100.0);
        let cleaning = v.files[1].name.clone();
        v.files[0].pass = Pass::Waiting;
        v.files[0].segments.clear();
        v.files[1].pass = Pass::Cleaning;
        let first = v.files[0].clone();
        v.selected = Some(first.id);
        assert_eq!(
            copy(&v, Some(&first), false, false),
            format!(
                "{} is waiting its turn: {cleaning} is cleaning.",
                first.name
            )
        );
        v.files[1].pass = Pass::Attention(Reason::NoSpace);
        assert_eq!(
            copy(&v, Some(&first), false, false),
            "There isn’t enough space on the disk."
        );
    }

    /// A2 on the page: after a strength change a saved row says at what
    /// strength it was saved and offers Save at the new one.
    #[test]
    fn a_saved_row_offers_save_at_a_new_strength() {
        crate::i18n::pin_english();
        let (mut w, log) = files("saved");
        assert!(w
            .harness
            .query_by_label("Show in folder: interview.m4a")
            .is_some());
        w.harness.get_by_label("Maximum").click();
        w.harness.run_steps(3);
        assert!(w
            .harness
            .query_by_label("Show in folder: interview.m4a")
            .is_none());
        assert!(w
            .harness
            .query_by_label("interview (cleaned).m4a was saved at Strong.")
            .is_some());
        let save = w.harness.get_by_label("Save interview (cleaned).m4a");
        save.click();
        w.harness.run_steps(2);
        assert!(log.borrow().iter().any(|c| matches!(c, Cmd::Save(_))));
    }

    /// What a strength does is the strength's own tooltip and description:
    /// no sentence about it stands on the page, at any strength or size.
    #[test]
    fn what_the_strength_does_is_not_a_standing_sentence() {
        crate::i18n::pin_english();
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let (mut w, _) = files_at("cleaned", size);
            for step in ["Strong", "Gentle", "Maximum"] {
                w.harness.get_by_label(step).click();
                w.harness.run_steps(3);
                let s = model::Strength::ALL
                    .into_iter()
                    .find(|s| s.word() == step)
                    .unwrap();
                for line in [s.files_line(), s.line()] {
                    assert!(
                        w.harness.query_by_label(&line).is_none(),
                        "{size:?} {step}: {line}"
                    );
                }
                let said = w
                    .harness
                    .query_by(|n| n.description().is_some_and(|d| d == s.files_line()));
                assert!(said.is_some(), "{size:?} {step}: the strength says it");
            }
        }
    }

    /// F16 (A111): the boards' recordings start on speech, so a finished
    /// file at 0:00 shows its cleaned voice at once, not a silent intro;
    /// and the picture is the same every time (the stills are compared).
    #[test]
    fn the_boards_recordings_start_on_speech() {
        use super::model::Source;
        use crate::miccheck::spectrum::{COLUMN_BYTES, ROWS};
        let mut d = DemoFiles::preset("cleaned-start", 0.0, 3.3);
        let v = d.view(0.0);
        assert_eq!(v.position, 0.0);
        let sheet = d.picture().expect("a picture");
        // 12:40 in columns of 64 hops, all of them cleaned
        assert_eq!((sheet.cols(), sheet.lit()), (713, 713));
        let mut levels = Vec::new();
        sheet.levels(0.0, &mut levels);
        let cleaned = |col: usize| &levels[col * COLUMN_BYTES + ROWS..(col + 1) * COLUMN_BYTES];
        assert!(cleaned(0).iter().any(|l| *l > 100), "{:?}", cleaned(0));
        let voiced = (0..60)
            .filter(|c| cleaned(*c).iter().any(|l| *l > 100))
            .count();
        assert!(voiced >= 30, "{voiced} of the first 60 columns voiced");
        // the room is in the original alone
        let top = |col: usize| levels[col * COLUMN_BYTES + ROWS - 1];
        assert!((0..60).all(|c| top(c) > 0 && cleaned(c)[ROWS - 1] < top(c)));
        // the same picture again
        let mut again = DemoFiles::preset("cleaned-start", 50.0, 3.3);
        again.view(50.0);
        assert_eq!(again.picture().as_deref(), Some(&*sheet));
        // a file half cleaned: half of the lane; one that waits: none yet
        let mut d = DemoFiles::preset("five", 0.0, 3.3);
        let v = d.view(0.0);
        let r = v.row().unwrap();
        let sheet = d.picture().expect("a picture");
        let share = sheet.lit() as f64 / sheet.cols() as f64;
        assert!((share - r.cleaned() / r.length).abs() < 0.01, "{share}");
    }

    /// A109, A113: while the selected file cleans, the player's row says
    /// how far it is ("Cleaning… 38% · ready in about 6 min", its row in
    /// the list may be scrolled away), whole, at every size.
    #[test]
    fn how_far_shows_while_the_file_cleans() {
        crate::i18n::pin_english();
        fn edit(v: &mut View) {
            v.strength = Strength::Maximum;
        }
        let mut v = DemoFiles::preset("five", 0.0, 3.3).view(0.0);
        edit(&mut v);
        let r = v.row().unwrap().clone();
        assert_eq!(r.pass, Pass::Cleaning);
        let progress = cleaning_line(&v, &r).expect("cleaning");
        assert!(
            progress
                .one
                .starts_with("Cleaning… 38% · ready in about\u{a0}"),
            "{progress:?}"
        );
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let log: Log = Default::default();
            let l = log.clone();
            let w = testing::at_60hz_with(
                crate::window::Link::recording(state(Scenario::OnInCall)),
                Page::Files,
                size,
                move |ps| {
                    let demo = DemoFiles::preset("five", 0.0, 3.3);
                    ps.files.source = Some(Box::new(Edited(demo, l.clone(), edit)));
                },
            );
            // the cleaned lane's caption says it: whole where the lane
            // has the room, else its first half
            let area = w.harness.get_by_label("Position in the file").rect();
            let head = progress.two.as_ref().map(|p| p.0.clone());
            let said = painted(&w, Rect::EVERYTHING)
                .into_iter()
                .find(|(t, _)| *t == progress.one || Some(t) == head.as_ref())
                .unwrap_or_else(|| panic!("{size:?}: how far it is"));
            assert!(area.contains_rect(said.1), "{size:?}: {said:?}");
            assert!(said.1.top() > area.center().y, "{size:?}: the lower lane");
            // and the player row has no line of its own for it
            assert!(w.harness.query_by_label(&progress.one).is_none());
        }
        // not cleaning: nothing to say
        let done = DemoFiles::preset("cleaned", 0.0, 3.3).view(0.0);
        assert_eq!(cleaning_line(&done, done.row().unwrap()), None);
    }

    /// A114 N-P3: under a minute left, said as such, whole and in halves.
    #[test]
    fn under_a_minute_left_is_said_as_such() {
        crate::i18n::pin_english();
        let v = DemoFiles::preset("five", 0.0, 3.3).view(0.0);
        let mut r = v.row().unwrap().clone();
        assert_eq!(r.pass, Pass::Cleaning);
        r.time_left = Some(30.0);
        let p = cleaning_line(&v, &r).expect("cleaning");
        assert_eq!(p.one, "Cleaning… 38% · ready in under 1 min");
        assert_eq!(
            p.two,
            Some(("Cleaning… 38%".into(), "Ready in under 1 min".into()))
        );
        // the threshold: under a minute up to 60 s, then whole minutes,
        // here and in the row
        let cap = |r: &Row| row_look(r, true, false, "").cap;
        r.time_left = Some(59.9);
        let p = cleaning_line(&v, &r).expect("cleaning");
        assert_eq!(p.one, "Cleaning… 38% · ready in under 1 min");
        assert_eq!(cap(&r), "Ready in under 1 min");
        r.time_left = Some(60.0);
        let p = cleaning_line(&v, &r).expect("cleaning");
        assert_eq!(p.one, "Cleaning… 38% · ready in about\u{a0}1\u{a0}min");
        assert_eq!(
            p.two,
            Some((
                "Cleaning… 38%".into(),
                "Ready in about\u{a0}1\u{a0}min".into()
            ))
        );
        assert_eq!(cap(&r), "Ready in about\u{a0}1\u{a0}min");
        // no estimate yet: how far alone, nothing to split
        r.time_left = None;
        let p = cleaning_line(&v, &r).expect("cleaning");
        assert_eq!((p.one.as_str(), p.two), ("Cleaning… 38%", None));
    }

    /// A114 N-P2: "about 6 min" never breaks inside (its spaces don't
    /// break), even where the line has room only for a word or two.
    #[test]
    fn about_minutes_stays_on_one_line() {
        crate::i18n::pin_english();
        let ctx = egui::Context::default();
        crate::ui::fonts::install(&ctx);
        let t = minutes(360.0);
        assert_eq!(t, "about\u{a0}6\u{a0}min");
        let _ = ctx.run(Default::default(), |ctx| {
            let line = tr!("files-ready-in", time = t.clone());
            let b = text::layout(ctx, &line, Type::CAP, Color32::WHITE, 40.0);
            assert!(
                b.text().lines().any(|l| l == t),
                "kept whole: {:?}",
                b.text()
            );
            // the space isn't drawn as a missing glyph: as wide as a space
            let (sp, nb) = (
                text::width(ctx, "a b", Type::CAP),
                text::width(ctx, "a\u{a0}b", Type::CAP),
            );
            assert!((sp - nb).abs() < 0.5, "{sp} {nb}");
        });
    }

    /// A114 SF-P1: a file paused for a call keeps how far it got, under
    /// the player and in its row.
    #[test]
    fn a_paused_file_keeps_how_far_it_got() {
        crate::i18n::pin_english();
        let v = DemoFiles::preset("yield", 0.0, 3.3).view(0.0);
        let r = v.row().unwrap().clone();
        assert_eq!(r.pass, Pass::Yielded);
        let pct = percent((r.cleaned() / r.length) as f32);
        let p = cleaning_line(&v, &r).expect("paused partway");
        assert_eq!(
            p,
            Progress {
                one: format!("Paused at {pct} · continues after your call"),
                two: Some((
                    format!("Paused at {pct}"),
                    "Continues after your call".into()
                )),
            }
        );
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let (w, _) = files_at("yield", size);
            shown_in_the_caption(&w, &p, size);
            let row = w.harness.get_by_label("lecture-03.mp4");
            let about = row.accesskit_node().description().unwrap_or_default();
            assert!(about.contains(&pct), "{size:?}: {about}");
        }
        // paused before it began: nothing to tell
        let mut r = r;
        r.segments.clear();
        assert_eq!(cleaning_line(&v, &r), None);
    }

    /// How far, in the cleaned lane's caption: whole, or its first half
    /// where the lane is too narrow for it. Where it stands.
    fn shown_in_the_caption(w: &testing::Window, p: &Progress, size: egui::Vec2) -> Rect {
        let area = w.harness.get_by_label("Position in the file").rect();
        let head = p.two.as_ref().map(|t| t.0.clone());
        let (said, at) = painted(w, Rect::EVERYTHING)
            .into_iter()
            .find(|(t, _)| *t == p.one || Some(t) == head.as_ref())
            .unwrap_or_else(|| panic!("{size:?}: {p:?} is not on the page"));
        assert!(at.height() <= Type::CAP.line + 0.5, "{size:?}: {said}");
        assert!(area.contains_rect(at), "{size:?}: {at:?} in {area:?}");
        assert!(at.top() > area.center().y, "{size:?}: the lower lane");
        at
    }

    /// A114 SF-P1: a file that stopped partway and waits its turn says how
    /// far it got and what cleans it on: playing it (it cleans first,
    /// `the_played_file_goes_first_and_the_other_waits_stopped`). While a
    /// call holds the cleaning, playing wouldn't: how far alone.
    #[test]
    fn a_stopped_file_says_how_far_and_that_play_goes_on() {
        crate::i18n::pin_english();
        fn edit(v: &mut View) {
            let sel = v.selected;
            for f in v.files.iter_mut().filter(|f| Some(f.id) == sel) {
                f.pass = Pass::Waiting;
            }
        }
        let mut v = DemoFiles::preset("five", 0.0, 3.3).view(0.0);
        edit(&mut v);
        let r = v.row().unwrap().clone();
        assert!(r.cleaned() > 0.0, "partway");
        let pct = percent((r.cleaned() / r.length) as f32);
        let p = cleaning_line(&v, &r).expect("stopped partway");
        assert_eq!(
            p,
            Progress {
                one: format!("Waiting at {pct} · play it to continue"),
                two: Some((format!("Waiting at {pct}"), "Play it to continue".into())),
            }
        );
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let log: Log = Default::default();
            let l = log.clone();
            let w = testing::at_60hz_with(
                crate::window::Link::recording(state(Scenario::OnInCall)),
                Page::Files,
                size,
                move |ps| {
                    let demo = DemoFiles::preset("five", 0.0, 3.3);
                    ps.files.source = Some(Box::new(Edited(demo, l.clone(), edit)));
                },
            );
            shown_in_the_caption(&w, &p, size);
        }
        // during a call
        let mut call = v.clone();
        let other = call.files.iter_mut().find(|f| Some(f.id) != v.selected);
        other.expect("another file").pass = Pass::Yielded;
        assert_eq!(
            cleaning_line(&call, &r),
            Some(Progress {
                one: format!("Waiting at {pct}"),
                two: None
            })
        );
        // stopped before it began: nothing to tell
        let mut r = r;
        r.segments.clear();
        assert_eq!(cleaning_line(&v, &r), None);
        // the dev state: stopped partway; Play cleans it on
        let mut d = DemoFiles::preset("stopped-partway", 0.0, 3.3);
        let v = d.view(3.3);
        let r = v.row().unwrap();
        assert_eq!(r.pass, Pass::Waiting);
        let one = cleaning_line(&v, r).expect("partway").one;
        assert!(one.ends_with(" · play it to continue"), "{one}");
        d.send(Cmd::Play);
        let v = d.view(4.0);
        assert_eq!(v.row().unwrap().pass, Pass::Cleaning);
    }

    /// How far a file is stands in the cleaned lane's caption, and the
    /// track picker keeps its long label at every size, on a line of its
    /// own under the row, ending where the row does.
    #[test]
    fn how_far_leaves_the_picker_its_room() {
        crate::i18n::pin_english();
        fn edit(v: &mut View) {
            for f in &mut v.files {
                (f.tracks, f.dropped, f.track) = (2, 0, 1);
                f.titles = vec!["Desktop Audio".into(), "Mic/Aux".into()];
            }
        }
        let mut v = DemoFiles::preset("five", 0.0, 3.3).view(0.0);
        edit(&mut v);
        let p = cleaning_line(&v, v.row().unwrap()).expect("cleaning");
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let log: Log = Default::default();
            let l = log.clone();
            let w = testing::at_60hz_with(
                crate::window::Link::recording(state(Scenario::OnInCall)),
                Page::Files,
                size,
                move |ps| {
                    let demo = DemoFiles::preset("five", 0.0, 3.3);
                    ps.files.source = Some(Box::new(Edited(demo, l.clone(), edit)));
                },
            );
            shown_in_the_caption(&w, &p, size);
            let short = Button::secondary("Track 2")
                .sm()
                .icon_after(Icon::Chevron)
                .width(&w.harness.ctx);
            let picker = w.harness.get_by_label("The sound track to clean").rect();
            assert!(picker.width() > short + 0.5, "{size:?}: {picker:?}");
            let hold = w.harness.get_by_label("Hold to hear the original").rect();
            assert_eq!(picker.height(), hold.height(), "{size:?}: one size");
            assert_eq!(picker.top(), hold.bottom() + replay::GAP_Y, "{size:?}");
            assert!((picker.right() - hold.right()).abs() < 0.5, "{size:?}");
            let add = w.harness.get_by_label("Add files…").rect();
            assert!(picker.bottom() + PLAYER_PAD < add.top(), "{size:?}");
        }
    }

    /// A104 SF-F9: what the strength does is the strength's own tooltip:
    /// that Maximum silences the pauses is said there.
    #[test]
    fn the_strengths_tooltip_says_what_it_does() {
        crate::i18n::pin_english();
        let max = strength_tip(Strength::Maximum, false);
        assert_eq!(
            max,
            "Removes as much noise as it can. The pauses between words become silent."
        );
        // one Maximum sentence on every page (the same engine setting)
        assert_eq!(max, Strength::Maximum.line());
        assert_eq!(
            strength_tip(Strength::Strong, true),
            "Most of the noise is gone. Files start at Strong to keep the room tone.\n\
             For this file and the files you add next."
        );
        // A108: the strength's tooltip and description are strength_tip's,
        // and the line is nowhere else on the page
        let (w, _) = files_at("cleaned", egui::vec2(720.0, 520.0));
        assert!(w
            .harness
            .query_by_label_contains("Files start at Strong")
            .is_none());
        let described = w.harness.query_by(|n| {
            n.description()
                .is_some_and(|d| d == strength_tip(Strength::Strong, false))
        });
        assert!(described.is_some(), "the strength says what it does");
    }

    /// A104 SF-Q1: while "Save all and quit" waits, the title says so and
    /// Cancel keeps HushMic.
    #[test]
    fn quitting_says_so_and_can_be_cancelled() {
        crate::i18n::pin_english();
        let (mut w, log) = files("quitting");
        assert!(w
            .harness
            .query_by_label("HushMic quits once 2 files are saved.")
            .is_some());
        w.harness.get_by_label("Cancel").click();
        w.harness.run_steps(2);
        assert!(sent(&log, &Cmd::CancelQuit), "{:?}", log.borrow());
    }

    /// A104 N-Y1: the call's title, narrow: whole over two lines, not cut
    /// in the second sentence.
    #[test]
    fn the_calls_title_is_whole_when_narrow() {
        crate::i18n::pin_english();
        let (w, _) = files_at("yield", egui::vec2(960.0, 640.0));
        assert!(w
            .harness
            .query_by_label("Paused while Zoom uses your microphone. Your call comes first.")
            .is_some());
        let (w, _) = files_at("yield", egui::vec2(720.0, 520.0));
        let t = w
            .harness
            .get_by_label("Paused while Zoom uses your microphone. Your call comes first.")
            .rect();
        assert!(
            t.height() > Type::H.line + 1.0 && t.height() <= 2.0 * Type::H.line + 0.5,
            "two lines: {t:?}"
        );
    }

    /// N-F5: the dropped-tracks line is whole, its advice included, at
    /// every size: on a line of its own under the row of controls, from
    /// the row's left edge, clear of the files area's line.
    #[test]
    fn the_dropped_tracks_line_keeps_its_advice_when_narrow() {
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let w = testing::at_60hz_with(
                crate::window::Link::recording(state(Scenario::OnInCall)),
                Page::Files,
                size,
                move |ps| {
                    let demo = DemoFiles::preset("video", 0.0, 3.3);
                    ps.files.source = Some(Box::new(Edited(demo, Default::default(), |v| {
                        for f in v.files.iter_mut() {
                            f.tracks = 1;
                            f.dropped = 2;
                        }
                    })));
                },
            );
            let hold = w.harness.get_by_label(HOLD).rect();
            let first = w.harness.get_by_label("Gentle").rect();
            let texts = painted(&w, Rect::EVERYTHING);
            let (said, at) = texts
                .iter()
                .find(|(t, _)| t.starts_with("Only the first of 3"))
                .unwrap_or_else(|| panic!("{size:?} {texts:?}"));
            assert_eq!(
                said.replace('\n', " "),
                "Only the first of 3 sound tracks is kept. Install ffmpeg to keep them all.",
                "{size:?}"
            );
            // whole on its line or wrapped, not cut short with "…"
            // (whose galley still holds the whole text)
            let natural = text::width(&w.harness.ctx, said, Type::CAP);
            let room = hold.right() - first.left();
            assert!(
                natural <= room || at.height() > 1.5 * Type::CAP.line,
                "{size:?} {at:?}"
            );
            assert!(at.right() <= hold.right() + 0.5, "{size:?} {at:?}");
            // under the row, on its left edge
            assert!(at.top() >= hold.bottom() + 4.0, "{size:?} {at:?}");
            assert!((at.left() - first.left()).abs() < 3.0, "{size:?} {at:?}");
            let add = w.harness.get_by_label("Add files…").rect();
            assert!(at.bottom() <= add.top() - 12.0, "{size:?} {at:?} {add:?}");
        }
    }

    /// The texts painted in `area`: each one's words and where it starts.
    fn painted(w: &testing::Window, area: Rect) -> Vec<(String, Rect)> {
        w.harness
            .output()
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => {
                    // where its glyphs are (a right-aligned galley's rows
                    // end at its x = 0)
                    let r = t.galley.rect.translate(t.pos.to_vec2());
                    area.contains(r.center())
                        .then(|| (t.galley.text().to_string(), r))
                }
                _ => None,
            })
            .collect()
    }

    /// Every rectangle painted in the last frame (nested shapes too).
    fn painted_rects(w: &testing::Window) -> Vec<egui::epaint::RectShape> {
        fn walk(s: &egui::Shape, out: &mut Vec<egui::epaint::RectShape>) {
            match s {
                egui::Shape::Rect(r) => out.push(r.clone()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for c in &w.harness.output().shapes {
            walk(&c.shape, &mut out);
        }
        out
    }

    /// A116 N-A1 (UX panel round 2, A18): the selected row is the kit's
    /// selection, its fill in its hairline (widgets::selected), in both
    /// themes; a row that isn't selected has neither.
    #[test]
    fn the_selected_row_is_drawn_as_the_selection() {
        crate::i18n::pin_english();
        use crate::ui::Theme;
        for theme in [Theme::Dark, Theme::Light] {
            let (mut w, _) = files_at("five", egui::vec2(960.0, 640.0));
            crate::ui::set_theme(&w.harness.ctx, theme);
            w.harness.run_steps(2);
            let pal = crate::ui::kit(&w.harness.ctx).pal;
            assert_eq!(pal.is_dark(), theme.is_dark());
            let rects = painted_rects(&w);
            // the row's own rectangle: a full row high, under its name
            let row = |name: &str| -> Vec<egui::epaint::RectShape> {
                let at = w.harness.get_by_label(name).rect().center();
                rects
                    .iter()
                    .filter(|r| r.rect.contains(at) && (r.rect.height() - ROW_H).abs() < 0.5)
                    .cloned()
                    .collect()
            };
            let selection = |r: &egui::epaint::RectShape| {
                r.fill == pal.selected_fill()
                    && r.stroke.color == pal.selected_line()
                    && r.stroke.width == 1.0
            };
            let sel = row("lecture-03.mp4");
            assert!(
                sel.iter().any(selection),
                "{theme:?}: the selected row is the selection: {sel:?}"
            );
            let other = row("voice-note.opus");
            assert!(
                !other
                    .iter()
                    .any(|r| r.fill == pal.selected_fill() || selection(r)),
                "{theme:?}: another row is not: {other:?}"
            );
        }
    }

    /// B13: a long name is cut in its middle and ends where the state
    /// column starts, the same for every row; a saved row has its check;
    /// how far reads "2%".
    #[test]
    fn rows_keep_their_columns_and_long_names_their_ends() {
        crate::i18n::pin_english();
        let ctx = egui::Context::default();
        crate::ui::fonts::install(&ctx);
        let long = "podcast-episode-12-final-mix-take-3.mp3";
        let cut = middle(&ctx, long, Type::LIST, 160.0);
        assert!(cut.contains('…') && cut.ends_with("take-3.mp3"), "{cut}");
        assert!(text::width(&ctx, &cut, Type::LIST) <= 160.0);
        assert_eq!(middle(&ctx, "a.wav", Type::LIST, 160.0), "a.wav");
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let (w, _) = files_at("five", size);
            let rows: Vec<Rect> = ["interview.m4a", "voice-note.opus", "lecture-03.mp4"]
                .iter()
                .map(|n| w.harness.get_by_label(n).rect())
                .collect();
            let texts = painted(&w, rows[0].union(rows[2]));
            // the state words start at one x on every row
            let starts: Vec<f32> = ["Saved", "Waiting", "Ready in"]
                .iter()
                .filter_map(|word| {
                    texts
                        .iter()
                        .find(|(t, _)| t.starts_with(word))
                        .map(|(_, r)| r.left())
                })
                .collect();
            assert!(starts.len() >= 2, "{size:?} {texts:?}");
            // the saved one's words sit after its check (18 px)
            let base = starts[1];
            assert!(
                (starts[0] - (base + 18.0)).abs() < 0.5,
                "{size:?} {starts:?}"
            );
            for x in &starts[2..] {
                assert!((x - base).abs() < 0.5, "{size:?} {starts:?}");
            }
            // a percent without a space
            assert!(texts
                .iter()
                .any(|(t, _)| t.ends_with('%') && !t.contains(' ')));
        }
        // the cut name keeps its end in a narrow window
        let (w, _) = files_at("five", egui::vec2(720.0, 520.0));
        let row = w
            .harness
            .get_by_label("podcast-episode-12-final-mix.mp3")
            .rect();
        let texts = painted(&w, row);
        let name = texts
            .iter()
            .find(|(t, _)| t.ends_with(".mp3"))
            .map(|(t, _)| t.clone())
            .unwrap_or_default();
        assert!(name.contains('…'), "{texts:?}");
    }

    /// The action column keeps its width through the usual states (Save,
    /// Cancel while it saves, Show in folder: an icon each); an action in
    /// words takes the room it needs from the names.
    #[test]
    fn the_action_column_does_not_move() {
        crate::i18n::pin_english();
        // where a row's name and state end: the action column's start
        let x_of = |name: &'static str, file: &str| {
            let (w, _) = files(name);
            w.harness.get_by_label(file).rect().right()
        };
        let cleaned = x_of("cleaned", "interview.m4a");
        assert_eq!(x_of("saved", "interview.m4a"), cleaned, "Show in folder");
        assert!(
            x_of("no-write", "interview.m4a") < cleaned,
            "Choose another folder…"
        );
        // one file alone, start to end: Save, Cancel, Show in folder
        let (w, _) = files("saving");
        let cancel = w
            .harness
            .get_by_label("Cancel saving lecture-03 (cleaned).mp4");
        let (w2, _) = files("cleaned");
        let save = w2.harness.get_by_label("Save interview (cleaned).m4a");
        assert_eq!(cancel.rect().width(), save.rect().width(), "an icon each");
    }

    /// The picture is the file's time line: both lanes are the whole
    /// file, and a click on either seeks to where it lands; the time
    /// stands in the picture's corner, not in a line of its own.
    #[test]
    fn a_click_on_either_lane_seeks_there() {
        crate::i18n::pin_english();
        let (mut w, log) = files("cleaned");
        let line = w.harness.get_by_label("Position in the file");
        assert_eq!(
            line.accesskit_node().description().as_deref(),
            Some("interview.m4a: the original above, the cleaned sound below.")
        );
        assert_eq!(
            line.accesskit_node().value().as_deref(),
            Some("3:53 of 12:40")
        );
        let area = line.rect();
        // two lanes, one over the other, between the card's paddings
        let hold = w.harness.get_by_label(HOLD).rect();
        // (on the left edge of the row of controls under them)
        let first = w.harness.get_by_label("Gentle").rect();
        assert!((area.left() - first.left()).abs() < 3.0, "{first:?}");
        assert!(area.height() > 2.0 * spectro::LANE_MIN, "{area:?}");
        assert!(area.bottom() < hold.top(), "{area:?} over {hold:?}");
        let clock = painted(&w, Rect::EVERYTHING)
            .into_iter()
            .find(|(t, _)| t == "3:53 / 12:40")
            .expect("where the file is, and how long it is")
            .1;
        // in the upper lane's right corner
        assert!(area.contains_rect(clock), "{clock:?} in {area:?}");
        assert!(clock.bottom() < area.top() + 30.0, "{clock:?}");
        assert!(area.right() - clock.right() < 16.0, "{clock:?}");
        for (x, y) in [(0.25, 0.15), (0.75, 0.9)] {
            log.borrow_mut().clear();
            let at = area.min + area.size() * egui::vec2(x, y);
            for pressed in [true, false] {
                let events = &mut w.harness.input_mut().events;
                events.push(egui::Event::PointerMoved(at));
                events.push(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
                w.harness.run_steps(2);
            }
            let to = log.borrow().iter().find_map(|c| match c {
                Cmd::Seek { to, .. } => Some(*to),
                _ => None,
            });
            let want = f64::from(x) * 760.0;
            assert!(
                to.is_some_and(|to| (to - want).abs() < 2.0),
                "{to:?}, not {want}"
            );
        }
    }

    /// The row under the picture is Live's: the file's strength leads
    /// it, Play and Hold end it, all 32 px high on the lanes' two edges.
    /// Play is a button like its neighbour, with its word; the toolbar
    /// over the rows holds the files' buttons only.
    #[test]
    fn the_row_under_the_picture_is_lives_row() {
        crate::i18n::pin_english();
        use egui::accesskit::Role;
        for size in [
            egui::vec2(1168.0, 704.0),
            egui::vec2(960.0, 640.0),
            egui::vec2(720.0, 520.0),
        ] {
            let (mut w, log) = files_at("cleaned", size);
            let lanes = w.harness.get_by_label("Position in the file").rect();
            let first = testing::by_role(&w.harness, "Gentle", Role::RadioButton).rect();
            let last = testing::by_role(&w.harness, "Maximum", Role::RadioButton).rect();
            let play = testing::by_role(&w.harness, "Play", Role::Button).rect();
            let hold = w.harness.get_by_label(HOLD).rect();
            assert!(
                (first.left() - lanes.left()).abs() < 3.0,
                "{size:?}: {first:?}"
            );
            assert!(
                (hold.right() - lanes.right()).abs() < 0.5,
                "{size:?}: {hold:?}"
            );
            assert_eq!((play.height(), hold.height()), (32.0, 32.0), "{size:?}");
            assert!((hold.left() - play.right() - 8.0).abs() < 0.1, "{size:?}");
            assert_eq!(play.top(), hold.top(), "{size:?}");
            assert!(play.top() >= lanes.bottom() + replay::PAD_TOP, "{size:?}");
            assert!((play.center().y - first.center().y).abs() < 0.5, "{size:?}");
            assert!(play.left() > last.right() + 8.0, "{size:?}");
            // its word shows at every size the window can have
            let labelled = play_width(&w.harness.ctx);
            assert!((play.width() - labelled).abs() < 0.1, "{size:?}: {play:?}");
            assert!(play.width() > 2.0 * play.height(), "{size:?}: {play:?}");
            // under them the toolbar: no strength in it
            let add = w.harness.get_by_label("Add files…").rect();
            assert!(add.top() > hold.bottom() + PLAYER_PAD, "{size:?}");
            let steps = w.harness.query_all_by_role(Role::RadioButton).count();
            assert_eq!(steps, 4, "{size:?}");
            // Play plays, and the strength is this file's
            w.harness.get_by_label("Play").click();
            w.harness.run_steps(3);
            assert!(sent(&log, &Cmd::Play), "{size:?}");
            testing::by_role(&w.harness, "Gentle", Role::RadioButton).click();
            w.harness.run_steps(2);
            assert!(sent(&log, &Cmd::Strength(Strength::Gentle)), "{size:?}");
        }
        // Pause takes Play's place and width
        let (w, _) = files("playing");
        let pause = testing::by_role(&w.harness, "Pause", Role::Button).rect();
        assert!((pause.width() - play_width(&w.harness.ctx)).abs() < 0.1);
        assert!(w.harness.query_by_label("Play").is_none());
    }

    /// The list takes what its rows need, with its toolbar up to two
    /// fifths of the card, and then scrolls; the lanes take all the rest,
    /// with no most. Lanes are never under their floor: the list gives
    /// way to them, down to two rows.
    #[test]
    fn the_lanes_and_the_list_share_the_height() {
        for (compact, room, player, files) in [
            (false, 584.0, 54.0, 1),
            (false, 584.0, 54.0, 5),
            (false, 584.0, 54.0, 20),
            (false, 1100.0, 54.0, 3),
            (false, 520.0, 54.0, 5),
            (false, 520.0, 200.0, 5),
            (true, 416.0, 54.0, 1),
            (true, 416.0, 54.0, 5),
            (true, 416.0, 94.0, 5),
            (true, 416.0, 180.0, 5),
        ] {
            let case = format!("{compact} {room} {player} {files}");
            let (lane, rows) = share(compact, room, player, files);
            let least = LIST_ROWS_MIN.min(files);
            let list = |rows: usize| LIST_FRAME + rows as f32 * ROW_H;
            let used =
                |lane: f32, rows: usize| Sizes::of(compact, lane).height() + player + list(rows);
            assert!((least..=files).contains(&rows), "{case}: {rows} rows");
            assert!(lane >= LANE_FLOOR, "{case}: lanes of {lane}");
            if lane > LANE_FLOOR {
                // the card is full to the pixel the two lanes share
                let left = room - used(lane, rows);
                assert!((0.0..2.0).contains(&left), "{case}: {left} left");
            } else {
                assert_eq!(rows, least, "{case}: the list gave way first");
            }
            if rows > least {
                assert!(list(rows) <= LIST_SHARE * room, "{case}: {rows} rows");
            }
            if rows < files {
                // it scrolls: one row more would pass the list's share, or
                // take the lanes under their floor
                let over = list(rows + 1) > LIST_SHARE * room;
                let low = used(LANE_FLOOR, rows + 1) > room;
                assert!(over || low, "{case}: {rows} of {files} rows");
            }
        }
        // one file in a tall card: the lanes have no most
        assert!(share(false, 1100.0, 54.0, 1).0 > 2.0 * spectro::LANE_MAX);
        // on the page: the card reaches the page's end whatever is in it,
        // and nothing in it is left empty
        let lane_of = |w: &testing::Window, compact| {
            let z = Sizes::of(compact, 0.0);
            let area = w.harness.get_by_label("Position in the file").rect();
            (area.height() - z.lane_gap) / 2.0
        };
        for (size, compact, pb) in [
            (egui::vec2(1168.0, 704.0), false, 32.0),
            (egui::vec2(960.0, 640.0), false, 32.0),
            (egui::vec2(720.0, 520.0), true, 24.0),
        ] {
            // the card (the tallest thing drawn in the page's column)
            // ends at the page's bottom padding
            let card_end = |w: &testing::Window| {
                let add = w.harness.query_by_label("Add files…").map(|n| n.rect());
                let x = add.map_or(size.x / 2.0, |a| a.center().x);
                painted_rects(w)
                    .iter()
                    .filter(|r| r.rect.x_range().contains(x) && r.rect.height() > 200.0)
                    .filter(|r| r.rect.height() < size.y - 60.0)
                    .map(|r| r.rect.bottom())
                    .fold(0.0, f32::max)
            };
            let end = size.y - pb;
            for name in ["empty", "cleaned", "five", "format"] {
                let (w, _) = files_at(name, size);
                let at = card_end(&w);
                assert!((at - end).abs() < 1.0, "{size:?} {name}: ends at {at}");
            }
            // one file: its row ends the card, and the lanes have the rest
            let (w, _) = files_at("cleaned", size);
            let row = w.harness.get_by_label("interview.m4a").rect();
            let under = end - (row.bottom() + LIST_PAD);
            assert!(
                (0.0..4.0).contains(&under),
                "{size:?}: {under} under {row:?}"
            );
            let one = lane_of(&w, compact);
            assert!(one >= 110.0, "{size:?}: {one}");
            if !compact {
                assert!(one > spectro::LANE_MAX, "{size:?}: {one}");
            }
            // five: the list scrolls, the lanes stay a picture, and the
            // selected file's row is in view
            let (w, _) = files_at("five", size);
            let lane = lane_of(&w, compact);
            assert!(lane >= 90.0 && lane < one, "{size:?}: {lane}");
            let bar = w
                .harness
                .query_by_role_and_label(egui::accesskit::Role::ScrollBar, "Files");
            assert!(bar.is_some(), "{size:?}: lanes of {lane}");
            let add = w.harness.get_by_label("Add files…").rect();
            let shown = w.harness.get_by_label("lecture-03.mp4").rect();
            assert!(
                shown.top() >= add.bottom() + LIST_GAP - 0.5,
                "{size:?}: {shown:?}"
            );
            assert!(
                shown.bottom() <= end - LIST_PAD + 0.5,
                "{size:?}: {shown:?}"
            );
        }
    }

    /// B9: a note about a drop shows as a toast for a few seconds, with
    /// no button.
    #[test]
    fn a_drops_note_shows_for_a_moment() {
        crate::i18n::pin_english();
        let mut w = testing::at_60hz_with(
            crate::window::Link::recording(state(Scenario::OnInCall)),
            Page::Files,
            egui::vec2(960.0, 640.0),
            move |ps| {
                let demo = DemoFiles::preset("queue", 0.0, 3.3);
                ps.files.source = Some(Box::new(Edited(demo, Default::default(), |v| {
                    v.note = Some(("notes.txt isn’t an audio or video file.".into(), 1));
                })));
            },
        );
        w.harness.run_steps(3);
        assert!(w
            .harness
            .query_by_label("notes.txt isn’t an audio or video file.")
            .is_some());
        assert!(w.harness.query_by_label("Undo").is_none());
        // still there after 6 s (F4: about 8 s, as the toast)
        w.harness.run_steps(60 * 6);
        assert!(w
            .harness
            .query_by_label("notes.txt isn’t an audio or video file.")
            .is_some());
        w.harness.run_steps(60 * 3);
        assert!(w
            .harness
            .query_by_label("notes.txt isn’t an audio or video file.")
            .is_none());
    }

    /// The toast goes after about 8 s; Undo is gone with it.
    #[test]
    fn the_clear_all_toast_goes_after_a_moment() {
        let mut w = testing::at_60hz_with(
            crate::window::Link::recording(state(Scenario::OnInCall)),
            Page::Files,
            egui::vec2(960.0, 640.0),
            move |ps| {
                ps.files.source = Some(Box::new(DemoFiles::preset("queue", 0.0, 3.3)));
            },
        );
        w.harness.run_steps(3);
        w.harness.get_by_label("Clear all").click();
        w.harness.run_steps(3);
        assert!(w.harness.query_by_label("Undo").is_some());
        // 60 frames a second
        w.harness.run_steps(60 * 9);
        assert!(w.harness.query_by_label("Undo").is_none());
    }

    #[test]
    fn an_empty_page_has_one_action() {
        let (mut w, log) = files("live");
        assert!(w
            .harness
            .query_by_role(egui::accesskit::Role::Link)
            .is_none());
        w.harness.get_by_label("Choose files…").click();
        w.harness.run_steps(2);
        assert_eq!(*log.borrow(), vec![Cmd::Choose]);
        // the demo's chooser picks its interview
        assert!(w.harness.query_by_label("Cleaning interview.m4a").is_some());
    }

    /// A row's icon action is the ×'s twin: 28 square, with air inside
    /// its 36 px row (a framed 32 filled the row); the toolbar's buttons
    /// are the small size (D2).
    #[test]
    fn row_actions_are_quiet_icons_like_the_x() {
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let (w, _) = files_at("queue", size);
            let save = w
                .harness
                .get_by_label("Save interview (cleaned).m4a")
                .rect();
            let x = w.harness.get_by_label("Remove interview.m4a").rect();
            let row = w.harness.get_by_label("interview.m4a").rect();
            let add = w.harness.get_by_label("Add files…").rect();
            assert_eq!(save.size(), egui::vec2(QUIET, QUIET), "{size:?}");
            assert_eq!(x.size(), save.size(), "{size:?}");
            assert_eq!(add.height(), 32.0, "{size:?}");
            assert!(
                (save.center().y - row.center().y).abs() < 0.5,
                "{size:?} {save:?} {row:?}"
            );
        }
    }

    /// B21: "Choose files…" pressed from the keyboard goes when the files
    /// come; the focus moves to "Add files…", not off the page.
    #[test]
    fn the_keyboard_keeps_its_place_after_the_first_files() {
        let (mut w, _) = files("live");
        tab_to(&mut w, "Choose files…");
        w.harness.key_press(egui::Key::Enter);
        w.harness.run_steps(4);
        assert!(w.harness.query_by_label("Choose files…").is_none());
        assert_eq!(focused(&w).map(|(l, _)| l).as_deref(), Some("Add files…"));
        // a mouse click leaves the focus where it was (nowhere)
        let (mut w, _) = files("live");
        w.harness.get_by_label("Choose files…").click();
        w.harness.run_steps(4);
        assert_eq!(focused(&w), None);
    }

    fn focused(w: &testing::Window) -> Option<(String, egui::Rect)> {
        w.harness.query_by(|n| n.is_focused()).map(|n| {
            let a = n.accesskit_node();
            (a.label().unwrap_or_default(), n.rect())
        })
    }

    /// Tab until `label` has the focus.
    fn tab_to(w: &mut testing::Window, label: &str) {
        for _ in 0..40 {
            w.harness.key_press(egui::Key::Tab);
            w.harness.run_steps(2);
            if focused(w).is_some_and(|(l, _)| l == label) {
                return;
            }
        }
        panic!("{label} never had the focus");
    }

    #[test]
    fn the_arrows_seek_and_keep_the_focus_on_the_time_line() {
        let (mut w, log) = files("paused");
        tab_to(&mut w, "Position in the file");
        log.borrow_mut().clear();
        for k in [
            egui::Key::ArrowLeft,
            egui::Key::ArrowLeft,
            egui::Key::ArrowDown,
            egui::Key::ArrowUp,
        ] {
            w.harness.key_press(k);
            w.harness.run_steps(2);
        }
        let seeks = log
            .borrow()
            .iter()
            .filter(|c| matches!(c, Cmd::Seek { drag: false, .. }))
            .count();
        assert_eq!(seeks, 4, "{:?}", log.borrow());
        assert_eq!(
            focused(&w).map(|f| f.0).as_deref(),
            Some("Position in the file")
        );
    }

    /// Round 3 (M5, M6): the rows' boxes on the toolbar's 16 px inset, and
    /// whole rows only, with a named bar when some are hidden.
    #[test]
    fn the_list_shows_whole_rows_on_the_toolbars_inset() {
        use egui::accesskit::Role;
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            // seven files: more than the list shows at either size (three
            // at 960 × 640, two at 720 × 520)
            let w = testing::with_setup(
                crate::window::Link::recording(state(Scenario::OnInCall)),
                Page::Files,
                size,
                |ps| {
                    let demo = DemoFiles::preset("five", 0.0, 3.3);
                    ps.files.source = Some(Box::new(Edited(demo, Default::default(), |v| {
                        for (id, name) in [(98, "sixth.wav"), (99, "seventh.wav")] {
                            let mut r = v.files[1].clone();
                            r.id = id;
                            r.name = name.into();
                            v.files.push(r);
                        }
                    })));
                },
            );
            let add = w.harness.get_by_label("Add files…").rect();
            let row = w.harness.get_by_label("interview.m4a").rect();
            assert_eq!(row.left(), add.left(), "{size:?}");
            // the rows don't all fit: a bar says so, and no row is cut
            assert!(w
                .harness
                .query_by_role_and_label(Role::ScrollBar, "Files")
                .is_some());
            let view = w
                .harness
                .query_by_role_and_label(Role::ScrollBar, "Files")
                .map(|b| b.rect())
                .unwrap();
            // the bar lies in the card's padding, clear of the rows (their
            // last control ends on the toolbar's right edge)
            let toolbar = w.harness.get_by_label("All files").rect();
            let end = w.harness.get_by_label("Remove interview.m4a").rect();
            assert!(end.right() <= toolbar.right() + 0.5, "{size:?}");
            assert!(
                view.left() >= toolbar.right() + 4.0,
                "{size:?} bar over the rows: bar {view:?} rows end {}",
                toolbar.right()
            );
            let shown = (view.height() / ROW_H).round();
            assert_eq!(shown, if size.x < 840.0 { 2.0 } else { 3.0 }, "{size:?}");
            for name in [
                "interview.m4a",
                "voice-note.opus",
                "lecture-03.mp4",
                "podcast-episode-12-final-mix.mp3",
                "team-meeting.flac",
            ] {
                let r = w.harness.get_by_label(name).rect();
                assert!(
                    r.bottom() <= view.bottom() + 0.5 || r.top() >= view.bottom() - 0.5,
                    "{size:?} {name} {r:?} cut at {view:?}"
                );
            }
        }
        // one file: no bar
        let (w, _) = files_at("added", egui::vec2(1440.0, 900.0));
        assert!(w
            .harness
            .query_by_role_and_label(Role::ScrollBar, "Files")
            .is_none());
    }

    /// That Space holds too is Hold's tooltip and description, at every
    /// size; no chip stands beside it.
    #[test]
    fn hold_says_that_space_does_the_same() {
        crate::i18n::pin_english();
        for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
            let (w, _) = files_at("playing", size);
            let hold = w.harness.get_by_label("Hold to hear the original");
            assert_eq!(
                hold.accesskit_node().description().as_deref(),
                Some(
                    "Press to switch, hold to compare. Space does the same from anywhere on \
                     the page."
                ),
                "{size:?}"
            );
            assert_eq!(hold.rect().height(), 32.0, "{size:?}");
            assert!(w.harness.query_by_label_contains("or hold").is_none());
        }
    }

    #[test]
    fn every_tab_stop_has_a_name_and_a_row_tabbed_to_comes_into_view() {
        for name in ["queue", "format", "playing"] {
            for size in [egui::vec2(960.0, 640.0), egui::vec2(720.0, 520.0)] {
                let (mut w, _) = files_at(name, size);
                let list_top = w.harness.get_by_label("interview.m4a").rect().top();
                for i in 0..40 {
                    w.harness.key_press(egui::Key::Tab);
                    w.harness.run_steps(2);
                    let Some((label, rect)) = focused(&w) else {
                        continue;
                    };
                    assert!(!label.is_empty(), "{name} {size:?}: stop {i} {rect:?}");
                    if label == "voice-note.opus" {
                        // a row Tab reaches comes into the list's view
                        // (whole 36 px rows: three at 720, five at 960)
                        w.harness.run_steps(30);
                        let r = w.harness.get_by_label("voice-note.opus").rect();
                        let max_h = if size.x < 840.0 { 3.0 } else { 5.0 } * ROW_H;
                        // (within a pixel: egui scrolls the row to the fold)
                        assert!(r.bottom() <= list_top + max_h + 11.5, "{size:?} {r:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn space_on_a_focused_button_is_that_button() {
        let (mut w, log) = files("cleaned");
        tab_to(&mut w, "Save interview (cleaned).m4a");
        log.borrow_mut().clear();
        w.harness.key_press(egui::Key::Space);
        w.harness.run_steps(3);
        assert!(
            log.borrow().iter().any(|c| matches!(c, Cmd::Save(_))),
            "{:?}",
            log.borrow()
        );
        assert!(!sent(&log, &Cmd::Play), "{:?}", log.borrow());
        // on the time line Space is still the page's Play
        tab_to(&mut w, "Position in the file");
        log.borrow_mut().clear();
        w.harness.key_down(egui::Key::Space);
        w.harness.run_steps(2);
        w.harness.key_up(egui::Key::Space);
        w.harness.run_steps(3);
        assert_eq!(*log.borrow(), vec![Cmd::Play]);
    }

    #[test]
    fn a_row_focused_from_the_keyboard_shows_the_ring() {
        let (mut w, _) = files("queue");
        tab_to(&mut w, "lecture-03.mp4");
        w.harness.run_steps(1);
        let row = w.harness.get_by_label("lecture-03.mp4").rect();
        let ring = crate::ui::kit(&w.harness.ctx).pal.focus_ring();
        let rings = w
            .harness
            .output()
            .shapes
            .iter()
            .filter(|c| match &c.shape {
                egui::Shape::Rect(r) => {
                    r.stroke.color == ring && (r.rect.center() - row.center()).length() < 1.0
                }
                _ => false,
            })
            .count();
        assert_eq!(rings, 1);
    }

    /// Round 3b: the list reads as a list. 36 px rows 12 px under the
    /// toolbar, a hairline between them (none next to the selected row),
    /// no progress line: the state ends with how far it is.
    #[test]
    fn the_list_reads_as_a_list() {
        crate::i18n::pin_english();
        let (w, _) = files_at("five", egui::vec2(960.0, 800.0));
        let names = [
            "interview.m4a",
            "voice-note.opus",
            "lecture-03.mp4",
            "podcast-episode-12-final-mix.mp3",
            "team-meeting.flac",
        ];
        let rows: Vec<egui::Rect> = names
            .iter()
            .map(|n| w.harness.get_by_label(n).rect())
            .collect();
        for pair in rows.windows(2) {
            assert_eq!(pair[1].top() - pair[0].top(), ROW_H, "{rows:?}");
        }
        assert_eq!(rows[0].height(), ROW_H);
        let bar = w.harness.get_by_label("All files").rect();
        assert_eq!(rows[0].top(), bar.bottom() + LIST_GAP);
        // five rows are within the list's share at 960 × 800: no scroll bar
        assert!(w
            .harness
            .query_by_role_and_label(egui::accesskit::Role::ScrollBar, "Files")
            .is_none());
        // the hairlines: above the 2nd and the 5th (the 3rd is selected,
        // so neither of its edges has one)
        let line = crate::ui::kit(&w.harness.ctx).pal.line;
        let list = Rect::from_min_max(rows[0].min, pos2(rows[0].right() + 400.0, rows[4].bottom()));
        let shapes = &w.harness.output().shapes;
        let rules: Vec<f32> = shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Rect(r)
                    if r.fill == line && r.rect.height() == 1.0 && list.contains(r.rect.min) =>
                {
                    Some(r.rect.top())
                }
                _ => None,
            })
            .collect();
        assert_eq!(rules, vec![rows[1].top(), rows[4].top()]);
        // no 2 px progress line in the list
        assert!(!shapes.iter().any(|c| matches!(&c.shape,
            egui::Shape::Rect(r) if r.rect.height() == 2.0 && list.contains(r.rect.center()))));
        // a cleaning file says how far it is
        let lecture = w.harness.get_by_label("lecture-03.mp4");
        let about = lecture.accesskit_node().description().unwrap_or_default();
        assert!(about.contains('%') && !about.contains("\u{a0}%"), "{about}");
    }

    #[test]
    fn a_row_tells_its_length_and_line() {
        let (w, _) = files("no-space");
        let row = w.harness.get_by_label("interview.m4a");
        assert_eq!(
            row.accesskit_node().description().as_deref(),
            Some("12:40, Not saved: the disk is full, Free some space, or save it somewhere else.")
        );
    }

    #[test]
    fn a_next_step_is_the_rows_button() {
        let (mut w, log) = files("engine");
        let row = w.harness.get_by_label("interview.m4a");
        assert_eq!(
            row.accesskit_node().description().as_deref(),
            Some("12:40, Didn’t start, Nothing was written. Try again in a moment.")
        );
        w.harness.get_by_label("Try again: interview.m4a").click();
        w.harness.run_steps(2);
        assert!(
            matches!(log.borrow().first(), Some(Cmd::Retry(_))),
            "{:?}",
            log.borrow()
        );
        let (mut w, log) = files("damaged");
        // the readable part is what the row shows; the label names how
        // long the file says it is
        let row = w.harness.get_by_label("interview.m4a");
        assert_eq!(
            row.accesskit_node().description().as_deref(),
            Some("3:12, Damaged after 3:12 of 12:40, HushMic can clean the first 3:12.")
        );
        w.harness
            .get_by_label("Clean the first 3:12: interview.m4a")
            .click();
        w.harness.run_steps(2);
        assert!(
            matches!(log.borrow().first(), Some(Cmd::CleanFirst(_))),
            "{:?}",
            log.borrow()
        );
        // no white button in the figure for it
        assert!(w.harness.query_by_label("Clean the first 3:12").is_none());
        assert!(w.harness.query_by_label("Choose files…").is_none());
        // nothing to clean again: the row's own fix
        let (mut w, log) = files("unreadable");
        assert!(w
            .harness
            .query_by_label("Try again: interview.m4a")
            .is_none());
        w.harness
            .get_by_label("Choose it again…: interview.m4a")
            .click();
        w.harness.run_steps(2);
        assert!(
            matches!(log.borrow().first(), Some(Cmd::ChooseAgain(_))),
            "{:?}",
            log.borrow()
        );
    }

    #[test]
    fn a_file_with_a_next_step_still_plays() {
        for name in ["stopped", "damaged", "cache", "engine"] {
            let (mut w, log) = files(name);
            assert!(w.harness.query_by_label("Play").is_some(), "{name}");
            w.harness.key_down(egui::Key::Space);
            w.harness.run_steps(2);
            w.harness.key_up(egui::Key::Space);
            w.harness.run_steps(3);
            assert!(sent(&log, &Cmd::Play), "{name}: {:?}", log.borrow());
        }
        // a file that doesn't open has nothing to play
        let (mut w, log) = files("unreadable");
        assert!(w.harness.query_by_label("Play").is_none());
        w.harness.key_down(egui::Key::Space);
        w.harness.run_steps(2);
        w.harness.key_up(egui::Key::Space);
        w.harness.run_steps(3);
        assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    }

    #[test]
    fn a_player_waiting_on_a_stalled_pass_says_it_waits() {
        crate::i18n::pin_english();
        let row = |stalled| Row {
            id: 1,
            name: "talk.flac".into(),
            length: 600.0,
            estimate: model::Estimate::Exact,
            damaged: None,
            stated: None,
            kind: Kind::Audio,
            pass: Pass::Cleaning,
            segments: vec![(0.0, 75.0)],
            output: String::new(),
            saved_name: String::new(),
            save: Save::No,
            time_left: None,
            stalled,
            reduced: false,
            tracks: 1,
            dropped: 0,
            track: 0,
            titles: Vec::new(),
            strength: model::Strength::Strong,
        };
        let v = View {
            play: Play::CaughtUp,
            ..View::default()
        };
        let title = |stalled| copy(&v, Some(&row(stalled)), false, false);
        assert_eq!(title(false), "Cleaning ahead…");
        assert_eq!(title(true), "Waiting for the cleaning to catch up.");
    }

    /// A file cleaned at its core's rate (HE-AAC, no ffmpeg) says so on
    /// hover; a row that already explains something keeps its own words.
    #[test]
    fn a_reduced_file_says_how_to_get_full_quality() {
        crate::i18n::pin_english();
        let row = |pass, reduced| Row {
            id: 1,
            name: "talk.m4a".into(),
            length: 60.0,
            estimate: model::Estimate::Exact,
            damaged: None,
            stated: None,
            kind: Kind::Audio,
            pass,
            segments: vec![(0.0, 60.0)],
            output: String::new(),
            saved_name: String::new(),
            save: Save::No,
            time_left: None,
            stalled: false,
            reduced,
            tracks: 1,
            dropped: 0,
            track: 0,
            titles: Vec::new(),
            strength: model::Strength::Strong,
        };
        let tip = |pass, reduced| row_look(&row(pass, reduced), false, false, "").tip;
        let said = Some("Cleaned at reduced quality. Install ffmpeg for full quality.".into());
        assert_eq!(tip(Pass::Cleaned, true), said);
        assert_eq!(tip(Pass::Cleaning, true), said);
        assert_eq!(tip(Pass::Cleaned, false), None);
        let busy = tip(Pass::Attention(Reason::NoSpace), true);
        assert!(busy.is_some() && busy != said, "{busy:?}");
        // F15: a recording's name with bytes that aren't one is damaged;
        // another name is no recording (and says so with its name last)
        let mut bad = row(Pass::Attention(Reason::NotMedia), false);
        assert_eq!(
            attention(&bad, &Reason::NotMedia).0,
            "Can’t read this file."
        );
        bad.kind = Kind::Other;
        bad.name = "notes.txt".into();
        assert_eq!(
            attention(&bad, &Reason::NotMedia).0,
            "Not a recording: notes.txt"
        );
        // F17: no full stop after an extension, one after other words
        assert_eq!(
            no_stop_after("Cleaning a.mp3.".into(), &["a.mp3"]),
            "Cleaning a.mp3"
        );
        assert_eq!(
            no_stop_after(
                "Saved a (cleaned).flac.".into(),
                &["a.mp3", "a (cleaned).flac"]
            ),
            "Saved a (cleaned).flac"
        );
        assert_eq!(
            no_stop_after("It is done.".into(), &["a.mp3"]),
            "It is done."
        );
        // F8: a waiting row offers "Clean next", unless it is next
        let waiting = row(Pass::Waiting, false);
        let next = row_look(&waiting, true, true, "").act.map(|a| a.cmd);
        assert_eq!(next, Some(Cmd::CleanNext(1)));
        assert!(row_look(&waiting, true, false, "").act.is_none());
        // F5: a saved row says the file its Save wrote
        let mut saved = row(Pass::Cleaned, false);
        saved.save = Save::Saved;
        saved.saved_name = "talk (cleaned, Gentle).wav".into();
        assert_eq!(
            row_look(&saved, false, false, "").tip.as_deref(),
            Some("Saved as talk (cleaned, Gentle).wav.")
        );
    }

    /// SF-A10: a recording whose tracks Save can't all keep says so on
    /// its row and under the player, seen without a hover; one that keeps
    /// them all says how many on its row.
    #[test]
    fn a_file_says_what_becomes_of_its_sound_tracks() {
        crate::i18n::pin_english();
        let row = |tracks, dropped| Row {
            id: 1,
            name: "stream.mkv".into(),
            length: 60.0,
            estimate: model::Estimate::Exact,
            damaged: None,
            stated: None,
            kind: Kind::Video,
            pass: Pass::Cleaned,
            segments: vec![(0.0, 60.0)],
            output: String::new(),
            saved_name: String::new(),
            save: Save::No,
            time_left: None,
            stalled: false,
            reduced: false,
            tracks,
            dropped,
            track: 0,
            titles: Vec::new(),
            strength: model::Strength::Strong,
        };
        let tip = |t, d| row_look(&row(t, d), false, false, "").tip;
        assert_eq!(
            tip(1, 1).as_deref(),
            Some("Only the first of 2 sound tracks is kept. Install ffmpeg to keep them all.")
        );
        assert_eq!(
            tip(2, 0).as_deref(),
            Some("Cleans track 1 of 2. The other sound tracks are kept as they are.")
        );
        // the voice's track by its title (F1)
        let mut obs = row(2, 0);
        (obs.track, obs.titles) = (1, vec!["Desktop Audio".into(), "Mic/Aux".into()]);
        assert_eq!(
            row_look(&obs, false, false, "").tip.as_deref(),
            Some("Cleans track 2 · Mic/Aux, of 2. The other sound tracks are kept as they are.")
        );
        assert_eq!(tip(1, 0), None);
        let w = testing::at_60hz_with(
            crate::window::Link::recording(state(Scenario::OnInCall)),
            Page::Files,
            egui::vec2(960.0, 640.0),
            |ps| {
                let demo = DemoFiles::preset("cleaned", 0.0, 3.3);
                ps.files.source = Some(Box::new(Edited(demo, Default::default(), |v| {
                    for f in &mut v.files {
                        (f.tracks, f.dropped) = (1, 1);
                    }
                })));
            },
        );
        // under the player, in place of the strength's words
        assert!(w
            .harness
            .query_by_label(
                "Only the first of 2 sound tracks is kept. Install ffmpeg to keep them all."
            )
            .is_some());
        assert!(w
            .harness
            .query_by_label(
                "Most of the noise is gone. Files start at Strong to keep the room tone."
            )
            .is_none());
    }

    /// F1: a recording with more sound tracks says under the player which
    /// one it cleans, and its menu cleans another.
    #[test]
    fn the_track_to_clean_can_be_changed() {
        crate::i18n::pin_english();
        let log: Log = Default::default();
        let l = log.clone();
        let mut w = testing::at_60hz_with(
            crate::window::Link::recording(state(Scenario::OnInCall)),
            Page::Files,
            egui::vec2(960.0, 640.0),
            move |ps| {
                let demo = DemoFiles::preset("cleaned", 0.0, 3.3);
                ps.files.source = Some(Box::new(Edited(demo, l.clone(), |v| {
                    for f in &mut v.files {
                        (f.tracks, f.dropped, f.track) = (2, 0, 1);
                        f.titles = vec!["Desktop Audio".into(), "Mic/Aux".into()];
                    }
                })));
            },
        );
        w.harness.run_steps(3);
        let b = w.harness.get_by_label("The sound track to clean");
        assert_eq!(
            b.accesskit_node().value().as_deref(),
            Some("Cleans track 2 · Mic/Aux, of 2")
        );
        b.click();
        w.harness.run_steps(3);
        w.harness.get_by_label("Track 1 · Desktop Audio").click();
        w.harness.run_steps(3);
        let id = log.borrow().iter().find_map(|c| match c {
            Cmd::Track(id, 0) => Some(*id),
            _ => None,
        });
        assert!(id.is_some(), "{:?}", log.borrow());
    }

    #[test]
    fn a_rough_length_is_not_shown() {
        let row = |estimate| Row {
            id: 1,
            name: "talk.mp3".into(),
            length: 760.0,
            estimate,
            damaged: None,
            stated: None,
            kind: Kind::Audio,
            pass: Pass::Cleaning,
            segments: vec![],
            output: String::new(),
            saved_name: String::new(),
            save: Save::No,
            time_left: None,
            stalled: false,
            reduced: false,
            tracks: 1,
            dropped: 0,
            track: 0,
            titles: Vec::new(),
            strength: model::Strength::Strong,
        };
        assert_eq!(clock_or_none(&row(model::Estimate::Exact)), "12:40");
        assert_eq!(clock_or_none(&row(model::Estimate::Close)), "12:40");
        assert_eq!(clock_or_none(&row(model::Estimate::Rough)), "");
    }

    #[test]
    fn dropped_files_are_added() {
        let (mut w, log) = files("live");
        w.harness.input_mut().hovered_files.push(egui::HoveredFile {
            path: Some("/tmp/a.wav".into()),
            ..Default::default()
        });
        w.harness.run_steps(2);
        assert!(w.harness.query_by_label("Drop to clean them.").is_some());
        w.harness.input_mut().hovered_files.clear();
        w.harness.input_mut().dropped_files.push(egui::DroppedFile {
            path: Some("/tmp/a.wav".into()),
            ..Default::default()
        });
        w.harness.run_steps(2);
        assert!(sent(&log, &Cmd::Add(vec!["/tmp/a.wav".into()])));
    }
}
