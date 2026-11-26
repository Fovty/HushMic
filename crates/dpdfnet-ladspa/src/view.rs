//! The engine's view for the app: the meter (M2) and the Mic check tap.
//!
//! Both exist only when the host names a meter file in `HUSHMIC_METER`
//! (HushMic's own chain does; EasyEffects and other hosts never do), and
//! only for the first instance in the process: HushMic's chain has one.
//!
//! The DSP worker owns the [`EngineView`] and feeds it once per hop, after
//! the engine call, with that call's input and output: wait-free stores
//! into pages faulted in beforehand. The PipeWire data thread never touches
//! any of it.
//!
//! The tap ring belongs to the `hushmic-tap` helper thread, which maps it
//! when the window asks for it and unmaps it when the window lets go (see
//! `hushmic_shm::tap`). The worker reaches it through a [`TapSlot`]: one
//! pointer and one busy flag, so the helper never unmaps under a running
//! copy and the worker never waits for the helper, plus a generation so
//! the worker starts a fresh write position in every ring it is handed.

use hushmic_denoiser::{Mode, LATENCY_SAMPLES};
use hushmic_shm::meter::{MeterWriter, MODE_BYPASS, MODE_MUTE, MODE_PASSTHROUGH, MODE_PROCESS};
use hushmic_shm::tap::{self, TapCursor, TapRing};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Only one instance per process publishes (see the module docs).
static CLAIMED: AtomicBool = AtomicBool::new(false);

/// The meter's code for an engine mode.
pub fn mode_code(mode: Mode) -> u32 {
    match mode {
        Mode::Process => MODE_PROCESS,
        Mode::Bypass => MODE_BYPASS,
        Mode::Mute => MODE_MUTE,
        // Mode is non-exhaustive: a future mode reads as processing.
        _ => MODE_PROCESS,
    }
}

/// The meter's code for one hop: the mode, or passthrough when the engine
/// processes but passed this hop through unfiltered.
pub fn hop_mode(mode: u32, unfiltered: bool) -> u32 {
    if mode == MODE_PROCESS && unfiltered {
        MODE_PASSTHROUGH
    } else {
        mode
    }
}

/// A plugin instance id that a restarted chain does not repeat.
fn new_instance_id() -> u64 {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed) as u64;
    (hushmic_shm::monotonic_ms() << 24) ^ ((std::process::id() as u64 & 0xffff) << 8) ^ (n & 0xff)
}

/// The worker-side handle to the tap: see the module docs.
#[derive(Default)]
pub struct TapSlot {
    ring: AtomicPtr<TapRing>,
    busy: AtomicBool,
    /// +1 per attach: tells the worker a ring is new even when it sits at
    /// a freed one's address.
    generation: AtomicU64,
}

/// Clears `busy` however the copy ends, so `detach` can never spin on a
/// copy that unwound.
struct Busy<'a>(&'a AtomicBool);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl TapSlot {
    /// Worker side: run `f` on the attached ring and its generation, if
    /// any. Never waits.
    pub fn with(&self, f: impl FnOnce(&TapRing, u64)) {
        if self.ring.load(Ordering::Relaxed).is_null() {
            return;
        }
        // Dekker handshake with `detach`: either the helper sees `busy` and
        // waits, or this load sees the pointer already cleared.
        self.busy.store(true, Ordering::SeqCst);
        let _busy = Busy(&self.busy);
        let p = self.ring.load(Ordering::SeqCst);
        if !p.is_null() {
            // Set before the pointer was stored (SeqCst), so it belongs to
            // this ring or a later one, never an earlier one.
            let generation = self.generation.load(Ordering::SeqCst);
            // SAFETY: the helper keeps the ring alive until `detach` has
            // cleared the pointer and observed `busy == false`.
            f(unsafe { &*p }, generation);
        }
    }

    /// Helper side: the worker writes into `ring` from its next hop on.
    /// `ring` must stay alive until [`TapSlot::detach`] returns.
    fn attach(&self, ring: &TapRing) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.ring
            .store(ring as *const TapRing as *mut TapRing, Ordering::SeqCst);
    }

    /// Helper side: take the ring back. Returns once the worker is
    /// guaranteed not to touch it again (one hop's copy at most).
    fn detach(&self) {
        self.ring.store(ptr::null_mut(), Ordering::SeqCst);
        while self.busy.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_micros(100));
        }
    }

    pub fn attached(&self) -> bool {
        !self.ring.load(Ordering::Acquire).is_null()
    }
}

/// Owned by the worker thread.
pub struct EngineView {
    meter: MeterWriter,
    tap: Arc<TapSlot>,
    /// The write position in the attached ring, and the generation it
    /// belongs to.
    cursor: TapCursor,
    cursor_generation: u64,
    path: PathBuf,
    instance: u64,
    /// This view holds the process's claim (see [`EngineView::from_env`]).
    claimed: bool,
}

impl EngineView {
    /// The view the host asked for in `HUSHMIC_METER`, if any, for the
    /// first instance in this process. Any failure is one log line and the
    /// plugin works on without a view.
    pub fn from_env() -> Option<EngineView> {
        let path = std::env::var_os(hushmic_shm::METER_ENV).filter(|p| !p.is_empty())?;
        if CLAIMED.swap(true, Ordering::AcqRel) {
            return None;
        }
        match EngineView::create(Path::new(&path)) {
            Ok(mut v) => {
                v.claimed = true;
                Some(v)
            }
            Err(e) => {
                CLAIMED.store(false, Ordering::Release);
                crate::log::contract_line(&format!(
                    "meter: not available ({}: {e})",
                    Path::new(&path).display()
                ));
                None
            }
        }
    }

    /// Create the meter at `path` for a new instance (no process claim:
    /// the test seam; production goes through [`EngineView::from_env`]).
    pub fn create(path: &Path) -> std::io::Result<EngineView> {
        let instance = new_instance_id();
        let lag = (LATENCY_SAMPLES / hushmic_denoiser::HOP) as u32;
        let meter = MeterWriter::create(path, instance, lag)?;
        Ok(EngineView {
            meter,
            tap: Arc::new(TapSlot::default()),
            cursor: TapCursor::default(),
            cursor_generation: 0,
            path: path.to_path_buf(),
            instance,
            claimed: false,
        })
    }

    /// Worker side, once per hop: `input` the engine just consumed,
    /// `output` what it produced from it.
    pub fn publish(&mut self, input: &[f32], output: &[f32], mode: u32) {
        self.meter.publish(input, output, mode);
        let (cursor, seen) = (&mut self.cursor, &mut self.cursor_generation);
        self.tap.with(|t, generation| {
            fresh_cursor(cursor, seen, generation);
            t.write_hop(cursor, input, output);
        });
    }

    /// Worker side: the engine was reset (stream restart).
    pub fn discontinuity(&mut self) {
        self.meter.discontinuity();
        let (cursor, seen) = (&mut self.cursor, &mut self.cursor_generation);
        self.tap.with(|t, generation| {
            fresh_cursor(cursor, seen, generation);
            t.discontinuity(cursor);
        });
    }

    pub fn instance(&self) -> u64 {
        self.instance
    }

    /// The helper thread that serves tap requests for this view.
    pub fn spawn_tap_helper(&self, shutdown: Arc<AtomicBool>) -> Option<JoinHandle<()>> {
        let slot = Arc::clone(&self.tap);
        let request = tap::request_path(&self.path);
        let instance = self.instance;
        let spawned = thread::Builder::new()
            .name("hushmic-tap".into())
            .spawn(move || tap_helper(&request, &slot, instance, &shutdown));
        match spawned {
            Ok(h) => Some(h),
            Err(e) => {
                crate::log::contract_line(&format!("tap: not available (no thread: {e})"));
                None
            }
        }
    }

    pub fn tap_slot(&self) -> &Arc<TapSlot> {
        &self.tap
    }
}

/// A ring the worker has not written yet starts at frame 0, epoch 0 (as
/// `mark_mapped` set its page).
fn fresh_cursor(cursor: &mut TapCursor, seen: &mut u64, generation: u64) {
    if *seen != generation {
        *cursor = TapCursor::default();
        *seen = generation;
    }
}

impl Drop for EngineView {
    fn drop(&mut self) {
        if self.claimed {
            CLAIMED.store(false, Ordering::Release);
        }
    }
}

/// Serve tap requests until `shutdown`: map a fresh request, hand it to
/// the worker, take it back when the window closes it or its heartbeat
/// stops. A new request while one is mapped replaces it (a window that
/// was re-created after a crash must not wait out the old heartbeat).
/// Unparked by the plugin on shutdown.
fn tap_helper(request: &Path, slot: &TapSlot, instance: u64, shutdown: &AtomicBool) {
    let mut ring: Option<Box<TapRing>> = None;
    let mut last_refusal = String::new();
    let mut lock_refused = false;
    let release = |r: Box<TapRing>| {
        slot.detach();
        r.mark_released();
    };
    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        if let Some(r) = &ring {
            if r.wants_release(hushmic_shm::monotonic_ms()) {
                release(ring.take().unwrap());
                crate::log::contract_line("tap: off");
            }
        }
        match TapRing::attach(request) {
            Ok(r) => {
                let r = Box::new(r);
                if let Some(old) = ring.take() {
                    // A newer window asked: the old one must not ask back.
                    slot.detach();
                    old.mark_replaced();
                    crate::log::contract_line("tap: off (replaced)");
                }
                // Locked only now, with the old ring gone: never two at
                // once against the memory-lock limit. Unlocked works too.
                if !r.pin() && !lock_refused {
                    lock_refused = true;
                    crate::log::contract_line("tap: memory lock refused, the tap runs unlocked");
                }
                // Mapped before the name goes: a window that sees its
                // name gone and still Waiting knows the request was lost.
                r.mark_mapped(instance, LATENCY_SAMPLES as u32);
                // The ring keeps no name: from here on it is the two
                // processes' memory only (a second name was refused at
                // attach; one made since is caught here).
                r.unlink_if_at(request);
                if r.nameless() {
                    slot.attach(&r);
                    crate::log::contract_line("tap: on");
                    last_refusal.clear();
                    ring = Some(r);
                } else {
                    r.mark_released();
                    crate::log::contract_line("tap: request refused (it has another name)");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let why = e.to_string();
                if why != last_refusal {
                    crate::log::contract_line(&format!("tap: request refused ({why})"));
                    last_refusal = why;
                }
            }
        }
        thread::park_timeout(Duration::from_millis(tap::POLL_MS));
    }
    if let Some(r) = ring.take() {
        release(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_waits_for_a_running_copy_and_stops_further_ones() {
        let d =
            hushmic_shm::test_dir_base().join(format!("hushmic-view-slot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let meter = d.join("meter");
        let window = TapRing::request(&meter).unwrap();
        let r = TapRing::attach(&tap::request_path(&meter)).unwrap();
        r.pin();
        r.mark_mapped(1, 0);
        let slot = Arc::new(TapSlot::default());
        slot.attach(&r);
        assert!(slot.attached());
        let worker_slot = Arc::clone(&slot);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let copied = Arc::new(AtomicBool::new(false));
        let copied_w = Arc::clone(&copied);
        let worker = thread::spawn(move || {
            worker_slot.with(|t, _| {
                entered_tx.send(()).unwrap();
                thread::sleep(Duration::from_millis(30));
                t.write_hop(&mut TapCursor::default(), &[1.0; 480], &[1.0; 480]);
                copied_w.store(true, Ordering::Release);
            });
        });
        entered_rx.recv().unwrap();
        slot.detach();
        // Told by the copy, not a clock: detach came back after it.
        assert!(copied.load(Ordering::Acquire), "waited for the copy");
        worker.join().unwrap();
        assert_eq!(window.head().write_frames, 480, "the copy finished");
        let mut ran = false;
        slot.with(|_, _| ran = true);
        assert!(!ran && !slot.attached());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn modes_map_to_the_meter_codes() {
        assert_eq!(mode_code(Mode::Process), MODE_PROCESS);
        assert_eq!(mode_code(Mode::Bypass), MODE_BYPASS);
        assert_eq!(mode_code(Mode::Mute), MODE_MUTE);
        // The passthrough tier shows as unfiltered; bypass and mute stay.
        assert_eq!(hop_mode(MODE_PROCESS, true), MODE_PASSTHROUGH);
        assert_eq!(hop_mode(MODE_PROCESS, false), MODE_PROCESS);
        assert_eq!(hop_mode(MODE_BYPASS, true), MODE_BYPASS);
        assert_eq!(hop_mode(MODE_MUTE, true), MODE_MUTE);
    }
}
