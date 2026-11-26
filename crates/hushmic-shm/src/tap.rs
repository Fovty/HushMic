//! The engine tap for Mic check: the raw and the clean audio of the same
//! engine hops, in one ring that exists only while the Mic check page is
//! visible.
//!
//! Who does what:
//! 1. The window creates the ring file next to the meter ([`request_path`]),
//!    sized and with its header written, by an atomic rename so the plugin
//!    never sees a half-written request ([`TapRing::request`]). It writes a
//!    heartbeat every [`HEARTBEAT_EVERY_MS`].
//! 2. The plugin's helper thread finds it within [`POLL_MS`], maps it
//!    ([`TapRing::attach`]), faults and locks the pages ([`TapRing::pin`],
//!    after it let go of a ring it replaces), removes the file's
//!    name (the ring then has no name on any filesystem, which the helper
//!    checks), marks it mapped and hands it to the DSP worker, which copies
//!    every hop into it ([`TapRing::write_hop`]): no syscall, no
//!    allocation, no lock. A new request while a ring is mapped (a window
//!    re-created after a crash) replaces it at once. A request the plugin
//!    refuses (stale, closed, malformed) loses its name too.
//! 3. The window closes it ([`TapRing::close`]), or stops its heartbeat
//!    (hidden, crashed). The helper sees either within [`POLL_MS`] or a
//!    heartbeat older than [`HEARTBEAT_TIMEOUT_MS`], takes the ring back
//!    from the worker and unmaps it. The memory is gone once both sides
//!    have unmapped.
//!
//! Layout v1: one header page, then `capacity` frames of two f32 channels,
//! interleaved `[raw, clean]`. Clean frame `n` was made from raw frame
//! `n - clean_lag` (the engine's fixed latency): the plugin writes both
//! sides of a hop at the same position and the reader applies the shift.
//!
//! | Offset | Field | Written by | Meaning |
//! |---|---|---|---|
//! | 0 | magic u64 | window | `HMTAP\0\0\0`, written last |
//! | 8 | version u32 | window | 1 |
//! | 12 | channels u32 | window | 2 |
//! | 16 | capacity u64 | window | [`CAPACITY`] frames |
//! | 24 | rate u32 | window | 48000 |
//! | 28 | clean_lag u32 | plugin | frames the clean side trails the raw side |
//! | 32 | instance u64 | plugin | the plugin instance writing |
//! | 40 | mapped u32 | plugin | 0 waiting, 1 mapped, 2 released, 3 replaced by a newer request |
//! | 44 | closed u32 | window | 1: release it |
//! | 48 | write_frames u64 | plugin | frames written so far |
//! | 56 | write_begin u64 | plugin | frames being written up to (torn-read check) |
//! | 64 | epoch u32 | plugin | +1 on every engine reset |
//! | 72 | epoch_frames u64 | plugin | `write_frames` at the last reset |
//! | 80 | heartbeat u64 | window | `CLOCK_MONOTONIC` ms |
//!
//! The plugin keeps its write position and epoch to itself ([`TapCursor`])
//! and only stores them here: nothing the window writes into the page can
//! move the plugin's writes.

use crate::region::{monotonic_ms, Region, PAGE};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, Ordering};

pub const MAGIC: u64 = u64::from_le_bytes(*b"HMTAP\0\0\0");
pub const VERSION: u32 = 1;
pub const CHANNELS: u32 = 2;
/// 2^19 frames: 10.9 s at 48 kHz, so a 10 s window can be copied while
/// the writer keeps going.
pub const CAPACITY: u64 = 1 << 19;
pub const RATE: u32 = 48_000;
pub const SIZE: usize = PAGE + CAPACITY as usize * CHANNELS as usize * 4;

/// The helper's look at the request file and the heartbeat.
pub const POLL_MS: u64 = 250;
/// The window refreshes its heartbeat this often while the page is visible.
pub const HEARTBEAT_EVERY_MS: u64 = 250;
/// A heartbeat older than this releases the ring (window hidden, frozen
/// or gone): with the last beat and one poll, the ring is gone within 2 s.
pub const HEARTBEAT_TIMEOUT_MS: u64 = 1500;

pub const MAPPED_WAITING: u32 = 0;
pub const MAPPED_YES: u32 = 1;
pub const MAPPED_RELEASED: u32 = 2;
pub const MAPPED_REPLACED: u32 = 3;

const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 8;
const OFF_CHANNELS: usize = 12;
const OFF_CAPACITY: usize = 16;
const OFF_RATE: usize = 24;
const OFF_LAG: usize = 28;
const OFF_INSTANCE: usize = 32;
const OFF_MAPPED: usize = 40;
const OFF_CLOSED: usize = 44;
const OFF_WRITE: usize = 48;
const OFF_BEGIN: usize = 56;
const OFF_EPOCH: usize = 64;
const OFF_EPOCH_FRAMES: usize = 72;
const OFF_HEARTBEAT: usize = 80;

const MASK: u64 = CAPACITY - 1;

/// Where the window asks for a tap of the chain whose meter is `meter`.
pub fn request_path(meter: &Path) -> PathBuf {
    let mut s = OsString::from(meter.as_os_str());
    s.push(".tap");
    PathBuf::from(s)
}

/// The ring as both sides see it.
pub struct TapRing {
    region: Region,
}

/// The plugin's write position in one ring (see the module docs). Start a
/// fresh one for every ring mapped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TapCursor {
    frames: u64,
    epoch: u32,
}

impl TapCursor {
    pub fn frames(&self) -> u64 {
        self.frames
    }
}

/// Where the plugin is with a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapState {
    /// No plugin has mapped it (yet).
    Waiting,
    /// The plugin instance `instance` writes into it.
    Mapped { instance: u64, clean_lag: u32 },
    /// The plugin let go of it (closed, stale heartbeat, chain stopped).
    Released,
    /// The plugin let go of it for a newer request: another window asked
    /// (asking again would take it back and forth).
    Replaced,
}

/// The write position and the last discontinuity, as one reader snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    pub write_frames: u64,
    pub epoch: u32,
    /// First clean frame index that has its raw partner in this epoch.
    pub epoch_start: u64,
    pub clean_lag: u32,
}

/// What [`TapRing::read_pairs`] appended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pairs {
    /// Clean frame index of the first pair appended (raw index is
    /// `start - clean_lag`).
    pub start: u64,
    pub len: usize,
    /// The epoch every appended pair belongs to.
    pub epoch: u32,
    pub head: u64,
}

impl TapRing {
    /// Window side: publish a tap request for the chain whose meter file is
    /// `meter` (see the module docs).
    pub fn request(meter: &Path) -> io::Result<TapRing> {
        use std::sync::atomic::AtomicU32;
        static N: AtomicU32 = AtomicU32::new(0);
        let final_path = request_path(meter);
        let mut tmp = OsString::from(final_path.as_os_str());
        tmp.push(format!(
            "-new-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp = PathBuf::from(tmp);
        let _ = std::fs::remove_file(&tmp);
        let region = Region::create_new(&tmp, SIZE)?;
        let r = &region;
        r.u32(OFF_VERSION).store(VERSION, Ordering::Relaxed);
        r.u32(OFF_CHANNELS).store(CHANNELS, Ordering::Relaxed);
        r.u64(OFF_CAPACITY).store(CAPACITY, Ordering::Relaxed);
        r.u32(OFF_RATE).store(RATE, Ordering::Relaxed);
        r.u64(OFF_HEARTBEAT)
            .store(monotonic_ms(), Ordering::Relaxed);
        r.u64(OFF_MAGIC).store(MAGIC, Ordering::Release);
        if let Err(e) = std::fs::rename(&tmp, &final_path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(TapRing { region })
    }

    /// Plugin side: map a request at `path` read-write. Refuses anything
    /// that is not a fresh, well-formed, private request with one name:
    /// such a file is never handed to the worker. A refused request that
    /// is private to this user is removed (nobody will map it). Call
    /// [`TapRing::pin`] before the worker writes.
    pub fn attach(path: &Path) -> io::Result<TapRing> {
        let region = Region::open(path, SIZE, true)?;
        let ring = TapRing { region };
        let checked = ring.check_request();
        if checked.is_err() {
            ring.unlink_if_at(path);
        }
        checked?;
        Ok(ring)
    }

    /// Plugin side: fault every page in, so the worker's writes never take
    /// a page fault, and keep them in RAM where the memory-lock limit
    /// allows. `false`: the lock was refused (the ring works, unlocked).
    pub fn pin(&self) -> bool {
        self.region.prefault();
        self.region.lock()
    }

    fn check_request(&self) -> io::Result<()> {
        self.validate()?;
        let r = &self.region;
        if r.u32(OFF_MAPPED).load(Ordering::Acquire) != MAPPED_WAITING {
            return Err(invalid("already taken"));
        }
        if self.wants_release(monotonic_ms()) {
            return Err(invalid("stale or closed"));
        }
        if self.region.links()? != 1 {
            return Err(invalid("more than one name"));
        }
        Ok(())
    }

    /// Plugin side, after removing the request's name: no other name
    /// keeps the ring on any filesystem.
    pub fn nameless(&self) -> bool {
        self.region.links().is_ok_and(|n| n == 0)
    }

    fn validate(&self) -> io::Result<()> {
        let r = &self.region;
        if r.u64(OFF_MAGIC).load(Ordering::Acquire) != MAGIC {
            return Err(invalid("bad magic"));
        }
        if r.u32(OFF_VERSION).load(Ordering::Relaxed) != VERSION {
            return Err(invalid("unknown version"));
        }
        if r.u32(OFF_CHANNELS).load(Ordering::Relaxed) != CHANNELS
            || r.u64(OFF_CAPACITY).load(Ordering::Relaxed) != CAPACITY
            || r.u32(OFF_RATE).load(Ordering::Relaxed) != RATE
        {
            return Err(invalid("unexpected geometry"));
        }
        Ok(())
    }

    /// Plugin side: the worker may write from now on. `clean_lag` is the
    /// engine latency in frames.
    pub fn mark_mapped(&self, instance: u64, clean_lag: u32) {
        let r = &self.region;
        r.u32(OFF_LAG).store(clean_lag, Ordering::Relaxed);
        r.u64(OFF_INSTANCE).store(instance, Ordering::Relaxed);
        r.u64(OFF_WRITE).store(0, Ordering::Relaxed);
        r.u64(OFF_BEGIN).store(0, Ordering::Relaxed);
        r.u64(OFF_EPOCH_FRAMES).store(0, Ordering::Relaxed);
        r.u32(OFF_EPOCH).store(0, Ordering::Relaxed);
        r.u32(OFF_MAPPED).store(MAPPED_YES, Ordering::Release);
    }

    /// Plugin side: the worker no longer writes and the plugin unmaps.
    pub fn mark_released(&self) {
        self.region
            .u32(OFF_MAPPED)
            .store(MAPPED_RELEASED, Ordering::Release);
    }

    /// Plugin side: as [`TapRing::mark_released`], for a newer request.
    pub fn mark_replaced(&self) {
        self.region
            .u32(OFF_MAPPED)
            .store(MAPPED_REPLACED, Ordering::Release);
    }

    /// Plugin side: the window closed the tap or stopped its heartbeat.
    pub fn wants_release(&self, now_ms: u64) -> bool {
        let r = &self.region;
        r.u32(OFF_CLOSED).load(Ordering::Acquire) != 0
            || now_ms.saturating_sub(r.u64(OFF_HEARTBEAT).load(Ordering::Acquire))
                > HEARTBEAT_TIMEOUT_MS
    }

    /// Worker side: append one hop of raw input and the clean output made
    /// in the same engine call at `cur` (this ring's cursor). Wait-free:
    /// atomic stores into memory that is already faulted in.
    pub fn write_hop(&self, cur: &mut TapCursor, raw: &[f32], clean: &[f32]) {
        let n = raw.len().min(clean.len());
        let r = &self.region;
        let w = cur.frames;
        let end = w.wrapping_add(n as u64);
        r.u64(OFF_BEGIN).store(end, Ordering::Relaxed);
        fence(Ordering::Release);
        let data = self.data();
        for k in 0..n {
            let i = (w.wrapping_add(k as u64) & MASK) as usize * 2;
            data[i].store(raw[k].to_bits(), Ordering::Relaxed);
            data[i + 1].store(clean[k].to_bits(), Ordering::Relaxed);
        }
        cur.frames = end;
        r.u64(OFF_WRITE).store(end, Ordering::Release);
    }

    /// Worker side: the engine was reset; what follows cannot be paired
    /// with what came before.
    pub fn discontinuity(&self, cur: &mut TapCursor) {
        let r = &self.region;
        r.u64(OFF_EPOCH_FRAMES).store(cur.frames, Ordering::Relaxed);
        cur.epoch = cur.epoch.wrapping_add(1);
        r.u32(OFF_EPOCH).store(cur.epoch, Ordering::Release);
    }

    fn data(&self) -> &[std::sync::atomic::AtomicU32] {
        self.region
            .u32s(PAGE, CAPACITY as usize * CHANNELS as usize)
    }

    /// Window side: still looking.
    pub fn heartbeat(&self) {
        self.region
            .u64(OFF_HEARTBEAT)
            .store(monotonic_ms(), Ordering::Release);
    }

    /// Window side: ask the plugin to let go now.
    pub fn close(&self) {
        self.region.u32(OFF_CLOSED).store(1, Ordering::Release);
    }

    pub fn state(&self) -> TapState {
        let r = &self.region;
        match r.u32(OFF_MAPPED).load(Ordering::Acquire) {
            MAPPED_WAITING => TapState::Waiting,
            MAPPED_YES => TapState::Mapped {
                instance: r.u64(OFF_INSTANCE).load(Ordering::Relaxed),
                clean_lag: r.u32(OFF_LAG).load(Ordering::Relaxed),
            },
            MAPPED_REPLACED => TapState::Replaced,
            _ => TapState::Released,
        }
    }

    pub fn head(&self) -> Head {
        let r = &self.region;
        let write_frames = r.u64(OFF_WRITE).load(Ordering::Acquire);
        let epoch = r.u32(OFF_EPOCH).load(Ordering::Acquire);
        let clean_lag = r.u32(OFF_LAG).load(Ordering::Relaxed);
        Head {
            write_frames,
            epoch,
            epoch_start: r
                .u64(OFF_EPOCH_FRAMES)
                .load(Ordering::Relaxed)
                .saturating_add(clean_lag as u64),
            clean_lag,
        }
    }

    /// Window side: append up to `max` aligned pairs, starting at clean
    /// frame `from` (or later, where older frames are gone or belong to an
    /// earlier epoch), to `raw` and `clean`. Pairs the writer overwrote
    /// during the copy are dropped from the front, never returned torn.
    pub fn read_pairs(
        &self,
        from: u64,
        max: usize,
        raw: &mut Vec<f32>,
        clean: &mut Vec<f32>,
    ) -> Pairs {
        let head = self.head();
        let lag = head.clean_lag as u64;
        // Page values are the plugin's; saturating maths keeps a broken
        // one from overflowing (it can only make the read empty).
        let oldest = head
            .write_frames
            .saturating_sub(CAPACITY)
            .saturating_add(lag);
        let lo = from.max(head.epoch_start).max(oldest);
        let hi = head.write_frames.min(lo.saturating_add(max as u64));
        let (raw_at, clean_at) = (raw.len(), clean.len());
        if hi <= lo {
            return Pairs {
                start: lo,
                len: 0,
                epoch: head.epoch,
                head: head.write_frames,
            };
        }
        let data = self.data();
        for n in lo..hi {
            let ci = ((n & MASK) as usize) * 2 + 1;
            let ri = ((n.wrapping_sub(lag) & MASK) as usize) * 2;
            raw.push(f32::from_bits(data[ri].load(Ordering::Relaxed)));
            clean.push(f32::from_bits(data[ci].load(Ordering::Relaxed)));
        }
        fence(Ordering::Acquire);
        let begin = self.region.u64(OFF_BEGIN).load(Ordering::Relaxed);
        // Raw frame `n - lag` is the older of a pair: it is clobbered once
        // the writer reaches `n - lag + CAPACITY`.
        let valid = begin.saturating_sub(CAPACITY).saturating_add(lag);
        let drop = valid.saturating_sub(lo).min(hi - lo) as usize;
        if drop > 0 {
            raw.drain(raw_at..raw_at + drop);
            clean.drain(clean_at..clean_at + drop);
        }
        Pairs {
            start: lo + drop as u64,
            len: (hi - lo) as usize - drop,
            epoch: head.epoch,
            head: head.write_frames,
        }
    }

    /// Remove `path` if it still names this ring (the request file).
    pub fn unlink_if_at(&self, path: &Path) -> bool {
        self.region.unlink_if_at(path)
    }

    pub fn is_at(&self, path: &Path) -> bool {
        self.region.is_at(path)
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("tap: {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::testdir;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::Arc;

    const LAG: u32 = 2400;

    fn pair_up(meter: &Path) -> (TapRing, TapRing) {
        let window = TapRing::request(meter).unwrap();
        let plugin = TapRing::attach(&request_path(meter)).unwrap();
        plugin.pin();
        plugin.mark_mapped(7, LAG);
        assert!(plugin.unlink_if_at(&request_path(meter)));
        (window, plugin)
    }

    /// The value raw frame `i` carries (exact in f32, never 0).
    fn v(i: u64) -> f32 {
        ((i % 4_000_000) + 1) as f32
    }

    /// The clean side is the raw side delayed by LAG, as the engine
    /// produces it.
    fn feed(plugin: &TapRing, cur: &mut TapCursor, from: u64, hops: u64) {
        let mut raw = [0f32; 480];
        let mut clean = [0f32; 480];
        for h in 0..hops {
            for k in 0..480u64 {
                let i = from + h * 480 + k;
                raw[k as usize] = v(i);
                clean[k as usize] = if i >= LAG as u64 {
                    v(i - LAG as u64)
                } else {
                    0.0
                };
            }
            plugin.write_hop(cur, &raw, &clean);
        }
    }

    #[test]
    fn request_attach_and_release_handshake() {
        let d = testdir::new("tap-hs");
        let meter = d.join("meter-1");
        let window = TapRing::request(&meter).unwrap();
        assert_eq!(window.state(), TapState::Waiting);
        assert!(window.is_at(&request_path(&meter)));
        // Nothing else is left in the directory (the temp name was renamed).
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        let plugin = TapRing::attach(&request_path(&meter)).unwrap();
        assert!(
            TapRing::attach(&request_path(&meter)).is_ok(),
            "not yet taken"
        );
        plugin.mark_mapped(5, LAG);
        assert!(!plugin.nameless());
        assert!(plugin.unlink_if_at(&request_path(&meter)));
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0, "no name left");
        assert!(plugin.nameless());
        assert_eq!(
            window.state(),
            TapState::Mapped {
                instance: 5,
                clean_lag: LAG
            }
        );
        let now = monotonic_ms();
        assert!(!plugin.wants_release(now));
        assert!(
            plugin.wants_release(now + HEARTBEAT_TIMEOUT_MS + 1),
            "stale"
        );
        window.heartbeat();
        window.close();
        assert!(plugin.wants_release(monotonic_ms()), "closed");
        plugin.mark_released();
        assert_eq!(window.state(), TapState::Released);
        plugin.mark_replaced();
        assert_eq!(window.state(), TapState::Replaced);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn malformed_or_stale_requests_are_refused() {
        let d = testdir::new("tap-bad");
        let meter = d.join("meter");
        let path = request_path(&meter);
        let poke = |off: usize, val: u64, wide: bool| {
            let w = TapRing::request(&meter).unwrap();
            if wide {
                w.region.u64(off).store(val, Ordering::Relaxed);
            } else {
                w.region.u32(off).store(val as u32, Ordering::Relaxed);
            }
            assert!(TapRing::attach(&path).is_err(), "offset {off} = {val}");
            assert!(!path.exists(), "a refused request loses its name");
        };
        poke(OFF_MAGIC, 0, true);
        poke(OFF_VERSION, 9, false);
        poke(OFF_CHANNELS, 1, false);
        poke(OFF_CAPACITY, 1 << 18, true);
        poke(OFF_RATE, 44_100, false);
        poke(OFF_CLOSED, 1, false);
        poke(OFF_HEARTBEAT, 0, true);
        poke(OFF_MAPPED, MAPPED_YES as u64, false);
        // A second name would outlive the plugin's unlink.
        let w = TapRing::request(&meter).unwrap();
        std::fs::hard_link(&path, d.join("copy")).unwrap();
        assert!(TapRing::attach(&path).is_err(), "two names");
        std::fs::remove_file(d.join("copy")).unwrap();
        drop(w);
        // A good request still attaches.
        let _w = TapRing::request(&meter).unwrap();
        assert!(TapRing::attach(&path).is_ok());
        // Wrong size, world-readable, symlink.
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(SIZE as u64 - 4).unwrap();
        assert!(TapRing::attach(&path).is_err());
        f.set_len(SIZE as u64).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(TapRing::attach(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(d.join("elsewhere"), &path).unwrap();
        assert!(TapRing::attach(&path).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn pairs_are_exactly_aligned_across_several_wraps() {
        let d = testdir::new("tap-wrap");
        let (window, plugin) = pair_up(&d.join("meter"));
        let (mut raw, mut clean) = (Vec::new(), Vec::new());
        let mut next = 0u64;
        let mut cur = TapCursor::default();
        // 3.5 ring lengths, read in 50 ms polls of 5 hops.
        let hops = (CAPACITY * 7 / 2) / 480;
        let mut fed = 0;
        while fed < hops {
            feed(&plugin, &mut cur, fed * 480, 5);
            fed += 5;
            let p = window.read_pairs(next, usize::MAX, &mut raw, &mut clean);
            assert_eq!(p.epoch, 0);
            if p.len > 0 {
                assert!(p.start >= next);
                next = p.start + p.len as u64;
            }
        }
        assert_eq!(next, fed * 480, "read up to the head");
        assert_eq!(raw.len(), (fed * 480 - LAG as u64) as usize);
        for (r, c) in raw.iter().zip(&clean) {
            assert_eq!(r, c, "clean[n] == raw[n - lag]");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_old_window_is_trimmed_to_what_the_ring_still_holds() {
        let d = testdir::new("tap-old");
        let (window, plugin) = pair_up(&d.join("meter"));
        feed(&plugin, &mut TapCursor::default(), 0, CAPACITY / 480 + 300);
        let (mut raw, mut clean) = (Vec::new(), Vec::new());
        let p = window.read_pairs(0, usize::MAX, &mut raw, &mut clean);
        let head = (CAPACITY / 480 + 300) * 480;
        assert_eq!(p.head, head);
        assert_eq!(p.start, head - CAPACITY + LAG as u64);
        assert_eq!(p.len, (CAPACITY - LAG as u64) as usize);
        assert_eq!(raw[0], v(p.start - LAG as u64));
        assert!(raw.iter().zip(&clean).all(|(r, c)| r == c));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_epoch_split_hides_pairs_across_the_reset() {
        let d = testdir::new("tap-epoch");
        let (window, plugin) = pair_up(&d.join("meter"));
        let mut cur = TapCursor::default();
        feed(&plugin, &mut cur, 0, 100);
        plugin.discontinuity(&mut cur);
        // After the reset the engine starts from silence again: the clean
        // side is the raw side of THIS epoch delayed by LAG.
        let base = 100 * 480u64;
        let mut raw = [0f32; 480];
        let mut clean = [0f32; 480];
        for h in 0..50u64 {
            for k in 0..480u64 {
                let local = h * 480 + k;
                raw[k as usize] = -((local + 1) as f32);
                clean[k as usize] = if local >= LAG as u64 {
                    -((local - LAG as u64 + 1) as f32)
                } else {
                    0.0
                };
            }
            plugin.write_hop(&mut cur, &raw, &clean);
        }
        let (mut r, mut c) = (Vec::new(), Vec::new());
        let p = window.read_pairs(0, usize::MAX, &mut r, &mut c);
        assert_eq!(p.epoch, 1);
        assert_eq!(
            p.start,
            base + LAG as u64,
            "first pair with its raw in this epoch"
        );
        assert_eq!(p.len, (50 * 480 - LAG) as usize);
        assert!(r.iter().all(|&x| x < 0.0), "nothing from before the reset");
        assert_eq!(r, c);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_window_cannot_move_the_plugins_writes() {
        let d = testdir::new("tap-cursor");
        let (window, plugin) = pair_up(&d.join("meter"));
        let mut cur = TapCursor::default();
        feed(&plugin, &mut cur, 0, 10);
        // A broken window scribbles over the plugin's fields.
        window
            .region
            .u64(OFF_WRITE)
            .store(u64::MAX - 7, Ordering::Relaxed);
        window
            .region
            .u64(OFF_BEGIN)
            .store(u64::MAX, Ordering::Relaxed);
        window
            .region
            .u64(OFF_EPOCH_FRAMES)
            .store(u64::MAX, Ordering::Relaxed);
        let (mut r, mut c) = (Vec::new(), Vec::new());
        let p = window.read_pairs(0, usize::MAX, &mut r, &mut c);
        assert_eq!(p.len, 0, "nonsense reads empty, never overflows");
        feed(&plugin, &mut cur, 10 * 480, 10);
        assert_eq!(cur.frames(), 20 * 480);
        window
            .region
            .u64(OFF_EPOCH_FRAMES)
            .store(0, Ordering::Relaxed);
        let p = window.read_pairs(0, usize::MAX, &mut r, &mut c);
        assert_eq!(p.head, 20 * 480, "the plugin wrote where it was");
        assert_eq!(p.len, (20 * 480 - LAG) as usize);
        assert!(r.iter().zip(&c).all(|(a, b)| a == b));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The writer at full speed against a slow reader that asks for the
    /// whole 10 s window each time: whatever is returned is exactly aligned
    /// and untorn, and slow copies drop only their overwritten front.
    #[test]
    fn reader_writer_stress_never_returns_a_torn_pair() {
        let d = testdir::new("tap-stress");
        let (window, plugin) = pair_up(&d.join("meter"));
        let plugin = Arc::new(plugin);
        let stop = Arc::new(AtomicBool::new(false));
        let hops = Arc::new(AtomicU64::new(0));
        let (p2, s2, h2) = (Arc::clone(&plugin), Arc::clone(&stop), Arc::clone(&hops));
        let writer = std::thread::spawn(move || {
            let mut h = 0u64;
            let mut cur = TapCursor::default();
            while !s2.load(Ordering::Relaxed) {
                // Hundreds of times faster than real time: the ring wraps
                // every few tens of milliseconds, about as long as one slow
                // 10 s copy takes.
                let t = std::time::Instant::now();
                feed(&p2, &mut cur, h * 480, 1);
                h += 1;
                h2.store(h, Ordering::Relaxed);
                while t.elapsed() < std::time::Duration::from_micros(20) {
                    std::hint::spin_loop();
                }
            }
        });
        let (mut raw, mut clean) = (Vec::new(), Vec::new());
        let started = std::time::Instant::now();
        let (mut reads, mut trimmed, mut checked) = (0u32, 0u32, 0usize);
        // At least 800 ms of it, and until the writer has wrapped the ring
        // and some copies came through (a loaded runner gets there later;
        // the cap is for a writer that stopped).
        let wrapped = |h: &AtomicU64| h.load(Ordering::Relaxed) > CAPACITY / 480;
        while started.elapsed() < std::time::Duration::from_millis(800)
            || !(wrapped(&hops) && reads > 5 && checked > 0)
        {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(120),
                "the writer wrapped: {} hops, {reads} reads, {checked} checked",
                hops.load(Ordering::Relaxed)
            );
            raw.clear();
            clean.clear();
            let head = window.head().write_frames;
            // Alternate the whole 10 s window (slow, often overrun) with
            // the newest second (fast, nearly always intact).
            let span = if reads % 2 == 0 { 480_000 } else { 48_000 };
            let from = head.saturating_sub(span);
            let p = window.read_pairs(from, usize::MAX, &mut raw, &mut clean);
            if p.start > from.max(LAG as u64) {
                trimmed += 1;
            }
            for (k, (r, c)) in raw.iter().zip(&clean).enumerate() {
                let n = p.start + k as u64;
                assert_eq!(*c, v(n - LAG as u64), "clean {n} torn");
                assert_eq!(r, c, "pair {n} misaligned");
            }
            checked += raw.len();
            reads += 1;
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        assert!(reads > 5, "{reads} reads");
        assert!(
            checked > 0,
            "some copies outran the writer ({trimmed} trimmed)"
        );
        assert!(
            hops.load(Ordering::Relaxed) > CAPACITY / 480,
            "the writer wrapped"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
