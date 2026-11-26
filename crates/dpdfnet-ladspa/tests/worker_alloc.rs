//! The DSP worker allocates nothing per hop, also while it publishes the
//! engine meter and copies every hop into a mapped Mic check tap (the tap's
//! promise: no allocation, no lock, no wait on the worker). A counting
//! allocator counts the worker thread's allocations only; the test thread
//! and the tap's helper thread may allocate as they like.
//!
//! One test in its own binary: the worker is found by its thread name.

use dpdfnet_ladspa::{EngineView, HopEngine, WorkerHandle};
use hushmic_denoiser::{Mode, HOP};
use hushmic_shm::tap::{TapRing, TapState};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static WORKER_TID: AtomicI64 = AtomicI64::new(-1);
static WORKER_ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

fn count() {
    let tid = WORKER_TID.load(Ordering::Relaxed);
    // SAFETY: gettid has no preconditions and allocates nothing.
    if tid >= 0 && unsafe { libc::syscall(libc::SYS_gettid) } == tid {
        WORKER_ALLOCS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: forwards every call to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        count();
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        count();
        System.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        count();
        System.realloc(p, l, n)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Output = input, times 0.5: an engine that allocates nothing itself.
struct Half;

impl HopEngine for Half {
    fn process_hop(&mut self, input: &[f32; HOP], output: &mut [f32; HOP]) -> Result<(), String> {
        for (o, &x) in output.iter_mut().zip(input) {
            *o = x * 0.5;
        }
        Ok(())
    }
    fn reset(&mut self) {}
    fn set_mode(&mut self, _: Mode) {}
    fn set_attenuation_limit_db(&mut self, _: f32) {}
}

/// The thread id of this process's only "hushmic-dsp" thread.
fn worker_tid() -> i64 {
    let mut found = Vec::new();
    for t in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let comm = std::fs::read_to_string(t.path().join("comm")).unwrap_or_default();
        if comm.trim() == "hushmic-dsp" {
            found.push(t.file_name().to_string_lossy().parse::<i64>().unwrap());
        }
    }
    assert_eq!(found.len(), 1, "one worker thread");
    found[0]
}

/// Push `hops` hops through the worker the way the plugin's run() does
/// and take its output back, without allocating on this thread's hot
/// path (the buffers are made once).
fn run_hops(w: &mut WorkerHandle, ring: &TapRing, input: &[f32], hops: usize) {
    for h in 0..hops {
        ring.heartbeat();
        let block = &input[(h % 100) * HOP..(h % 100 + 1) * HOP];
        w.note_quantum(HOP);
        assert_eq!(w.push_input(block), HOP);
        w.wake();
        let deadline = Instant::now() + Duration::from_secs(10);
        while w.output_available() < HOP {
            assert!(Instant::now() < deadline, "worker never caught up");
            w.wake();
            std::thread::sleep(Duration::from_micros(200));
        }
        for _ in 0..HOP {
            w.pop_output();
        }
    }
}

#[test]
fn the_worker_allocates_nothing_per_hop_with_the_meter_and_the_tap_on() {
    let dir = hushmic_shm::test_dir_base().join(format!("hushmic-alloc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let meter = dir.join("meter-1");
    let view = EngineView::create(&meter).unwrap();
    let mut w = WorkerHandle::spawn_timeshare_with_view(Half, view).expect("spawn");
    assert!(w.request_reset_and_wait(Duration::from_secs(2)));
    w.drain_output();

    let ring = TapRing::request(&meter).unwrap();
    let t = Instant::now();
    while !matches!(ring.state(), TapState::Mapped { .. }) {
        assert!(t.elapsed() < Duration::from_secs(3), "tap never mapped");
        ring.heartbeat();
        std::thread::sleep(Duration::from_millis(20));
    }
    let input: Vec<f32> = (0..100 * HOP)
        .map(|i| (i as f32 * 0.01).sin() * 0.3)
        .collect();
    // Warm up: the first hops into the ring, first-touch paths.
    run_hops(&mut w, &ring, &input, 50);
    let before = ring.head().write_frames;

    WORKER_TID.store(worker_tid(), Ordering::Relaxed);
    WORKER_ALLOCS.store(0, Ordering::Relaxed);
    run_hops(&mut w, &ring, &input, 300);
    WORKER_TID.store(-1, Ordering::Relaxed);
    let allocs = WORKER_ALLOCS.load(Ordering::Relaxed);

    assert_eq!(
        ring.head().write_frames - before,
        300 * HOP as u64,
        "every hop went into the tap"
    );
    assert_eq!(allocs, 0, "the worker allocated {allocs} times in 300 hops");
    ring.close();
    drop(w);
    let _ = std::fs::remove_dir_all(&dir);
}
