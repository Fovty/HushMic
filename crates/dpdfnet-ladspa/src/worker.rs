//! The inference worker (issue #10): owns the engine off the RT thread.
//!
//! The RT callback exchanges samples with the worker through two
//! pre-sized wait-free SPSC rings and never blocks, allocates, or runs
//! inference — every quantum meets its deadline by construction. The
//! worker pops one hop at a time, runs it through the engine, and pushes
//! the result; a stall shows up as ring starvation, which the
//! [`Aligner`](crate::Aligner) turns into substituted silence instead of
//! host-inserted dropouts.

use hushmic_denoiser::{Denoiser, Mode, HOP};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Ring capacity in samples (~680 ms at 48 kHz) for both directions:
/// far beyond the largest PipeWire quantum (8192) plus any stall the
/// stall-headroom design intends to survive, so overflow only happens
/// when the worker is already ~680 ms behind (which the plugin degrades
/// to a stream restart — see [`WorkerHandle::request_reset`]). Fixed at
/// construction — the rings never reallocate.
pub const RING_CAPACITY: usize = 32_768;

/// The seam between the worker and the DSP: implemented by the real
/// [`Denoiser`] and by test fakes, so the whole threading/alignment
/// machinery is testable without ONNX Runtime or model files.
///
/// Contract (mirrors `Denoiser::process_hop`): `output` is ALWAYS
/// filled, even on `Err` — the stream must stay aligned through
/// transient failures.
pub trait HopEngine: Send + 'static {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String>;
    fn reset(&mut self);
    fn set_mode(&mut self, mode: Mode);
    fn set_attenuation_limit_db(&mut self, db: f32);
}

impl HopEngine for Denoiser {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        Denoiser::process_hop(self, input, output).map_err(|e| e.to_string())
    }
    fn reset(&mut self) {
        Denoiser::reset(self);
    }
    fn set_mode(&mut self, mode: Mode) {
        Denoiser::set_mode(self, mode);
    }
    fn set_attenuation_limit_db(&mut self, db: f32) {
        Denoiser::set_attenuation_limit_db(self, db);
    }
}

/// Cross-thread state. Controls travel as raw f32 bit patterns so the
/// worker can detect changes with a plain integer compare (NaN-proof).
struct Shared {
    attn_bits: AtomicU32,
    mode_bits: AtomicU32,
    /// Bumped by `request_reset_and_wait`; the worker drains its input,
    /// resets the engine, and echoes the epoch into `ack_epoch`.
    reset_epoch: AtomicU32,
    ack_epoch: AtomicU32,
    shutdown: AtomicBool,
    /// A panic escaped the engine: the worker is gone and the plugin
    /// degrades to the established working-but-silent node.
    engine_dead: AtomicBool,
}

/// The RT-side handle: sample rings, control stores, lifecycle.
pub struct WorkerHandle {
    input: rtrb::Producer<f32>,
    output: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    /// The epoch of the most recent reset request (RT-side only).
    requested_epoch: u32,
}

impl WorkerHandle {
    /// Spawn the worker and move `engine` into it. `None` if the OS
    /// refuses a thread (the caller degrades to the silent node).
    pub fn spawn(engine: impl HopEngine) -> Option<WorkerHandle> {
        let (in_prod, in_cons) = rtrb::RingBuffer::new(RING_CAPACITY);
        let (out_prod, out_cons) = rtrb::RingBuffer::new(RING_CAPACITY);
        let shared = Arc::new(Shared {
            attn_bits: AtomicU32::new(f32::NAN.to_bits()),
            mode_bits: AtomicU32::new(f32::NAN.to_bits()),
            reset_epoch: AtomicU32::new(0),
            ack_epoch: AtomicU32::new(0),
            shutdown: AtomicBool::new(false),
            engine_dead: AtomicBool::new(false),
        });
        let worker_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("hushmic-dsp".into())
            .spawn(move || {
                // Panics from process_hop are handled (and contained)
                // inside the loop; this outer guard covers the rest of
                // the HopEngine surface (reset/controls), so ANY escape
                // marks the engine dead instead of leaving a silently
                // missing worker behind a wrong diagnosis and a 2 s
                // timeout on every future activation.
                let loop_shared = Arc::clone(&worker_shared);
                if catch_unwind(AssertUnwindSafe(move || {
                    worker_loop(engine, in_cons, out_prod, loop_shared)
                }))
                .is_err()
                {
                    eprintln!("[dpdfnet-ladspa] worker panicked; the node goes silent");
                    worker_shared.engine_dead.store(true, Ordering::Release);
                }
            });
        match thread {
            Ok(t) => Some(WorkerHandle {
                input: in_prod,
                output: out_cons,
                shared,
                thread: Some(t),
                requested_epoch: 0,
            }),
            Err(e) => {
                eprintln!("[dpdfnet-ladspa] cannot spawn the inference worker: {e}");
                None
            }
        }
    }

    /// RT-safe: store the attenuation limit for the worker to apply
    /// before its next hop.
    pub fn set_attn_db(&self, db: f32) {
        self.shared.attn_bits.store(db.to_bits(), Ordering::Release);
    }

    /// RT-safe: store the raw Mode control value (the worker maps it
    /// exactly like the inline path did).
    pub fn set_mode_control(&self, v: f32) {
        self.shared.mode_bits.store(v.to_bits(), Ordering::Release);
    }

    /// RT-safe: wake the worker (a single futex wake).
    pub fn wake(&self) {
        if let Some(t) = &self.thread {
            t.thread().unpark();
        }
    }

    /// RT-safe: push as many of `samples` as fit; returns how many were
    /// accepted. Fewer than `samples.len()` means the ring is full — the
    /// caller restarts the stream (see [`Self::request_reset`]).
    pub fn push_input(&mut self, samples: &[f32]) -> usize {
        for (i, &s) in samples.iter().enumerate() {
            if self.input.push(s).is_err() {
                return i;
            }
        }
        samples.len()
    }

    /// RT-safe: samples currently poppable from the output ring.
    pub fn output_available(&self) -> usize {
        self.output.slots()
    }

    /// RT-safe: pop one produced sample.
    pub fn pop_output(&mut self) -> Option<f32> {
        self.output.pop().ok()
    }

    /// Throw away everything in the output ring (activation only —
    /// stale pre-reset audio must not leak into the new session).
    pub fn drain_output(&mut self) {
        while self.output.pop().is_ok() {}
    }

    /// A panic escaped the engine; the node is silent until re-instantiated.
    pub fn engine_dead(&self) -> bool {
        self.shared.engine_dead.load(Ordering::Acquire)
    }

    /// RT-safe, non-blocking: ask the worker to drain its input and
    /// reset the engine. Used for the stream-restart degradation when
    /// the input ring overflows (worker >680 ms behind): the caller
    /// emits silence and drains the output ring until [`Self::reset_acked`],
    /// then resumes fresh — resynced to now instead of replaying
    /// seconds-stale audio into a live call.
    pub fn request_reset(&mut self) {
        self.requested_epoch = self.requested_epoch.wrapping_add(1);
        self.shared
            .reset_epoch
            .store(self.requested_epoch, Ordering::Release);
        self.wake();
    }

    /// RT-safe: has the worker acknowledged the most recent
    /// [`Self::request_reset`]? Once true, the input ring has been
    /// drained and the engine reset; after one final `drain_output`
    /// (the ack is stored before the worker parks, so nothing new can
    /// appear until fresh input is pushed) the stream is clean.
    pub fn reset_acked(&self) -> bool {
        self.shared.ack_epoch.load(Ordering::Acquire) == self.requested_epoch
    }

    /// Activation handshake (NOT the RT path; the host calls activate
    /// before streaming): [`Self::request_reset`] plus a bounded wait
    /// for the ack. `false` = worker dead or stuck; the caller degrades
    /// to silence and may retry on the next activation. Call
    /// `drain_output` after a `true` return — the worker is parked with
    /// an empty input ring at that point, so no concurrent push can
    /// race the drain.
    pub fn request_reset_and_wait(&mut self, timeout: Duration) -> bool {
        self.request_reset();
        let deadline = Instant::now() + timeout;
        loop {
            if self.reset_acked() {
                return true;
            }
            if self.engine_dead() || Instant::now() > deadline {
                return false;
            }
            self.wake();
            thread::sleep(Duration::from_micros(500));
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            // The worker only ever parks or runs bounded hops, so this
            // terminates promptly.
            let _ = t.join();
        }
    }
}

fn worker_loop(
    mut engine: impl HopEngine,
    mut input: rtrb::Consumer<f32>,
    mut output: rtrb::Producer<f32>,
    shared: Arc<Shared>,
) {
    let mut seen_epoch = 0u32;
    // Caches start at the same NAN sentinel the shared atomics are
    // initialized with — NOT loaded from the atomics, or a control stored
    // between spawn and the first loop iteration would be swallowed.
    let mut attn_cache = f32::NAN.to_bits();
    let mut mode_cache = f32::NAN.to_bits();
    let mut hop_in = [0f32; HOP];
    let mut hop_out = [0f32; HOP];
    let mut err_logged = false;
    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            return;
        }
        let epoch = shared.reset_epoch.load(Ordering::Acquire);
        if epoch != seen_epoch {
            while input.pop().is_ok() {}
            engine.reset();
            err_logged = false; // a fresh session logs its own first failure
            seen_epoch = epoch;
            shared.ack_epoch.store(epoch, Ordering::Release);
            continue;
        }
        let attn = shared.attn_bits.load(Ordering::Acquire);
        if attn != attn_cache {
            engine.set_attenuation_limit_db(f32::from_bits(attn));
            attn_cache = attn;
        }
        let mode = shared.mode_bits.load(Ordering::Acquire);
        if mode != mode_cache {
            engine.set_mode(crate::mode_from_control(f32::from_bits(mode)));
            mode_cache = mode;
        }
        if input.slots() < HOP {
            // Timeout guards a lost wakeup; the RT side unparks on every
            // push, so this is normally a pure park.
            thread::park_timeout(Duration::from_millis(50));
            continue;
        }
        for s in hop_in.iter_mut() {
            *s = input.pop().unwrap_or(0.0);
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            engine.process_hop(&hop_in, &mut hop_out)
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => {
                // The engine contract fills `hop_out` even on Err (aligned
                // near-silence); emit it so the stream never desyncs.
                if !err_logged {
                    eprintln!("[dpdfnet-ladspa] inference failed (recovering per-hop): {msg}");
                    err_logged = true;
                }
            }
            Err(_) => {
                eprintln!("[dpdfnet-ladspa] engine panicked; the node goes silent");
                shared.engine_dead.store(true, Ordering::Release);
                return;
            }
        }
        let mut pushed = 0;
        while pushed < HOP {
            if shared.shutdown.load(Ordering::Acquire) {
                return;
            }
            if shared.reset_epoch.load(Ordering::Acquire) != seen_epoch {
                break; // abandon the partial hop; the reset drains everything
            }
            match output.push(hop_out[pushed]) {
                Ok(()) => pushed += 1,
                // Output ring full: the RT side stopped popping (host
                // paused the stream). Wait for space.
                Err(_) => thread::park_timeout(Duration::from_millis(1)),
            }
        }
    }
}
