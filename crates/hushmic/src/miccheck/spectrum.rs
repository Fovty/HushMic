//! The spectrum of the last 10 seconds: the microphone and what apps hear
//! as two lanes of levels on one time axis, for the page's picture.
//!
//! A column is one [`HOP`] of audio (three to a timeline column), seen
//! through a Hann window of [`WINDOW`] frames centred on it; a row is one
//! of [`ROWS`] bands from [`LOW_HZ`] to [`HIGH_HZ`], spaced evenly on a
//! scale that is logarithmic above [`BEND_HZ`] and close to linear below
//! it (as the mel scale, with a lower knee: speech fills the lane, and no
//! band is much narrower than the transform resolves). Both sides
//! go through one transform (the raw side as the real part, the clean
//! side as the imaginary one), so a column of the two lanes is the same
//! frames by construction.
//!
//! A level is a byte on a fixed scale: [`RANGE_DB`] above [`FLOOR_DB`],
//! where 0 dB is a full-scale sine in its band. Nothing is normalised to
//! what is in view, so a cleaned pause is empty next to a noisy original.
//!
//! A frozen take keeps, for every cell, the power of both sides and the
//! real part of their cross spectrum ([`Cell`]): the power of any blend of
//! the two follows from those three numbers, so a strength change redraws
//! the take's clean lane without going back to the audio.
//!
//! The backend writes into a [`Picture`]; the page reads the columns it
//! has not seen yet. The picture lives and dies with the backend's audio
//! (see the module docs of [`super`]): it is cleared when the buffer is,
//! and never leaves memory.

use super::take::{alpha, FrozenTake};
use super::types::{Engine, COLUMN_FRAMES, SAMPLE_RATE, TAKE_FRAMES};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Frames per column: 16.7 ms, fine enough for syllables and key clicks.
pub const HOP: usize = 800;
/// Columns per timeline column (50 ms).
pub const HOPS_PER_COLUMN: usize = COLUMN_FRAMES / HOP;
/// The picture: the last 10 seconds.
pub const COLS: usize = TAKE_FRAMES / HOP;
/// The analysis window: 43 ms, 23 Hz a bin.
pub const WINDOW: usize = 2048;
/// Bands per lane.
pub const ROWS: usize = 96;
pub const LOW_HZ: f32 = 80.0;
pub const HIGH_HZ: f32 = 16_000.0;
/// The knee of the frequency scale.
pub const BEND_HZ: f32 = 250.0;
/// The level scale's floor, in dB re a full-scale sine in the band.
pub const FLOOR_DB: f32 = -90.0;
pub const RANGE_DB: f32 = 70.0;
/// One column's bytes: the raw lane's rows (lowest band first), then the
/// clean lane's.
pub const COLUMN_BYTES: usize = 2 * ROWS;

/// Frames of a column's window before its hop.
pub const LEAD: usize = (WINDOW - HOP) / 2;
/// The clean side counts as the raw one (bypass, or the engine passing
/// audio through) when their difference is this far under the raw side.
const SAME_BELOW: f32 = 1e-6;

/// How a column's clean lane is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// What HushMic made of the microphone.
    Clean,
    /// The microphone as it is: apps heard the original.
    Original,
}

fn scale(hz: f32) -> f32 {
    (1.0 + hz / BEND_HZ).ln()
}

fn unscale(s: f32) -> f32 {
    BEND_HZ * (s.exp() - 1.0)
}

/// The centre of row `row` (which may lie one past either end), in Hz.
fn centre_hz(row: i32) -> f32 {
    let (lo, hi) = (scale(LOW_HZ), scale(HIGH_HZ));
    unscale(lo + (row as f32 + 0.5) * (hi - lo) / ROWS as f32)
}

/// Where a frequency sits in a lane: 0 at its bottom ([`LOW_HZ`]) to 1 at
/// its top ([`HIGH_HZ`]).
pub fn height_of(hz: f32) -> f32 {
    (scale(hz) - scale(LOW_HZ)) / (scale(HIGH_HZ) - scale(LOW_HZ))
}

/// The row a frequency falls in (0 is the lowest band), if any.
pub fn row_of(hz: f32) -> Option<usize> {
    let h = height_of(hz);
    (0.0..1.0)
        .contains(&h)
        .then(|| ((h * ROWS as f32) as usize).min(ROWS - 1))
}

/// The level of a band's power on the fixed scale.
pub fn level(power: f32) -> u8 {
    if power.is_nan() || power <= 0.0 {
        return 0;
    }
    let t = (10.0 * power.log10() - FLOOR_DB) / RANGE_DB;
    (t.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The power a level stands for ([`level`] back; nothing at 0).
pub fn power(level: u8) -> f32 {
    if level == 0 {
        return 0.0;
    }
    10f32.powf((FLOOR_DB + level as f32 / 255.0 * RANGE_DB) / 10.0)
}

/// One band of one column: the power of each side and the real part of
/// their cross spectrum, summed over the band.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cell {
    pub raw: f32,
    pub clean: f32,
    pub cross: f32,
}

impl Cell {
    /// The power of `a * raw + b * clean` in this band:
    /// `a²|R|² + b²|C|² + 2ab·Re(R·C*)`.
    pub fn blend(&self, a: f32, b: f32) -> f32 {
        (a * a * self.raw + b * b * self.clean + 2.0 * a * b * self.cross).max(0.0)
    }

    /// The clean side as the engine would have left it at another
    /// strength: `raw + k * (clean - raw)` ([`FrozenTake::at_strength`]).
    pub fn at(&self, k: f32) -> f32 {
        self.blend(1.0 - k, k)
    }
}

/// One band's share of the transform's bins: a triangle that reaches to
/// its neighbours' centres, and at least one bin to each side (a band
/// narrower than a bin reads between the two bins around it).
struct Band {
    first: usize,
    /// Its weights in [`Analyzer::weights`].
    at: usize,
    len: usize,
}

/// Turns [`WINDOW`] frames of both sides into one column of cells.
pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    bands: Vec<Band>,
    weights: Vec<f32>,
    buf: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    pub fn new() -> Analyzer {
        Analyzer::at_rate(SAMPLE_RATE)
    }

    /// For sound at `rate` frames a second (a file read at its own rate):
    /// the same bands, those above half the rate empty.
    pub fn at_rate(rate: u32) -> Analyzer {
        let fft = FftPlanner::new().plan_fft_forward(WINDOW);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        // Hann, scaled so a full-scale sine on a bin has magnitude 1
        let window = (0..WINDOW)
            .map(|n| {
                let w = 0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / WINDOW as f32).cos();
                w * 4.0 / WINDOW as f32
            })
            .collect();
        let bin_hz = rate.max(1) as f32 / WINDOW as f32;
        let mut bands = Vec::with_capacity(ROWS);
        let mut weights = Vec::new();
        for row in 0..ROWS as i32 {
            let centre = centre_hz(row);
            let below = (centre - centre_hz(row - 1)).max(bin_hz);
            let above = (centre_hz(row + 1) - centre).max(bin_hz);
            let first = (((centre - below) / bin_hz).floor().max(0.0) as usize + 1).min(WINDOW / 2);
            let at = weights.len();
            for bin in first..WINDOW / 2 {
                let d = bin as f32 * bin_hz - centre;
                let w = 1.0 - if d < 0.0 { -d / below } else { d / above };
                if w <= 0.0 {
                    break;
                }
                weights.push(w);
            }
            bands.push(Band {
                first,
                at,
                len: weights.len() - at,
            });
        }
        Analyzer {
            fft,
            window,
            bands,
            weights,
            buf: vec![Complex::default(); WINDOW],
            scratch,
        }
    }

    /// One column from the [`WINDOW`] frames of each side (shorter slices
    /// count as followed by silence).
    pub fn column(&mut self, raw: &[f32], clean: &[f32], out: &mut [Cell; ROWS]) {
        let finite = |x: Option<&f32>| x.copied().filter(|v| v.is_finite()).unwrap_or(0.0);
        for (n, (z, w)) in self.buf.iter_mut().zip(&self.window).enumerate() {
            *z = Complex::new(finite(raw.get(n)) * w, finite(clean.get(n)) * w);
        }
        self.fft
            .process_with_scratch(&mut self.buf, &mut self.scratch);
        // z = r + i·c: R[k] = (Z[k] + Z*[N-k]) / 2, C[k] = (Z[k] - Z*[N-k]) / 2i,
        // so with p = Z[k]·Z[N-k]:
        // |R|², |C|² = (|Z[k]|² + |Z[N-k]|² ± 2·Re p) / 4, Re(R·C*) = Im p / 2
        for (band, cell) in self.bands.iter().zip(out.iter_mut()) {
            let mut sum = Cell::default();
            let weights = &self.weights[band.at..band.at + band.len];
            for (i, w) in weights.iter().enumerate() {
                let k = band.first + i;
                let (a, b) = (self.buf[k], self.buf[WINDOW - k]);
                let both = a.norm_sqr() + b.norm_sqr();
                let p = a * b;
                sum.raw += w * (both + 2.0 * p.re) / 4.0;
                sum.clean += w * (both - 2.0 * p.re) / 4.0;
                sum.cross += w * p.im / 2.0;
            }
            *cell = sum;
        }
    }
}

/// A column of cells as the bytes the picture holds, with the clean side
/// at `k` ([`Cell::at`]; 1: as recorded). Columns recorded muted show an
/// empty clean lane, bypassed ones the raw side.
pub fn levels(cells: &[Cell; ROWS], k: f32, mode: Engine, out: &mut [u8; COLUMN_BYTES]) -> Look {
    let (mut raw, mut diff) = (0.0f32, 0.0f32);
    for c in cells {
        raw += c.raw;
        diff += c.blend(1.0, -1.0);
    }
    let same = mode == Engine::Bypass || (raw > 0.0 && diff <= SAME_BELOW * raw);
    for (row, c) in cells.iter().enumerate() {
        out[row] = level(c.raw);
        out[ROWS + row] = match mode {
            Engine::Muted | Engine::Off => 0,
            _ if same => out[row],
            _ => level(c.at(k)),
        };
    }
    if same {
        Look::Original
    } else {
        Look::Clean
    }
}

/// The newest [`COLS`] columns of levels, as a ring.
struct Lanes {
    levels: Vec<u8>,
    looks: Vec<Look>,
    /// Columns written since the start.
    head: u64,
    /// `head` after the newest column that shows anything (0: none).
    sound: u64,
}

impl Lanes {
    fn new() -> Lanes {
        Lanes {
            levels: vec![0; COLS * COLUMN_BYTES],
            looks: vec![Look::Clean; COLS],
            head: 0,
            sound: 0,
        }
    }

    fn clear(&mut self) {
        self.head = 0;
        self.sound = 0;
    }

    fn push(&mut self, column: &[u8; COLUMN_BYTES], look: Look) {
        let slot = (self.head % COLS as u64) as usize;
        self.levels[slot * COLUMN_BYTES..(slot + 1) * COLUMN_BYTES].copy_from_slice(column);
        self.looks[slot] = look;
        self.head += 1;
        if column.iter().any(|&l| l > 0) {
            self.sound = self.head;
        }
    }

    /// Nothing in view shows anything.
    fn resting(&self) -> bool {
        self.sound == 0 || self.head - self.sound >= COLS as u64
    }

    fn column(&self, index: u64) -> (&[u8], Look) {
        let slot = (index % COLS as u64) as usize;
        (
            &self.levels[slot * COLUMN_BYTES..(slot + 1) * COLUMN_BYTES],
            self.looks[slot],
        )
    }
}

struct Inner {
    /// Bumped whenever the columns in view are replaced as a whole (the
    /// buffer started over, a take was frozen or let go, its strength
    /// moved).
    epoch: u64,
    live: Lanes,
    frozen: Option<Lanes>,
    /// The frozen take is in view (a replay plays).
    show_frozen: bool,
}

impl Inner {
    fn frozen_shown(&self) -> bool {
        self.show_frozen && self.frozen.is_some()
    }

    fn shown(&self) -> &Lanes {
        match &self.frozen {
            Some(f) if self.show_frozen => f,
            _ => &self.live,
        }
    }
}

/// What a reader has seen of a [`Picture`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Cursor {
    epoch: Option<u64>,
    head: u64,
}

/// What [`Picture::read`] found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shown {
    /// The columns in view were replaced: the reader was handed all of
    /// them again.
    pub reset: bool,
    /// Columns since the start of what is in view; the newest [`COLS`] of
    /// them are in view, the newest at the right edge.
    pub head: u64,
    /// A frozen take is in view.
    pub frozen: bool,
    /// Nothing in view shows anything.
    pub resting: bool,
}

/// The two lanes the page draws, shared between the backend (the writer)
/// and the page. See the module docs.
pub struct Picture {
    wanted: AtomicBool,
    /// The reader stopped asking for frames: the next column that changes
    /// what is in view wakes it.
    idle: AtomicBool,
    inner: Mutex<Inner>,
}

impl Default for Picture {
    fn default() -> Self {
        Self::new()
    }
}

impl Picture {
    pub fn new() -> Picture {
        Picture {
            wanted: AtomicBool::new(false),
            idle: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                epoch: 0,
                live: Lanes::new(),
                frozen: None,
                show_frozen: false,
            }),
        }
    }

    /// A page draws this picture: the backend analyses from now on.
    pub fn want(&self) {
        self.wanted.store(true, Ordering::Release);
    }

    pub fn wanted(&self) -> bool {
        self.wanted.load(Ordering::Acquire)
    }

    /// The reader asks for no more frames until something moves.
    pub fn rest(&self) {
        self.idle.store(true, Ordering::Release);
    }

    /// Hand `column` every column in view that `cursor` has not seen (its
    /// index, its [`COLUMN_BYTES`] levels, its look), oldest first.
    pub fn read(&self, cursor: &mut Cursor, mut column: impl FnMut(u64, &[u8], Look)) -> Shown {
        let inner = self.inner.lock().unwrap();
        let lanes = inner.shown();
        let reset = cursor.epoch != Some(inner.epoch);
        let oldest = lanes.head.saturating_sub(COLS as u64);
        let from = if reset {
            oldest
        } else {
            cursor.head.clamp(oldest, lanes.head)
        };
        for i in from..lanes.head {
            let (levels, look) = lanes.column(i);
            column(i, levels, look);
        }
        cursor.epoch = Some(inner.epoch);
        cursor.head = lanes.head;
        Shown {
            reset,
            head: lanes.head,
            frozen: inner.frozen_shown(),
            resting: lanes.resting(),
        }
    }

    /// The live lanes start over. True: a resting reader should look.
    pub fn clear(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.live.head == 0 {
            return false;
        }
        inner.live.clear();
        inner.epoch += 1;
        self.idle.swap(false, Ordering::AcqRel)
    }

    /// Append a live column. True: a resting reader should look.
    pub fn push(&self, column: &[u8; COLUMN_BYTES], look: Look) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let was_resting = inner.live.resting();
        inner.live.push(column, look);
        let moves = !inner.frozen_shown() && !(was_resting && inner.live.resting());
        moves && self.idle.swap(false, Ordering::AcqRel)
    }

    /// The frozen take's lanes: its columns, oldest first (the newest
    /// [`COLS`] are kept).
    pub fn freeze(&self, columns: impl Iterator<Item = ([u8; COLUMN_BYTES], Look)>) {
        let mut lanes = Lanes::new();
        for (column, look) in columns {
            lanes.push(&column, look);
        }
        let mut inner = self.inner.lock().unwrap();
        inner.frozen = Some(lanes);
        if inner.show_frozen {
            inner.epoch += 1;
        }
    }

    /// The frozen take is gone.
    pub fn unfreeze(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.frozen.take().is_some() && inner.show_frozen {
            inner.epoch += 1;
        }
    }

    /// Put the frozen take (true) or the live lanes in view.
    pub fn show_frozen(&self, on: bool) {
        let mut inner = self.inner.lock().unwrap();
        if inner.show_frozen != on {
            inner.show_frozen = on;
            if inner.frozen.is_some() {
                inner.epoch += 1;
            }
        }
    }
}

/// The live analysis: fed the aligned pairs as they arrive, it appends a
/// column to the picture for every [`HOP`] whose window is complete.
/// Columns sit on multiples of [`HOP`] of the source's frame index, as the
/// timeline's sit on multiples of [`COLUMN_FRAMES`]; a gap in that index
/// starts the lanes over, as it does the buffer.
pub struct Live {
    picture: Arc<Picture>,
    analyzer: Option<Analyzer>,
    /// The frames from the next column's window start on.
    raw: Vec<f32>,
    clean: Vec<f32>,
    /// The index of the next frame expected.
    next: Option<u64>,
    /// Frames still to pass before the first column's window starts.
    skip: usize,
    cells: Box<[Cell; ROWS]>,
    /// A resting reader should look.
    woke: bool,
}

impl Live {
    pub fn new(picture: Arc<Picture>) -> Live {
        Live {
            picture,
            analyzer: None,
            raw: Vec::with_capacity(WINDOW + 48_000),
            clean: Vec::with_capacity(WINDOW + 48_000),
            next: None,
            skip: 0,
            cells: Box::new([Cell::default(); ROWS]),
            woke: false,
        }
    }

    /// Start over at frame `at`: the first column is the first whole hop
    /// from there (what came before it counts as silence).
    fn restart(&mut self, at: u64) {
        self.clear();
        let to_hop = (HOP - (at % HOP as u64) as usize) % HOP;
        // that hop's window begins LEAD frames before it
        let silence = LEAD.saturating_sub(to_hop);
        self.raw.resize(silence, 0.0);
        self.clean.resize(silence, 0.0);
        self.skip = to_hop.saturating_sub(LEAD);
    }

    /// Forget everything; the next push starts new lanes.
    pub fn clear(&mut self) {
        self.raw.clear();
        self.clean.clear();
        self.next = None;
        self.skip = 0;
        self.woke |= self.picture.clear();
    }

    /// Append aligned pairs starting at frame `at`, recorded in `mode`.
    /// Nothing is kept or analysed while no page wants the picture.
    pub fn push(&mut self, at: u64, raw: &[f32], clean: &[f32], mode: Engine) {
        if !self.picture.wanted() {
            self.next = None;
            return;
        }
        if self.next != Some(at) {
            self.restart(at);
        }
        let n = raw.len().min(clean.len());
        self.next = Some(at + n as u64);
        let skip = self.skip.min(n);
        self.skip -= skip;
        self.raw.extend_from_slice(&raw[skip..n]);
        self.clean.extend_from_slice(&clean[skip..n]);
        let analyzer = self.analyzer.get_or_insert_with(Analyzer::new);
        let mut from = 0;
        let mut column = [0u8; COLUMN_BYTES];
        while self.raw.len() - from >= WINDOW {
            analyzer.column(
                &self.raw[from..from + WINDOW],
                &self.clean[from..from + WINDOW],
                &mut self.cells,
            );
            let look = levels(&self.cells, 1.0, mode, &mut column);
            self.woke |= self.picture.push(&column, look);
            from += HOP;
        }
        self.raw.drain(..from);
        self.clean.drain(..from);
    }

    /// Whether a resting reader should look, once.
    pub fn take_woke(&mut self) -> bool {
        std::mem::take(&mut self.woke)
    }
}

/// A frozen take's cells, and how its clean lane is drawn now.
pub struct Frozen {
    cells: Vec<Cell>,
    cols: usize,
    /// Per timeline column: the mode it was recorded in (empty: unknown,
    /// drawn as filtering) and the factor its clean side is drawn at.
    modes: Vec<Engine>,
    k: Vec<f32>,
}

impl Frozen {
    /// Analyse a take (its edges see silence beyond them).
    pub fn of(analyzer: &mut Analyzer, take: &FrozenTake) -> Frozen {
        let cols = take.frames() / HOP;
        let mut cells = vec![Cell::default(); cols * ROWS];
        let mut column = Box::new([Cell::default(); ROWS]);
        let (mut raw, mut clean) = (vec![0.0f32; WINDOW], vec![0.0f32; WINDOW]);
        for j in 0..cols {
            let start = (j * HOP) as i64 - LEAD as i64;
            for n in 0..WINDOW {
                let i = start + n as i64;
                let at = |x: &[f32]| {
                    if i >= 0 {
                        x.get(i as usize).copied().unwrap_or(0.0)
                    } else {
                        0.0
                    }
                };
                raw[n] = at(&take.raw);
                clean[n] = at(&take.clean);
            }
            analyzer.column(&raw, &clean, &mut column);
            cells[j * ROWS..(j + 1) * ROWS].copy_from_slice(&column[..]);
        }
        Frozen {
            cells,
            cols,
            modes: take.modes.clone(),
            k: vec![1.0; take.columns.len()],
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn cells(&self, col: usize) -> &[Cell; ROWS] {
        self.cells[col * ROWS..(col + 1) * ROWS]
            .try_into()
            .expect("a column")
    }

    /// The take was re-rendered from the strengths `was` (per timeline
    /// column) to `db`: its clean lane follows, as
    /// [`FrozenTake::at_strength`] moved the audio.
    pub fn restrength(&mut self, was: &[f32], db: f32) {
        for (i, k) in self.k.iter_mut().enumerate() {
            let filtering = self.modes.get(i).is_none_or(|m| *m == Engine::Filtering);
            let step = was
                .get(i)
                .and_then(|w| super::take::restrength(*w, db))
                .filter(|_| filtering);
            if let Some(step) = step {
                *k *= step;
            }
        }
    }

    /// One column's levels and look as the take is drawn now.
    pub fn column(&self, col: usize, out: &mut [u8; COLUMN_BYTES]) -> Look {
        let i = col / HOPS_PER_COLUMN;
        let mode = self.modes.get(i).copied().unwrap_or(Engine::Filtering);
        let k = self.k.get(i).copied().unwrap_or(1.0);
        levels(self.cells(col), k, mode, out)
    }

    /// The take's columns as it is drawn now, oldest first.
    pub fn columns(&self) -> impl Iterator<Item = ([u8; COLUMN_BYTES], Look)> + '_ {
        (0..self.cols).map(|j| {
            let mut column = [0u8; COLUMN_BYTES];
            let look = self.column(j, &mut column);
            (column, look)
        })
    }
}

/// The factor that turns a take recorded at full strength into one at
/// `db` ([`Cell::at`]): a mock take's clean side at a strength.
pub fn k_from_full(db: f32) -> f32 {
    1.0 - alpha(db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, amp: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|n| amp * (std::f32::consts::TAU * hz * n as f32 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    /// Deterministic noise in -1..1.
    fn noise(seed: u32, frames: usize) -> Vec<f32> {
        let mut s = seed;
        (0..frames)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect()
    }

    fn column_of(raw: &[f32], clean: &[f32]) -> Box<[Cell; ROWS]> {
        let mut cells = Box::new([Cell::default(); ROWS]);
        Analyzer::new().column(raw, clean, &mut cells);
        cells
    }

    #[test]
    fn a_sine_lands_in_its_row_at_its_level() {
        for hz in [120.0, 440.0, 1000.0, 3000.0, 9000.0] {
            // -20 dBFS: the top of the scale
            let x = sine(hz, 0.1, WINDOW);
            let cells = column_of(&x, &vec![0.0; WINDOW]);
            let loudest = (0..ROWS)
                .max_by(|a, b| cells[*a].raw.total_cmp(&cells[*b].raw))
                .unwrap();
            let want = row_of(hz).unwrap();
            assert!(
                loudest.abs_diff(want) <= 1,
                "{hz} Hz: row {loudest}, not {want}"
            );
            let db = 10.0 * cells[loudest].raw.log10();
            assert!((-27.0..=-18.0).contains(&db), "{hz} Hz at {db} dB");
            // far from it there is nothing, and the silent side stays empty
            let far = if want > ROWS / 2 { 2 } else { ROWS - 3 };
            assert_eq!(level(cells[far].raw), 0, "{hz} Hz leaks into row {far}");
            assert!(cells.iter().all(|c| level(c.clean) == 0), "{hz} Hz");
        }
        assert_eq!(row_of(LOW_HZ), Some(0));
        assert_eq!(row_of(HIGH_HZ - 1.0), Some(ROWS - 1));
        assert_eq!(row_of(40.0), None);
        assert_eq!(row_of(20_000.0), None);
    }

    #[test]
    fn the_scale_is_fixed_and_silence_is_empty() {
        assert_eq!(level(0.0), 0);
        assert_eq!(level(f32::NAN), 0);
        assert_eq!(level(10f32.powf(FLOOR_DB / 10.0)), 0);
        assert_eq!(level(10f32.powf((FLOOR_DB + RANGE_DB) / 10.0)), 255);
        assert_eq!(level(1.0), 255);
        assert!((127..=128).contains(&level(10f32.powf((FLOOR_DB + RANGE_DB / 2.0) / 10.0))));
        let silence = vec![0.0; WINDOW];
        let cells = column_of(&silence, &silence);
        let mut out = [9u8; COLUMN_BYTES];
        levels(&cells, 1.0, Engine::Filtering, &mut out);
        assert!(out.iter().all(|&l| l == 0));
        // noise 100 dB under full scale is under the floor in every band
        let faint: Vec<f32> = noise(7, WINDOW).iter().map(|x| x * 1e-5).collect();
        let cells = column_of(&faint, &faint);
        levels(&cells, 1.0, Engine::Filtering, &mut out);
        assert!(out.iter().all(|&l| l == 0));
        // the same sound 20 dB louder is 20 dB higher, wherever it is
        let quiet = column_of(&sine(1000.0, 0.001, WINDOW), &silence);
        let loud = column_of(&sine(1000.0, 0.01, WINDOW), &silence);
        let row = row_of(1000.0).unwrap();
        let step = level(loud[row].raw) as f32 - level(quiet[row].raw) as f32;
        assert!((step - 20.0 / RANGE_DB * 255.0).abs() <= 1.0, "{step}");
    }

    /// The power of any blend of the two sides follows from a cell.
    #[test]
    fn a_blend_follows_from_the_cells() {
        let raw: Vec<f32> = noise(1, WINDOW)
            .iter()
            .zip(sine(700.0, 0.2, WINDOW))
            .map(|(n, s)| 0.05 * n + s)
            .collect();
        let clean: Vec<f32> = noise(2, WINDOW)
            .iter()
            .zip(sine(700.0, 0.19, WINDOW))
            .map(|(n, s)| 0.002 * n + s)
            .collect();
        let cells = column_of(&raw, &clean);
        for (a, b) in [
            (1.0, 0.0),
            (0.0, 1.0),
            (0.5, 0.5),
            (0.75, 0.25),
            (1.0, -1.0),
        ] {
            let mixed: Vec<f32> = raw.iter().zip(&clean).map(|(r, c)| a * r + b * c).collect();
            let direct = column_of(&mixed, &vec![0.0; WINDOW]);
            for row in 0..ROWS {
                let (want, got) = (direct[row].raw, cells[row].blend(a, b));
                assert!(
                    (want - got).abs() <= 1e-3 * want.max(got) + 1e-9,
                    "a={a} b={b} row {row}: {got} for {want}"
                );
            }
        }
    }

    fn take_of(raw: Vec<f32>, clean: Vec<f32>, db: f32) -> FrozenTake {
        let mut t = FrozenTake::from_audio(raw, clean);
        t.modes = vec![Engine::Filtering; t.columns.len()];
        t.strengths = vec![db; t.columns.len()];
        t
    }

    /// A frozen take's clean lane at another strength is the spectrum of
    /// the audio re-rendered at it; a bypassed column stays.
    #[test]
    fn a_frozen_take_follows_the_strength() {
        let frames = 8 * COLUMN_FRAMES;
        let raw: Vec<f32> = noise(3, frames)
            .iter()
            .zip(sine(300.0, 0.2, frames))
            .map(|(n, s)| 0.02 * n + s)
            .collect();
        let model = sine(300.0, 0.2, frames);
        let a = alpha(100.0);
        let clean: Vec<f32> = raw
            .iter()
            .zip(&model)
            .map(|(r, m)| a * r + (1.0 - a) * m)
            .collect();
        let mut take = take_of(raw, clean, 100.0);
        take.modes[5] = Engine::Bypass;
        let mut an = Analyzer::new();
        let mut frozen = Frozen::of(&mut an, &take);
        assert_eq!(frozen.cols(), 8 * HOPS_PER_COLUMN);
        let mut now = take.clone();
        for db in [6.0, 24.0, 100.0, 12.0] {
            frozen.restrength(&now.strengths, db);
            now = now.at_strength(db).unwrap();
            let direct = Frozen::of(&mut an, &now);
            let (mut got, mut want) = ([0u8; COLUMN_BYTES], [0u8; COLUMN_BYTES]);
            // away from the bypassed column's edges, where a window sees
            // both sides of the change
            for col in (0..14).chain(19..24) {
                frozen.column(col, &mut got);
                direct.column(col, &mut want);
                for row in 0..COLUMN_BYTES {
                    assert!(
                        got[row].abs_diff(want[row]) <= 1,
                        "{db} dB, column {col}, row {row}: {} for {}",
                        got[row],
                        want[row]
                    );
                }
            }
        }
        // at 6 dB the noise between the harmonics is back: half the raw
        frozen.restrength(&now.strengths, 6.0);
        let mut col = [0u8; COLUMN_BYTES];
        assert_eq!(frozen.column(6, &mut col), Look::Clean);
        let row = row_of(4000.0).unwrap();
        let drop = col[row] as f32 - col[ROWS + row] as f32;
        assert!((drop - 6.0 / RANGE_DB * 255.0).abs() <= 2.0, "{drop}");
        // the bypassed column shows the original
        assert_eq!(
            frozen.column(5 * HOPS_PER_COLUMN + 1, &mut col),
            Look::Original
        );
        assert_eq!(col[..ROWS], col[ROWS..]);
    }

    /// The same frames are the same column in both lanes, live and
    /// frozen, however the audio arrives.
    #[test]
    fn both_lanes_show_a_moment_in_the_same_column() {
        let frames = 40 * HOP;
        let click_at = 23 * HOP + 300;
        let mut raw = vec![0.0f32; frames];
        for (i, x) in noise(5, 96).iter().enumerate() {
            raw[click_at + i] = 0.5 * x;
        }
        let clean: Vec<f32> = raw.iter().map(|x| 0.5 * x).collect();
        let energy = |col: &[u8]| -> (u32, u32) {
            let sum = |x: &[u8]| x.iter().map(|&l| l as u32).sum();
            (sum(&col[..ROWS]), sum(&col[ROWS..]))
        };
        // frozen
        let take = take_of(raw.clone(), clean.clone(), 100.0);
        let frozen = Frozen::of(&mut Analyzer::new(), &take);
        let mut col = [0u8; COLUMN_BYTES];
        let loudest = |side: usize| {
            (0..frozen.cols())
                .max_by_key(|j| {
                    let mut c = [0u8; COLUMN_BYTES];
                    frozen.column(*j, &mut c);
                    [energy(&c).0, energy(&c).1][side]
                })
                .unwrap()
        };
        assert_eq!((loudest(0), loudest(1)), (23, 23));
        frozen.column(20, &mut col);
        assert_eq!(energy(&col), (0, 0));
        // live, in uneven pieces from an index inside a hop: the columns
        // are the same ones
        let offset = 3 * HOP + 137;
        let picture = Arc::new(Picture::new());
        picture.want();
        let mut live = Live::new(Arc::clone(&picture));
        let mut at = offset;
        for n in [1usize, 479, 1024, 7, 4096, 333].iter().cycle() {
            if at >= frames {
                break;
            }
            let end = (at + n).min(frames);
            live.push(at as u64, &raw[at..end], &clean[at..end], Engine::Filtering);
            at = end;
        }
        let mut cursor = Cursor::default();
        let mut seen = Vec::new();
        let read = picture.read(&mut cursor, |i, levels, look| {
            seen.push((i, levels.to_vec(), look));
        });
        // the first whole hop after the start is column 0; the last one
        // waits for the rest of its window
        let first = 4;
        assert!(read.reset && read.head as usize == seen.len());
        assert_eq!(seen.len(), 40 - first - 1);
        for (i, levels, look) in &seen {
            let j = *i as usize + first;
            frozen.column(j, &mut col);
            // the first live column's window may reach before the start
            if j > first {
                assert_eq!(levels[..], col[..], "column {j}");
            }
            assert_eq!(*look, Look::Clean);
        }
    }

    #[test]
    fn bypass_shows_the_original_and_mute_nothing() {
        let x: Vec<f32> = sine(500.0, 0.1, WINDOW);
        let half: Vec<f32> = x.iter().map(|v| 0.5 * v).collect();
        let mut out = [0u8; COLUMN_BYTES];
        // the engine passes the audio through: the same sound, by the data
        let same = column_of(&x, &x);
        assert_eq!(
            levels(&same, 1.0, Engine::Filtering, &mut out),
            Look::Original
        );
        assert_eq!(out[..ROWS], out[ROWS..]);
        // bypass says so itself (the fallback's streams are never equal)
        let cells = column_of(&x, &half);
        assert_eq!(
            levels(&cells, 1.0, Engine::Bypass, &mut out),
            Look::Original
        );
        assert_eq!(out[..ROWS], out[ROWS..]);
        assert_eq!(
            levels(&cells, 1.0, Engine::Filtering, &mut out),
            Look::Clean
        );
        let row = row_of(500.0).unwrap();
        assert!(out[row] > out[ROWS + row] && out[ROWS + row] > 0);
        levels(&same, 1.0, Engine::Muted, &mut out);
        assert!(out[row] > 0 && out[ROWS..].iter().all(|&l| l == 0));
    }

    #[test]
    fn a_reader_gets_each_column_once_and_all_of_them_after_a_change() {
        let p = Picture::new();
        let mut cursor = Cursor::default();
        let count = |p: &Picture, c: &mut Cursor| {
            let mut n = 0;
            let r = p.read(c, |_, _, _| n += 1);
            (n, r)
        };
        let mut sound = [0u8; COLUMN_BYTES];
        sound[3] = 200;
        let silent = [0u8; COLUMN_BYTES];
        // nothing yet: at rest
        let (n, r) = count(&p, &mut cursor);
        assert!(n == 0 && r.reset && r.resting && !r.frozen && r.head == 0);
        for _ in 0..5 {
            p.push(&sound, Look::Clean);
        }
        let (n, r) = count(&p, &mut cursor);
        assert!(n == 5 && !r.reset && !r.resting && r.head == 5);
        assert_eq!(count(&p, &mut cursor).0, 0);
        // more than the ring holds: the newest COLS
        for _ in 0..COLS + 40 {
            p.push(&silent, Look::Clean);
        }
        let (n, r) = count(&p, &mut cursor);
        assert!(n == COLS && r.head == COLS as u64 + 45 && r.resting);
        // a frozen take comes into view, and goes
        let take = take_of(
            sine(440.0, 0.1, 4 * COLUMN_FRAMES),
            vec![0.0; 4 * COLUMN_FRAMES],
            100.0,
        );
        let frozen = Frozen::of(&mut Analyzer::new(), &take);
        p.freeze(frozen.columns());
        assert_eq!(count(&p, &mut cursor).0, 0, "not in view yet");
        p.show_frozen(true);
        let (n, r) = count(&p, &mut cursor);
        assert!(n == 12 && r.reset && r.frozen && r.head == 12 && !r.resting);
        p.push(&sound, Look::Clean);
        assert_eq!(
            count(&p, &mut cursor).0,
            0,
            "live columns wait behind the take"
        );
        p.show_frozen(false);
        let (n, r) = count(&p, &mut cursor);
        assert!(n == COLS && r.reset && !r.frozen && !r.resting);
        p.unfreeze();
        assert!(!count(&p, &mut cursor).1.reset);
        p.clear();
        let (n, r) = count(&p, &mut cursor);
        assert!(n == 0 && r.reset && r.head == 0 && r.resting);
    }

    /// A reader that rests is woken by the first column that changes what
    /// is in view, and by nothing else.
    #[test]
    fn a_resting_reader_is_woken_by_what_it_would_see() {
        let p = Picture::new();
        let mut sound = [0u8; COLUMN_BYTES];
        sound[40] = 1;
        let silent = [0u8; COLUMN_BYTES];
        assert!(!p.push(&sound, Look::Clean), "nobody rests");
        p.rest();
        assert!(p.push(&silent, Look::Clean), "the sound in view moves on");
        assert!(!p.push(&silent, Look::Clean), "woken once");
        for _ in 0..COLS {
            p.push(&silent, Look::Clean);
        }
        p.rest();
        assert!(!p.push(&silent, Look::Clean), "silence over silence");
        assert!(p.push(&sound, Look::Clean));
        // behind a frozen take nothing in view moves
        let take = take_of(vec![0.1; COLUMN_FRAMES], vec![0.1; COLUMN_FRAMES], 100.0);
        p.freeze(Frozen::of(&mut Analyzer::new(), &take).columns());
        p.show_frozen(true);
        p.rest();
        assert!(!p.push(&sound, Look::Clean));
        p.show_frozen(false);
        assert!(p.clear(), "the buffer started over");
        assert!(!p.clear(), "nothing to clear");
    }

    #[test]
    fn nothing_is_analysed_until_a_page_wants_the_picture() {
        let picture = Arc::new(Picture::new());
        let mut live = Live::new(Arc::clone(&picture));
        let x = sine(440.0, 0.1, 4 * WINDOW);
        live.push(0, &x, &x, Engine::Filtering);
        let mut cursor = Cursor::default();
        assert_eq!(picture.read(&mut cursor, |_, _, _| {}).head, 0);
        picture.want();
        live.push(x.len() as u64, &x, &x, Engine::Filtering);
        let head = picture.read(&mut cursor, |_, _, _| {}).head;
        assert!(head > 0);
        // a gap in the index starts the lanes over
        live.push(9 * x.len() as u64, &x, &x, Engine::Filtering);
        let shown = picture.read(&mut cursor, |_, _, _| {});
        assert!(shown.reset && shown.head <= head);
        live.clear();
        assert_eq!(picture.read(&mut cursor, |_, _, _| {}).head, 0);
    }
}
