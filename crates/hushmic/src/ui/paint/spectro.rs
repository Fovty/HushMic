//! The picture of sound: two spectrograms on one time axis, the
//! microphone above and what apps hear below, newest at the right edge.
//!
//! The lanes come from a [`Picture`] (`crate::miccheck::spectrum`): levels
//! on a fixed scale, the same for both lanes, so a cleaned pause is empty
//! under a noisy original. The microphone is drawn in a neutral ramp
//! (transparent through grey to the ink), what apps hear in the accent;
//! where apps heard the original (noise suppression off), their lane is
//! neutral too. Both ramps share one curve ([`strength`]): what lies at a
//! room's noise floor is a faint haze, a voice stands clear of it.
//!
//! The lanes sit on the card, each on a ground only a little darker.
//! What is written in a lane stands on a ground of its own, a patch of
//! the lane's ground that is solid under the words and fades out around
//! them: the caption in the lane's upper left corner, what ends the
//! caption row at its right (a level, a clock, "Playing"). The picture
//! between and around the patches is untouched. Live's oldest sound fades
//! out at the left.
//!
//! Both lanes are one texture, a ring of columns: a new column is one
//! narrow upload, and only a change of everything in view (a frozen take,
//! the theme) builds the texture again. The lanes scroll on a clock of
//! their own that follows the columns as they arrive, so they move evenly
//! at any frame rate; under reduced motion they step with the columns.
//!
//! During a replay the take stands still, a playhead crosses both lanes,
//! and the lane that is heard is at full strength while the other is dim.
//!
//! A file's picture ([`Still`], the Files page) is the same two lanes with
//! the whole file as their time axis: the original above, the cleaned
//! sound below as far as it is cleaned, the playhead where the file is.

use eframe::egui::{
    pos2, Color32, ColorImage, Context, Mesh, Painter, Rect, Shape, Stroke, StrokeKind,
    TextureHandle, TextureOptions,
};

use super::super::fonts::Type;
use super::super::icons::{self, Icon};
use super::super::text;
use super::super::tokens::{Palette, Theme};
use super::{alpha, image, mix};
use crate::files::picture::COLS_MAX;
use crate::miccheck::spectrum::{self, Cursor, Look, Picture, COLS, COLUMN_BYTES, ROWS};

/// Columns the texture holds beyond those in view: how far the lanes may
/// run behind the newest column.
const SLACK: usize = 40;
const TEX_COLS: usize = COLS + SLACK;
const TEX_ROWS: usize = 2 * ROWS;
/// Columns a second.
const RATE: f64 = crate::miccheck::types::SAMPLE_RATE as f64 / spectrum::HOP as f64;
/// The lanes run this many columns behind the newest (the columns come in
/// bursts; this keeps the scroll from catching up with them and stalling).
const BEHIND: f64 = 4.0;
/// How strongly the scroll leans towards [`BEHIND`], per column off.
const PULL: f64 = 0.25;

/// The oldest sound fades out over this stretch at the left edge.
const FADE: f32 = 72.0;
/// A caption stands this far inside its lane's corner.
pub const INSET: (f32, f32) = (10.0, 6.0);
/// How much of a lane's ground is in the patch under what is written in
/// it: enough that the brightest sound behind leaves the words readable
/// (4.5:1 and more, in both themes).
const SCRIM: f32 = 0.92;
/// A patch is solid this far beside its words, and fades out over this
/// much past that (half of it above and below).
const PATCH_PAD: f32 = 8.0;
const PATCH_FADE: f32 = 10.0;
/// The hatch over a lane nothing goes through: its strength and the
/// distance between its strokes.
const HATCH: f32 = 0.06;
const HATCH_STEP: f32 = 12.0;
/// How much of the window's ground is in a lane's (the rest is the
/// card's).
const WELL: f32 = 0.4;
/// The level scale's knee, of its range: the top of a quiet room's floor
/// (-59 dB in a band), under which sound is a haze of at most [`HAZE`].
const KNEE: f32 = 0.44;
const HAZE: f32 = 0.12;
/// A lane is at least this high (the least window). Where something else
/// takes the page's height (Files' list), at most [`LANE_MAX`].
pub const LANE_MIN: f32 = 50.0;
pub const LANE_MAX: f32 = 160.0;
/// A lane's corner radius.
const RADIUS: f32 = 8.0;
/// The two pages' pictures, for [`palette`].
pub const LIVE_KEY: &str = "hm-live-picture";
pub const FILES_KEY: &str = "hm-files-picture";
/// The lane that is not heard during a replay, and its caption.
const DIM: f32 = 0.3;

/// The picture's sizes: at the full and the compact window, with lanes
/// of one height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sizes {
    pub pad_x: f32,
    pub pad_top: f32,
    pub pad_bottom: f32,
    /// Between the lanes: the card's own ground.
    pub lane_gap: f32,
    pub lane: f32,
}

impl Sizes {
    pub fn of(compact: bool, lane: f32) -> Sizes {
        if compact {
            Sizes {
                pad_x: 16.0,
                pad_top: 12.0,
                pad_bottom: 4.0,
                lane_gap: 8.0,
                lane,
            }
        } else {
            Sizes {
                pad_x: 16.0,
                pad_top: 16.0,
                pad_bottom: 6.0,
                lane_gap: 10.0,
                lane,
            }
        }
    }

    /// The lanes' height for a picture that may be `room` high: what is
    /// left after the gaps, shared by the two, at least [`LANE_MIN`].
    pub fn lane_for(compact: bool, room: f32) -> f32 {
        let rest = Sizes::of(compact, 0.0).height();
        ((room - rest) / 2.0).floor().max(LANE_MIN)
    }

    pub fn height(&self) -> f32 {
        self.pad_top + 2.0 * self.lane + self.lane_gap + self.pad_bottom
    }
}

/// One lane: its caption, and what stands in it when it has no sound.
#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    /// Before the caption.
    pub glyph: Icon,
    /// "Microphone", "What apps hear".
    pub label: String,
    /// The device, or the app. `strong`: in ink and medium (an app is
    /// using HushMic, a microphone is there).
    pub name: String,
    pub strong: bool,
    /// The name is a control the page puts in its place
    /// ([`Painted::names`]), this wide: the painter leaves it out.
    pub select: Option<f32>,
    /// Room kept free at the caption row's right end, for what the page
    /// writes there.
    pub tail: f32,
    /// The lane draws its sound.
    pub sound: bool,
    /// Words in the middle of the lane ("No sound", "Starting…"), with
    /// their icon.
    pub note: Option<(Option<Icon>, String)>,
    /// Nothing goes through this lane (muted): a faint hatch over its
    /// ground says so at a glance; its caption says why.
    pub shut: bool,
}

/// A replay in the picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Playing {
    /// The playhead, 0..=1 of the take.
    pub position: f32,
    /// How much of what is heard is the original, 0..=1 (eased by the
    /// page while the sides switch).
    pub original: f32,
}

/// What one frame draws.
#[derive(Clone, Debug, PartialEq)]
pub struct Show {
    pub mic: Lane,
    pub apps: Lane,
    pub playing: Option<Playing>,
    /// The words beside the lane that is heard ("Playing").
    pub playing_label: String,
    pub compact: bool,
    /// A lane's height ([`Sizes::lane_for`]).
    pub lane: f32,
}

/// Where the last paint put things.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Painted {
    /// The two lanes' sound.
    pub lanes: [Rect; 2],
    /// The names in the captions, and whether they were cut short; for
    /// a name the page puts there ([`Lane::select`]), its room.
    pub names: [(Rect, bool); 2],
    /// The caption rows, from the lanes' inset to it.
    pub rows: [Rect; 2],
    /// The solid parts of the patches under each caption's left and
    /// right group; nothing where a lane has no sound, or no such group.
    pub solid: [[Rect; 2]; 2],
}

/// The ramps for a theme: 256 premultiplied colours each.
struct Ramps {
    theme: Theme,
    neutral: Vec<Color32>,
    accent: Vec<Color32>,
}

/// How strongly a level is drawn, 0..=1 over the scale: up to the
/// [`KNEE`] a haze that grows slowly (a room's floor, and its grain,
/// stay faint), above it steeply and then easing into full strength (a
/// voice is bright from its quiet parts on).
pub fn strength(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t <= KNEE {
        HAZE * (t / KNEE).powf(1.5)
    } else {
        let u = (t - KNEE) / (1.0 - KNEE);
        HAZE + (1.0 - HAZE) * (1.0 - (1.0 - u) * (1.0 - u))
    }
}

fn ramp(low: Color32, high: Color32, peak: Color32) -> Vec<Color32> {
    (0..256)
        .map(|i| {
            let t = i as f32 / 255.0;
            // the top third reaches the peak colour
            let colour = mix(
                mix(low, high, (t / 0.7).min(1.0)),
                peak,
                ((t - 0.7) / 0.3).max(0.0),
            );
            alpha(colour, strength(t))
        })
        .collect()
}

impl Ramps {
    /// The two ramps, in the picture's palette ([`Palette::picture`]).
    fn of(pal: &Palette) -> Ramps {
        let neutral = ramp(pal.ink_muted, pal.ink, pal.ink);
        let accent = ramp(
            mix(pal.accent, pal.bg, 0.35),
            pal.accent,
            mix(pal.accent, Color32::WHITE, 0.55),
        );
        Ramps {
            theme: pal.theme,
            neutral,
            accent,
        }
    }

    /// One column's pixels, top to bottom: the microphone's lane, then
    /// what apps hear.
    fn column(&self, levels: &[u8], look: Look, out: &mut [Color32]) {
        let apps = match look {
            Look::Clean => &self.accent,
            Look::Original => &self.neutral,
        };
        for row in 0..ROWS {
            out[ROWS - 1 - row] = self.neutral[levels[row] as usize];
            out[TEX_ROWS - 1 - row] = apps[levels[ROWS + row] as usize];
        }
    }
}

/// The picture's painter. Keep one per page; [`Spectro::forget`] when the
/// page is no longer shown.
pub struct Spectro {
    tex: Option<TextureHandle>,
    ramps: Option<Ramps>,
    cursor: Cursor,
    /// Columns in the texture (the newest is `head - 1`).
    head: u64,
    /// The column at the lanes' right edge, as the scroll has it.
    shown: f64,
    frozen: bool,
    resting: bool,
    last: Option<f64>,
    /// The columns of one read: their pixels and their places in the ring.
    pixels: Vec<Color32>,
    slots: Vec<usize>,
}

impl Default for Spectro {
    fn default() -> Self {
        Self::new()
    }
}

impl Spectro {
    pub fn new() -> Spectro {
        Spectro {
            tex: None,
            ramps: None,
            cursor: Cursor::default(),
            head: 0,
            shown: 0.0,
            frozen: false,
            resting: true,
            last: None,
            pixels: Vec::new(),
            slots: Vec::new(),
        }
    }

    /// Drop the texture and everything read: the page is not shown.
    pub fn forget(&mut self) {
        *self = Spectro::new();
    }

    /// Nothing of the picture is held (tests).
    #[cfg(test)]
    pub fn forgotten(&self) -> bool {
        self.tex.is_none() && self.head == 0
    }

    /// The columns read so far, and whether a frozen take is in view
    /// (tests).
    #[cfg(test)]
    pub fn state(&self) -> (u64, bool) {
        (self.head, self.frozen)
    }

    /// Take in what `picture` holds now, at `now` (egui's time). True
    /// while the lanes move: the page asks for the next frame.
    pub fn sync(&mut self, ctx: &Context, picture: &Picture, now: f64) -> bool {
        let kit = super::super::kit(ctx);
        if self.ramps.as_ref().map(|r| r.theme) != Some(kit.pal.picture().theme) {
            // the colours are in the texture: read everything again
            self.ramps = Some(Ramps::of(&kit.pal.picture()));
            self.cursor = Cursor::default();
        }
        if self.tex.is_none() {
            self.cursor = Cursor::default();
            self.tex = Some(ctx.load_texture(
                "hm-spectro",
                ColorImage::filled([TEX_COLS, TEX_ROWS], Color32::TRANSPARENT),
                TextureOptions::LINEAR,
            ));
        }
        let (Some(ramps), Some(tex)) = (self.ramps.as_ref(), self.tex.as_mut()) else {
            return false;
        };
        // the columns not seen yet, as pixels (top to bottom, one column
        // after the other)
        let (pixels, slots) = (&mut self.pixels, &mut self.slots);
        pixels.clear();
        slots.clear();
        let shown = picture.read(&mut self.cursor, |index, levels, look| {
            debug_assert_eq!(levels.len(), COLUMN_BYTES);
            let at = pixels.len();
            pixels.resize(at + TEX_ROWS, Color32::TRANSPARENT);
            ramps.column(levels, look, &mut pixels[at..]);
            slots.push((index % TEX_COLS as u64) as usize);
        });
        let columns = slots.iter().zip(pixels.as_chunks::<TEX_ROWS>().0);
        if shown.reset {
            // everything in view changed: one image
            let mut img = ColorImage::filled([TEX_COLS, TEX_ROWS], Color32::TRANSPARENT);
            for (slot, column) in columns {
                for (y, px) in column.iter().enumerate() {
                    img.pixels[y * TEX_COLS + slot] = *px;
                }
            }
            tex.set(img, TextureOptions::LINEAR);
            // and no more memory than a few columns need from here on
            pixels.clear();
            pixels.shrink_to(8 * TEX_ROWS);
        } else {
            for (slot, column) in columns {
                tex.set_partial(
                    [*slot, 0],
                    ColorImage::new([1, TEX_ROWS], column.to_vec()),
                    TextureOptions::LINEAR,
                );
            }
        }
        self.head = shown.head;
        self.frozen = shown.frozen;
        self.resting = shown.resting;
        let dt = self.last.map_or(0.0, |t| (now - t).clamp(0.0, 0.1));
        self.last = Some(now);
        let head = shown.head as f64;
        if shown.reset || shown.frozen || shown.resting || kit.reduced_motion {
            self.shown = head;
            return false;
        }
        let rate = RATE * (1.0 + PULL * (head - self.shown - BEHIND)).clamp(0.5, 3.0);
        self.shown = (self.shown + dt * rate)
            .min(head)
            .max(head - (SLACK - 2) as f64);
        head - self.shown > 1e-3
    }

    /// The lanes' strengths in a replay: (the microphone's, what apps
    /// hear's).
    fn strengths(playing: Option<Playing>) -> (f32, f32) {
        match playing {
            Some(p) => {
                let o = p.original.clamp(0.0, 1.0);
                (DIM + (1.0 - DIM) * o, DIM + (1.0 - DIM) * (1.0 - o))
            }
            None => (1.0, 1.0),
        }
    }

    /// Draw the picture into `rect` ([`Sizes::height`] tall).
    pub fn paint(&mut self, ctx: &Context, p: &Painter, rect: Rect, show: &Show) -> Painted {
        let frame = Frame {
            lanes: [&show.mic, &show.apps],
            k: Self::strengths(show.playing),
            playing: show.playing.is_some(),
            playing_label: &show.playing_label,
            clock: None,
            // the take ends at the right edge; a short one starts inside
            playhead: show.playing.filter(|_| self.frozen).map(|playing| {
                let cols = self.head.min(COLS as u64) as f32;
                (COLS as f32 - cols + playing.position.clamp(0.0, 1.0) * cols) / COLS as f32
            }),
            compact: show.compact,
            lane: show.lane,
            key: LIVE_KEY,
        };
        frame.paint(ctx, p, rect, &mut |i, well, k| {
            self.lane(p, well, rows(i), k);
        })
    }

    /// One lane's sound into `well`: the texture's rows from `v.0` at its
    /// top to `v.1` at its bottom, the columns in view from the scroll,
    /// at strength `k`; the oldest fade out at the left: at the lane's
    /// left edge, or where a picture that does not fill the lane yet
    /// begins (it grows in from the right, and a hard start read as a
    /// grey box).
    fn lane(&self, p: &Painter, well: Rect, v: (f32, f32), k: f32) {
        let Some(tex) = self.tex.as_ref() else {
            return;
        };
        let (v0, v1) = inset(v);
        // in view: the columns [to - COLS, to), of those that exist
        let to = self.shown;
        let from = (to - COLS as f64).max(0.0);
        if to - from < 1e-6 {
            return;
        }
        let x_of = |col: f64| well.right() - ((to - col) / COLS as f64) as f32 * well.width();
        // texels half a column in from both ends hold nothing else
        let (lo, hi) = (
            self.head.saturating_sub(TEX_COLS as u64) as f64 + 0.5,
            self.head as f64 - 0.5,
        );
        let mut m = Mesh::with_texture(tex.id());
        // cut where the ring wraps and where the fade ends
        let mut cuts = vec![from, to];
        let wrap = (from / TEX_COLS as f64).floor() * TEX_COLS as f64 + TEX_COLS as f64;
        if wrap > from && wrap < to {
            cuts.push(wrap);
        }
        let start = x_of(from).max(well.left());
        let col = to - ((well.right() - (start + FADE)) / well.width()) as f64 * COLS as f64;
        if col > from && col < to {
            cuts.push(col);
        }
        cuts.sort_by(f64::total_cmp);
        let tint = |x: f32| {
            let fade = ((x - start) / FADE).clamp(0.0, 1.0);
            Color32::from_white_alpha((255.0 * k * fade).round() as u8)
        };
        for pair in cuts.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if b - a < 1e-9 {
                continue;
            }
            // a segment lies within one lap of the ring
            let lap = (a / TEX_COLS as f64).floor() * TEX_COLS as f64;
            let u = |col: f64| ((col.clamp(lo, hi) - lap) / TEX_COLS as f64) as f32;
            let (xa, xb) = (x_of(a), x_of(b));
            let n = m.vertices.len() as u32;
            for (x, col) in [(xa, a), (xb, b)] {
                for (y, v) in [(well.top(), v0), (well.bottom(), v1)] {
                    m.vertices.push(eframe::egui::epaint::Vertex {
                        pos: pos2(x, y),
                        uv: pos2(u(col), v),
                        color: tint(x),
                    });
                }
            }
            m.indices
                .extend_from_slice(&[n, n + 1, n + 2, n + 1, n + 3, n + 2]);
        }
        p.add(Shape::mesh(m));
    }
}

/// The texture's rows for lane `i`, from the lane's top to its bottom
/// (0..1 down the texture: the upper lane's rows, then the lower one's,
/// each highest band first).
fn rows(i: usize) -> (f32, f32) {
    if i == 0 {
        (0.0, 0.5)
    } else {
        (0.5, 1.0)
    }
}

/// Half a texel in from both ends of a lane's rows, so the other lane
/// never bleeds in.
fn inset(v: (f32, f32)) -> (f32, f32) {
    let half = 0.5 / TEX_ROWS as f32;
    (v.0 + half, v.1 - half)
}

/// A lane's ground: the card's, a little darker.
pub fn ground(pal: &Palette) -> Color32 {
    mix(pal.card_fill(), pal.bg, WELL)
}

/// Whether the lanes are a picture: one of them shows sound, or is shut.
/// Before that (HushMic off, no file yet) they are two quiet wells.
pub fn lit(lanes: [&Lane; 2]) -> bool {
    lanes.iter().any(|l| l.sound || l.shut)
}

/// The palette the lanes and all that stands in them are painted in:
/// the picture's ([`Palette::picture`]) once they are `lit`, the page's
/// own before. In the grey scheme the two differ, and the lanes ease from
/// one to the other (`key` names the page's picture); in the dark theme
/// they are the same.
pub fn palette(ctx: &Context, key: &'static str, lit: bool) -> Palette {
    let kit = super::super::kit(ctx);
    let page = kit.pal;
    if page.is_dark() {
        return page.picture();
    }
    let t = if kit.reduced_motion {
        f32::from(u8::from(lit))
    } else {
        ctx.animate_bool_with_time(
            eframe::egui::Id::new(key),
            lit,
            super::super::tokens::duration::BASE,
        )
    };
    page.blend(&page.picture(), t)
}

/// What a lane's caption is written in, over a patch: the label, a name
/// that is there, one that is not.
pub fn inks(pal: &Palette) -> [Color32; 3] {
    [pal.ink_muted, pal.ink, pal.ink_muted]
}

/// The height of a lane's caption zone: the row and its inset.
fn caption_zone() -> f32 {
    INSET.1 + Type::CAP.line + 2.0
}

/// A patch under words in a lane: the lane's ground, solid over `core`
/// and fading out past it on the `sides` (left, top, right, bottom) that
/// are not a lane's edge, so it has no edge of its own.
fn patch(core: Rect, ground: Color32, sides: [bool; 4]) -> Mesh {
    let (on, off) = (alpha(ground, SCRIM), Color32::TRANSPARENT);
    let far = |side: bool, f: f32| if side { f } else { 0.0 };
    let xs = [
        core.left() - far(sides[0], PATCH_FADE),
        core.left(),
        core.right(),
        core.right() + far(sides[2], PATCH_FADE),
    ];
    let ys = [
        core.top() - far(sides[1], PATCH_FADE / 2.0),
        core.top(),
        core.bottom(),
        core.bottom() + far(sides[3], PATCH_FADE / 2.0),
    ];
    let mut m = Mesh::default();
    for (j, y) in ys.into_iter().enumerate() {
        for (i, x) in xs.into_iter().enumerate() {
            let solid = (1..=2).contains(&i) && (1..=2).contains(&j);
            m.colored_vertex(pos2(x, y), if solid { on } else { off });
        }
    }
    for j in 0..3u32 {
        for i in 0..3u32 {
            let a = j * 4 + i;
            m.indices
                .extend_from_slice(&[a, a + 1, a + 4, a + 1, a + 5, a + 4]);
        }
    }
    m
}

/// A faint diagonal hatch over a lane nothing goes through.
fn hatch(p: &Painter, well: Rect, colour: Color32) {
    let p = p.with_clip_rect(well.intersect(p.clip_rect()));
    let h = well.height();
    let mut x = well.left() - h;
    while x < well.right() {
        p.line_segment(
            [pos2(x, well.bottom()), pos2(x + h, well.top())],
            Stroke::new(1.0_f32, colour),
        );
        x += HATCH_STEP;
    }
}

/// What the two pictures share: the lanes' grounds, the captions in their
/// corners on their patches, the playhead under those, the words in a
/// lane without sound.
struct Frame<'a> {
    lanes: [&'a Lane; 2],
    /// The lanes' strengths.
    k: (f32, f32),
    /// Something plays: the lane at more than half strength is the one
    /// heard, and says so.
    playing: bool,
    playing_label: &'a str,
    /// In the upper lane's right corner: where a file is and how long.
    clock: Option<&'a str>,
    /// Where what plays is, 0..=1 of the lanes' width.
    playhead: Option<f32>,
    compact: bool,
    lane: f32,
    /// Names the page's picture ([`palette`]).
    key: &'static str,
}

impl Frame<'_> {
    /// Draw into `rect`; `sound(lane, well, strength)` draws a lane's
    /// sound into its well.
    fn paint(
        &self,
        ctx: &Context,
        p: &Painter,
        rect: Rect,
        sound: &mut dyn FnMut(usize, Rect, f32),
    ) -> Painted {
        let kit = super::super::kit(ctx);
        // the card the lanes lie on, and their own palette
        let card = kit.pal;
        let pal = palette(ctx, self.key, lit(self.lanes));
        let z = Sizes::of(self.compact, self.lane);
        let mut out = Painted {
            lanes: [Rect::NOTHING; 2],
            names: [(Rect::NOTHING, false); 2],
            rows: [Rect::NOTHING; 2],
            solid: [[Rect::NOTHING; 2]; 2],
        };
        let (left, right) = (rect.left() + z.pad_x, rect.right() - z.pad_x);
        let top = rect.top() + z.pad_top;
        let well = |i: usize| {
            let y = top + i as f32 * (z.lane + z.lane_gap);
            Rect::from_min_max(pos2(left, y), pos2(right, y + z.lane))
        };
        let ground = ground(&pal);
        for (i, (lane, k)) in self.lanes.into_iter().zip([self.k.0, self.k.1]).enumerate() {
            let w = well(i);
            p.rect_filled(w, RADIUS, ground);
            if lane.shut {
                hatch(p, w, alpha(pal.ink, HATCH));
            }
            if lane.sound {
                sound(i, w, k);
            }
            out.lanes[i] = w;
        }
        // the playhead lies on the sound and under what is written: a
        // caption's patch covers it as it covers the sound
        if let Some(at) = self.playhead {
            playhead(ctx, p, &out.lanes, at);
        }
        for (i, (lane, k)) in self.lanes.into_iter().zip([self.k.0, self.k.1]).enumerate() {
            let w = well(i);
            let heard = self.playing && k > 0.5 * (1.0 + DIM);
            let (name, row, solid) = caption(
                ctx,
                p,
                &pal,
                lane,
                w,
                self.clock.filter(|_| i == 0),
                heard.then_some(self.playing_label),
            );
            out.names[i] = name;
            out.rows[i] = row;
            out.solid[i] = solid;
            if let Some((icon, words)) = &lane.note {
                note(ctx, p, &pal, w, *icon, words);
            }
            // the sound's and the patches' square corners, under the
            // card again
            p.rect_stroke(
                w,
                RADIUS,
                // a square corner reaches 0.414 of the radius past the
                // round one
                Stroke::new((0.42 * RADIUS).ceil(), card.card_fill()),
                StrokeKind::Outside,
            );
        }
        out
    }
}

/// The playhead in both lanes, `at` (0..=1) of their width: a line and
/// its glow, neither of which leaves a lane (the ground between the two
/// stays clear).
fn playhead(ctx: &Context, p: &Painter, lanes: &[Rect; 2], at: f32) {
    let kit = super::super::kit(ctx);
    let pal = kit.pal.picture();
    let (a, b) = (lanes[0], lanes[1]);
    let x = super::snap(
        a.left() + at.clamp(0.0, 1.0) * a.width(),
        ctx.pixels_per_point(),
    );
    let x = x.clamp(a.left(), a.right() - 1.0);
    let (top, bottom) = (a.top(), b.bottom());
    for lane in [a, b] {
        let within = p.with_clip_rect(lane.intersect(p.clip_rect()));
        if pal.is_dark() && !kit.software {
            if let Some(tex) = kit.tex.as_deref() {
                within.add(Shape::mesh(image(
                    tex.head.id(),
                    Rect::from_min_max(pos2(x - 12.0, top), pos2(x + 12.0, bottom)),
                    Color32::WHITE.additive(),
                )));
            }
        }
        within.rect_filled(
            Rect::from_min_max(pos2(x, top), pos2(x + 1.0, bottom)),
            0.0,
            alpha(pal.light, 0.95),
        );
    }
}

/// What one frame of a file's picture draws.
#[derive(Clone, Debug, PartialEq)]
pub struct FileShow {
    pub original: Lane,
    pub cleaned: Lane,
    /// How much of the lanes' width the picture's columns stand for (the
    /// whole width is the file's length).
    pub covered: f32,
    /// Where the file is and how long it is ("1:38 / 12:40"), over the
    /// upper lane's right end.
    pub clock: Option<String>,
    /// The playhead, 0..=1 of the file.
    pub position: Option<f32>,
    /// While it plays: how much of what is heard is the original, 0..=1
    /// (eased by the page while the sides switch).
    pub heard: Option<f32>,
    /// The words beside the lane that is heard ("Playing").
    pub playing_label: String,
    pub compact: bool,
    /// A lane's height.
    pub lane: f32,
}

/// A whole file's picture: both lanes in one texture, a column of it for
/// each of the picture's. Only the columns that changed since the last
/// frame are uploaded (the pass adds a few at a time; a strength change,
/// another file or the theme builds the texture again).
pub struct Still {
    tex: Option<TextureHandle>,
    ramps: Option<Ramps>,
    /// The levels in the texture, a column after the other.
    levels: Vec<u8>,
}

impl Default for Still {
    fn default() -> Self {
        Self::new()
    }
}

impl Still {
    pub fn new() -> Still {
        Still {
            tex: None,
            ramps: None,
            levels: Vec::new(),
        }
    }

    /// Drop the texture: the page is not shown.
    pub fn forget(&mut self) {
        *self = Still::new();
    }

    /// The columns in the texture.
    pub fn cols(&self) -> usize {
        self.levels.len() / COLUMN_BYTES
    }

    /// Take in the picture's `levels` (`picture::Sheet::levels`: at most
    /// [`COLS_MAX`] columns). Returns the columns uploaded.
    pub fn set(&mut self, ctx: &Context, levels: &[u8]) -> usize {
        let kit = super::super::kit(ctx);
        let levels = &levels[..levels.len().min(COLS_MAX * COLUMN_BYTES)];
        let mut all = self.levels.len() != levels.len();
        if self.ramps.as_ref().map(|r| r.theme) != Some(kit.pal.picture().theme) {
            // the colours are in the texture
            self.ramps = Some(Ramps::of(&kit.pal.picture()));
            all = true;
        }
        if self.tex.is_none() {
            all = true;
            self.tex = Some(ctx.load_texture(
                "hm-file-picture",
                ColorImage::filled([COLS_MAX, TEX_ROWS], Color32::TRANSPARENT),
                TextureOptions::LINEAR,
            ));
        }
        let (Some(ramps), Some(tex)) = (self.ramps.as_ref(), self.tex.as_mut()) else {
            return 0;
        };
        let columns = levels.as_chunks::<COLUMN_BYTES>().0;
        let changed: Vec<usize> = if all {
            (0..columns.len()).collect()
        } else {
            let was = self.levels.as_chunks::<COLUMN_BYTES>().0;
            (0..columns.len())
                .filter(|c| columns[*c] != was[*c])
                .collect()
        };
        let mut px = vec![Color32::TRANSPARENT; TEX_ROWS];
        if all || changed.len() > COLS_MAX / 8 {
            let mut img = ColorImage::filled([COLS_MAX, TEX_ROWS], Color32::TRANSPARENT);
            for (col, column) in columns.iter().enumerate() {
                ramps.column(column, Look::Clean, &mut px);
                for (y, c) in px.iter().enumerate() {
                    img.pixels[y * COLS_MAX + col] = *c;
                }
            }
            tex.set(img, TextureOptions::LINEAR);
        } else {
            for col in &changed {
                ramps.column(&columns[*col], Look::Clean, &mut px);
                tex.set_partial(
                    [*col, 0],
                    ColorImage::new([1, TEX_ROWS], px.clone()),
                    TextureOptions::LINEAR,
                );
            }
        }
        if !changed.is_empty() || all {
            self.levels.clear();
            self.levels.extend_from_slice(levels);
        }
        changed.len()
    }

    /// The lanes' strengths: while it plays, the lane that is heard at
    /// full strength, the other dim.
    fn strengths(heard: Option<f32>) -> (f32, f32) {
        Spectro::strengths(heard.map(|original| Playing {
            position: 0.0,
            original,
        }))
    }

    /// Draw the picture into `rect` ([`Sizes::height`] tall).
    pub fn paint(&mut self, ctx: &Context, p: &Painter, rect: Rect, show: &FileShow) -> Painted {
        let frame = Frame {
            lanes: [&show.original, &show.cleaned],
            k: Self::strengths(show.heard),
            playing: show.heard.is_some(),
            playing_label: &show.playing_label,
            clock: show.clock.as_deref(),
            playhead: show.position,
            compact: show.compact,
            lane: show.lane,
            key: FILES_KEY,
        };
        frame.paint(ctx, p, rect, &mut |i, well, k| {
            self.lane(p, well, rows(i), k, show.covered);
        })
    }

    /// One lane's sound into `well`: the texture's rows `v` (0..1), its
    /// columns over `covered` of the width, at strength `k`.
    fn lane(&self, p: &Painter, well: Rect, v: (f32, f32), k: f32, covered: f32) {
        let (Some(tex), cols) = (self.tex.as_ref(), self.cols()) else {
            return;
        };
        if cols == 0 || covered <= 0.0 {
            return;
        }
        // half a texel in from the columns' ends too
        let (v0, v1) = inset(v);
        // columns past the file's end (its last one is rarely whole) are
        // not in view
        let shown = (cols as f32 / covered.max(1.0)).max(1.0);
        let (u0, u1) = (0.5 / COLS_MAX as f32, (shown - 0.5) / COLS_MAX as f32);
        let right = well.left() + covered.min(1.0) * well.width();
        let tint = Color32::from_white_alpha((255.0 * k).round() as u8);
        let mut m = Mesh::with_texture(tex.id());
        for (x, u) in [(well.left(), u0), (right, u1)] {
            for (y, v) in [(well.top(), v0), (well.bottom(), v1)] {
                m.vertices.push(eframe::egui::epaint::Vertex {
                    pos: pos2(x, y),
                    uv: pos2(u, v),
                    color: tint,
                });
            }
        }
        m.indices.extend_from_slice(&[0, 1, 2, 1, 3, 2]);
        p.add(Shape::mesh(m));
    }
}

/// A lane's caption in the upper corners of its `well`: at the left the
/// glyph, the label and the name; at the right a file's `clock`, the
/// words for the lane that is heard, or the room the page asked for
/// ([`Lane::tail`]). Over sound, each of the two groups stands on a patch
/// of the lane's ground. Returns the name's rect and whether it was cut
/// short (for a name the page puts there: its room), the caption row,
/// and the patches' solid parts.
fn caption(
    ctx: &Context,
    p: &Painter,
    pal: &Palette,
    lane: &Lane,
    well: Rect,
    clock: Option<&str>,
    heard: Option<&str>,
) -> ((Rect, bool), Rect, [Rect; 2]) {
    let [label_ink, strong_ink, weak_ink] = inks(pal);
    let mut solid = [Rect::NOTHING; 2];
    let y = well.top() + INSET.1;
    let row = Rect::from_min_max(
        pos2(well.left() + INSET.0, y),
        pos2(well.right() - INSET.0, y + Type::CAP.line),
    );
    let cy = row.center().y;
    let ground = ground(pal);
    let under = |from: f32, to: f32| {
        Rect::from_min_max(
            pos2(from, well.top()),
            pos2(to, well.top() + caption_zone()),
        )
    };
    // the right end first: the name gets the room it leaves
    let clock = clock.map(|c| text::layout(ctx, c, Type::MONO, label_ink, f32::INFINITY));
    let heard = heard.map(|w| text::layout(ctx, w, Type::CAP, label_ink, f32::INFINITY));
    let mut right = row.right();
    if let Some(b) = &clock {
        right -= b.size().x;
    }
    if let Some(b) = &heard {
        if clock.is_some() {
            right -= 12.0;
        }
        right -= b.size().x + 6.0 + 16.0;
    } else if lane.tail > 0.0 {
        right -= lane.tail;
    }
    let ends = right < row.right();
    if ends && lane.sound {
        let from = (right - PATCH_PAD).max(well.left());
        solid[1] = under(from, well.right());
        p.add(Shape::mesh(patch(
            solid[1],
            ground,
            [true, false, false, true],
        )));
    }
    if let Some(b) = clock {
        let at = row.right() - b.size().x;
        b.paint_centered_y(p, at, cy);
    }
    if let Some(b) = heard {
        icons::paint(p, Icon::Speaker, pos2(right + 8.0, cy), 16.0, label_ink);
        b.paint_centered_y(p, right + 16.0 + 6.0, cy);
    }
    if ends {
        right -= 12.0;
    }
    // the left group: glyph, label, name
    let label = text::layout(ctx, &lane.label, Type::CAP, label_ink, f32::INFINITY);
    let x = row.left() + 16.0 + 8.0;
    let name_x = x + label.size().x + 8.0;
    let room = (right - name_x).max(24.0);
    let (ty, colour) = if lane.strong {
        (Type::CAP.medium(), strong_ink)
    } else {
        (Type::CAP, weak_ink)
    };
    let name = lane
        .select
        .is_none()
        .then(|| text::layout_elided(ctx, &lane.name, ty, colour, room));
    let name_w = match (&name, lane.select) {
        (Some(b), _) => b.size().x,
        (None, w) => w.unwrap_or(0.0).min(room),
    };
    if lane.sound {
        // without a name the patch ends after the label
        let end = if name_w > 0.0 {
            name_x + name_w
        } else {
            name_x - 8.0
        };
        let to = (end + PATCH_PAD).min(well.right());
        solid[0] = under(well.left(), to);
        p.add(Shape::mesh(patch(
            solid[0],
            ground,
            [false, false, true, true],
        )));
    }
    icons::paint(p, lane.glyph, pos2(row.left() + 8.0, cy), 16.0, label_ink);
    label.paint_centered_y(p, x, cy);
    let named = match name {
        Some(b) => {
            let cut = b.elided();
            (b.paint_centered_y(p, name_x, cy), cut)
        }
        None => (
            Rect::from_min_max(pos2(name_x, row.top()), pos2(name_x + room, row.bottom())),
            false,
        ),
    };
    (named, row, solid)
}

/// Words in the middle of a lane that has no sound to show.
fn note(ctx: &Context, p: &Painter, pal: &Palette, well: Rect, icon: Option<Icon>, words: &str) {
    let b = text::layout_elided(ctx, words, Type::CAP, pal.ink_muted, well.width() - 48.0);
    let iw = if icon.is_some() { 16.0 + 6.0 } else { 0.0 };
    let x = well.center().x - (iw + b.size().x) / 2.0;
    let cy = well.center().y;
    if let Some(icon) = icon {
        icons::paint(p, icon, pos2(x + 8.0, cy), 16.0, pal.ink_subtle);
    }
    b.paint_centered_y(p, x + iw, cy);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ramps_start_at_nothing_and_end_at_the_theme_colours() {
        for theme in [Theme::Dark, Theme::Light] {
            // the picture's palette, whichever theme the page has
            let pal = Palette::of(theme).picture();
            let r = Ramps::of(&pal);
            for ramp in [&r.neutral, &r.accent] {
                assert_eq!(ramp.len(), 256);
                assert_eq!(ramp[0], Color32::TRANSPARENT);
                assert_eq!(ramp[255].a(), 255);
                // louder is never fainter
                assert!(ramp.windows(2).all(|w| w[0].a() <= w[1].a()));
            }
            assert_eq!(r.neutral[255], pal.ink);
            // the microphone's lane has no colour of its own: the
            // palette's grey, whose cool cast is a few steps at most
            let n = r.neutral[180];
            let (lo, hi) = (n.r().min(n.g()).min(n.b()), n.r().max(n.g()).max(n.b()));
            assert!(hi - lo <= 8, "{n:?}");
            let a = r.accent[178].to_srgba_unmultiplied();
            let want = pal.accent.to_srgba_unmultiplied();
            for i in 0..3 {
                assert!(a[i].abs_diff(want[i]) <= 4, "the accent at 70 %: {a:?}");
            }
        }
    }

    /// The level scale is one for both lanes; what it is drawn with has a
    /// knee: a quiet room's floor stays a haze, speech stands clear of it.
    #[test]
    fn a_rooms_floor_is_a_haze_and_speech_is_clear() {
        assert_eq!(strength(0.0), 0.0);
        assert!((strength(KNEE) - HAZE).abs() < 1e-6);
        assert_eq!(strength(1.0), 1.0);
        let steps: Vec<f32> = (0..=255).map(|i| strength(i as f32 / 255.0)).collect();
        assert!(steps.windows(2).all(|w| w[0] <= w[1]));
        // the floor's middle is nearly nothing; a voice's quieter
        // harmonics (-48 dB in a band) are past half
        assert!(strength(0.3) < 0.08, "{}", strength(0.3));
        assert!(strength(0.6) > 0.5, "{}", strength(0.6));
    }

    /// A patch is solid over its core and gone at the end of its fade,
    /// which it has on the sides asked for only: at a lane's edge it is
    /// flush.
    #[test]
    fn a_patch_is_solid_under_the_words_and_fades_out_beside_them() {
        let ground = ground(&Palette::of(Theme::Dark));
        let core = Rect::from_min_max(pos2(100.0, 50.0), pos2(180.0, 70.0));
        let m = patch(core, ground, [false, false, true, true]);
        let mut bounds = Rect::NOTHING;
        for (n, v) in m.vertices.iter().enumerate() {
            bounds.extend_with(v.pos);
            // a 4 x 4 grid: the four in the middle are the core's corners
            let solid = (1..=2).contains(&(n % 4)) && (1..=2).contains(&(n / 4));
            let want = if solid {
                alpha(ground, SCRIM)
            } else {
                Color32::TRANSPARENT
            };
            assert_eq!(v.color, want, "{n} at {:?}", v.pos);
            if solid {
                let corner = |a: f32, r: eframe::egui::Rangef| a == r.min || a == r.max;
                assert!(corner(v.pos.x, core.x_range()) && corner(v.pos.y, core.y_range()));
            }
        }
        assert_eq!(bounds.left_top(), core.left_top(), "flush at the edge");
        assert_eq!(
            bounds.right_bottom(),
            core.right_bottom() + eframe::egui::vec2(PATCH_FADE, PATCH_FADE / 2.0)
        );
        // every side fading: the patch is larger all round
        let m = patch(core, ground, [true; 4]);
        let mut bounds = Rect::NOTHING;
        for v in &m.vertices {
            bounds.extend_with(v.pos);
        }
        assert_eq!(
            bounds,
            core.expand2(eframe::egui::vec2(PATCH_FADE, PATCH_FADE / 2.0))
        );
        assert_eq!(m.indices.len(), 9 * 6);
    }

    /// What is written in a lane reads over the loudest sound behind it:
    /// at least 4.5:1 against the brightest colour either ramp ends in,
    /// seen through the solid part of its patch, in both themes.
    #[test]
    fn what_is_written_in_a_lane_reads_over_the_loudest_sound() {
        for theme in [Theme::Dark, Theme::Light] {
            // the picture's palette, whichever theme the page has
            let pal = Palette::of(theme).picture();
            let r = Ramps::of(&pal);
            let ground = ground(&pal);
            for peak in [r.neutral[255], r.accent[255]] {
                // the sound over the ground, then the solid part of a
                // patch over both
                let core = Rect::from_min_max(pos2(0.0, 0.0), pos2(40.0, 20.0));
                let solid = patch(core, ground, [true; 4]).vertices[5].color;
                assert_eq!(solid, alpha(ground, SCRIM));
                let behind = mix(peak, ground, SCRIM);
                let mut written = inks(&pal).to_vec();
                written.push(pal.warn);
                for ink in written {
                    let c = crate::ui::tokens::tests::contrast(ink, behind);
                    assert!(c >= 4.5, "{theme:?}: {ink:?} on {behind:?} is {c:.2}:1");
                }
            }
        }
    }

    #[test]
    fn a_column_goes_into_the_texture_highest_band_first() {
        let r = Ramps::of(&Palette::of(Theme::Dark));
        let mut levels = [0u8; COLUMN_BYTES];
        levels[0] = 255;
        levels[ROWS + ROWS - 1] = 255;
        let mut px = vec![Color32::TRANSPARENT; TEX_ROWS];
        r.column(&levels, Look::Clean, &mut px);
        assert_eq!(
            px[ROWS - 1],
            r.neutral[255],
            "the lowest band: the lane's bottom"
        );
        assert_eq!(
            px[ROWS], r.accent[255],
            "the highest: the top of the lane below"
        );
        assert!(px[..ROWS - 1].iter().all(|c| *c == Color32::TRANSPARENT));
        // where apps heard the original, their lane is neutral too
        r.column(&levels, Look::Original, &mut px);
        assert_eq!(px[ROWS], r.neutral[255]);
    }

    #[test]
    fn a_files_picture_uploads_only_the_columns_that_changed() {
        let ctx = Context::default();
        crate::ui::install(&ctx, Theme::Dark);
        let mut still = Still::new();
        let mut levels = vec![0u8; 700 * COLUMN_BYTES];
        assert_eq!(still.set(&ctx, &levels), 700, "a new picture: all of it");
        assert_eq!(still.cols(), 700);
        assert_eq!(still.set(&ctx, &levels), 0, "nothing new");
        // the pass cleaned three more columns
        for col in 300..303 {
            levels[col * COLUMN_BYTES + ROWS + 7] = 200;
        }
        assert_eq!(still.set(&ctx, &levels), 3);
        assert_eq!(still.set(&ctx, &levels), 0);
        // another file: its own columns
        assert_eq!(still.set(&ctx, &vec![9u8; 40 * COLUMN_BYTES]), 40);
        assert_eq!(still.cols(), 40);
        // no more than the texture is wide
        assert_eq!(
            still.set(&ctx, &vec![1u8; (COLS_MAX + 9) * COLUMN_BYTES]),
            COLS_MAX
        );
        still.forget();
        assert_eq!(still.cols(), 0);
    }

    #[test]
    fn the_lane_that_is_heard_is_at_full_strength() {
        assert_eq!(Spectro::strengths(None), (1.0, 1.0));
        let at = |original| {
            Spectro::strengths(Some(Playing {
                position: 0.5,
                original,
            }))
        };
        assert_eq!(at(0.0), (DIM, 1.0));
        assert_eq!(at(1.0), (1.0, DIM));
        let (m, a) = at(0.5);
        assert!((m - a).abs() < 1e-6);
    }
}
