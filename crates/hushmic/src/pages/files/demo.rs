//! The demo source: the boards' file model (`AppFile.js` FileModel and the
//! App-Files board's queue), for mock windows, screenshots and tests. It
//! simulates the pass and the playback clock on a recording of speech
//! built from the demo clips' levels the way the boards build it, and
//! draws that recording's picture from Live's demo take, so a still of a
//! state is the same picture every time.

use super::model::{
    Cmd, Estimate, FileId, Kind, Pass, Place, Play, Reason, Row, Save, Source, Strength, View,
};
use crate::files::picture::{self, Sheet};
use crate::miccheck::spectrum::{Cell, HOP, HOPS_PER_COLUMN, ROWS};
use crate::ui::paint::clips::{self, Clip};
use std::sync::{Arc, OnceLock};

/// One column of the recording's levels: 50 ms.
const COL: f64 = 0.05;

/// The pass, × real time: Quality on one core (appeng's measurement).
pub const SPEED: f64 = 4.2;
/// Review stills that would last under a second at [`SPEED`] run at this
/// labelled demo speed (the boards' caught-up states).
const DEMO_SPEED: f64 = 0.05;
/// To the first cleaned block ("about half a second"); also a new front's
/// warm-up after a seek.
const FIRST_BLOCK: f64 = 0.5;
/// After catching up, play resumes once the front is this far ahead (or
/// 30 s × (1 − speed) on a device slower than real time).
const RESUME_LEAD: f64 = 2.0;
const POUR: f64 = 0.4;

// ---------------------------------------------------------------- the sample

/// The boards' seeded generator (`mulberry32`), so the sample is the same
/// recording on every board.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let s = self.0;
        let mut t = (s ^ (s >> 15)).wrapping_mul(1 | s);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
}

fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

/// The clip's phrases: runs of voice of at least 0.3 s.
fn phrases(c: &Clip) -> Vec<(usize, usize)> {
    let (f, mut out, mut i) = (c.frames(), Vec::new(), 0);
    while i < f {
        while i < f && c.clean[i] == 0.0 {
            i += 1;
        }
        if i >= f {
            break;
        }
        let (a, mut gap) = (i, 0);
        while i < f && gap <= 2 {
            gap = if c.clean[i] == 0.0 { gap + 1 } else { 0 };
            i += 1;
        }
        let b = i - gap;
        if b - a >= 6 {
            out.push((a, b));
        }
    }
    out
}

/// A recording of `n` columns built from the clip's real levels as speech:
/// its phrases varied in length, order and level, pauses of 0.5 to 3 s
/// between them (now and then 5 to 10 s), room noise after the last word.
/// It starts on speech, so a board at 0:00 shows the cleaned voice at once
/// (F16), not a silent intro. A file no longer than the clip is the clip.
fn sample(n: usize, c: &Clip) -> (Vec<f32>, Vec<f32>) {
    let (mut raw, mut clean) = (vec![0.0f32; n], vec![0.0f32; n]);
    let noise: Vec<f32> = (0..c.frames())
        .filter(|&i| c.clean[i] == 0.0)
        .map(|i| c.raw[i])
        .collect();
    let phr = phrases(c);
    if n <= c.frames() || phr.is_empty() || noise.is_empty() {
        for i in 0..n {
            (raw[i], clean[i]) = c.at(i);
        }
        return (raw, clean);
    }
    let mut r = Rng(7);
    let (mut at, mut ni) = (0usize, 0usize);
    let mut quiet = |sec: f64, at: &mut usize, raw: &mut [f32], clean: &mut [f32]| {
        let mut q = (sec / COL).round() as i64;
        while q > 0 && *at < n {
            raw[*at] = noise[ni % noise.len()];
            clean[*at] = 0.0;
            ni += 1;
            q -= 1;
            *at += 1;
        }
    };
    let outro = (4.0 / COL).round() as usize;
    while at + outro < n {
        let p = phr[(r.next() * phr.len() as f64).floor() as usize];
        let len = p.1 - p.0;
        let used = 6usize.max((len as f64 * (0.6 + 0.4 * r.next())).round() as usize);
        let from = p.0 + (r.next() * (len as f64 - used as f64 + 1.0)).floor() as usize;
        let rev = r.next() < 0.4;
        let g = (r.next() - 0.6) * 0.08;
        let mut k = 0;
        while k < used && at + outro < n {
            let j = if rev { from + used - 1 - k } else { from + k };
            let (cr, cl) = (f64::from(c.raw[j]), f64::from(c.clean[j]));
            let rv = if cl > 0.0 {
                cr.max(clamp01(cr + g))
            } else {
                cr
            };
            raw[at] = rv as f32;
            // a denoiser only takes away
            clean[at] = if cl > 0.0 {
                f64::from(raw[at]).min(clamp01(cl + g)) as f32
            } else {
                0.0
            };
            k += 1;
            at += 1;
        }
        let pause = if r.next() < 0.1 {
            5.0 + 5.0 * r.next()
        } else {
            0.5 + 2.5 * r.next()
        };
        quiet(pause, &mut at, &mut raw, &mut clean);
    }
    let rest = (n - at) as f64 * COL;
    quiet(rest, &mut at, &mut raw, &mut clean);
    (raw, clean)
}

/// The hops of Live's demo take, sorted into those with a voice in them
/// and those with the room alone.
fn hops() -> &'static (Vec<usize>, Vec<usize>) {
    static HOPS: OnceLock<(Vec<usize>, Vec<usize>)> = OnceLock::new();
    HOPS.get_or_init(|| {
        let take = crate::miccheck::demo::spectrum();
        (0..take.cols()).partition(|j| take.cells(*j).iter().map(|c| c.clean).sum::<f32>() > 1e-9)
    })
}

/// The picture of a recording `length` seconds long whose 50 ms columns
/// have a voice where `voice` says: every hop is one of the demo take's,
/// the voiced ones and the room's each in their own order, so words stay
/// words. A long file's column is the mean of a few of its hops.
fn sheet(length: f64, voice: &[bool]) -> Sheet {
    let frames = (length * 48_000.0).round().max(1.0) as u64;
    let span = picture::span_for(Some(frames));
    let cols = frames.div_ceil(span) as usize;
    let per = (span / HOP as u64) as usize;
    let take = crate::miccheck::demo::spectrum();
    let (voiced, quiet) = hops();
    // voiced columns before each column
    let before: Vec<usize> = voice
        .iter()
        .scan(0, |n, v| {
            let was = *n;
            *n += *v as usize;
            Some(was)
        })
        .collect();
    let mut out = Sheet::new(span, cols);
    let mut cells = Box::new([Cell::default(); ROWS]);
    for col in 0..cols {
        cells.fill(Cell::default());
        let mut n = 0usize;
        for hop in (col * per..(col + 1) * per).step_by((per / 4).max(1)) {
            let k = hop / HOPS_PER_COLUMN;
            let Some(&v) = voice.get(k) else {
                break;
            };
            let (of, nth) = if v {
                (voiced, before[k])
            } else {
                (quiet, k - before[k])
            };
            if of.is_empty() {
                continue;
            }
            let j = of[(nth * HOPS_PER_COLUMN + hop % HOPS_PER_COLUMN) % of.len()];
            for (sum, c) in cells.iter_mut().zip(take.cells(j)) {
                sum.raw += c.raw;
                sum.clean += c.clean;
                sum.cross += c.cross;
            }
            n += 1;
        }
        if n > 0 {
            let k = 1.0 / n as f32;
            for c in cells.iter_mut() {
                *c = Cell {
                    raw: c.raw * k,
                    clean: c.clean * k,
                    cross: c.cross * k,
                };
            }
        }
        out.set(col, &cells, true);
    }
    out
}

// ---------------------------------------------------------------- one file

#[derive(Clone, Copy, Debug)]
struct Seg {
    a: f64,
    b: f64,
    /// The front grows from this time on (a warm-up after a seek).
    from: f64,
}

/// One file's pass and playback (FileModel).
struct Sim {
    length: f64,
    /// The recording's picture, cleaned all through, and as far as the
    /// pass has come.
    whole: Sheet,
    sheet: Arc<Sheet>,
    segs: Vec<Seg>,
    active: Option<usize>,
    rendering: bool,
    waiting: bool,
    failed: Option<Reason>,
    pos: f64,
    playing: bool,
    peek: bool,
    hold: bool,
    caught: bool,
    fresh: bool,
    loaded: f64,
    first: f64,
    done_at: Option<f64>,
    speed: f64,
    yielded: bool,
    /// The strength in force: the original's share in the mix.
    alpha: f32,
}

impl Sim {
    fn load(length: f64, clip: &Clip, waiting: bool, now: f64) -> Sim {
        let n = 1usize.max((length / COL).ceil() as usize);
        let (_, clean) = sample(n, clip);
        let voice: Vec<bool> = clean.iter().map(|c| *c > 0.0).collect();
        let whole = sheet(length, &voice);
        let mut s = Sim {
            length,
            sheet: Arc::new(Sheet::new(1, 0)),
            whole,
            segs: vec![Seg {
                a: 0.0,
                b: 0.0,
                from: now + FIRST_BLOCK,
            }],
            active: Some(0),
            rendering: true,
            waiting,
            failed: None,
            pos: 0.0,
            playing: false,
            peek: false,
            hold: false,
            caught: false,
            fresh: true,
            loaded: now,
            first: now + FIRST_BLOCK,
            done_at: None,
            speed: SPEED,
            yielded: false,
            alpha: 0.0,
        };
        if waiting {
            s.first = f64::INFINITY;
            s.rendering = false;
            s.segs.clear();
            s.active = None;
        }
        s
    }

    /// The picture as far as the pass has come: the columns whose middle
    /// is cleaned are lit. The same `Arc` while nothing changed.
    fn relight(&mut self) {
        let secs = self.whole.column_secs();
        let segs = &self.segs;
        let end = self.length - 1e-3;
        let lit = |col: usize| {
            // (the last column is rarely whole)
            let t = ((col as f64 + 0.5) * secs).min(end);
            segs.iter().any(|g| t >= g.a - 1e-6 && t < g.b - 1e-6)
        };
        let n = (0..self.whole.cols()).filter(|c| lit(*c)).count();
        if self.sheet.cols() == self.whole.cols() && self.sheet.lit() == n {
            return;
        }
        let mut sheet = self.whole.clone();
        sheet.light(lit);
        self.sheet = Arc::new(sheet);
    }

    /// A waiting file's pass starts (the queue reached it, or it plays).
    fn start(&mut self, now: f64) {
        if !self.waiting {
            return;
        }
        self.waiting = false;
        self.first = now + FIRST_BLOCK;
        self.loaded = self.loaded.min(now - POUR);
        self.rendering = true;
        self.segs = vec![Seg {
            a: 0.0,
            b: 0.0,
            from: self.first,
        }];
        self.active = Some(0);
    }

    /// Straight to a moment, the pour and the morph over (board states).
    fn preset(&mut self, p: &Pre, now: f64) {
        let past = now - 10.0;
        let l = self.length;
        self.loaded = past;
        self.first = past;
        self.segs = match &p.segments {
            Some(sg) => sg
                .iter()
                .map(|&(a, b)| Seg {
                    a,
                    b: b.min(l),
                    from: past,
                })
                .collect(),
            None => vec![Seg {
                a: 0.0,
                b: p.rendered.map_or(l, |r| r.min(l)),
                from: past,
            }],
        };
        self.rendering = self.covered() < l - 1e-6;
        self.active = None;
        if self.rendering {
            let mut act = 0;
            for (i, g) in self.segs.iter().enumerate() {
                if p.pos >= g.a - 1e-6 {
                    act = i;
                }
            }
            self.active = Some(act);
        }
        self.done_at = (!self.rendering).then_some(past);
        self.pos = p.pos.clamp(0.0, l);
        self.failed = None;
        self.fresh = false;
        self.waiting = false;
        self.hold = p.holding;
        self.peek = false;
        self.playing = p.playing;
        self.caught = p.caught;
    }

    fn covered(&self) -> f64 {
        self.segs.iter().map(|g| g.b - g.a).sum()
    }

    fn seg_at(&self, s: f64) -> Option<usize> {
        self.segs
            .iter()
            .position(|g| s >= g.a - 1e-6 && s < g.b - 1e-6)
    }

    fn done(&self) -> bool {
        self.failed.is_none() && !self.rendering && !self.waiting
    }

    fn time_left(&self, now: f64) -> Option<f64> {
        if !self.rendering || self.yielded || now - self.loaded.max(self.first - FIRST_BLOCK) < 3.0
        {
            return None;
        }
        Some((self.length - self.covered()) / self.speed)
    }

    fn play(&mut self, now: f64) {
        if self.failed.is_some() {
            return;
        }
        if self.waiting {
            self.start(now);
        }
        if self.pos >= self.length - 1e-3 {
            self.seek(0.0, now);
        }
        self.playing = true;
        self.peek = false;
    }

    fn pause(&mut self) {
        self.playing = false;
        self.peek = false;
        self.caught = false;
    }

    /// The original on or off; while paused, on plays for as long as it
    /// stays on (a peek).
    fn hold(&mut self, on: bool) {
        if self.failed.is_some() {
            return;
        }
        let peek = on && !self.playing;
        if self.hold == on && !peek {
            return;
        }
        self.hold = on;
        if peek {
            self.playing = true;
            self.peek = true;
        } else if !on && self.peek {
            self.playing = false;
            self.peek = false;
        }
    }

    fn seek(&mut self, to: f64, now: f64) {
        self.pos = to.clamp(0.0, self.length);
        self.caught = false;
        if self.rendering && self.seg_at(self.pos).is_none() && self.pos < self.length {
            // a seek past the cleaned part starts a new front at the
            // playhead (after its warm-up)
            let g = Seg {
                a: self.pos,
                b: self.pos,
                from: now + FIRST_BLOCK,
            };
            let at = self.segs.partition_point(|x| x.a <= g.a);
            self.segs.insert(at, g);
            self.active = Some(at);
        }
        self.fresh = true;
    }

    fn tick(&mut self, dt: f64, now: f64) {
        if self.failed.is_some() {
            return;
        }
        if self.rendering {
            self.render_on(dt, now);
        }
        if self.playing {
            self.play_on(dt);
        }
    }

    fn render_on(&mut self, dt: f64, now: f64) {
        let l = self.length;
        let Some(i) = self.active else {
            return;
        };
        if self.yielded {
            return;
        }
        let g = self.segs[i];
        let adv = self.speed * dt.min(now - g.from).max(0.0);
        let mut b = (g.b + adv).min(l);
        // merged with the next part once the front reaches it
        while i + 1 < self.segs.len() && b >= self.segs[i + 1].a {
            b = b.max(self.segs[i + 1].b);
            self.segs.remove(i + 1);
        }
        self.segs[i].b = b;
        if b >= l - 1e-6 {
            // the end: on with the first gap from the start, if any
            let (mut gap, mut k) = (0.0f64, 0);
            while k < self.segs.len() && self.segs[k].a <= gap + 1e-6 {
                gap = gap.max(self.segs[k].b);
                k += 1;
            }
            if gap >= l - 1e-6 {
                self.rendering = false;
                self.active = None;
                self.done_at = Some(now);
                return;
            }
            self.segs.insert(
                k,
                Seg {
                    a: gap,
                    b: gap,
                    from: now,
                },
            );
            self.active = Some(k);
        }
    }

    fn play_on(&mut self, dt: f64) {
        let (l, p0) = (self.length, self.pos);
        let p1 = if self.hold {
            // the original needs no pass
            self.caught = false;
            (p0 + dt).min(l)
        } else {
            let seg = self.seg_at(p0).map(|i| self.segs[i]);
            let limit = seg.map_or(p0, |g| g.b);
            if self.caught {
                let lead = if self.fresh {
                    0.25
                } else {
                    RESUME_LEAD.max(30.0 * (1.0 - self.speed))
                };
                if seg.is_some_and(|g| g.b - p0 >= lead || g.b >= l - 1e-6) {
                    self.caught = false;
                }
                p0
            } else {
                let p1 = (p0 + dt).min(limit);
                if p1 >= limit - 1e-6 && limit < l - 1e-6 {
                    self.caught = true;
                }
                p1
            }
        };
        if p1 > p0 {
            self.fresh = false;
        }
        self.pos = p1;
        if p1 >= l - 1e-6 {
            self.playing = false;
            self.peek = false;
        }
    }
}

// ---------------------------------------------------------------- the page's files

/// A board state's moment for the selected file.
#[derive(Clone, Debug, Default)]
struct Pre {
    pos: f64,
    /// Seconds cleaned from the start (None: all).
    rendered: Option<f64>,
    segments: Option<Vec<(f64, f64)>>,
    playing: bool,
    caught: bool,
    holding: bool,
}

/// One file on the page (the board's row).
#[derive(Clone)]
struct DRow {
    id: FileId,
    name: String,
    length: f64,
    kind: Kind,
    clip: &'static Clip,
    /// Seconds cleaned from the start; None: all.
    rendered: Option<f64>,
    segments: Option<Vec<(f64, f64)>>,
    pos: f64,
    saved: bool,
    /// The strength it was saved at.
    saved_at: Strength,
    want: bool,
    att: Option<Reason>,
    yielded: bool,
    /// Stopped partway, waiting its turn (another file was played): it
    /// stands still until it plays.
    stopped: bool,
    damaged: Option<f64>,
}

impl DRow {
    fn new(id: FileId, name: &str, length: f64) -> DRow {
        DRow {
            id,
            name: name.into(),
            length,
            kind: Kind::Audio,
            clip: &clips::KEYBOARD,
            rendered: Some(0.0),
            segments: None,
            pos: 0.0,
            saved: false,
            saved_at: Strength::Strong,
            want: false,
            att: None,
            yielded: false,
            stopped: false,
            damaged: None,
        }
    }

    fn done(&self) -> bool {
        self.rendered.is_none() && self.segments.is_none()
    }

    fn covered(&self) -> f64 {
        match (&self.segments, self.rendered) {
            (Some(sg), _) => sg.iter().map(|(a, b)| b - a).sum(),
            (None, Some(r)) => r,
            (None, None) => self.length,
        }
    }

    fn parts(&self) -> Vec<(f64, f64)> {
        match (&self.segments, self.rendered) {
            (Some(sg), _) => sg.clone(),
            (None, Some(r)) if r > 0.0 => vec![(0.0, r.min(self.length))],
            (None, Some(_)) => vec![],
            (None, None) => vec![(0.0, self.length)],
        }
    }
}

fn interview(id: FileId) -> DRow {
    DRow::new(id, "interview.m4a", 760.0)
}

fn lecture(id: FileId) -> DRow {
    DRow {
        kind: Kind::Video,
        clip: &clips::CAFE,
        ..DRow::new(id, "lecture-03.mp4", 2650.0)
    }
}

fn voice_note(id: FileId) -> DRow {
    DRow {
        clip: &clips::FAN,
        ..DRow::new(id, "voice-note.opus", 95.0)
    }
}

/// The cleaned file's name: "interview (cleaned).m4a" (O3: the same format).
pub fn output_name(name: &str) -> String {
    match name.rfind('.') {
        Some(i) => format!("{} (cleaned){}", &name[..i], &name[i..]),
        None => format!("{name} (cleaned)"),
    }
}

/// The demo's files: the board's queue and the selected file's model.
pub struct DemoFiles {
    rows: Vec<DRow>,
    sel: usize,
    sim: Option<Sim>,
    strength: Strength,
    /// Save waits for the selected file's pass.
    save_wait: bool,
    /// A long write: how far it is.
    saving: Option<f64>,
    /// A review still at the labelled demo speed.
    demo_speed: bool,
    /// Screenshots: the clock stands still.
    frozen: bool,
    last: Option<f64>,
    next_id: FileId,
    serial: u64,
    /// Clear all took these (and the selection then), at this moment:
    /// Undo brings them back for [`crate::files::jobs::UNDO_FOR`].
    cleared: Option<(Vec<DRow>, usize, f64)>,
    /// "Save all and quit" waits for this many files (the dev "quitting"
    /// state).
    quitting: Option<usize>,
}

impl Default for DemoFiles {
    fn default() -> DemoFiles {
        DemoFiles {
            rows: Vec::new(),
            sel: 0,
            sim: None,
            strength: Strength::Strong,
            save_wait: false,
            saving: None,
            demo_speed: false,
            frozen: false,
            last: None,
            next_id: 1,
            serial: 1,
            cleared: None,
            quitting: None,
        }
    }
}

/// States the daemon reports that have no board (PLAN §3 copy).
pub const MORE_STATES: [&str; 13] = [
    "unreadable",
    "no-sound",
    "silent",
    "engine",
    "stopped",
    "stopped-again",
    "cache",
    "damaged",
    "no-write",
    "save-encode",
    "save-read",
    "save-failed",
    "moved-save",
];

/// App-Files2's states that App-Files has no name for (its "Several ·"
/// states; "queue" is its "Several · cleaning").
pub const LIST_STATES: [&str; 3] = ["save-all", "all-saved", "five"];

/// The board's state names (App-Files' "State" tweak).
pub const STATES: [&str; 19] = [
    "empty",
    "drag-over",
    "added",
    "playing",
    "holding",
    "strength-change",
    "paused",
    "caught-up",
    "seek-past-the-front",
    "save-waiting",
    "cleaned",
    "saving",
    "saved",
    "queue",
    "yield",
    "video",
    "format",
    "no-space",
    "moved",
];

impl DemoFiles {
    fn id(&mut self) -> FileId {
        self.next_id += 1;
        self.next_id - 1
    }

    /// A board state at `now`, then `secs` of it played (the boards render
    /// their stills 3 s after the state is set); the clock then stands
    /// still. Unknown names give the empty page.
    pub fn preset(name: &str, now: f64, secs: f64) -> DemoFiles {
        let mut d = DemoFiles::default();
        let mut pre = Pre::default();
        let mut fresh = false;
        let at = |r: DRow, rendered: Option<f64>, pos: f64| DRow { rendered, pos, ..r };
        let (i1, i2, i3) = (d.id(), d.id(), d.id());
        let saved = |r: DRow| DRow { saved: true, ..r };
        let want = |r: DRow| DRow { want: true, ..r };
        match name {
            "added" => {
                d.rows = vec![interview(i1)];
                fresh = true;
            }
            "playing" | "holding" | "strength-change" | "save-waiting" => {
                d.rows = vec![at(interview(i1), Some(420.0), 95.0)];
                pre.playing = true;
                pre.holding = name == "holding";
                if name == "save-waiting" {
                    d.rows[0].want = true;
                    d.save_wait = true;
                }
            }
            "paused" => d.rows = vec![at(interview(i1), Some(420.0), 95.0)],
            "caught-up" => {
                d.rows = vec![at(interview(i1), Some(212.3), 212.0)];
                pre.playing = true;
                pre.caught = true;
                d.demo_speed = true;
            }
            "seek-past-the-front" => {
                d.rows = vec![DRow {
                    segments: Some(vec![(0.0, 300.0), (520.0, 520.6)]),
                    ..at(interview(i1), Some(0.0), 520.0)
                }];
                pre.playing = true;
                pre.caught = true;
                d.demo_speed = true;
            }
            "cleaned" => d.rows = vec![at(interview(i1), None, 233.0)],
            // a finished file just opened: nothing heard yet (F16)
            "cleaned-start" => d.rows = vec![at(interview(i1), None, 0.0)],
            // dev: the selected file stopped partway (another was played)
            "stopped-partway" => {
                d.rows = vec![
                    at(interview(i1), Some(400.0), 0.0),
                    DRow {
                        stopped: true,
                        ..at(lecture(i2), Some(1010.0), 0.0)
                    },
                    voice_note(i3),
                ];
                d.sel = 1;
            }
            "saving" => {
                d.rows = vec![at(lecture(i1), None, 610.0)];
                d.saving = Some(0.35);
            }
            "saved" => {
                d.rows = vec![DRow {
                    saved: true,
                    ..at(interview(i1), None, 233.0)
                }]
            }
            "queue" => {
                d.rows = vec![
                    at(interview(i1), None, 233.0),
                    at(lecture(i2), Some(1010.0), 0.0),
                    voice_note(i3),
                ]
            }
            "save-all" => {
                d.rows = vec![
                    saved(at(interview(i1), None, 0.0)),
                    want(at(lecture(i2), Some(1010.0), 0.0)),
                    want(voice_note(i3)),
                ];
                d.sel = 1;
                d.save_wait = true;
            }
            "all-saved" => {
                d.rows = vec![
                    saved(at(interview(i1), None, 233.0)),
                    saved(at(lecture(i2), None, 0.0)),
                    saved(at(voice_note(i3), None, 0.0)),
                ]
            }
            "five" => {
                let (i4, i5) = (d.id(), d.id());
                d.rows = vec![
                    saved(at(interview(i1), None, 0.0)),
                    at(voice_note(i2), None, 0.0),
                    at(lecture(i3), Some(1010.0), 0.0),
                    DRow {
                        clip: &clips::CAFE,
                        ..DRow::new(i4, "podcast-episode-12-final-mix.mp3", 3310.0)
                    },
                    DRow {
                        clip: &clips::FAN,
                        ..DRow::new(i5, "team-meeting.flac", 1820.0)
                    },
                ];
                d.sel = 2;
            }
            "quitting" => {
                d.rows = vec![
                    saved(at(interview(i1), None, 0.0)),
                    at(lecture(i2), Some(1010.0), 0.0),
                    voice_note(i3),
                ];
                d.sel = 1;
                d.quitting = Some(2);
            }
            "yield" => {
                d.rows = vec![
                    DRow {
                        saved: true,
                        ..at(interview(i1), None, 0.0)
                    },
                    DRow {
                        yielded: true,
                        ..at(lecture(i2), Some(1010.0), 0.0)
                    },
                    voice_note(i3),
                ];
                d.sel = 1;
            }
            "video" => {
                d.rows = vec![at(lecture(i1), Some(900.0), 312.0)];
                pre.playing = true;
            }
            "format" => {
                d.rows = vec![
                    at(interview(i1), None, 0.0),
                    DRow {
                        att: Some(Reason::Unsupported { ext: "wma".into() }),
                        ..DRow::new(i2, "meeting.wma", 0.0)
                    },
                ];
                d.sel = 1;
            }
            "no-space" => {
                d.rows = vec![DRow {
                    att: Some(Reason::NoSpace),
                    ..at(interview(i1), None, 233.0)
                }]
            }
            // a Save that failed otherwise: cleaned, not saved
            "no-write" | "save-encode" | "save-read" | "save-failed" => {
                let att = match name {
                    "no-write" => Reason::NoWrite {
                        folder: "Cleaned".into(),
                    },
                    "save-encode" => Reason::SaveEncode {
                        format: "M4A".into(),
                        why: Some(
                            "Invalid audio stream. Exactly one MP3 audio stream is required".into(),
                        ),
                    },
                    "save-read" => Reason::SaveRead,
                    _ => Reason::SaveFailed {
                        why: Some("Input/output error".into()),
                    },
                };
                d.rows = vec![DRow {
                    att: Some(att),
                    ..at(interview(i1), None, 233.0)
                }]
            }
            "moved-save" => {
                d.rows = vec![DRow {
                    att: Some(Reason::Moved { at_save: true }),
                    ..at(interview(i1), None, 0.0)
                }]
            }
            "moved" => {
                d.rows = vec![DRow {
                    att: Some(Reason::Moved { at_save: false }),
                    ..at(interview(i1), Some(388.0), 0.0)
                }]
            }
            // the daemon's other problems (PLAN §3, no boards)
            "unreadable" => {
                d.rows = vec![DRow {
                    att: Some(Reason::Unreadable),
                    ..DRow::new(i1, "interview.m4a", 0.0)
                }]
            }
            "no-sound" => {
                d.rows = vec![DRow {
                    att: Some(Reason::NoSoundTrack),
                    ..at(lecture(i1), Some(0.0), 0.0)
                }]
            }
            "silent" | "engine" | "stopped" | "stopped-again" | "cache" | "damaged" => {
                let att = match name {
                    "silent" => Reason::Silent,
                    "engine" => Reason::Engine,
                    "stopped" => Reason::Failed { again: false },
                    "stopped-again" => Reason::Crashed { again: true },
                    "cache" => Reason::Cache {
                        bytes: Some(300_000_000),
                    },
                    _ => Reason::Damaged,
                };
                let rendered = if name == "silent" { None } else { Some(0.0) };
                d.rows = vec![DRow {
                    att: Some(att),
                    damaged: (name == "damaged").then_some(192.0),
                    ..at(interview(i1), rendered, 0.0)
                }]
            }
            _ => {}
        }
        if name == "strength-change" {
            // the board's still shows the played part at Medium too (its
            // model keeps the strength across the state's two loads)
            d.strength = Strength::Medium;
        }
        d.load(d.sel, Some(&pre), fresh, now);
        // the board's "Added" still is captured just before the pass's
        // rate is known (3 s): Save, not "Ready in …"
        let secs = if fresh { secs.min(2.9) } else { secs };
        d.last = Some(now);
        d.advance(secs, now);
        d.frozen = true;
        d
    }

    /// The live demo (mock windows): `n` sample files to start with.
    pub fn live() -> DemoFiles {
        DemoFiles::default()
    }

    /// Play `secs` of the demo in 50 ms steps.
    fn advance(&mut self, secs: f64, now: f64) {
        let steps = (secs / COL).round() as usize;
        let t0 = now - secs;
        if let Some(sim) = self.sim.as_mut() {
            // the state was set `secs` ago
            sim.loaded -= secs;
            sim.first -= secs;
            if let Some(d) = sim.done_at.as_mut() {
                *d -= secs;
            }
            for g in sim.segs.iter_mut() {
                g.from -= secs;
            }
        }
        for i in 0..steps {
            self.step(COL, t0 + (i + 1) as f64 * COL);
        }
    }

    /// The first file whose pass isn't done: the pass works on one file at
    /// a time, in order.
    fn active_index(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| r.att.is_none() && r.length > 0.0 && !r.done())
    }

    /// Load row `i` into the model (the board's `loadRow`).
    fn load(&mut self, i: usize, pre: Option<&Pre>, fresh: bool, now: f64) {
        self.sim = None;
        let Some(r) = self.rows.get(i).cloned() else {
            return;
        };
        if r.length <= 0.0 {
            return;
        }
        let plays = pre.is_some_and(|p| p.playing);
        // a queued file waits (no cleaned block) until the queue reaches it
        // or it plays
        let waiting = !r.yielded
            && !plays
            && r.rendered == Some(0.0)
            && r.segments.is_none()
            && self.active_index() != Some(i);
        let mut sim = Sim::load(r.length, r.clip, waiting, now);
        sim.speed = if self.demo_speed { DEMO_SPEED } else { SPEED };
        sim.yielded = r.yielded || r.stopped;
        sim.alpha = self.strength.alpha();
        if !fresh && !waiting {
            let mut p = pre.cloned().unwrap_or_default();
            p.pos = r.pos;
            p.rendered = r.rendered;
            p.segments = r.segments.clone();
            sim.preset(&p, now);
        }
        if let Some(a) = r.att.clone().filter(|a| !a.save_only()) {
            // a pass that needs attention stands still
            sim.failed = Some(a);
            sim.rendering = false;
            sim.playing = false;
        }
        self.sim = Some(sim);
    }

    /// The selected row takes the model's progress (before another row
    /// is loaded).
    fn snapshot(&mut self) {
        let (Some(sim), Some(r)) = (self.sim.as_ref(), self.rows.get_mut(self.sel)) else {
            return;
        };
        r.pos = sim.pos;
        if sim.done() {
            r.rendered = None;
            r.segments = None;
        } else if !sim.waiting {
            r.segments = Some(sim.segs.iter().map(|g| (g.a, g.b)).collect());
        }
    }

    fn step(&mut self, dt: f64, now: f64) {
        let st = self.strength;
        if let Some(sim) = self.sim.as_mut() {
            sim.tick(dt, now);
            if sim.done() && self.save_wait {
                // Save asked before the end: it saves now
                if let Some(r) = self.rows.get_mut(self.sel) {
                    r.saved = true;
                    r.saved_at = st;
                    r.want = false;
                    r.rendered = None;
                }
                self.save_wait = false;
            }
        }
        // the queue's pass, when it isn't the selected file's
        let sel_runs = self.sim.as_ref().is_some_and(|s| s.rendering);
        if !sel_runs {
            if let Some(i) = self.active_index().filter(|&i| i != self.sel) {
                let speed = if self.demo_speed { DEMO_SPEED } else { SPEED };
                let r = &mut self.rows[i];
                if !r.yielded && !r.stopped {
                    let c = r.covered() + speed * dt;
                    if c >= r.length {
                        r.rendered = None;
                        r.segments = None;
                    } else if r.segments.is_none() {
                        r.rendered = Some(c);
                    }
                }
            }
        }
        if let Some(v) = self.saving {
            let len = self.rows.get(self.sel).map_or(0.0, |r| r.length);
            let dur = (len / 40.0).max(4.0);
            let v = v + dt / dur;
            if v >= 1.0 {
                self.saving = None;
                if let Some(r) = self.rows.get_mut(self.sel) {
                    r.saved = true;
                    r.saved_at = st;
                }
            } else {
                self.saving = Some(v);
            }
        }
        self.serial += 1;
    }

    fn select(&mut self, i: usize, now: f64) {
        if i == self.sel || i >= self.rows.len() {
            return;
        }
        self.snapshot();
        if let Some(sim) = self.sim.as_mut() {
            sim.pause();
        }
        self.sel = i;
        self.save_wait = self.rows[i].want;
        self.saving = None;
        self.load(i, None, false, now);
    }

    fn index(&self, id: FileId) -> Option<usize> {
        self.rows.iter().position(|r| r.id == id)
    }

    fn row_view(&self, i: usize, now: f64) -> Row {
        let r = &self.rows[i];
        let sel = i == self.sel;
        let sim = self.sim.as_ref().filter(|_| sel);
        let done = sim.map_or(r.done(), |s| s.done());
        let segments = match sim {
            Some(s) if !s.waiting => s.segs.iter().map(|g| (g.a, g.b)).collect(),
            Some(_) => vec![],
            None => r.parts(),
        };
        let pass = if let Some(a) = &r.att {
            Pass::Attention(a.clone())
        } else if done {
            Pass::Cleaned
        } else if r.yielded {
            Pass::Yielded
        } else if r.stopped
            || sim.is_some_and(|s| s.waiting)
            || (sim.is_none() && self.active_index() != Some(i))
        {
            Pass::Waiting
        } else {
            Pass::Cleaning
        };
        let output = if r.length > 0.0 && r.att.as_ref().is_none_or(Reason::save_only) {
            output_name(&r.name)
        } else {
            String::new()
        };
        let save = if r.saved && r.saved_at != self.strength {
            Save::Stale {
                was: r.saved_at,
                now: self.strength,
            }
        } else if r.saved {
            Save::Saved
        } else if sel && self.saving.is_some() {
            let f = self.saving.unwrap_or(0.0);
            Save::Writing {
                fraction: f as f32,
                secs_left: (1.0 - f) * (r.length / 40.0).max(4.0),
            }
        } else if r.want || (sel && self.save_wait) {
            Save::WhenCleaned
        } else {
            Save::No
        };
        let time_left = match sim {
            Some(s) if !self.demo_speed => s.time_left(now),
            // a file further down the list, at the pass's speed (as the
            // board's rows)
            None if !self.demo_speed => {
                let covered = match (&r.segments, r.rendered) {
                    (Some(g), _) => g.iter().map(|(a, b)| b - a).sum(),
                    (None, Some(x)) => x,
                    (None, None) => r.length,
                };
                Some(((r.length - covered) / SPEED).max(0.0))
            }
            _ => None,
        };
        Row {
            id: r.id,
            name: r.name.clone(),
            // a damaged file reads as long as its readable part, and states
            // its whole length
            length: r.damaged.unwrap_or(r.length),
            estimate: Estimate::Exact,
            damaged: r.damaged,
            stated: r.damaged.map(|_| r.length),
            kind: r.kind,
            pass,
            segments,
            saved_name: if r.saved {
                output_name(&r.name)
            } else {
                String::new()
            },
            output,
            save,
            time_left,
            stalled: false,
            reduced: false,
            tracks: 1,
            dropped: 0,
            track: 0,
            titles: Vec::new(),
            strength: self.strength,
        }
    }
}

impl Source for DemoFiles {
    fn send(&mut self, c: Cmd) {
        let now = self.last.unwrap_or(0.0);
        let st = self.strength;
        let sel_id = self.rows.get(self.sel).map(|r| r.id);
        match c {
            Cmd::Choose => {
                // the boards' sample: the long interview
                if self.rows.is_empty() {
                    let id = self.id();
                    self.rows.push(interview(id));
                    self.sel = 0;
                    self.load(0, None, true, now);
                }
            }
            Cmd::Add(paths) => {
                for p in paths {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let id = self.id();
                    self.rows.push(DRow::new(id, &name, 95.0));
                }
                if self.sim.is_none() {
                    self.sel = 0;
                    self.load(0, None, true, now);
                }
            }
            Cmd::Select(id) => {
                if let Some(i) = self.index(id) {
                    self.select(i, now);
                }
            }
            Cmd::Remove(id) => {
                if let Some(i) = self.index(id) {
                    self.rows.remove(i);
                    self.saving = None;
                    self.save_wait = false;
                    self.sel = 0;
                    self.load(0, None, false, now);
                }
            }
            Cmd::Play => {
                // a file stopped partway cleans on once it plays
                if let Some(r) = self.rows.get_mut(self.sel).filter(|r| r.stopped) {
                    r.stopped = false;
                    if let Some(s) = self.sim.as_mut() {
                        s.yielded = r.yielded;
                    }
                }
                if let Some(s) = self.sim.as_mut() {
                    s.play(now);
                }
            }
            Cmd::Pause => {
                if let Some(s) = self.sim.as_mut() {
                    s.pause();
                }
            }
            Cmd::Seek { to, .. } => {
                if let Some(s) = self.sim.as_mut() {
                    s.seek(to, now);
                }
            }
            Cmd::Original(on) => {
                if let Some(s) = self.sim.as_mut() {
                    s.hold(on);
                }
            }
            Cmd::Strength(st) => {
                self.strength = st;
                if let Some(s) = self.sim.as_mut() {
                    s.alpha = st.alpha();
                }
            }
            Cmd::Save(id) => {
                let done = self.sim.as_ref().is_some_and(|s| s.done());
                if Some(id) == sel_id && done {
                    let r = &mut self.rows[self.sel];
                    r.saved = true;
                    r.saved_at = st;
                    r.want = false;
                    self.save_wait = false;
                }
            }
            Cmd::SaveAll => {
                let done = self.sim.as_ref().is_some_and(|s| s.done());
                let sel = self.sel;
                let mut wait = false;
                for (i, r) in self.rows.iter_mut().enumerate() {
                    if r.att.is_some() || r.saved {
                        continue;
                    }
                    let d = if i == sel { done } else { r.done() };
                    if d {
                        r.saved = true;
                        r.saved_at = st;
                    } else {
                        r.want = true;
                        wait |= i == sel;
                    }
                }
                self.save_wait = wait;
            }
            Cmd::CancelSave(_) => {
                self.saving = None;
                self.save_wait = false;
                for r in self.rows.iter_mut() {
                    r.want = false;
                }
            }
            Cmd::SaveElsewhere(_)
            | Cmd::ShowInFolder(_)
            | Cmd::ChooseAgain(_)
            | Cmd::Retry(_)
            | Cmd::CleanFirst(_) => {}
            Cmd::ClearAll => {
                if !self.rows.is_empty() {
                    let rows = std::mem::take(&mut self.rows);
                    self.cleared = Some((rows, self.sel, now));
                    self.saving = None;
                    self.save_wait = false;
                    self.sim = None;
                    self.sel = 0;
                }
            }
            // the boards' files have one sound track and no call
            Cmd::Track(..) | Cmd::CleanAnyway | Cmd::CleanNext(_) => {}
            Cmd::CancelQuit => self.quitting = None,
            Cmd::UndoClear => {
                if let Some((mut rows, sel, _)) = self.cleared.take() {
                    rows.append(&mut self.rows);
                    self.rows = rows;
                    self.sel = sel;
                    self.load(sel, None, false, now);
                }
            }
        }
        self.serial += 1;
    }

    fn view(&mut self, now: f64) -> View {
        if !self.frozen {
            let last = *self.last.get_or_insert(now);
            let dt = (now - last).clamp(0.0, 1.0);
            if dt > 0.0 {
                self.step(dt, now);
            }
            self.last = Some(now);
        }
        // a still keeps its moment (the time left counts from it)
        let now = if self.frozen {
            self.last.unwrap_or(now)
        } else {
            now
        };
        let files: Vec<Row> = (0..self.rows.len())
            .map(|i| self.row_view(i, now))
            .collect();
        let selected = self.rows.get(self.sel).map(|r| r.id);
        // the first file that waits starts next, as the daemon says (its
        // row offers no "Clean next")
        let next_up = files.iter().find(|r| r.pass == Pass::Waiting).map(|r| r.id);
        let undo = crate::files::jobs::UNDO_FOR.as_secs_f64();
        if self.cleared.as_ref().is_some_and(|c| now - c.2 >= undo) {
            self.cleared = None;
        }
        if let Some(s) = self.sim.as_mut() {
            s.relight();
        }
        let sim = self.sim.as_ref();
        let play = match sim {
            Some(s) if s.playing && s.caught && !s.fresh && !s.hold => Play::CaughtUp,
            Some(s) if s.playing => Play::Playing,
            _ => Play::Paused,
        };
        View {
            files,
            selected,
            position: sim.map_or(0.0, |s| s.pos),
            play,
            original: sim.is_some_and(|s| s.hold),
            peek: sim.is_some_and(|s| s.peek),
            strength: self.strength,
            place: Place::NextToOriginal,
            cleared: self.cleared.as_ref().map_or(0, |c| c.0.len()),
            cleared_name: None,
            yield_app: Some("Zoom".into()),
            next_up,
            quitting: self.quitting,
            note: None,
            serial: self.serial,
        }
    }

    fn picture(&self) -> Option<Arc<Sheet>> {
        // a file that waits its turn has no pass, and so no picture yet
        self.sim
            .as_ref()
            .filter(|s| !s.waiting)
            .map(|s| s.sheet.clone())
    }

    fn busy(&self) -> bool {
        !self.frozen
            && (self.sim.as_ref().is_some_and(|s| s.playing || s.rendering)
                || self.saving.is_some()
                || self.active_index().is_some())
    }
}
