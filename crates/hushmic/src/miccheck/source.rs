//! Where Mic check's audio comes from.
//!
//! - [`TapSource`]: the engine tap (raw and clean of the same engine hops,
//!   exact after the fixed latency shift). The backend's [`KeepAlive`]
//!   consumer keeps the chain running while the page listens.
//! - [`StreamsSource`]: the fallback, two recording streams (the raw
//!   microphone and HushMic's output), paired at a working lag for the
//!   live view and aligned by cross-correlation when a take is frozen.

use super::proc;
use super::stream::{self, F32Reader, Header, SampleEndian, StreamInfo};
use super::take::{ColumnStats, FrozenTake, Rolling};
use super::types::{Column, TAKE_FRAMES};
use super::xcorr::{self, Alignment};
use hushmic_shm::tap::{self, TapRing, TapState};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The plugin maps a request within one poll (250 ms), also while it still
/// holds a crashed window's ring; this is the budget before the fallback
/// takes over (the old heartbeat timeout plus two polls, with a margin).
pub const MAP_TIMEOUT: Duration = Duration::from_millis(2_500);
/// A mapped tap whose write position stands still this long: the chain is
/// not running (the keep-alive could not start or does not wake it).
pub const STALL_TIMEOUT: Duration = Duration::from_secs(3);
/// A keep-alive that ended is started again at most this often.
const KEEP_ALIVE_RETRY: Duration = Duration::from_secs(1);
/// HushMic's virtual microphone.
pub const HUSHMIC_NODE: &str = "hushmic_source";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapPoll {
    /// Asked; the plugin has not mapped it yet.
    Waiting,
    Running,
    /// Not mapped in time, or mapped but no audio comes: no tap here.
    Unavailable,
    /// The plugin let go (chain stopped, or our heartbeat stalled).
    Lost,
}

/// The keep-alive consumer ([`proc::spawn_keep_alive`]) on hushmic_source.
/// One lasts as long as the page listens, whatever the source does
/// meanwhile: a chain restart (a new meter and tap, or none for a moment),
/// a lost tap, the fallback streams. hushmic_source, in its own host,
/// outlives them all. A recorder stopped and started again just as a
/// restarted chain links its microphone has left another client's stream
/// in that graph stuck for good on PipeWire 1.0 (the CI's virtual
/// microphone went silent after it came back).
pub struct KeepAlive {
    child: Option<Child>,
    /// When it may be started again, after it ended or could not start.
    retry: Option<Instant>,
    /// Why the last one could not start.
    error: Option<std::io::ErrorKind>,
}

impl KeepAlive {
    pub fn start(now: Instant) -> KeepAlive {
        let mut k = KeepAlive {
            child: None,
            retry: None,
            error: None,
        };
        k.spawn(now);
        k
    }

    fn spawn(&mut self, now: Instant) {
        match proc::spawn_keep_alive(HUSHMIC_NODE) {
            Ok(c) => {
                self.child = Some(c);
                self.error = None;
            }
            Err(e) => {
                self.error = Some(e.kind());
                self.retry = Some(now + KEEP_ALIVE_RETRY);
            }
        }
    }

    /// Start it again if it ended, but not in a loop when it cannot run.
    pub fn poll(&mut self, now: Instant) {
        if let Some(c) = self.child.as_mut() {
            if let Ok(Some(_)) = c.try_wait() {
                // Without a consumer the chain goes idle.
                self.child = None;
                self.retry = Some(now + KEEP_ALIVE_RETRY);
            }
        }
        if self.child.is_none() && self.retry.is_some_and(|at| now >= at) {
            self.retry = None;
            self.spawn(now);
        }
    }

    /// Why it could not start, if it could not.
    pub fn error(&self) -> Option<std::io::ErrorKind> {
        self.error
    }
}

impl Drop for KeepAlive {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            proc::stop_child(&mut c);
        }
    }
}

/// See the module docs.
pub struct TapSource {
    ring: TapRing,
    meter: PathBuf,
    request: PathBuf,
    requested_at: Instant,
    last_beat: Instant,
    next: u64,
    /// The write position at the last poll, and when it last moved (or the
    /// ring was mapped).
    head: u64,
    moved_at: Option<Instant>,
    /// Audio has come through this ring.
    flowing: bool,
    raw: Vec<f32>,
    clean: Vec<f32>,
}

impl TapSource {
    /// Ask the chain whose meter file is `meter` for its tap.
    pub fn request(meter: &Path) -> std::io::Result<TapSource> {
        let ring = TapRing::request(meter)?;
        let now = Instant::now();
        Ok(TapSource {
            ring,
            meter: meter.to_path_buf(),
            request: tap::request_path(meter),
            requested_at: now,
            last_beat: now,
            next: 0,
            head: 0,
            moved_at: None,
            flowing: false,
            raw: Vec::with_capacity(48_000),
            clean: Vec::with_capacity(48_000),
        })
    }

    /// Heartbeat, and move the new pairs into `rolling`.
    pub fn poll(
        &mut self,
        now: Instant,
        rolling: &mut Rolling,
        done: &mut Vec<(Column, ColumnStats)>,
    ) -> TapPoll {
        if now.saturating_duration_since(self.last_beat)
            >= Duration::from_millis(tap::HEARTBEAT_EVERY_MS)
        {
            self.ring.heartbeat();
            self.last_beat = now;
        }
        match self.ring.state() {
            TapState::Waiting => {
                if now.saturating_duration_since(self.requested_at) > MAP_TIMEOUT {
                    return TapPoll::Unavailable;
                }
                // Our name is gone but nobody mapped the ring: the request
                // was removed under us (a plugin's unlink racing our
                // rename). The plugin marks a ring mapped before it removes
                // the name, so this is never a ring it took.
                if !self.ring.is_at(&self.request) && self.ring.state() == TapState::Waiting {
                    if let Ok(ring) = TapRing::request(&self.meter) {
                        self.ring = ring;
                    }
                }
                TapPoll::Waiting
            }
            TapState::Released => TapPoll::Lost,
            // Another window took the tap (a newer one of ours, while this
            // one closes): asking back would take turns, so listen without.
            TapState::Replaced => TapPoll::Unavailable,
            TapState::Mapped { .. } => {
                // The plugin removes the name; a leftover is ours to remove.
                self.ring.unlink_if_at(&self.request);
                let head = self.ring.head().write_frames;
                if self.moved_at.is_none() || head != self.head {
                    self.flowing |= self.moved_at.is_some();
                    self.head = head;
                    self.moved_at = Some(now);
                }
                if self
                    .moved_at
                    .is_some_and(|t| now.saturating_duration_since(t) > STALL_TIMEOUT)
                {
                    return TapPoll::Unavailable;
                }
                if !self.flowing {
                    return TapPoll::Waiting;
                }
                loop {
                    self.raw.clear();
                    self.clean.clear();
                    let p = self
                        .ring
                        .read_pairs(self.next, 48_000, &mut self.raw, &mut self.clean);
                    if p.len == 0 {
                        break;
                    }
                    rolling.push(p.start, &self.raw, &self.clean, done);
                    self.next = p.start + p.len as u64;
                }
                TapPoll::Running
            }
        }
    }
}

impl Drop for TapSource {
    fn drop(&mut self) {
        // Tell the plugin now (it lets go within its poll), and leave no
        // request behind if it never mapped this one.
        self.ring.close();
        self.ring.unlink_if_at(&self.request);
    }
}

/// One recording stream feeding a shared history.
struct Capture {
    buf: Arc<Mutex<History>>,
    _rec: Option<Recorder>,
}

#[derive(Default)]
struct History {
    samples: VecDeque<f32>,
    /// Samples received since the start (absolute index of the next one).
    total: u64,
    /// The stream ended or never started.
    died: bool,
    /// The recorder exited before writing anything to the pipe: this
    /// pw-cat cannot record to a pipe.
    unsupported: bool,
}

/// History kept per stream: the take plus room for the alignment search.
const HISTORY: usize = TAKE_FRAMES + 48_000;

impl Capture {
    fn start(node: String) -> Capture {
        let buf = Arc::new(Mutex::new(History::default()));
        let b = Arc::clone(&buf);
        let rec = Recorder::spawn("miccheck-rec", move |stop, pid| {
            let end = record(&node, proc::Purpose::MicCheck, stop, pid, |chunk| {
                let mut h = b.lock().unwrap();
                h.total += chunk.len() as u64;
                h.samples.extend(chunk.iter().copied());
                let excess = h.samples.len().saturating_sub(HISTORY);
                h.samples.drain(..excess);
                true
            });
            if let Ended::Died { unsupported } = end {
                let mut h = b.lock().unwrap();
                h.died = true;
                h.unsupported |= unsupported;
            }
        });
        if rec.is_none() {
            buf.lock().unwrap().died = true;
        }
        Capture { buf, _rec: rec }
    }
}

/// A thread that records, and the means to end it from outside.
pub(super) struct Recorder {
    stop: Arc<AtomicBool>,
    /// The recorder child's pid while it is unreaped: `Drop` ends it so a
    /// read blocked in the thread returns. See [`record`] for the locking.
    pid: Arc<Mutex<Option<u32>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Recorder {
    /// Unpark the thread (it waits between recordings).
    pub(super) fn wake(&self) {
        if let Some(t) = &self.thread {
            t.thread().unpark();
        }
    }

    /// Run `body(stop, pid)` on a new thread; it must pass both to
    /// [`record`] and return soon after `stop` is set (a thread waiting
    /// between recordings parks; `Drop` unparks it). None: no thread.
    pub(super) fn spawn(
        name: &str,
        body: impl FnOnce(&AtomicBool, &Mutex<Option<u32>>) + Send + 'static,
    ) -> Option<Recorder> {
        let stop = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(Mutex::new(None));
        let (s, p) = (Arc::clone(&stop), Arc::clone(&pid));
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || body(&s, &p))
            .ok()?;
        Some(Recorder {
            stop,
            pid,
            thread: Some(thread),
        })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(pid) = *self.pid.lock().unwrap() {
            // SAFETY: kill(2) on an unreaped child (see `pid`).
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

/// How a [`record`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ended {
    /// `stop` was set, or the sink asked to end.
    Stopped,
    /// The stream ended on its own; `unsupported`: the recorder exited
    /// before its first byte (a pw-cat that cannot record to a pipe).
    Died { unsupported: bool },
}

/// Record `node` as 48 kHz mono f32 until `stop` is set, the stream ends,
/// or `sink` returns false, handing each chunk to `sink`. Owns and reaps
/// its pw-record child.
///
/// `pid` holds the child's pid while it is unreaped. It is published under
/// the lock only while `stop` is still clear, and taken under the lock
/// before the reap, so [`Recorder`]'s drop either sees the pid (and ends
/// the child) or this sees `stop` (and never blocks on a read), and a pid
/// is never signalled after reuse.
pub(super) fn record(
    node: &str,
    purpose: proc::Purpose,
    stop: &AtomicBool,
    pid: &Mutex<Option<u32>>,
    mut sink: impl FnMut(&[f32]) -> bool,
) -> Ended {
    let mut child = match proc::spawn_record_pipe(node, purpose) {
        Ok(c) => c,
        Err(_) => return Ended::Died { unsupported: false },
    };
    {
        let mut slot = pid.lock().unwrap();
        if stop.load(Ordering::Acquire) {
            drop(slot);
            proc::stop_child(&mut child);
            return Ended::Stopped;
        }
        *slot = Some(child.id());
    }
    let reap = |child: &mut Child| {
        pid.lock().unwrap().take();
        proc::stop_child(child);
    };
    let ended = |stop: &AtomicBool, unsupported: bool| {
        if stop.load(Ordering::Acquire) {
            Ended::Stopped
        } else {
            Ended::Died { unsupported }
        }
    };
    let Some(mut stdout) = child.stdout.take() else {
        reap(&mut child);
        return Ended::Died { unsupported: false };
    };
    let mut reader = match stream::read_header(&mut stdout) {
        Ok(Header::Parsed(StreamInfo {
            channels: 1,
            sample_rate: 48_000,
            endian,
        })) => F32Reader::new(stdout, endian, 16 * 1024),
        Ok(Header::Raw(prefix)) => {
            F32Reader::new_with_prefix(stdout, SampleEndian::Le, 16 * 1024, &prefix)
        }
        Ok(Header::Parsed(_)) | Err(_) => {
            reap(&mut child);
            // An old pw-cat that cannot write to a pipe exits before its
            // first byte (unless we stopped it ourselves).
            return ended(stop, true);
        }
    };
    let mut chunk = Vec::with_capacity(4096);
    let end = loop {
        if stop.load(Ordering::Acquire) {
            break Ended::Stopped;
        }
        match reader.read_samples(&mut chunk) {
            Ok(true) => {
                if !sink(&chunk) {
                    break Ended::Stopped;
                }
            }
            _ => break ended(stop, false),
        }
    };
    reap(&mut child);
    end
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamsPoll {
    Waiting,
    Running,
    /// A stream ended; `unsupported`: this pw-cat cannot record to a pipe.
    Died {
        unsupported: bool,
    },
}

/// See the module docs. Indices are each stream's own sample count; the
/// lag says which clean sample pairs with which raw one
/// (`clean[i]` with `raw[i - lag]`).
pub struct StreamsSource {
    raw: Capture,
    clean: Capture,
    lag: Option<i64>,
    /// The working lag was measured on a take (not nominal).
    measured: bool,
    /// Next clean index to pair.
    next: u64,
    raw_tmp: Vec<f32>,
    clean_tmp: Vec<f32>,
    started: Instant,
}

/// Until a take has been aligned: the engine latency plus graph delays.
pub const NOMINAL_LATENCY: i64 = 4_800;
/// The alignment search range, around the streams' start difference.
const MAX_LAG: usize = 24_000;
/// Both streams must deliver within this.
const START_TIMEOUT: Duration = Duration::from_secs(3);

impl StreamsSource {
    pub fn start(mic_node: &str) -> StreamsSource {
        StreamsSource {
            raw: Capture::start(mic_node.to_string()),
            clean: Capture::start(HUSHMIC_NODE.to_string()),
            lag: None,
            measured: false,
            next: 0,
            raw_tmp: Vec::new(),
            clean_tmp: Vec::new(),
            started: Instant::now(),
        }
    }

    /// Pair what both streams delivered at the working lag into `rolling`.
    pub fn poll(
        &mut self,
        rolling: &mut Rolling,
        done: &mut Vec<(Column, ColumnStats)>,
    ) -> StreamsPoll {
        let r = self.raw.buf.lock().unwrap();
        let c = self.clean.buf.lock().unwrap();
        if r.died || c.died {
            return StreamsPoll::Died {
                unsupported: r.unsupported || c.unsupported,
            };
        }
        if r.total == 0 || c.total == 0 {
            return if self.started.elapsed() > START_TIMEOUT {
                StreamsPoll::Died { unsupported: false }
            } else {
                StreamsPoll::Waiting
            };
        }
        // Each count runs from its own start: the difference of the counts
        // is the difference of the starts.
        let lag = *self
            .lag
            .get_or_insert(c.total as i64 - r.total as i64 + NOMINAL_LATENCY);
        let r_first = (r.total - r.samples.len() as u64) as i64;
        let c_first = (c.total - c.samples.len() as u64) as i64;
        let lo = (self.next as i64).max(c_first).max(r_first + lag).max(0);
        let hi = (c.total as i64).min(r.total as i64 + lag);
        if hi > lo {
            self.raw_tmp.clear();
            self.clean_tmp.clear();
            for i in lo..hi {
                self.clean_tmp.push(c.samples[(i - c_first) as usize]);
                self.raw_tmp.push(r.samples[(i - lag - r_first) as usize]);
            }
            drop((r, c));
            rolling.push(lo as u64, &self.raw_tmp, &self.clean_tmp, done);
            self.next = hi as u64;
        }
        StreamsPoll::Running
    }

    /// Freeze the newest `frames` of both streams into an aligned take,
    /// re-estimating the lag. Without a voice in common the working lag
    /// stands (it was measured on an earlier take, or is nominal: then the
    /// take counts as unaligned); a take whose halves disagree is
    /// unaligned. Returns whether the working lag changed, so the live
    /// pairing must start over.
    pub fn freeze(&mut self, frames: usize) -> Option<(FrozenTake, bool, bool)> {
        let (raw, clean, r_first, c_first) = {
            let r = self.raw.buf.lock().unwrap();
            let c = self.clean.buf.lock().unwrap();
            (
                r.samples.iter().copied().collect::<Vec<f32>>(),
                c.samples.iter().copied().collect::<Vec<f32>>(),
                (r.total - r.samples.len() as u64) as i64,
                (c.total - c.samples.len() as u64) as i64,
            )
        };
        if raw.len() < 96_000 || clean.len() < 96_000 {
            return None;
        }
        // In the local vectors: clean[n] pairs with raw[n - local].
        let to_local = |abs: i64| abs - (c_first - r_first);
        let old = self.lag;
        // Clean index where the take may start at the earliest.
        let mut from = 0i64;
        let (local, aligned) = match xcorr::align(&raw, &clean, MAX_LAG) {
            Alignment::Aligned(e) => (Some(e.lag), true),
            Alignment::AlignedTail { estimate, from: f } => {
                // A stream lost data earlier: replay the newer part only.
                from = f as i64 + estimate.lag.max(0);
                (Some(estimate.lag), true)
            }
            Alignment::NoPeak => (old.map(to_local), self.measured),
            Alignment::Inconsistent => (None, false),
        };
        let take = match local {
            Some(l) => {
                let end = (clean.len() as i64).min(raw.len() as i64 + l);
                let start = (end - frames as i64).max(l).max(0).max(from);
                let pairs = (end - start).max(0) as usize;
                let c = clean[start as usize..start as usize + pairs].to_vec();
                let r = (0..pairs)
                    .map(|k| raw[(start + k as i64 - l) as usize])
                    .collect();
                FrozenTake::from_audio(r, c)
            }
            None => {
                let n = frames.min(raw.len()).min(clean.len());
                FrozenTake::from_audio(
                    raw[raw.len() - n..].to_vec(),
                    clean[clean.len() - n..].to_vec(),
                )
            }
        };
        let mut changed = false;
        if let (Some(l), true) = (local, aligned) {
            let abs = l + (c_first - r_first);
            changed = old != Some(abs);
            self.lag = Some(abs);
            self.measured = true;
        }
        Some((take, aligned && local.is_some(), changed))
    }
}
