//! The rolling last 10 seconds of aligned raw and clean audio, its
//! timeline columns, and the frozen take a replay plays.
//!
//! Frames are addressed by an absolute index in the source's own count
//! (the tap's clean frame index, or the fallback's sample count); a gap in
//! that count starts the buffer over, because a take must never pair
//! audio across a hole. Columns sit on multiples of [`COLUMN_FRAMES`] of
//! that index, so a frozen take shows exactly the columns the live
//! timeline showed at that moment.

use super::spectrum;
use super::types::{Column, Engine, COLUMNS, COLUMN_FRAMES, SAMPLE_RATE, TAKE_FRAMES};
use crate::meter::level;
use std::collections::VecDeque;
use std::sync::Arc;

/// Held frames: the full timeline plus the column being filled.
const CAPACITY: usize = TAKE_FRAMES + COLUMN_FRAMES;
/// Exact zeros count as "no sound" only after this many in a row (3 s).
pub const SILENT_FRAMES: u64 = 3 * SAMPLE_RATE as u64;
/// A sample at or beyond this magnitude is at the converter's rail.
const RAIL: f32 = 0.999;

/// Per-column facts the level check needs, besides the drawn levels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColumnStats {
    /// RMS in dBFS (-120 for digital silence).
    pub raw_db: f32,
    pub clean_db: f32,
    pub raw_peak: f32,
    /// Two consecutive samples at the rail, or one beyond full scale.
    pub clipped: bool,
}

pub(crate) fn db(ms: f64) -> f32 {
    if ms > 1e-12 {
        (10.0 * ms.log10()) as f32
    } else {
        -120.0
    }
}

fn column_of(raw_ms: f64, clean_ms: f64) -> Column {
    Column {
        raw: level(raw_ms.sqrt() as f32),
        clean: level(clean_ms.sqrt() as f32),
    }
}

#[derive(Default)]
struct Acc {
    raw_ms: f64,
    clean_ms: f64,
    raw_peak: f32,
    clipped: bool,
    prev_rail: bool,
    n: usize,
}

/// See the module docs.
pub struct Rolling {
    raw: Vec<f32>,
    clean: Vec<f32>,
    /// Absolute index of the next frame to append.
    write: u64,
    /// Absolute index of the first complete column held.
    first_col: u64,
    columns: VecDeque<(Column, ColumnStats)>,
    /// The engine mode each held column was recorded in.
    modes: VecDeque<Engine>,
    /// The mode new columns are recorded in.
    engine: Engine,
    /// The strength (dB) each held column was recorded at, and the one
    /// new columns are.
    strengths: VecDeque<f32>,
    strength: f32,
    acc: Acc,
    /// The first column boundary at or after a (re)start: frames before it
    /// belong to no column.
    aligned_from: u64,
    /// Exact zeros in a row on the raw side, across columns.
    zero_run: u64,
    started: bool,
    /// The picture's lanes of the same pairs, when a page draws them.
    spectrum: Option<spectrum::Live>,
}

impl Default for Rolling {
    fn default() -> Self {
        Self::new()
    }
}

impl Rolling {
    pub fn new() -> Rolling {
        Rolling {
            raw: vec![0.0; CAPACITY],
            clean: vec![0.0; CAPACITY],
            write: 0,
            first_col: 0,
            columns: VecDeque::with_capacity(COLUMNS + 1),
            modes: VecDeque::with_capacity(COLUMNS + 1),
            engine: Engine::Filtering,
            strengths: VecDeque::with_capacity(COLUMNS + 1),
            strength: 100.0,
            acc: Acc::default(),
            aligned_from: 0,
            zero_run: 0,
            started: false,
            spectrum: None,
        }
    }

    /// A buffer whose pairs also go into `picture`'s live lanes.
    pub fn with_picture(picture: Arc<spectrum::Picture>) -> Rolling {
        Rolling {
            spectrum: Some(spectrum::Live::new(picture)),
            ..Rolling::new()
        }
    }

    /// Whether the picture changed for a page that stopped looking, once.
    pub fn take_woke(&mut self) -> bool {
        self.spectrum
            .as_mut()
            .is_some_and(spectrum::Live::take_woke)
    }

    /// Forget everything; the next push starts a new buffer.
    pub fn clear(&mut self) {
        if let Some(s) = self.spectrum.as_mut() {
            s.clear();
        }
        self.columns.clear();
        self.modes.clear();
        self.strengths.clear();
        self.acc = Acc::default();
        self.zero_run = 0;
        self.started = false;
    }

    /// The engine mode the next columns are recorded in.
    pub fn set_engine(&mut self, engine: Engine) {
        self.engine = engine;
    }

    /// The strength (dB) the next columns are recorded at.
    pub fn set_strength(&mut self, db: f32) {
        self.strength = db;
    }

    /// The modes of the newest `n` columns (fewer if fewer are held).
    pub fn modes_tail(&self, n: usize) -> Vec<Engine> {
        let skip = self.modes.len().saturating_sub(n);
        self.modes.iter().skip(skip).copied().collect()
    }

    /// The strengths of the newest `n` columns (fewer if fewer are held).
    pub fn strengths_tail(&self, n: usize) -> Vec<f32> {
        let skip = self.strengths.len().saturating_sub(n);
        self.strengths.iter().skip(skip).copied().collect()
    }

    fn restart(&mut self, at: u64) {
        self.clear();
        self.started = true;
        self.write = at;
        self.aligned_from = at.div_ceil(COLUMN_FRAMES as u64) * COLUMN_FRAMES as u64;
        self.first_col = self.aligned_from;
    }

    /// Append aligned pairs starting at absolute frame `at`. A gap (or an
    /// overlap) in the index starts the buffer over at `at`. Returns the
    /// columns completed by this push, with their stats.
    pub fn push(
        &mut self,
        at: u64,
        raw: &[f32],
        clean: &[f32],
        done: &mut Vec<(Column, ColumnStats)>,
    ) {
        let n = raw.len().min(clean.len());
        if !self.started || at != self.write {
            self.restart(at);
        }
        if let Some(s) = self.spectrum.as_mut() {
            s.push(at, &raw[..n], &clean[..n], self.engine);
        }
        for k in 0..n {
            let i = self.write;
            let slot = (i % CAPACITY as u64) as usize;
            let (r, c) = (raw[k], clean[k]);
            let (r, c) = (
                if r.is_finite() { r } else { 0.0 },
                if c.is_finite() { c } else { 0.0 },
            );
            self.raw[slot] = r;
            self.clean[slot] = c;
            self.write += 1;
            self.zero_run = if r == 0.0 { self.zero_run + 1 } else { 0 };
            if i < self.aligned_from {
                continue;
            }
            let a = &mut self.acc;
            a.raw_ms += (r as f64) * (r as f64);
            a.clean_ms += (c as f64) * (c as f64);
            let mag = r.abs();
            a.raw_peak = a.raw_peak.max(mag);
            let rail = mag >= RAIL;
            if mag > 1.0 || (rail && a.prev_rail) {
                a.clipped = true;
            }
            a.prev_rail = rail;
            a.n += 1;
            if a.n == COLUMN_FRAMES {
                let ms = (
                    a.raw_ms / COLUMN_FRAMES as f64,
                    a.clean_ms / COLUMN_FRAMES as f64,
                );
                let col = column_of(ms.0, ms.1);
                let stats = ColumnStats {
                    raw_db: db(ms.0),
                    clean_db: db(ms.1),
                    raw_peak: a.raw_peak,
                    clipped: a.clipped,
                };
                self.acc = Acc {
                    prev_rail: rail,
                    ..Acc::default()
                };
                self.columns.push_back((col, stats));
                self.modes.push_back(self.engine);
                self.strengths.push_back(self.strength);
                done.push((col, stats));
                if self.columns.len() > COLUMNS {
                    self.columns.pop_front();
                    self.modes.pop_front();
                    self.strengths.pop_front();
                    self.first_col += COLUMN_FRAMES as u64;
                }
            }
        }
    }

    /// The complete columns held, oldest first.
    pub fn columns(&self) -> impl Iterator<Item = Column> + '_ {
        self.columns.iter().map(|(c, _)| *c)
    }

    pub fn stats(&self) -> impl Iterator<Item = ColumnStats> + '_ {
        self.columns.iter().map(|(_, s)| *s)
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// Seconds of complete columns held.
    pub fn held_secs(&self) -> f32 {
        self.columns.len() as f32 * COLUMN_FRAMES as f32 / SAMPLE_RATE as f32
    }

    /// Exact zeros in a row on the raw side, right now.
    pub fn zero_run(&self) -> u64 {
        self.zero_run
    }

    /// Absolute index of the next frame expected.
    pub fn next_index(&self) -> Option<u64> {
        self.started.then_some(self.write)
    }

    /// Copy the complete columns' audio out as a take (what the timeline
    /// shows right now). `None` with nothing held.
    pub fn freeze(&self) -> Option<FrozenTake> {
        let cols = self.columns.len();
        if cols == 0 {
            return None;
        }
        let frames = cols * COLUMN_FRAMES;
        let mut raw = Vec::with_capacity(frames);
        let mut clean = Vec::with_capacity(frames);
        for i in self.first_col..self.first_col + frames as u64 {
            let slot = (i % CAPACITY as u64) as usize;
            raw.push(self.raw[slot]);
            clean.push(self.clean[slot]);
        }
        Some(FrozenTake {
            columns: self.columns().collect(),
            modes: self.modes.iter().copied().collect(),
            strengths: self.strengths.iter().copied().collect(),
            raw,
            clean,
        })
    }
}

/// The share of the input a strength leaves in the output, as the engine
/// blends it (hushmic-denoiser's attn: `10^(-dB/20)`, 1 at 0 dB and
/// below, 0 once it is below 1e-6).
pub fn alpha(db: f32) -> f32 {
    let db = crate::pipewire::attn_sanitized(db);
    if db <= 0.0 {
        return 1.0;
    }
    let a = 10f32.powf(-db / 20.0).min(1.0);
    if a > 1e-6 {
        a
    } else {
        0.0
    }
}

/// A take recorded at this little suppression (under 0.1 dB) holds too
/// little of the engine's own output to re-render from.
const MIN_SHARE: f32 = 0.01;

/// What turns audio cleaned at `from` dB into audio cleaned at `to`:
/// `out' = in + k * (out - in)`, `k = (1 - a') / (1 - a)` (the engine's
/// blend after the model, see [`FrozenTake::at_strength`]). None when
/// nothing changes, or `from` left too little of the model's output.
pub fn restrength(from: f32, to: f32) -> Option<f32> {
    let (a, b) = (alpha(from), alpha(to));
    (a != b && 1.0 - a >= MIN_SHARE).then(|| (1.0 - b) / (1.0 - a))
}

fn columns_of(raw: &[f32], clean: &[f32]) -> Vec<Column> {
    raw.as_chunks::<COLUMN_FRAMES>()
        .0
        .iter()
        .zip(clean.as_chunks::<COLUMN_FRAMES>().0)
        .map(|(r, c)| {
            let ms = |x: &[f32]| {
                x.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / x.len() as f64
            };
            column_of(ms(r), ms(c))
        })
        .collect()
}

/// A frozen take: aligned raw and clean audio (sample `i` of each is the
/// same moment), its columns, and the engine mode of each column (empty
/// when unknown: a take made from audio alone).
#[derive(Clone, Debug, PartialEq)]
pub struct FrozenTake {
    pub raw: Vec<f32>,
    pub clean: Vec<f32>,
    pub columns: Vec<Column>,
    pub modes: Vec<Engine>,
    /// The strength (dB) each column was recorded at (empty: unknown).
    pub strengths: Vec<f32>,
}

impl FrozenTake {
    /// Build a take from aligned audio, computing its columns.
    pub fn from_audio(raw: Vec<f32>, clean: Vec<f32>) -> FrozenTake {
        let n = raw.len().min(clean.len()) / COLUMN_FRAMES * COLUMN_FRAMES;
        let (mut raw, mut clean) = (raw, clean);
        raw.truncate(n);
        clean.truncate(n);
        let columns = columns_of(&raw, &clean);
        FrozenTake {
            raw,
            clean,
            columns,
            modes: Vec::new(),
            strengths: Vec::new(),
        }
    }

    /// This take as the engine would have cleaned it at `db`: the strength
    /// is a blend after the model, `out = a * in + (1 - a) * model`, so
    /// `out' = in + (1 - a') / (1 - a) * (out - in)` (exact: pinned against
    /// the engine in hushmic-denoiser's tests/strength.rs). Columns not
    /// filtered (bypass, muted) stay as they were. None when nothing would
    /// change, or the strengths or modes are unknown.
    pub fn at_strength(&self, db: f32) -> Option<FrozenTake> {
        let n = self.columns.len();
        if self.modes.len() != n || self.strengths.len() != n {
            return None;
        }
        let mut clean = self.clean.clone();
        let mut strengths = self.strengths.clone();
        let mut moved = false;
        for (i, (mode, was)) in self.modes.iter().zip(&self.strengths).enumerate() {
            let Some(k) = restrength(*was, db).filter(|_| *mode == Engine::Filtering) else {
                continue;
            };
            let span = i * COLUMN_FRAMES..(i + 1) * COLUMN_FRAMES;
            for (c, r) in clean[span.clone()].iter_mut().zip(&self.raw[span]) {
                *c = r + k * (*c - r);
            }
            strengths[i] = db;
            moved = true;
        }
        if !moved {
            return None;
        }
        Some(FrozenTake {
            columns: columns_of(&self.raw, &clean),
            raw: self.raw.clone(),
            clean,
            modes: self.modes.clone(),
            strengths,
        })
    }

    /// The columns recorded in `mode`, joined into one take (None when
    /// the modes are unknown).
    pub fn only(&self, mode: Engine) -> Option<FrozenTake> {
        if self.modes.len() != self.columns.len() {
            return None;
        }
        let (mut raw, mut clean) = (Vec::new(), Vec::new());
        for (i, m) in self.modes.iter().enumerate() {
            if *m == mode {
                let r = i * COLUMN_FRAMES..(i + 1) * COLUMN_FRAMES;
                raw.extend_from_slice(&self.raw[r.clone()]);
                clean.extend_from_slice(&self.clean[r]);
            }
        }
        let strengths: Vec<f32> = if self.strengths.len() == self.columns.len() {
            self.modes
                .iter()
                .zip(&self.strengths)
                .filter(|(m, _)| **m == mode)
                .map(|(_, s)| *s)
                .collect()
        } else {
            Vec::new()
        };
        let mut t = FrozenTake::from_audio(raw, clean);
        t.modes = vec![mode; t.columns.len()];
        t.strengths = strengths;
        Some(t)
    }

    pub fn frames(&self) -> usize {
        self.raw.len()
    }

    pub fn secs(&self) -> f32 {
        self.frames() as f32 / SAMPLE_RATE as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(from: u64, n: usize, amp: f32) -> Vec<f32> {
        (from..from + n as u64)
            .map(|i| amp * ((i as f32) * 0.05).sin())
            .collect()
    }

    #[test]
    fn columns_sit_on_absolute_boundaries_and_roll_over() {
        let mut r = Rolling::new();
        let mut done = Vec::new();
        // Start mid-column: the partial first column is skipped.
        let start = 1000u64;
        let mut at = start;
        for _ in 0..500 {
            let a = tone(at, 1024, 0.1);
            let b: Vec<f32> = a.iter().map(|x| x * 0.5).collect();
            r.push(at, &a, &b, &mut done);
            at += 1024;
        }
        let total = (at - 2400) / 2400; // complete columns after the first boundary
        assert_eq!(done.len() as u64, total);
        assert_eq!(r.column_count(), COLUMNS);
        assert!((r.held_secs() - 10.0).abs() < 1e-6);
        let c = r.columns().last().unwrap();
        // 0.1 sine: RMS 0.0707 = -23 dBFS -> 0.616; clean 6 dB lower.
        assert!((c.raw - 0.616).abs() < 0.01, "{c:?}");
        assert!((c.raw - c.clean - 6.02 / 60.0).abs() < 0.002, "{c:?}");
        let take = r.freeze().unwrap();
        assert_eq!(take.frames(), TAKE_FRAMES);
        assert_eq!(take.columns, r.columns().collect::<Vec<_>>());
        // The take is the newest 200 complete columns, sample for sample.
        let last_boundary = (at / 2400) * 2400;
        let first = last_boundary - TAKE_FRAMES as u64;
        assert_eq!(take.raw[0], tone(first, 1, 0.1)[0]);
        assert_eq!(
            *take.raw.last().unwrap(),
            tone(last_boundary - 1, 1, 0.1)[0]
        );
        assert_eq!(take.clean[7], take.raw[7] * 0.5);
        assert_eq!(take.modes, vec![Engine::Filtering; COLUMNS]);
        assert_eq!(take.strengths, vec![100.0; COLUMNS]);
        let rebuilt = FrozenTake::from_audio(take.raw.clone(), take.clean.clone());
        assert_eq!(rebuilt.columns, take.columns);
        assert_eq!((rebuilt.raw, rebuilt.clean), (take.raw, take.clean));
    }

    /// A take recorded at 12 dB, re-rendered at 24 dB, is what the engine
    /// would have made at 24 dB; a bypassed column stays; unknown
    /// strengths give nothing.
    #[test]
    fn a_take_renders_at_another_strength() {
        let raw = tone(0, 4 * COLUMN_FRAMES, 0.3);
        // what the model made of it
        let model: Vec<f32> = tone(7, 4 * COLUMN_FRAMES, 0.1);
        let at = |db: f32| -> Vec<f32> {
            let a = alpha(db);
            raw.iter()
                .zip(&model)
                .map(|(r, m)| a * r + (1.0 - a) * m)
                .collect()
        };
        let mut take = FrozenTake::from_audio(raw.clone(), at(12.0));
        take.modes = vec![
            Engine::Filtering,
            Engine::Filtering,
            Engine::Bypass,
            Engine::Filtering,
        ];
        take.strengths = vec![12.0; 4];
        let t = take.at_strength(24.0).unwrap();
        let want = at(24.0);
        for i in (0..2 * COLUMN_FRAMES).chain(3 * COLUMN_FRAMES..4 * COLUMN_FRAMES) {
            assert!((t.clean[i] - want[i]).abs() < 1e-5, "{i}");
        }
        let bypassed = 2 * COLUMN_FRAMES..3 * COLUMN_FRAMES;
        assert_eq!(t.clean[bypassed.clone()], take.clean[bypassed]);
        assert_eq!(t.strengths, vec![24.0, 24.0, 12.0, 24.0]);
        let rebuilt = FrozenTake::from_audio(t.raw.clone(), t.clean.clone());
        assert_eq!(t.columns, rebuilt.columns);
        assert!(
            t.columns[0].clean < take.columns[0].clean,
            "stronger: lower"
        );
        assert_eq!(t.raw, take.raw);
        // and back: the 12 dB take again
        let back = t.at_strength(12.0).unwrap();
        for (b, c) in back.clean.iter().zip(&take.clean) {
            assert!((b - c).abs() < 1e-5);
        }
        // nothing to do, or nothing known
        assert!(take.at_strength(12.0).is_none());
        let mut unknown = take.clone();
        unknown.strengths.clear();
        assert!(unknown.at_strength(24.0).is_none());
        // a take at 0 dB holds no model output to work from
        let mut open = take.clone();
        open.strengths = vec![0.0; 4];
        assert!(open.at_strength(24.0).is_none());
    }

    #[test]
    fn strength_blends_as_the_engine_does() {
        assert_eq!(alpha(0.0), 1.0);
        assert_eq!(alpha(-3.0), 1.0);
        assert!((alpha(6.0) - 0.501).abs() < 0.001);
        assert!((alpha(100.0) - 1e-5).abs() < 1e-7);
        assert_eq!(alpha(f32::INFINITY), alpha(100.0));
    }

    #[test]
    fn a_gap_starts_over_and_zeros_and_clipping_are_seen() {
        let mut r = Rolling::new();
        let mut done = Vec::new();
        r.push(0, &[0.2; 4800], &[0.2; 4800], &mut done);
        assert_eq!(r.column_count(), 2);
        r.push(9600, &[0.0; 2400], &[0.0; 2400], &mut done);
        assert_eq!(r.column_count(), 1, "the gap dropped the old columns");
        assert_eq!(r.zero_run(), 2400);
        assert_eq!(r.stats().last().unwrap().raw_db, -120.0);
        r.push(12_000, &[0.0; 1], &[0.0; 1], &mut done);
        assert_eq!(r.zero_run(), 2401);
        let mut clip = vec![0.1f32; 2399];
        clip[100] = 1.0;
        clip[101] = -1.0;
        r.push(12_001, &clip, &clip, &mut done);
        assert!(r.stats().last().unwrap().clipped);
        assert_eq!(r.zero_run(), 0);
        let mut one = vec![0.1f32; 2400];
        one[5] = 1.0; // one sample at the rail is not clipping
        r.push(14_400, &one, &one, &mut done);
        assert!(!r.stats().last().unwrap().clipped);
        assert_eq!(r.next_index(), Some(16_800));
        r.clear();
        assert_eq!(r.next_index(), None);
        assert!(r.freeze().is_none());
    }

    /// The picture's lanes are made from the buffer's audio and go with
    /// it: a buffer that is cleared leaves nothing to draw.
    #[test]
    fn the_picture_goes_with_the_buffer() {
        use spectrum::{Cursor, Picture};
        let picture = Arc::new(Picture::new());
        picture.want();
        let mut r = Rolling::with_picture(Arc::clone(&picture));
        let tone: Vec<f32> = (0..9600).map(|i| (i as f32 * 0.13).sin() * 0.3).collect();
        let mut done = Vec::new();
        r.push(0, &tone, &tone, &mut done);
        let mut heard = 0u64;
        let shown = picture.read(&mut Cursor::default(), |_, levels, _| {
            heard += levels.iter().map(|b| u64::from(*b)).sum::<u64>();
        });
        assert!(shown.head > 0 && heard > 0, "{shown:?}");
        r.clear();
        let mut left = 0;
        let shown = picture.read(&mut Cursor::default(), |_, _, _| left += 1);
        assert_eq!((shown.head, left), (0, 0));
    }
}
