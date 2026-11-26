//! The engine meter (M2): per 10 ms hop, the level of the raw microphone
//! going into the engine and of what leaves it, published by the plugin's
//! DSP worker into shared memory that the app maps read-only. The ring
//! holds about 41 s, so a window that opens mid-call can draw the recent
//! past at once ([`MeterReader::rewind`]).
//!
//! Layout v2 (little endian, every field naturally aligned): a header
//! page, then the ring of `slots` x 32 bytes from offset 4096.
//!
//! | Offset | Field | Meaning |
//! |---|---|---|
//! | 0 | magic u64 | `HMMETER\0`; written last on (re)initialisation |
//! | 8 | version u32 | 2 |
//! | 12 | slots u32 | ring length in hops ([`SLOTS`]) |
//! | 16 | slot_bytes u32 | 32 |
//! | 20 | hop_frames u32 | 480 |
//! | 24 | rate u32 | 48000 |
//! | 28 | lag_hops u32 | the input side is delayed by this many hops so a slot's in and out are the same sound |
//! | 32 | instance u64 | the plugin instance; a new one restarts `hops` at 0 |
//! | 40 | hops u64 | hops published so far; hop `i` lives in slot `i % slots` |
//! | 48 | epoch u32 | +1 on every engine reset (stream restart) and after every idle gap of more than [`RUN_GAP_MS`]: one epoch is one unbroken run |
//! | 56 | last_ms u64 | CLOCK_MONOTONIC ms of the newest hop (0: none yet), so a reader that just opened knows whether audio flows |
//! | 4096 | slots | `slots` x 32 bytes, see below |
//!
//! Slot: `seq u64` (hop index + 1; 0 while the writer is in it), then f32
//! bit patterns `in_peak`, `out_peak`, `in_ms`, `out_ms` (peak magnitude and
//! mean square over the hop), `mode u32` ([`MODE_PROCESS`], [`MODE_BYPASS`],
//! [`MODE_MUTE`], [`MODE_PASSTHROUGH`]) and `epoch u32`.
//!
//! Versions: the reader also maps layout v1 (one page; 120 slots from
//! offset 64; the same header and slots), so a window updated under a
//! running chain keeps its meter. A reader of v1 alone refuses a v2 file
//! by its size and shows no meter until the window itself runs the new
//! version (a restarted chain writes v2 again). The writer
//! never resizes a file in place: a file of another size is replaced by a
//! new one, so no reader of another version can fault on pages that went
//! away.
//!
//! The writer never blocks or allocates per hop, and its only call out is
//! one CLOCK_MONOTONIC read (vDSO, as the worker's own timing). A
//! reader never blocks the writer: each slot carries its own sequence
//! number, cleared before and set after the data, so a slot overwritten
//! while it was read is detected and dropped.

use crate::region::{Region, PAGE};
use std::io;
use std::path::Path;
use std::sync::atomic::{fence, Ordering};

pub const MAGIC: u64 = u64::from_le_bytes(*b"HMMETER\0");
pub const VERSION: u32 = 2;
/// Hops kept: 41 s of history for a window that opens mid-call (a
/// 1920 px Live picture needs about 16 s, an ultrawide one more).
pub const SLOTS: usize = 4096;
pub const SLOT_BYTES: usize = 32;
pub const HEADER_BYTES: usize = 64;
pub const HOP_FRAMES: u32 = 480;
pub const RATE: u32 = 48_000;
/// The ring starts on the second page.
const RING: usize = PAGE;
/// The meter file: the header page and the ring (33 pages).
pub const SIZE: usize = RING + SLOTS * SLOT_BYTES;
/// A publish this long after the previous one starts a new run (epoch):
/// the engine stood still (no app used HushMic) in between.
pub const RUN_GAP_MS: u64 = 250;
/// Hops a rewind leaves alone at the old end of the ring: the writer
/// overwrites those next.
const REWIND_MARGIN: u64 = 16;

/// One layout version's geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    version: u32,
    slots: u64,
    ring: usize,
    size: usize,
}

impl Layout {
    fn slot(&self, i: u64) -> usize {
        self.ring + (i % self.slots) as usize * SLOT_BYTES
    }
}

const V2: Layout = Layout {
    version: VERSION,
    slots: SLOTS as u64,
    ring: RING,
    size: SIZE,
};
/// The first layout: one page, 120 slots after the header.
const V1: Layout = Layout {
    version: 1,
    slots: 120,
    ring: HEADER_BYTES,
    size: PAGE,
};
/// Upper bound for `lag_hops` (the shipped models need 5).
pub const MAX_LAG_HOPS: usize = 16;

pub const MODE_PROCESS: u32 = 0;
pub const MODE_BYPASS: u32 = 1;
pub const MODE_MUTE: u32 = 2;
/// Processing is on, but the engine stepped down to passing the raw
/// microphone through (the adaptive engine's passthrough tier): apps hear
/// it unfiltered, as in bypass. Layout v2 only.
pub const MODE_PASSTHROUGH: u32 = 3;

const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 8;
const OFF_SLOTS: usize = 12;
const OFF_SLOT_BYTES: usize = 16;
const OFF_HOP_FRAMES: usize = 20;
const OFF_RATE: usize = 24;
const OFF_LAG: usize = 28;
const OFF_INSTANCE: usize = 32;
const OFF_HOPS: usize = 40;
const OFF_EPOCH: usize = 48;
const OFF_LAST_MS: usize = 56;

const S_SEQ: usize = 0;
const S_IN_PEAK: usize = 8;
const S_OUT_PEAK: usize = 12;
const S_IN_MS: usize = 16;
const S_OUT_MS: usize = 20;
const S_MODE: usize = 24;
const S_EPOCH: usize = 28;

const _: () = assert!(HEADER_BYTES <= RING && SLOTS.is_power_of_two());
const _: () = assert!(V1.ring + V1.slots as usize * SLOT_BYTES <= V1.size);

/// Peak magnitude and mean square of a block. Non-finite samples count as
/// silence, so one bad sample cannot poison a column.
pub fn peak_ms(x: &[f32]) -> (f32, f32) {
    let mut peak = 0f32;
    let mut sum = 0f32;
    for &s in x {
        let s = if s.is_finite() { s } else { 0.0 };
        peak = peak.max(s.abs());
        sum += s * s;
    }
    (
        peak,
        if x.is_empty() {
            0.0
        } else {
            sum / x.len() as f32
        },
    )
}

/// The plugin side. Owned by the DSP worker.
pub struct MeterWriter {
    region: Region,
    hops: u64,
    epoch: u32,
    /// When the previous hop was published (0: none yet).
    last_ms: u64,
    lag: [(f32, f32); MAX_LAG_HOPS],
    lag_hops: usize,
    lag_pos: usize,
}

impl MeterWriter {
    /// Create (or take over) the meter file at `path` for a new plugin
    /// instance. `lag_hops` delays the input side so that each slot pairs
    /// an output hop with the input hop it was made from.
    pub fn create(path: &Path, instance: u64, lag_hops: u32) -> io::Result<MeterWriter> {
        let lag_hops = lag_hops as usize;
        if lag_hops > MAX_LAG_HOPS {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "lag too long"));
        }
        // Never resize a file a reader may have mapped (see the module
        // docs): another size gets a new file.
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.len() != SIZE as u64) {
            let _ = std::fs::remove_file(path);
        }
        let region = Region::create_or_open(path, SIZE)?;
        region.prefault();
        // A reader of a previous instance must never mistake the half
        // initialised page for data: the magic goes first and comes back last.
        region.u64(OFF_MAGIC).store(0, Ordering::Relaxed);
        fence(Ordering::Release);
        region.u32(OFF_VERSION).store(VERSION, Ordering::Relaxed);
        region.u32(OFF_SLOTS).store(SLOTS as u32, Ordering::Relaxed);
        region
            .u32(OFF_SLOT_BYTES)
            .store(SLOT_BYTES as u32, Ordering::Relaxed);
        region
            .u32(OFF_HOP_FRAMES)
            .store(HOP_FRAMES, Ordering::Relaxed);
        region.u32(OFF_RATE).store(RATE, Ordering::Relaxed);
        region
            .u32(OFF_LAG)
            .store(lag_hops as u32, Ordering::Relaxed);
        region.u64(OFF_HOPS).store(0, Ordering::Relaxed);
        region.u32(OFF_EPOCH).store(0, Ordering::Relaxed);
        region.u64(OFF_LAST_MS).store(0, Ordering::Relaxed);
        for i in 0..SLOTS as u64 {
            region.u64(V2.slot(i) + S_SEQ).store(0, Ordering::Relaxed);
        }
        region.u64(OFF_INSTANCE).store(instance, Ordering::Release);
        region.u64(OFF_MAGIC).store(MAGIC, Ordering::Release);
        Ok(MeterWriter {
            region,
            hops: 0,
            epoch: 0,
            last_ms: 0,
            lag: [(0.0, 0.0); MAX_LAG_HOPS],
            lag_hops,
            lag_pos: 0,
        })
    }

    /// Publish one hop: `input` is the raw block the engine just consumed,
    /// `output` the block it just produced. Wait-free: no allocation, no
    /// lock, one clock read.
    pub fn publish(&mut self, input: &[f32], output: &[f32], mode: u32) {
        self.publish_at(input, output, mode, crate::monotonic_ms());
    }

    /// [`MeterWriter::publish`] at a given CLOCK_MONOTONIC time (tests and
    /// tools that must not depend on the scheduler).
    pub fn publish_at(&mut self, input: &[f32], output: &[f32], mode: u32, now: u64) {
        if self.last_ms != 0 && now.saturating_sub(self.last_ms) > RUN_GAP_MS {
            // The engine stood still in between: a new run.
            self.epoch = self.epoch.wrapping_add(1);
            self.region
                .u32(OFF_EPOCH)
                .store(self.epoch, Ordering::Release);
        }
        self.last_ms = now;
        let (in_peak, in_ms) = self.delay(peak_ms(input));
        let (out_peak, out_ms) = peak_ms(output);
        let i = self.hops;
        let base = V2.slot(i);
        let r = &self.region;
        r.u64(base + S_SEQ).store(0, Ordering::Relaxed);
        fence(Ordering::Release);
        r.u32(base + S_IN_PEAK)
            .store(in_peak.to_bits(), Ordering::Relaxed);
        r.u32(base + S_OUT_PEAK)
            .store(out_peak.to_bits(), Ordering::Relaxed);
        r.u32(base + S_IN_MS)
            .store(in_ms.to_bits(), Ordering::Relaxed);
        r.u32(base + S_OUT_MS)
            .store(out_ms.to_bits(), Ordering::Relaxed);
        r.u32(base + S_MODE).store(mode, Ordering::Relaxed);
        r.u32(base + S_EPOCH).store(self.epoch, Ordering::Relaxed);
        r.u64(base + S_SEQ).store(i + 1, Ordering::Release);
        self.hops = i + 1;
        r.u64(OFF_HOPS).store(self.hops, Ordering::Release);
        r.u64(OFF_LAST_MS).store(now, Ordering::Relaxed);
    }

    /// The engine was reset: the stream restarts, and the delayed input
    /// side starts over from silence (as the engine itself does).
    pub fn discontinuity(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.lag = [(0.0, 0.0); MAX_LAG_HOPS];
        self.lag_pos = 0;
        self.region
            .u32(OFF_EPOCH)
            .store(self.epoch, Ordering::Release);
    }

    pub fn hops(&self) -> u64 {
        self.hops
    }

    fn delay(&mut self, now: (f32, f32)) -> (f32, f32) {
        if self.lag_hops == 0 {
            return now;
        }
        let old = self.lag[self.lag_pos];
        self.lag[self.lag_pos] = now;
        self.lag_pos = (self.lag_pos + 1) % self.lag_hops;
        old
    }
}

/// One published hop as a reader sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hop {
    pub index: u64,
    pub in_peak: f32,
    pub out_peak: f32,
    pub in_ms: f32,
    pub out_ms: f32,
    pub mode: u32,
    pub epoch: u32,
}

/// What one [`MeterReader::read`] call ran into besides the hops it returned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadReport {
    /// Hops published but not returned: overwritten before the reader got
    /// to them (the reader stalled for longer than the ring holds).
    pub lost: u64,
    /// A new plugin instance took the file over; reading restarted at its
    /// newest hop.
    pub restarted: bool,
}

/// The app side: a read-only mapping.
pub struct MeterReader {
    region: Region,
    layout: Layout,
    instance: u64,
    next: u64,
}

impl MeterReader {
    /// Map the meter at `path` read-only (layout v2, or v1: see the module
    /// docs). Fails on a missing, foreign or malformed file (wrong magic,
    /// version or geometry): such a file is never trusted. Reading starts
    /// at the newest hop.
    pub fn open(path: &Path) -> io::Result<MeterReader> {
        let region = Region::open_any(path, &[V2.size, V1.size], false)?;
        let layout = if region.len() == V2.size { V2 } else { V1 };
        validate(&region, layout)?;
        let instance = region.u64(OFF_INSTANCE).load(Ordering::Acquire);
        let next = region.u64(OFF_HOPS).load(Ordering::Acquire);
        Ok(MeterReader {
            region,
            layout,
            instance,
            next,
        })
    }

    /// The file's layout version.
    pub fn version(&self) -> u32 {
        self.layout.version
    }

    /// The file's ring length in hops.
    pub fn slots(&self) -> u64 {
        self.layout.slots
    }

    /// Start the next [`MeterReader::read`] up to `max` hops before the
    /// newest: no further back than the ring holds (less the slots the
    /// writer overwrites next) and than the newest run (one epoch: no
    /// engine reset, no idle gap in between). Returns how many hops.
    pub fn rewind(&mut self, max: u64) -> u64 {
        self.instance = self.region.u64(OFF_INSTANCE).load(Ordering::Acquire);
        let head = self.region.u64(OFF_HOPS).load(Ordering::Acquire);
        let limit = max
            .min(self.layout.slots.saturating_sub(REWIND_MARGIN))
            .min(head);
        let mut n = 0;
        let mut epoch = None;
        while n < limit {
            match self.slot(head - 1 - n) {
                Some(h) if epoch.is_none_or(|e| e == h.epoch) => {
                    epoch = Some(h.epoch);
                    n += 1;
                }
                _ => break,
            }
        }
        self.next = head - n;
        n
    }

    pub fn instance(&self) -> u64 {
        self.instance
    }

    /// The writer's run as of its newest hop: its instance and epoch, read
    /// now (not where this reader stands). A change is a new run: another
    /// instance, an engine reset or an idle gap.
    pub fn run(&self) -> (u64, u32) {
        (
            self.region.u64(OFF_INSTANCE).load(Ordering::Acquire),
            self.region.u32(OFF_EPOCH).load(Ordering::Acquire),
        )
    }

    /// Hops the writer has published so far.
    pub fn published(&self) -> u64 {
        self.region.u64(OFF_HOPS).load(Ordering::Acquire)
    }

    /// When the newest hop was published (CLOCK_MONOTONIC ms, compare with
    /// [`crate::monotonic_ms`]); 0 before the first.
    pub fn last_publish_ms(&self) -> u64 {
        self.region.u64(OFF_LAST_MS).load(Ordering::Relaxed)
    }

    /// The input side's delay in hops, as the writer declared it.
    pub fn lag_hops(&self) -> u32 {
        self.region.u32(OFF_LAG).load(Ordering::Relaxed)
    }

    /// The writer is (re)initialising or the file was invalidated.
    pub fn valid(&self) -> bool {
        self.region.u64(OFF_MAGIC).load(Ordering::Acquire) == MAGIC
    }

    /// Append every hop published since the last call to `out`, oldest
    /// first. Never blocks; allocates only if `out` must grow.
    pub fn read(&mut self, out: &mut Vec<Hop>) -> ReadReport {
        let mut report = ReadReport::default();
        let instance = self.region.u64(OFF_INSTANCE).load(Ordering::Acquire);
        let head = self.region.u64(OFF_HOPS).load(Ordering::Acquire);
        if instance != self.instance {
            self.instance = instance;
            self.next = head;
            report.restarted = true;
            return report;
        }
        if head < self.next {
            // Re-initialised under the same instance id: start over.
            self.next = head;
            report.restarted = true;
            return report;
        }
        let slots = self.layout.slots;
        if head - self.next > slots {
            report.lost += head - self.next - slots;
            self.next = head - slots;
        }
        for i in self.next..head {
            match self.slot(i) {
                Some(h) => out.push(h),
                None => report.lost += 1,
            }
        }
        self.next = head;
        report
    }

    fn slot(&self, i: u64) -> Option<Hop> {
        let base = self.layout.slot(i);
        let r = &self.region;
        let seq = r.u64(base + S_SEQ);
        if seq.load(Ordering::Acquire) != i.wrapping_add(1) {
            return None;
        }
        let f = |off| f32::from_bits(r.u32(base + off).load(Ordering::Relaxed));
        let hop = Hop {
            index: i,
            in_peak: f(S_IN_PEAK),
            out_peak: f(S_OUT_PEAK),
            in_ms: f(S_IN_MS),
            out_ms: f(S_OUT_MS),
            mode: r.u32(base + S_MODE).load(Ordering::Relaxed),
            epoch: r.u32(base + S_EPOCH).load(Ordering::Relaxed),
        };
        fence(Ordering::Acquire);
        (seq.load(Ordering::Relaxed) == i.wrapping_add(1)).then_some(hop)
    }
}

fn validate(r: &Region, layout: Layout) -> io::Result<()> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, format!("meter: {what}"));
    if r.u64(OFF_MAGIC).load(Ordering::Acquire) != MAGIC {
        return Err(bad("bad magic"));
    }
    if r.u32(OFF_VERSION).load(Ordering::Relaxed) != layout.version {
        return Err(bad("unknown version"));
    }
    if r.u32(OFF_SLOTS).load(Ordering::Relaxed) as u64 != layout.slots
        || r.u32(OFF_SLOT_BYTES).load(Ordering::Relaxed) != SLOT_BYTES as u32
        || r.u32(OFF_HOP_FRAMES).load(Ordering::Relaxed) != HOP_FRAMES
        || r.u32(OFF_RATE).load(Ordering::Relaxed) != RATE
        || r.u32(OFF_LAG).load(Ordering::Relaxed) as usize > MAX_LAG_HOPS
    {
        return Err(bad("unexpected geometry"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::testdir;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::Arc;

    fn hop_of(v: f32) -> [f32; 480] {
        let mut h = [0f32; 480];
        h[17] = v;
        h[18] = -v;
        h
    }

    #[test]
    fn hops_round_trip_with_the_input_delayed_by_the_lag() {
        let d = testdir::new("meter-rt");
        let p = d.join("meter");
        let mut w = MeterWriter::create(&p, 42, 5).unwrap();
        let mut r = MeterReader::open(&p).unwrap();
        assert_eq!((r.instance(), r.lag_hops()), (42, 5));
        for i in 0..12 {
            w.publish(
                &hop_of(i as f32 / 100.0),
                &hop_of(i as f32 / 200.0),
                MODE_BYPASS,
            );
        }
        let mut out = Vec::new();
        assert_eq!(r.read(&mut out), ReadReport::default());
        assert_eq!(out.len(), 12);
        for (i, h) in out.iter().enumerate() {
            assert_eq!(h.index, i as u64);
            assert_eq!(h.out_peak, i as f32 / 200.0);
            let want_in = if i < 5 { 0.0 } else { (i - 5) as f32 / 100.0 };
            assert_eq!(h.in_peak, want_in, "hop {i}: in is 5 hops behind");
            assert!((h.out_ms - 2.0 * h.out_peak * h.out_peak / 480.0).abs() < 1e-9);
            assert_eq!(h.mode, MODE_BYPASS);
        }
        out.clear();
        assert_eq!(r.read(&mut out), ReadReport::default());
        assert!(out.is_empty(), "nothing new");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_stalled_reader_loses_the_overwritten_hops_only() {
        let d = testdir::new("meter-wrap");
        let p = d.join("meter");
        let mut w = MeterWriter::create(&p, 1, 0).unwrap();
        let mut r = MeterReader::open(&p).unwrap();
        for i in 0..(SLOTS as u64 * 3 + 7) {
            w.publish(&hop_of(i as f32), &hop_of(i as f32), MODE_PROCESS);
        }
        let mut out = Vec::new();
        let rep = r.read(&mut out);
        assert_eq!(rep.lost, SLOTS as u64 * 2 + 7);
        assert_eq!(out.len(), SLOTS);
        assert_eq!(out[0].index, SLOTS as u64 * 2 + 7);
        assert!(out.iter().all(|h| h.in_peak == h.index as f32));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_new_instance_restarts_the_reader_and_epochs_mark_resets() {
        let d = testdir::new("meter-inst");
        let p = d.join("meter");
        let mut w = MeterWriter::create(&p, 1, 2).unwrap();
        let mut r = MeterReader::open(&p).unwrap();
        w.publish(&hop_of(0.5), &hop_of(0.5), MODE_PROCESS);
        w.publish(&hop_of(0.5), &hop_of(0.5), MODE_PROCESS);
        w.discontinuity();
        w.publish(&hop_of(0.25), &hop_of(0.25), MODE_MUTE);
        let mut out = Vec::new();
        r.read(&mut out);
        assert_eq!(out.len(), 3);
        assert_eq!((out[1].epoch, out[2].epoch), (0, 1));
        assert_eq!(r.run(), (1, 1));
        assert_eq!(
            out[2].in_peak, 0.0,
            "the delayed input restarts from silence"
        );
        drop(w);
        let mut w2 = MeterWriter::create(&p, 2, 2).unwrap();
        w2.publish(&hop_of(0.1), &hop_of(0.1), MODE_PROCESS);
        out.clear();
        let rep = r.read(&mut out);
        assert!(rep.restarted && out.is_empty());
        assert_eq!(r.instance(), 2);
        assert_eq!(r.run(), (2, 0));
        w2.publish(&hop_of(0.2), &hop_of(0.2), MODE_PROCESS);
        r.read(&mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].out_peak, 0.2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn malformed_files_are_never_trusted() {
        let d = testdir::new("meter-bad");
        let p = d.join("meter");
        let w = MeterWriter::create(&p, 1, 5).unwrap();
        drop(w);
        let poke = |off: usize, val: u32| {
            let r = Region::open(&p, SIZE, true).unwrap();
            let old = r.u32(off).swap(val, Ordering::Relaxed);
            assert!(MeterReader::open(&p).is_err(), "offset {off} = {val}");
            r.u32(off).store(old, Ordering::Relaxed);
            assert!(MeterReader::open(&p).is_ok());
        };
        poke(OFF_MAGIC, 0);
        poke(OFF_VERSION, 1);
        poke(OFF_VERSION, 3);
        poke(OFF_SLOTS, 64);
        poke(OFF_SLOT_BYTES, 16);
        poke(OFF_HOP_FRAMES, 512);
        poke(OFF_RATE, 44_100);
        poke(OFF_LAG, 99);
        // Wrong size.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&p)
            .unwrap()
            .set_len(2 * SIZE as u64)
            .unwrap();
        assert!(MeterReader::open(&p).is_err());
        assert!(MeterReader::open(&d.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rewind_returns_the_newest_run_within_the_ring() {
        let d = testdir::new("meter-rewind");
        let p = d.join("meter");
        let mut w = MeterWriter::create(&p, 3, 0).unwrap();
        let mut r = MeterReader::open(&p).unwrap();
        assert_eq!((r.version(), r.slots()), (VERSION, SLOTS as u64));
        assert_eq!(r.rewind(100), 0, "nothing published yet");
        // On a clock of its own: 10 ms per hop, never a gap.
        let mut t = 1_000_000u64;
        let mut publish = |w: &mut MeterWriter, v: f32| {
            t += 10;
            w.publish_at(&hop_of(v), &hop_of(v), MODE_PROCESS, t);
        };
        for i in 0..50 {
            publish(&mut w, i as f32);
        }
        w.discontinuity();
        for i in 50..80 {
            publish(&mut w, i as f32);
        }
        // A reader that opens now starts at the newest hop...
        let mut r = MeterReader::open(&p).unwrap();
        let mut out = Vec::new();
        r.read(&mut out);
        assert!(out.is_empty());
        // ...and can go back over the newest run, not across the reset.
        assert_eq!(r.rewind(1_000), 30);
        assert_eq!(r.read(&mut out), ReadReport::default());
        assert_eq!(out.len(), 30);
        assert_eq!(out[0].index, 50);
        assert!(out.iter().all(|h| h.epoch == 1));
        out.clear();
        assert_eq!(r.rewind(10), 10);
        r.read(&mut out);
        assert_eq!(out.first().map(|h| h.index), Some(70));
        // Bounded by the ring, less the slots the writer takes next.
        for i in 0..(SLOTS as u64 + 100) {
            publish(&mut w, i as f32);
        }
        let n = r.rewind(u64::MAX);
        assert_eq!(n, SLOTS as u64 - REWIND_MARGIN);
        out.clear();
        let rep = r.read(&mut out);
        assert_eq!((rep.lost, out.len() as u64), (0, n));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_idle_gap_starts_a_new_run() {
        let d = testdir::new("meter-gap");
        let p = d.join("meter");
        let mut w = MeterWriter::create(&p, 3, 0).unwrap();
        let mut t = 1_000_000u64;
        for _ in 0..5 {
            t += 10;
            w.publish_at(&hop_of(0.1), &hop_of(0.1), MODE_PROCESS, t);
        }
        // A pause of exactly the gap is still one run; a ms more is a new one.
        t += RUN_GAP_MS;
        w.publish_at(&hop_of(0.1), &hop_of(0.1), MODE_PROCESS, t);
        t += RUN_GAP_MS + 1;
        for _ in 0..7 {
            w.publish_at(&hop_of(0.2), &hop_of(0.2), MODE_PROCESS, t);
            t += 10;
        }
        let mut r = MeterReader::open(&p).unwrap();
        assert_eq!(r.rewind(100), 7, "the call before the pause is another run");
        let mut out = Vec::new();
        r.read(&mut out);
        assert!(out.iter().all(|h| h.epoch == 1 && h.out_peak == 0.2));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A v1 file, as a plugin from before the ring grew writes it.
    fn write_v1(p: &Path, hops: u64) {
        let r = Region::create_new(p, PAGE).unwrap();
        r.u32(OFF_VERSION).store(1, Ordering::Relaxed);
        r.u32(OFF_SLOTS).store(120, Ordering::Relaxed);
        r.u32(OFF_SLOT_BYTES).store(32, Ordering::Relaxed);
        r.u32(OFF_HOP_FRAMES).store(HOP_FRAMES, Ordering::Relaxed);
        r.u32(OFF_RATE).store(RATE, Ordering::Relaxed);
        r.u32(OFF_LAG).store(5, Ordering::Relaxed);
        r.u64(OFF_INSTANCE).store(77, Ordering::Relaxed);
        for i in 0..hops {
            let base = V1.slot(i);
            r.u32(base + S_OUT_PEAK)
                .store((i as f32).to_bits(), Ordering::Relaxed);
            r.u64(base + S_SEQ).store(i + 1, Ordering::Relaxed);
        }
        r.u64(OFF_HOPS).store(hops, Ordering::Relaxed);
        r.u64(OFF_MAGIC).store(MAGIC, Ordering::Release);
    }

    #[test]
    fn a_first_layout_file_is_still_read() {
        let d = testdir::new("meter-v1");
        let p = d.join("meter");
        write_v1(&p, 300);
        let mut r = MeterReader::open(&p).unwrap();
        assert_eq!((r.version(), r.slots(), r.instance()), (1, 120, 77));
        assert_eq!(r.rewind(1_000), 120 - REWIND_MARGIN);
        let mut out = Vec::new();
        assert_eq!(r.read(&mut out), ReadReport::default());
        assert_eq!(out.len() as u64, 120 - REWIND_MARGIN);
        assert!(out.iter().all(|h| h.out_peak == h.index as f32));
        assert_eq!(out.last().unwrap().index, 299);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_writer_replaces_a_file_of_another_size_instead_of_resizing_it() {
        let d = testdir::new("meter-replace");
        let p = d.join("meter");
        write_v1(&p, 10);
        let old = MeterReader::open(&p).unwrap();
        let mut w = MeterWriter::create(&p, 5, 0).unwrap();
        // The old mapping is a file of its own now, whole and readable.
        assert!(!old.region.is_at(&p));
        assert_eq!(old.published(), 10);
        assert_eq!(old.region.u32(PAGE - 4).load(Ordering::Relaxed), 0);
        w.publish(&hop_of(0.5), &hop_of(0.5), MODE_PROCESS);
        let r = MeterReader::open(&p).unwrap();
        assert_eq!((r.version(), r.published()), (VERSION, 1));
        // The same size is taken over in place (a new instance, same file).
        let inode = |p: &Path| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(p).unwrap().ino()
        };
        let before = inode(&p);
        drop(w);
        MeterWriter::create(&p, 6, 0).unwrap();
        assert_eq!(inode(&p), before);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn peak_and_mean_square_ignore_non_finite_samples() {
        assert_eq!(
            peak_ms(&[0.5, -1.0, f32::NAN, f32::INFINITY]),
            (1.0, 1.25 / 4.0)
        );
        assert_eq!(peak_ms(&[]), (0.0, 0.0));
    }

    /// Writer and reader on two threads at full speed: every hop the
    /// reader accepts is exactly what was written for its index, in order,
    /// and nothing is silently skipped (accepted + lost covers the stream).
    #[test]
    fn reader_writer_stress_never_returns_a_torn_hop() {
        let d = testdir::new("meter-stress");
        let p = d.join("meter");
        let mut w = MeterWriter::create(&p, 9, 0).unwrap();
        let mut r = MeterReader::open(&p).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let written = Arc::new(AtomicU64::new(0));
        let (stop_w, written_w) = (Arc::clone(&stop), Arc::clone(&written));
        let value = |i: u64| (i % 100_003) as f32;
        let writer = std::thread::spawn(move || {
            let mut a = [0f32; 480];
            let mut b = [0f32; 480];
            let mut i = 0u64;
            while !stop_w.load(Ordering::Relaxed) {
                // Distinct values per field so a mix of two hops shows.
                a[0] = value(i);
                b[0] = value(i) + 0.5;
                w.publish(&a, &b, (i % 3) as u32);
                i += 1;
                written_w.store(i, Ordering::Release);
            }
        });
        let mut out = Vec::with_capacity(4096);
        let (mut accepted, mut lost, mut last) = (0u64, 0u64, None::<u64>);
        // Until the writer is well past several rings, counted in hops,
        // not a clock: a throttled runner writes slowly, not less.
        let started = std::time::Instant::now();
        let mut spin = 0u32;
        while written.load(Ordering::Acquire) < 5 * SLOTS as u64 {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(120),
                "the writer stalled"
            );
            out.clear();
            let rep = r.read(&mut out);
            assert!(!rep.restarted);
            lost += rep.lost;
            for h in &out {
                if let Some(l) = last {
                    assert!(h.index > l, "out of order: {} after {l}", h.index);
                }
                last = Some(h.index);
                assert_eq!(h.in_peak, value(h.index), "torn hop {}", h.index);
                assert_eq!(h.out_peak, value(h.index) + 0.5, "torn hop {}", h.index);
                assert_eq!(h.in_ms, h.in_peak * h.in_peak / 480.0);
                assert_eq!(h.mode, (h.index % 3) as u32);
            }
            accepted += out.len() as u64;
            spin = spin.wrapping_add(1);
            // Mix fast polls with stalls longer than the ring so both the
            // wrap and the in-flight overwrite paths run.
            if spin.is_multiple_of(64) {
                std::thread::sleep(std::time::Duration::from_millis(3));
            }
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        let total = written.load(Ordering::Acquire);
        out.clear();
        let rep = r.read(&mut out);
        accepted += out.len() as u64;
        lost += rep.lost;
        assert_eq!(accepted + lost, total, "every hop accepted or counted lost");
        assert!(
            accepted > 1000,
            "the reader kept up at all ({accepted} of {total})"
        );
        // Whether a stall overran the ring depends on the scheduler (a
        // throttled CI pod freezes the writer too); the wrap itself is pinned
        // by a_stalled_reader_loses_the_overwritten_hops_only.
        let _ = lost;
        let _ = std::fs::remove_dir_all(&d);
    }
}
