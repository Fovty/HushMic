//! A microphone's live level, before HushMic runs on it: first run's step 1
//! shows it under the microphone list ("the bar moves when this microphone
//! hears you").
//!
//! [`LevelProbe`] records the device straight from PipeWire on its own
//! thread (no engine, no shared memory) and keeps the RMS of the last 50 ms
//! on the engine meter's scale. It wakes the window only while there is
//! something to show, so a quiet room costs one frame every few seconds.
//! As Mic check, it stops recording when the window has not read a changed
//! level for [`super::LOOK_TIMEOUT`] (the window is not drawing), and
//! starts again on the next read. In a quiet room nothing changes, so the
//! probe asks for that one frame every [`QUIET_CHECK`] to learn whether
//! the window still draws: a hidden window never keeps the microphone.

use super::proc::Purpose;
use super::source::{record, Ended, Recorder};
use super::Repaint;
use crate::meter;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One level per this many samples (50 ms at 48 kHz, the live views' rate).
const BLOCK: usize = 2_400;
/// A device that is missing, or whose stream ended, is tried again after
/// this.
const RETRY: Duration = Duration::from_secs(2);
/// Recordings that end before their first byte this many times in a row
/// mean this pw-cat cannot record to a pipe (one could be a hiccup).
const UNSUPPORTED_AFTER: u32 = 2;
/// The delay handed to the window's repaint (an immediate repaint from
/// another thread paints twice).
const WAKE: Duration = Duration::from_millis(5);
/// While the level stands still, the window is asked for a frame this
/// often, to see that it still reads.
pub const QUIET_CHECK: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProbeState {
    /// Finding the device, or waiting for its first samples.
    Starting,
    /// The RMS of the last 50 ms, 0..1 on [`meter::level`]'s scale.
    Live(f32),
    /// No such microphone right now (or none to follow as the default), or
    /// its stream ended; tried again every 2 s.
    Unavailable,
    /// This PipeWire cannot stream a recording to HushMic (pw-cat before
    /// mid-2022): first run shows its note instead of the bar.
    Unsupported,
}

/// See the module docs. Dropping it stops the recording.
pub struct LevelProbe {
    node: Option<String>,
    shared: Arc<Shared>,
    rec: Option<Recorder>,
}

struct Shared {
    state: Mutex<ProbeState>,
    /// CLOCK_MONOTONIC ms of the window's last read.
    read_ms: AtomicU64,
    /// Not recording for want of reads.
    paused: AtomicBool,
}

impl LevelProbe {
    /// Start listening to `node` (a source's node name), or with None to
    /// the microphone HushMic follows on "System default". `repaint` is
    /// called when the state changes and with every level while the
    /// microphone hears something.
    pub fn start(node: Option<String>, repaint: Repaint) -> LevelProbe {
        let shared = Arc::new(Shared {
            state: Mutex::new(ProbeState::Starting),
            read_ms: AtomicU64::new(hushmic_shm::monotonic_ms()),
            paused: AtomicBool::new(false),
        });
        let (s, n) = (Arc::clone(&shared), node.clone());
        let rec = Recorder::spawn("hushmic-level", move |stop, pid| {
            run(n.as_deref(), &s, &repaint, stop, pid)
        });
        if rec.is_none() {
            *shared.state.lock().unwrap() = ProbeState::Unavailable;
        }
        LevelProbe { node, shared, rec }
    }

    /// The current state. Call it from the frames that draw the meter: the
    /// reads are how the probe knows someone is looking.
    pub fn state(&self) -> ProbeState {
        self.shared
            .read_ms
            .store(hushmic_shm::monotonic_ms(), Ordering::Release);
        if self.shared.paused.swap(false, Ordering::AcqRel) {
            if let Some(r) = &self.rec {
                r.wake();
            }
        }
        *self.shared.state.lock().unwrap()
    }

    /// The level while the microphone streams.
    pub fn level(&self) -> Option<f32> {
        match self.state() {
            ProbeState::Live(l) => Some(l),
            _ => None,
        }
    }

    /// The node this probe was started for (None = the default).
    pub fn node(&self) -> Option<&str> {
        self.node.as_deref()
    }
}

/// The probe thread: resolve, record, retry until stopped.
fn run(
    node: Option<&str>,
    shared: &Shared,
    repaint: &Repaint,
    stop: &AtomicBool,
    pid: &Mutex<Option<u32>>,
) {
    let set = |s: ProbeState| {
        let mut g = shared.state.lock().unwrap();
        if *g != s {
            *g = s;
            drop(g);
            repaint(WAKE);
        }
    };
    let look_ms = super::LOOK_TIMEOUT.as_millis() as u64;
    let check_ms = QUIET_CHECK.as_millis() as u64;
    let mut early_deaths = 0;
    while !stop.load(Ordering::Acquire) {
        if shared.paused.load(Ordering::Acquire) {
            // Until the window reads again (it unparks this thread).
            std::thread::park_timeout(Duration::from_secs(1));
            continue;
        }
        let Some(target) = resolve(node) else {
            set(ProbeState::Unavailable);
            pause(stop, RETRY);
            continue;
        };
        set(ProbeState::Starting);
        let mut meter = BlockMeter::default();
        let mut shown = 0.0f32;
        let mut got = false;
        let mut unread_since: Option<u64> = None;
        let mut woke = hushmic_shm::monotonic_ms();
        let mut not_looking = false;
        let end = record(&target, Purpose::Level, stop, pid, |chunk| {
            got = true;
            meter.push(chunk, |l| {
                *shared.state.lock().unwrap() = ProbeState::Live(l);
                // Quiet stays quiet without level frames; the step down to
                // the floor is still shown.
                if l > 0.0 || shown > 0.0 {
                    repaint(WAKE);
                    woke = hushmic_shm::monotonic_ms();
                    unread_since.get_or_insert(woke);
                }
                shown = l;
            });
            let now = hushmic_shm::monotonic_ms();
            if unread_since.is_some_and(|t| shared.read_ms.load(Ordering::Acquire) >= t) {
                unread_since = None;
            }
            if unread_since.is_none() && now.saturating_sub(woke) >= check_ms {
                // Is anyone still looking? One frame tells.
                repaint(WAKE);
                woke = now;
                unread_since = Some(now);
            }
            not_looking = unread_since.is_some_and(|t| now.saturating_sub(t) > look_ms);
            !not_looking
        });
        if not_looking && !stop.load(Ordering::Acquire) {
            *shared.state.lock().unwrap() = ProbeState::Starting;
            shared.paused.store(true, Ordering::Release);
            continue;
        }
        match end {
            Ended::Stopped => return,
            Ended::Died { unsupported } => {
                early_deaths = if unsupported && !got {
                    early_deaths + 1
                } else {
                    0
                };
                if early_deaths >= UNSUPPORTED_AFTER {
                    set(ProbeState::Unsupported);
                    return;
                }
                set(ProbeState::Unavailable);
                pause(stop, RETRY);
            }
        }
    }
}

/// Sleep up to `d`, waking early when stopped (the recorder's drop
/// unparks this thread).
fn pause(stop: &AtomicBool, d: Duration) {
    let until = Instant::now() + d;
    while !stop.load(Ordering::Acquire) {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::park_timeout(left);
    }
}

/// The record target for `node`: the node itself when it is there, else
/// the source a chain on "System default" follows (never HushMic's own
/// node or a monitor). None: nothing to record now.
fn resolve(node: Option<&str>) -> Option<String> {
    let dump = crate::pipewire::pw_dump();
    match node {
        Some(n) => {
            // Without a snapshot, try it anyway: pw-record's own failure
            // is the answer then.
            let present = dump.as_deref().is_none_or(|d| {
                crate::pipewire::parse_pwdump_nodes(d)
                    .iter()
                    .any(|s| s.name == n)
            });
            present.then(|| n.to_string())
        }
        None => {
            let prior = crate::controller::persisted_prior_default();
            match dump.as_deref() {
                Some(d) => crate::pipewire::followed_default(d, prior.as_deref()),
                None => crate::pipewire::get_default_source()
                    .filter(|d| crate::config::can_own_profile(d))
                    .or(prior),
            }
        }
    }
}

/// Splits a stream into 50 ms blocks and reports each block's RMS level.
#[derive(Default)]
struct BlockMeter {
    sum: f64,
    n: usize,
}

impl BlockMeter {
    fn push(&mut self, samples: &[f32], mut done: impl FnMut(f32)) {
        for &s in samples {
            let s = if s.is_finite() { s as f64 } else { 0.0 };
            self.sum += s * s;
            self.n += 1;
            if self.n == BLOCK {
                done(meter::level((self.sum / BLOCK as f64).sqrt() as f32));
                self.sum = 0.0;
                self.n = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_report_rms_on_the_meter_scale() {
        let mut m = BlockMeter::default();
        let mut out = Vec::new();
        // A full-scale square wave has RMS 1.0; a -20 dBFS one 0.1.
        let loud: Vec<f32> = (0..BLOCK)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let soft: Vec<f32> = loud.iter().map(|s| s * 0.1).collect();
        // Split across chunk boundaries as a pipe delivers it.
        m.push(&loud[..1000], |l| out.push(l));
        assert!(out.is_empty(), "no level before a whole block");
        m.push(&loud[1000..], |l| out.push(l));
        m.push(&soft, |l| out.push(l));
        m.push(&vec![0.0; BLOCK], |l| out.push(l));
        assert_eq!(out.len(), 3);
        assert!((out[0] - 1.0).abs() < 1e-6);
        assert!((out[1] - meter::level(0.1)).abs() < 1e-6);
        assert!(
            (out[1] - 40.0 / 60.0).abs() < 1e-4,
            "-20 dBFS on a 60 dB scale"
        );
        assert_eq!(out[2], 0.0);
    }

    #[test]
    fn broken_samples_do_not_poison_the_level() {
        let mut m = BlockMeter::default();
        let mut out = Vec::new();
        let mut block = vec![0.5f32; BLOCK];
        block[7] = f32::NAN;
        block[8] = f32::INFINITY;
        m.push(&block, |l| out.push(l));
        assert_eq!(out.len(), 1);
        assert!(out[0].is_finite() && out[0] > 0.8);
    }

    #[test]
    fn pause_returns_early_when_stopped() {
        let stop = Arc::new(AtomicBool::new(false));
        let s = Arc::clone(&stop);
        let t = std::thread::spawn(move || {
            let t0 = Instant::now();
            pause(&s, Duration::from_secs(30));
            t0.elapsed()
        });
        std::thread::sleep(Duration::from_millis(50));
        stop.store(true, Ordering::Release);
        t.thread().unpark();
        assert!(t.join().unwrap() < Duration::from_secs(5));
    }
}
