//! A file's picture: the spectrum of the original and of the cleaned
//! sound over the whole file, the two lanes the page draws on one time
//! axis.
//!
//! A column is a stretch of the file ([`span_for`]: so many hops of the
//! spectrum that a file of a stated length has at most [`COLS_MAX`]
//! columns, however long it is); a row is one of the spectrum's bands
//! (`miccheck::spectrum`, Live's picture). A column holds the mean of its
//! hops as levels on the spectrum's fixed scale: the original's, the
//! cleaned sound's, and how alike the two are in each band (their cross
//! spectrum over their levels). The level of any mix of the two follows
//! from those three, so a strength change redraws the cleaned lane without
//! going back to the audio.
//!
//! The pass writes the columns into the file's cache folder as it goes:
//! the full read the original's ([`Original`], at the file's own rate),
//! every front the cleaned ones ([`Cleaned`]). The window reads them as a
//! [`Sheet`], for the file it shows; the folder goes with the file.

use crate::miccheck::spectrum::{self, Analyzer, Cell, COLUMN_BYTES, HOP, LEAD, ROWS, WINDOW};
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

/// The picture's file in the cache folder.
pub const NAME: &str = "picture.u8";
/// Columns of a file whose length is stated; one that turns out longer,
/// or states none, has more, and the window draws them [`COLS_MAX`] wide.
pub const COLS_MAX: usize = 1024;
/// A column of a file that states no length: about a second.
const SPAN_UNSTATED: u64 = (HOP as u64) << 6;
/// The engine's rate, the columns' clock.
const RATE: u64 = 48_000;

const MAGIC: [u8; 4] = *b"HMPC";
const VERSION: u8 = 1;
/// The file starts with the magic, the version, the rows and the span.
const HEADER: usize = 16;
/// A column's original part: its hops (0: not read yet), its levels.
const ORIGINAL: usize = 2 + ROWS;
/// Its cleaned part: its hops (0: not cleaned yet), its levels, how alike.
const CLEANED: usize = 2 + 2 * ROWS;
/// One column on disk.
pub const RECORD: usize = ORIGINAL + CLEANED;
/// The most a picture takes on disk when the file's length is stated.
pub const BYTES_MAX: u64 = (HEADER + COLS_MAX * RECORD) as u64;

/// 48 kHz frames per column for a file that states `frames` of them.
pub fn span_for(frames: Option<u64>) -> u64 {
    let Some(frames) = frames else {
        return SPAN_UNSTATED;
    };
    let mut span = HOP as u64;
    while frames.div_ceil(span) > COLS_MAX as u64 {
        span *= 2;
    }
    span
}

/// Start the picture in `file`: its header (the columns follow as they
/// are put).
pub fn start(file: &File, span: u64) -> io::Result<()> {
    let mut h = [0u8; HEADER];
    h[..4].copy_from_slice(&MAGIC);
    h[4] = VERSION;
    h[5] = ROWS as u8;
    h[8..].copy_from_slice(&span.to_le_bytes());
    file.write_all_at(&h, 0)
}

fn at(col: u64) -> u64 {
    HEADER as u64 + col * RECORD as u64
}

/// Column `col` of the original: the mean of `hops` hops.
pub fn put_original(file: &File, col: u64, hops: u16, levels: &[u8; ROWS]) -> io::Result<()> {
    let mut b = [0u8; ORIGINAL];
    b[..2].copy_from_slice(&hops.to_le_bytes());
    b[2..].copy_from_slice(levels);
    file.write_all_at(&b, at(col))
}

/// Column `col` of the cleaned sound, unless what is there is the mean of
/// more hops (two fronts meet inside a column: the fuller one stays). One
/// writer at a time.
pub fn put_cleaned(
    file: &File,
    col: u64,
    hops: u16,
    levels: &[u8; ROWS],
    like: &[i8; ROWS],
) -> io::Result<()> {
    let to = at(col) + ORIGINAL as u64;
    let mut n = [0u8; 2];
    let has = match file.read_at(&mut n, to)? {
        2 => u16::from_le_bytes(n),
        _ => 0,
    };
    if has > hops {
        return Ok(());
    }
    let mut b = [0u8; CLEANED];
    b[..2].copy_from_slice(&hops.to_le_bytes());
    b[2..2 + ROWS].copy_from_slice(levels);
    for (o, l) in b[2 + ROWS..].iter_mut().zip(like) {
        *o = *l as u8;
    }
    file.write_all_at(&b, to)
}

/// The original's columns in a picture's bytes (the whole file, as read),
/// to put them into a new one: (column, hops, levels).
pub fn originals(bytes: &[u8]) -> impl Iterator<Item = (u64, u16, [u8; ROWS])> + '_ {
    bytes
        .get(HEADER..)
        .unwrap_or_default()
        .chunks(RECORD)
        .enumerate()
        .filter_map(|(col, r)| {
            let hops = u16::from_le_bytes([*r.first()?, *r.get(1)?]);
            let levels: [u8; ROWS] = r.get(2..ORIGINAL)?.try_into().ok()?;
            (hops > 0).then_some((col as u64, hops, levels))
        })
}

/// One column's hops, summed.
struct Sum {
    hops: u32,
    cells: Box<[Cell; ROWS]>,
}

impl Sum {
    fn new() -> Sum {
        Sum {
            hops: 0,
            cells: Box::new([Cell::default(); ROWS]),
        }
    }

    fn add(&mut self, cells: &[Cell; ROWS]) {
        self.hops += 1;
        for (s, c) in self.cells.iter_mut().zip(cells) {
            s.raw += c.raw;
            s.clean += c.clean;
            s.cross += c.cross;
        }
    }

    fn hops(&self) -> u16 {
        self.hops.min(u16::MAX as u32) as u16
    }

    fn clear(&mut self) {
        self.hops = 0;
        self.cells.fill(Cell::default());
    }
}

/// The original's side, fed the file as it is read (every channel, at the
/// file's own rate): a hop every 1/60 s, each through the spectrum's
/// window, summed into the column its middle falls in.
pub struct Original {
    analyzer: Analyzer,
    rate: u64,
    hop: u64,
    span: u64,
    /// The channels' mean, from frame `base` on.
    buf: Vec<f32>,
    base: i64,
    /// Frames fed.
    end: u64,
    /// The next hop.
    next: u64,
    col: u64,
    sum: Sum,
    cells: Box<[Cell; ROWS]>,
}

impl Original {
    pub fn new(rate: u32, span: u64) -> Original {
        let rate = rate.max(1) as u64;
        Original {
            analyzer: Analyzer::at_rate(rate as u32),
            rate,
            hop: (HOP as u64 * rate).div_ceil(RATE).max(1),
            span: span.max(1),
            // the first hop's window begins before the file does
            buf: vec![0.0; WINDOW / 2],
            base: -((WINDOW / 2) as i64),
            end: 0,
            next: 0,
            col: 0,
            sum: Sum::new(),
            cells: Box::new([Cell::default(); ROWS]),
        }
    }

    /// The next frames of the file, planar. `put(column, hops, levels)`
    /// takes every column that is through.
    pub fn push(&mut self, block: &[Vec<f32>], put: &mut dyn FnMut(u64, u16, &[u8; ROWS])) {
        let n = block.first().map_or(0, Vec::len);
        let k = 1.0 / block.len().max(1) as f32;
        self.buf
            .extend((0..n).map(|i| block.iter().map(|c| c[i]).sum::<f32>() * k));
        self.end += n as u64;
        self.run(false, put);
    }

    /// The file has ended: the last hops (what follows is silence) and the
    /// last column.
    pub fn finish(&mut self, put: &mut dyn FnMut(u64, u16, &[u8; ROWS])) {
        self.run(true, put);
        self.flush(put);
    }

    fn run(&mut self, last: bool, put: &mut dyn FnMut(u64, u16, &[u8; ROWS])) {
        let start_of = |hop: u64, each: u64| (hop * each + each / 2) as i64 - (WINDOW / 2) as i64;
        loop {
            let middle = self.next * self.hop + self.hop / 2;
            let start = start_of(self.next, self.hop);
            let have = self.base + self.buf.len() as i64;
            if last {
                if middle >= self.end {
                    break;
                }
            } else if start + WINDOW as i64 > have {
                break;
            }
            let a = ((start - self.base).max(0) as usize).min(self.buf.len());
            let b = (a + WINDOW).min(self.buf.len());
            self.analyzer.column(&self.buf[a..b], &[], &mut self.cells);
            let col = middle * RATE / self.rate / self.span;
            if col != self.col {
                self.flush(put);
                self.col = col;
            }
            self.sum.add(&self.cells);
            self.next += 1;
        }
        let keep = start_of(self.next, self.hop) - self.base;
        let drop = keep.clamp(0, self.buf.len() as i64);
        self.buf.drain(..drop as usize);
        self.base += drop;
    }

    fn flush(&mut self, put: &mut dyn FnMut(u64, u16, &[u8; ROWS])) {
        if self.sum.hops == 0 {
            return;
        }
        let k = 1.0 / self.sum.hops as f32;
        let mut levels = [0u8; ROWS];
        for (l, c) in levels.iter_mut().zip(self.sum.cells.iter()) {
            *l = spectrum::level(c.raw * k);
        }
        put(self.col, self.sum.hops(), &levels);
        self.sum.clear();
    }
}

/// What a front hands on for a column: its number, its hops, the cleaned
/// sound's levels and how alike the original is in each band.
pub type PutCleaned<'a> = &'a mut dyn FnMut(u64, u16, &[u8; ROWS], &[i8; ROWS]);

/// The cleaned side, fed by one front: the original and what the engine
/// made of it, frame for frame at 48 kHz. Hops sit on multiples of the
/// spectrum's hop of the file's frames, so every front's columns are the
/// same stretches.
pub struct Cleaned {
    analyzer: Analyzer,
    span: u64,
    /// Hops before this frame are the front's warm-up: they don't count.
    from: u64,
    raw: Vec<f32>,
    clean: Vec<f32>,
    /// The frame the buffers start at.
    base: i64,
    /// The frame expected next.
    end: Option<u64>,
    /// The next hop.
    next: u64,
    col: u64,
    sum: Sum,
    cells: Box<[Cell; ROWS]>,
}

impl Cleaned {
    pub fn new(span: u64, from: u64) -> Cleaned {
        Cleaned {
            analyzer: Analyzer::new(),
            span: span.max(1),
            from,
            raw: Vec::new(),
            clean: Vec::new(),
            base: 0,
            end: None,
            next: 0,
            col: 0,
            sum: Sum::new(),
            cells: Box::new([Cell::default(); ROWS]),
        }
    }

    /// Frames from `at` on, both sides. `put` takes every column that is
    /// through.
    pub fn push(&mut self, at: u64, raw: &[f32], clean: &[f32], put: PutCleaned<'_>) {
        let n = raw.len().min(clean.len());
        if n == 0 {
            return;
        }
        if self.end != Some(at) {
            // elsewhere in the file: what came before it counts as silence
            self.flush(put);
            self.next = at.div_ceil(HOP as u64);
            self.base = at as i64;
            self.raw.clear();
            self.clean.clear();
        }
        self.end = Some(at + n as u64);
        self.raw.extend_from_slice(&raw[..n]);
        self.clean.extend_from_slice(&clean[..n]);
        self.run(false, put);
    }

    /// The front is through here: the column it was in, as far as it got.
    pub fn flush(&mut self, put: PutCleaned<'_>) {
        if self.sum.hops == 0 {
            return;
        }
        let k = 1.0 / self.sum.hops as f32;
        let (mut levels, mut like) = ([0u8; ROWS], [0i8; ROWS]);
        for (row, c) in self.sum.cells.iter().enumerate() {
            levels[row] = spectrum::level(c.clean * k);
            let both = (c.raw * c.clean).sqrt();
            if both > 0.0 {
                like[row] = ((c.cross / both).clamp(-1.0, 1.0) * 127.0).round() as i8;
            }
        }
        put(self.col, self.sum.hops(), &levels, &like);
        self.sum.clear();
    }

    /// The file ends where the frames did: its last hops (what follows is
    /// silence) and its last column.
    pub fn finish(&mut self, put: PutCleaned<'_>) {
        self.run(true, put);
        self.flush(put);
    }

    fn run(&mut self, last: bool, put: PutCleaned<'_>) {
        let Some(end) = self.end else {
            return;
        };
        loop {
            let hop = self.next * HOP as u64;
            let start = hop as i64 - LEAD as i64;
            if last {
                if hop >= end {
                    break;
                }
            } else if start + WINDOW as i64 > end as i64 {
                break;
            }
            let len = self.raw.len();
            // a window that begins before the buffers reads silence there
            let lead = ((self.base - start).max(0) as usize).min(WINDOW);
            let a = ((start - self.base).max(0) as usize).min(len);
            let b = (a + WINDOW - lead).min(len);
            if lead == 0 {
                self.analyzer
                    .column(&self.raw[a..b], &self.clean[a..b], &mut self.cells);
            } else {
                let pad = |x: &[f32]| {
                    let mut w = vec![0.0; lead];
                    w.extend_from_slice(&x[a..b]);
                    w
                };
                let (r, c) = (pad(&self.raw), pad(&self.clean));
                self.analyzer.column(&r, &c, &mut self.cells);
            }
            if hop >= self.from {
                let col = hop / self.span;
                if col != self.col {
                    self.flush(put);
                    self.col = col;
                }
                self.sum.add(&self.cells);
            }
            self.next += 1;
        }
        let keep = (self.next * HOP as u64) as i64 - LEAD as i64 - self.base;
        let drop = keep.clamp(0, self.raw.len() as i64) as usize;
        self.raw.drain(..drop);
        self.clean.drain(..drop);
        self.base += drop as i64;
    }
}

/// A file's picture as the window holds it for the file it shows: at most
/// [`COLS_MAX`] columns.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    /// 48 kHz frames per column.
    span: u64,
    cols: usize,
    /// Per column and band (lowest first): the original's level, the
    /// cleaned sound's, how alike.
    raw: Vec<u8>,
    clean: Vec<u8>,
    like: Vec<i8>,
    /// The column has cleaned sound.
    lit: Vec<bool>,
}

impl Sheet {
    /// An empty picture of `cols` columns of `span` frames (the demo's
    /// files fill theirs with [`Sheet::set`]).
    pub fn new(span: u64, cols: usize) -> Sheet {
        Sheet {
            span: span.max(1),
            cols,
            raw: vec![0; cols * ROWS],
            clean: vec![0; cols * ROWS],
            like: vec![0; cols * ROWS],
            lit: vec![false; cols],
        }
    }

    /// Column `col` from the mean of some hops' cells, cleaned or not yet.
    pub fn set(&mut self, col: usize, cells: &[Cell; ROWS], lit: bool) {
        let at = col * ROWS;
        for (row, c) in cells.iter().enumerate() {
            self.raw[at + row] = spectrum::level(c.raw);
            self.clean[at + row] = spectrum::level(c.clean);
            let both = (c.raw * c.clean).sqrt();
            self.like[at + row] = if both > 0.0 {
                ((c.cross / both).clamp(-1.0, 1.0) * 127.0).round() as i8
            } else {
                0
            };
        }
        self.lit[col] = lit;
    }

    /// Which columns show cleaned sound: those `lit` says.
    pub fn light(&mut self, lit: impl Fn(usize) -> bool) {
        for (col, l) in self.lit.iter_mut().enumerate() {
            *l = lit(col);
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Seconds a column stands for.
    pub fn column_secs(&self) -> f64 {
        self.span as f64 / RATE as f64
    }

    /// Columns with cleaned sound.
    pub fn lit(&self) -> usize {
        self.lit.iter().filter(|l| **l).count()
    }

    /// The picture in the cache folder `dir`, as far as it is written;
    /// None where there is none (yet).
    pub fn read(dir: &Path) -> io::Result<Option<Sheet>> {
        let bytes = match std::fs::read(dir.join(NAME)) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(Sheet::parse(&bytes))
    }

    fn parse(bytes: &[u8]) -> Option<Sheet> {
        let h = bytes.get(..HEADER)?;
        if h[..4] != MAGIC || h[4] != VERSION || h[5] as usize != ROWS {
            return None;
        }
        let span = u64::from_le_bytes(h[8..].try_into().ok()?);
        if span == 0 {
            return None;
        }
        let body = &bytes[HEADER..];
        let n = body.len().div_ceil(RECORD);
        // more columns than the window draws: the loudest of each few
        let each = n.div_ceil(COLS_MAX).max(1);
        let mut sheet = Sheet::new(span * each as u64, n.div_ceil(each));
        let mut record = [0u8; RECORD];
        for (i, r) in body.chunks(RECORD).enumerate() {
            // the last may be half written: the rest is not there yet
            record.fill(0);
            record[..r.len()].copy_from_slice(r);
            let at = i / each * ROWS;
            if u16::from_le_bytes([record[0], record[1]]) > 0 {
                for (o, l) in sheet.raw[at..at + ROWS].iter_mut().zip(&record[2..]) {
                    *o = (*o).max(*l);
                }
            }
            let c = &record[ORIGINAL..];
            if u16::from_le_bytes([c[0], c[1]]) > 0 {
                let first = !sheet.lit[i / each];
                sheet.lit[i / each] = true;
                for row in 0..ROWS {
                    let (level, like) = (c[2 + row], c[2 + ROWS + row] as i8);
                    if first || level > sheet.clean[at + row] {
                        sheet.clean[at + row] = level;
                        sheet.like[at + row] = like;
                    }
                }
            }
        }
        Some(sheet)
    }

    /// The lanes as levels, a column after the other (the original's
    /// rows, then the cleaned sound's, as the spectrum's columns), with
    /// `alpha` of the original left in the cleaned sound (the strength).
    /// Columns not cleaned yet have an empty cleaned lane.
    pub fn levels(&self, alpha: f32, out: &mut Vec<u8>) {
        let power: Vec<f32> = (0..=255).map(spectrum::power).collect();
        let (a, b) = (alpha.clamp(0.0, 1.0), 1.0 - alpha.clamp(0.0, 1.0));
        out.clear();
        out.resize(self.cols * COLUMN_BYTES, 0);
        for col in 0..self.cols {
            let (at, to) = (col * ROWS, col * COLUMN_BYTES);
            out[to..to + ROWS].copy_from_slice(&self.raw[at..at + ROWS]);
            if !self.lit[col] {
                continue;
            }
            for row in 0..ROWS {
                let clean = self.clean[at + row];
                out[to + ROWS + row] = if a == 0.0 {
                    clean
                } else {
                    let (r, c) = (power[self.raw[at + row] as usize], power[clean as usize]);
                    let cross = self.like[at + row] as f32 / 127.0 * (r * c).sqrt();
                    spectrum::level((a * a * r + b * b * c + 2.0 * a * b * cross).max(0.0))
                };
            }
        }
    }

    /// Bytes this picture holds in memory.
    pub fn bytes(&self) -> usize {
        self.raw.len() + self.clean.len() + self.like.len() + self.lit.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, amp: f32, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|n| amp * (std::f32::consts::TAU * hz * n as f32 / rate as f32).sin())
            .collect()
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hushmic-picture-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_stated_length_has_at_most_the_columns_the_window_draws() {
        assert_eq!(
            span_for(Some(10 * RATE)),
            HOP as u64,
            "10 s: a hop a column"
        );
        for secs in [1u64, 30, 760, 2650, 3 * 3600, 12 * 3600] {
            let frames = secs * RATE;
            let span = span_for(Some(frames));
            let cols = frames.div_ceil(span);
            assert!(cols <= COLS_MAX as u64, "{secs} s: {cols} columns");
            assert!(
                cols > COLS_MAX as u64 / 2 || span == HOP as u64,
                "{secs} s: {cols} columns of {span}"
            );
            assert_eq!(span % HOP as u64, 0);
        }
        assert_eq!(span_for(None), 51_200);
    }

    #[test]
    fn the_original_lands_in_its_columns_at_any_rate() {
        // 1 kHz for the first second, silence for the second, at three
        // rates: the same columns, the same band, the same level
        let span = 4 * HOP as u64;
        let want = spectrum::row_of(1000.0).unwrap();
        for rate in [48_000u32, 44_100, 16_000] {
            let mut x = sine(1000.0, 0.1, rate, rate as usize);
            x.extend(vec![0.0; rate as usize]);
            let mut cols: Vec<(u64, u16, [u8; ROWS])> = Vec::new();
            let mut o = Original::new(rate, span);
            // stereo, in uneven blocks
            for block in x.chunks(1000) {
                o.push(&[block.to_vec(), block.to_vec()], &mut |c, n, l| {
                    cols.push((c, n, *l))
                });
            }
            o.finish(&mut |c, n, l| cols.push((c, n, *l)));
            assert_eq!(cols.len(), 30, "{rate}: 2 s in columns of 4 hops");
            assert!(cols.iter().enumerate().all(|(i, c)| c.0 == i as u64));
            assert!(cols[..29].iter().all(|c| c.1 == 4), "{rate}: whole columns");
            let (_, _, tone) = cols[7];
            let loudest = (0..ROWS).max_by_key(|r| tone[*r]).unwrap();
            assert!(loudest.abs_diff(want) <= 1, "{rate}: row {loudest}");
            assert!(
                tone[loudest] >= 235,
                "{rate}: -20 dB is the top of the scale"
            );
            assert!(cols[20].2.iter().all(|l| *l == 0), "{rate}: then silence");
        }
    }

    #[test]
    fn a_fronts_columns_are_the_files_whatever_its_blocks() {
        // noise with a tone taken out of it, fed whole and in blocks from
        // a warm-up on: the same columns
        let n = 40 * HOP;
        let mut s = 7u32;
        let raw: Vec<f32> = (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((s >> 8) as f32 / (1u32 << 23) as f32 - 1.0) * 0.05
            })
            .collect();
        let tone = sine(700.0, 0.05, 48_000, n);
        let both: Vec<f32> = raw.iter().zip(&tone).map(|(r, t)| r + t).collect();
        type Col = (u64, u16, [u8; ROWS], [i8; ROWS]);
        let run = |blocks: usize, from: u64| {
            let mut out: Vec<Col> = Vec::new();
            let mut c = Cleaned::new(4 * HOP as u64, from);
            let mut at = 0;
            for (r, t) in both.chunks(blocks).zip(tone.chunks(blocks)) {
                c.push(at, r, t, &mut |col, n, l, k| out.push((col, n, *l, *k)));
                at += r.len() as u64;
            }
            c.finish(&mut |col, n, l, k| out.push((col, n, *l, *k)));
            out
        };
        let whole = run(n, 0);
        assert_eq!(whole.len(), 10);
        assert!(whole.iter().all(|c| c.1 == 4));
        assert_eq!(run(1234, 0), whole);
        // a front that warms up over the first two columns writes the rest
        let late = run(777, 8 * HOP as u64);
        assert_eq!(late[..], whole[2..]);
        // the tone's band: all of it was kept; a band of noise alone: none
        let row = spectrum::row_of(700.0).unwrap();
        let (_, _, levels, like) = whole[5];
        assert!(levels[row] > 200 && like[row] > 100, "{levels:?} {like:?}");
        let quiet = spectrum::row_of(5000.0).unwrap();
        assert!(levels[quiet] < 40, "{}", levels[quiet]);
    }

    #[test]
    fn a_sheet_reads_what_was_put_and_mixes_it_at_any_strength() {
        let d = tmp("sheet");
        assert_eq!(Sheet::read(&d).unwrap(), None, "no picture yet");
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(d.join(NAME))
            .unwrap();
        start(&f, 3200).unwrap();
        let (mut raw, mut clean, mut like) = ([0u8; ROWS], [0u8; ROWS], [0i8; ROWS]);
        // band 10: the voice, kept whole; band 20: noise, taken out
        (raw[10], clean[10], like[10]) = (200, 200, 127);
        (raw[20], clean[20], like[20]) = (150, 0, 0);
        for col in 0..6 {
            put_original(&f, col, 4, &raw).unwrap();
        }
        put_cleaned(&f, 0, 4, &clean, &like).unwrap();
        put_cleaned(&f, 1, 4, &clean, &like).unwrap();
        // a front that saw less of a column leaves the fuller one
        put_cleaned(&f, 1, 2, &[9; ROWS], &[0; ROWS]).unwrap();
        // one that seeks ahead writes past the others
        put_cleaned(&f, 4, 1, &clean, &like).unwrap();
        let s = Sheet::read(&d).unwrap().unwrap();
        assert_eq!((s.cols(), s.lit()), (6, 3));
        assert!((s.column_secs() - 3200.0 / 48_000.0).abs() < 1e-9);
        let mut out = Vec::new();
        s.levels(0.0, &mut out);
        assert_eq!(out.len(), 6 * COLUMN_BYTES);
        let col = |out: &[u8], c: usize| out[c * COLUMN_BYTES..(c + 1) * COLUMN_BYTES].to_vec();
        assert_eq!(col(&out, 1)[..ROWS], raw);
        assert_eq!(col(&out, 1)[ROWS..], clean, "Maximum: as cleaned");
        assert!(col(&out, 2)[ROWS..].iter().all(|l| *l == 0), "not cleaned");
        assert_eq!(col(&out, 4)[ROWS..], clean);
        // half of the original left: the voice as it was, the noise 6 dB
        // down (0.27 dB a step)
        s.levels(0.5, &mut out);
        let c = col(&out, 0);
        assert_eq!(c[ROWS + 10], 200);
        assert_eq!(c[ROWS + 20], 150 - 22);
        // all of it: the original
        s.levels(1.0, &mut out);
        assert_eq!(col(&out, 0)[ROWS..], raw);
        // the original's columns go into a new picture
        let bytes = std::fs::read(d.join(NAME)).unwrap();
        let o: Vec<_> = originals(&bytes).collect();
        assert_eq!(o.len(), 6);
        assert_eq!(o[5], (5, 4, raw));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_picture_with_more_columns_is_read_as_wide_as_it_is_drawn() {
        let d = tmp("wide");
        let f = File::create(d.join(NAME)).unwrap();
        start(&f, 51_200).unwrap();
        let n = 3 * COLS_MAX as u64 - 3;
        for col in 0..n {
            let mut raw = [0u8; ROWS];
            raw[0] = (col % 3) as u8 * 100;
            put_original(&f, col, 1, &raw).unwrap();
        }
        let s = Sheet::read(&d).unwrap().unwrap();
        assert_eq!(s.cols(), COLS_MAX - 1);
        assert!((s.column_secs() - 3.2).abs() < 1e-9);
        let mut out = Vec::new();
        s.levels(0.0, &mut out);
        assert!(
            (0..s.cols()).all(|c| out[c * COLUMN_BYTES] == 200),
            "the loudest"
        );
        assert!(s.bytes() < 300_000);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
