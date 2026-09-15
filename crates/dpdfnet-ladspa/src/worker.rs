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

/// Fallback run budget when the process has no finite RTTIME limit.
/// The guard reserves at least 80 ms for another hop. This is a mitigation,
/// not a proof: a single inference can exceed any measured reserve.
pub const RUN_GUARD_AFTER: Duration = Duration::from_millis(120);
/// Minimum guard sleep; the sleep is one eighth of the accumulated run time.
pub const RUN_GUARD_MIN_PARK: Duration = Duration::from_millis(6);
const HOP_RESERVE: Duration = Duration::from_millis(80);
const PARK_BLOCKED_MIN: Duration = Duration::from_micros(100);

#[derive(Default)]
struct RunGuard {
    active: bool,
    run: Duration,
    longest_hop: Duration,
}

impl RunGuard {
    fn set_active(&mut self, active: bool) {
        if active != self.active {
            self.run = Duration::ZERO;
            self.longest_hop = Duration::ZERO;
        }
        self.active = active;
    }

    fn prepare(&mut self, armed: bool, forced: bool, realtime: bool) {
        self.set_active(armed || forced);
        if armed && !forced && !realtime {
            self.run = Duration::ZERO;
            self.longest_hop = Duration::ZERO;
        }
    }

    fn account(&mut self, elapsed: Duration) {
        if self.active {
            self.run += elapsed;
            self.longest_hop = self.longest_hop.max(elapsed);
        }
    }

    fn parked(&mut self, elapsed: Duration, blocked: bool) {
        // Elapsed time alone can also mean preemption. A voluntary context
        // switch confirms a wait; an unpark token alone resets nothing.
        if blocked && elapsed >= PARK_BLOCKED_MIN {
            self.run = Duration::ZERO;
        }
    }

    fn pause(&self, limit: Option<Duration>) -> Option<Duration> {
        let after = limit.map_or(RUN_GUARD_AFTER, |limit| {
            RUN_GUARD_AFTER.min(limit.saturating_sub(HOP_RESERVE.max(self.longest_hop)))
        });
        (self.active && self.run >= after).then(|| (self.run / 8).max(RUN_GUARD_MIN_PARK))
    }
}

fn rttime_limit() -> Option<Duration> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid output pointer, no process state is changed.
    if unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut limit) } == 0
        && limit.rlim_cur != libc::RLIM_INFINITY
    {
        Some(Duration::from_micros(limit.rlim_cur))
    } else {
        None
    }
}

fn voluntary_switches() -> Option<libc::c_long> {
    // SAFETY: rusage is a plain output structure.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid output pointer for the calling thread.
    (unsafe { libc::getrusage(libc::RUSAGE_THREAD, &mut usage) } == 0).then_some(usage.ru_nvcsw)
}

fn park_accounted(guard: &mut RunGuard, timeout: Duration) {
    let before = voluntary_switches();
    let started = Instant::now();
    thread::park_timeout(timeout);
    let elapsed = started.elapsed();
    let blocked = before.zip(voluntary_switches()).is_some_and(|(a, b)| b > a);
    guard.parked(elapsed, blocked);
}

#[derive(Default)]
struct LagWindow(Vec<u32>);

impl LagWindow {
    fn observe(&mut self, queued: usize, per_cycle: usize) -> u32 {
        let raw = queued.saturating_sub(1) as u32;
        // A complete fresh callback is occupancy, not overdue work.
        let overdue = queued.saturating_sub(per_cycle) as u32;
        if overdue >= crate::ladder::LAG_PANIC {
            self.0.clear();
        }
        self.0.push(raw);
        let excess = self.0.len().saturating_sub(per_cycle);
        self.0.drain(..excess);
        self.0.iter().copied().min().unwrap_or(raw).min(overdue)
    }
}

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
    /// Overdue hops after allowing for a fresh host callback burst,
    /// smoothed over one callback. Default: ignored.
    fn set_lag_hops(&mut self, _hops: u32) {}
    /// The worker slept this long on purpose (the run guard) since the
    /// last hop: time the live path spent not producing, which the engine's
    /// cost model would otherwise not see. Default: ignored.
    fn note_pause(&mut self, _pause: Duration) {}
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
    shutdown: Arc<AtomicBool>,
    rt_pending: Arc<AtomicBool>,
    force_guard: AtomicBool,
    /// A panic escaped the engine: the worker is gone and the plugin
    /// degrades to the established working-but-silent node.
    engine_dead: AtomicBool,
    overflow_report: AtomicBool,
    /// The host's cycle size in samples, stored by `note_quantum` on the
    /// RT side. A cycle larger than one hop pushes several hops at once, so
    /// the raw lag reading alternates; the worker smooths it over one
    /// cycle's worth of hops.
    quantum: AtomicU32,
    /// The helper requested guard arming. It must wait for rt_ready before
    /// requesting a policy change. The worker checks the actual policy too.
    rt_granted: AtomicBool,
    rt_ready: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        Self {
            attn_bits: AtomicU32::new(f32::NAN.to_bits()),
            mode_bits: AtomicU32::new(f32::NAN.to_bits()),
            reset_epoch: AtomicU32::new(0),
            ack_epoch: AtomicU32::new(0),
            shutdown: Arc::new(AtomicBool::new(false)),
            rt_pending: Arc::new(AtomicBool::new(true)),
            force_guard: AtomicBool::new(false),
            engine_dead: AtomicBool::new(false),
            overflow_report: AtomicBool::new(false),
            quantum: AtomicU32::new(HOP as u32),
            rt_granted: AtomicBool::new(false),
            rt_ready: AtomicBool::new(false),
        }
    }
}

/// The RT-side handle: sample rings, control stores, lifecycle.
pub struct WorkerHandle {
    input: rtrb::Producer<f32>,
    output: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    /// The realtime helper. Drop waits briefly, then detaches with the
    /// plugin mapping retained for any unfinished thread.
    rt_helper: Option<JoinHandle<()>>,
    /// The epoch of the most recent reset request (RT-side only).
    requested_epoch: u32,
}

impl WorkerHandle {
    /// Spawn the worker and move `engine` into it. `None` if the OS
    /// refuses a thread (the caller degrades to the silent node).
    pub fn spawn(engine: impl HopEngine) -> Option<WorkerHandle> {
        Self::spawn_with(engine, true)
    }

    /// Test seam: a worker that never asks for realtime. Paced tests drive
    /// the stream from a timeshare thread; as root (a CI container) a
    /// realtime worker would starve that thread, the exact case the data
    /// loop rule in rt.rs keeps out of production.
    pub fn spawn_timeshare(engine: impl HopEngine) -> Option<WorkerHandle> {
        Self::spawn_with(engine, false)
    }

    fn spawn_with(engine: impl HopEngine, request_rt: bool) -> Option<WorkerHandle> {
        let can_detach = request_rt && crate::rt::keep_plugin_loaded();
        let (in_prod, in_cons) = rtrb::RingBuffer::new(RING_CAPACITY);
        let (out_prod, out_cons) = rtrb::RingBuffer::new(RING_CAPACITY);
        let shared = Arc::new(Shared::new());
        let worker_shared = Arc::clone(&shared);
        // The worker reports its kernel tid so a helper can ask for
        // realtime scheduling on its behalf (rt.rs) without the worker ever
        // blocking on D-Bus.
        let (tid_tx, tid_rx) = std::sync::mpsc::channel::<libc::pid_t>();
        let thread = thread::Builder::new()
            .name("hushmic-dsp".into())
            .spawn(move || {
                // SAFETY: gettid has no preconditions.
                let tid = unsafe { libc::syscall(libc::SYS_gettid) } as libc::pid_t;
                let _ = tid_tx.send(tid);
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
                // Keep the tid alive while a broker might still apply a grant.
                while worker_shared.rt_pending.load(Ordering::Acquire) {
                    thread::park_timeout(Duration::from_millis(5));
                }
            });
        match thread {
            Ok(t) => Some(WorkerHandle {
                input: in_prod,
                output: out_cons,
                rt_helper: if request_rt {
                    spawn_rt_helper(tid_rx, Arc::clone(&shared), can_detach)
                } else {
                    shared.rt_pending.store(false, Ordering::Release);
                    None
                },
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

    /// Test seam: run the guard as if realtime had been granted (the test
    /// binary never gets it).
    pub fn force_run_guard(&self) {
        self.shared.force_guard.store(true, Ordering::Release);
    }

    /// The realtime request finished, granted or not. A test seam: a
    /// worker that gets realtime (root in a container) is woken by the
    /// grant handshake, so exact lag assertions wait for this first.
    pub fn rt_settled(&self) -> bool {
        !self.shared.rt_pending.load(Ordering::Acquire)
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

    /// RT-safe: record the host's cycle size (samples per `run()`).
    pub fn note_quantum(&self, samples: usize) {
        self.shared
            .quantum
            .store(samples.min(u32::MAX as usize) as u32, Ordering::Release);
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
        self.note_quantum(samples.len());
        for (i, &s) in samples.iter().enumerate() {
            if self.input.push(s).is_err() {
                self.shared.overflow_report.store(true, Ordering::Release);
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

/// Wait for the worker's tid, request realtime for it, log the one
/// contract line. Never blocks the worker or the host.
fn spawn_rt_helper(
    tid_rx: std::sync::mpsc::Receiver<libc::pid_t>,
    shared: Arc<Shared>,
    can_detach: bool,
) -> Option<JoinHandle<()>> {
    if !can_detach {
        shared.rt_pending.store(false, Ordering::Release);
        crate::log::contract_line("worker: realtime priority not available (cannot retain plugin)");
        return None;
    }
    let helper_shared = Arc::clone(&shared);
    let spawned = thread::Builder::new()
        .name("hushmic-rt".into())
        .spawn(move || {
            let shared = helper_shared;
            let deadline = Instant::now() + crate::rt::REQUEST_TIMEOUT;
            let Ok(tid) = tid_rx.recv_timeout(crate::rt::REQUEST_TIMEOUT) else {
                shared.rt_pending.store(false, Ordering::Release);
                return;
            };
            if !arm_rt_guard(&shared, deadline) {
                shared.rt_pending.store(false, Ordering::Release);
                return;
            }
            let outcome = crate::rt::request_realtime_bounded(
                tid,
                Arc::clone(&shared.shutdown),
                Arc::clone(&shared.rt_pending),
                deadline.saturating_duration_since(Instant::now()),
            );
            crate::log::contract_line(&crate::rt::contract(&outcome));
        });
    match spawned {
        Ok(h) => Some(h),
        Err(e) => {
            shared.rt_pending.store(false, Ordering::Release);
            crate::log::contract_line(&format!(
                "worker: realtime priority not available (no thread: {e})"
            ));
            None
        }
    }
}

fn arm_rt_guard(shared: &Shared, deadline: Instant) -> bool {
    // Arm before any syscall or broker can change the worker policy.
    shared.rt_granted.store(true, Ordering::Release);
    while !shared.rt_ready.load(Ordering::Acquire) {
        if shared.shutdown.load(Ordering::Acquire) || Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(1));
    }
    true
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        if let Some(t) = &self.thread {
            t.thread().unpark();
        }
        let deadline = Instant::now() + Duration::from_millis(50);
        for handle in [self.thread.take(), self.rt_helper.take()]
            .into_iter()
            .flatten()
        {
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else if !crate::rt::keep_plugin_loaded() {
                // Without a permanent code mapping detaching is unsafe.
                let _ = handle.join();
            }
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
    let mut overflow_logged = false;
    let mut guard = RunGuard::default();
    // Raw lag readings over the last cycle's worth of hops (see
    // `Shared::quantum`); the minimum is what the engine sees.
    let mut lag_window = LagWindow(Vec::with_capacity(RING_CAPACITY / HOP + 1));
    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            return;
        }
        // Observe the actual policy so broker latency and timeshare work
        // cannot become a guard sleep when the grant arrives late.
        let armed = shared.rt_granted.load(Ordering::Acquire);
        let forced = shared.force_guard.load(Ordering::Acquire);
        let realtime = armed && crate::rt::current_thread_is_realtime();
        guard.prepare(armed, forced, realtime);
        if armed {
            shared.rt_ready.store(true, Ordering::Release);
        }
        if shared.overflow_report.swap(false, Ordering::AcqRel) && !overflow_logged {
            eprintln!("[dpdfnet-ladspa] input ring overflow; restarting the stream");
            overflow_logged = true;
        }
        let epoch = shared.reset_epoch.load(Ordering::Acquire);
        if epoch != seen_epoch {
            while input.pop().is_ok() {}
            lag_window.0.clear();
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
            // The lag window survives the park: with a cycle of several
            // hops the first reading after a push is legitimately high, and
            // only the previous cycle's trailing reading says it drained.
            park_accounted(&mut guard, Duration::from_millis(50));
            continue;
        }
        let pause = if realtime || forced {
            guard.pause(rttime_limit())
        } else {
            None
        };
        if let Some(pause) = pause {
            let started = Instant::now();
            thread::sleep(pause);
            engine.note_pause(started.elapsed());
            guard.run = Duration::ZERO;
        }
        let queued = input.slots() / HOP;
        let per_cycle = (shared.quantum.load(Ordering::Acquire) as usize)
            .div_ceil(HOP)
            .max(1);
        engine.set_lag_hops(lag_window.observe(queued, per_cycle));
        for s in hop_in.iter_mut() {
            *s = input.pop().unwrap_or(0.0);
        }
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| {
            engine.process_hop(&hop_in, &mut hop_out)
        }));
        guard.account(started.elapsed());
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
                Err(_) => park_accounted(&mut guard, Duration::from_millis(1)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_grant_discards_timeshare_run() {
        let mut guard = RunGuard::default();
        guard.account(Duration::from_secs(5));
        for _ in 0..500 {
            guard.prepare(true, false, false);
            guard.account(Duration::from_millis(10));
        }
        guard.prepare(true, false, false);
        guard.prepare(true, false, true);
        assert_eq!(guard.run, Duration::ZERO);
        assert_eq!(guard.pause(None), None);
        guard.account(Duration::from_millis(120));
        assert_eq!(guard.pause(None), Some(Duration::from_millis(15)));
    }

    #[test]
    fn guard_is_armed_before_a_grant_can_preempt_the_helper() {
        let mut guard = RunGuard {
            run: Duration::from_secs(5),
            ..RunGuard::default()
        };
        guard.prepare(true, false, false);
        assert!(guard.active);
        assert_eq!(guard.run, Duration::ZERO);
        // The worker publishes rt_ready only after this preparation.
        guard.account(Duration::from_millis(120));
        guard.prepare(true, false, true);
        assert_eq!(guard.pause(None), Some(Duration::from_millis(15)));
    }

    #[test]
    fn grant_handshake_waits_for_the_worker_to_arm_accounting() {
        let shared = Arc::new(Shared::new());
        let helper_shared = Arc::clone(&shared);
        let (granted, grant) = std::sync::mpsc::channel();
        let helper = thread::spawn(move || {
            assert!(arm_rt_guard(
                &helper_shared,
                Instant::now() + Duration::from_secs(1)
            ));
            granted.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !shared.rt_granted.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(grant.try_recv().is_err());
        let mut guard = RunGuard::default();
        guard.prepare(true, false, false);
        shared.rt_ready.store(true, Ordering::Release);
        grant.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(guard.active);
        helper.join().unwrap();
    }

    #[test]
    fn consumed_park_token_does_not_clear_run() {
        std::thread::spawn(|| {
            let mut guard = RunGuard::default();
            guard.set_active(true);
            guard.account(RUN_GUARD_AFTER);
            std::thread::current().unpark();
            park_accounted(&mut guard, Duration::from_millis(1));
            assert_eq!(guard.run, RUN_GUARD_AFTER);
            guard.parked(Duration::from_millis(50), false);
            assert_eq!(guard.run, RUN_GUARD_AFTER, "preemption is not a wait");
            guard.parked(Duration::from_millis(1), true);
            assert_eq!(guard.run, Duration::ZERO);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn finite_rttime_reserves_one_more_hop() {
        let mut guard = RunGuard::default();
        guard.set_active(true);
        guard.account(Duration::from_millis(110));
        assert!(guard.pause(None).is_none());
        assert!(guard.pause(Some(Duration::from_millis(200))).is_some());
        guard.run = Duration::from_millis(19);
        guard.longest_hop = Duration::ZERO;
        assert!(guard.pause(Some(Duration::from_millis(100))).is_none());
        guard.account(Duration::from_millis(1));
        assert!(guard.pause(Some(Duration::from_millis(100))).is_some());
        assert!(guard.pause(Some(Duration::ZERO)).is_some());
    }

    #[test]
    fn quantum_shrink_trims_the_entire_lag_window() {
        let mut window = LagWindow::default();
        for queued in (1..=18).rev() {
            assert_eq!(window.observe(queued, 18), 0);
        }
        assert_eq!(window.0.len(), 18);
        assert_eq!(window.observe(3, 1), 2);
        assert_eq!(window.0, vec![2]);
    }

    #[test]
    fn overdue_work_still_triggers_panic_after_a_large_burst() {
        let mut window = LagWindow::default();
        assert_eq!(window.observe(18, 18), 0);
        assert_eq!(window.observe(22, 18), 4);
    }
}
