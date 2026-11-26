//! HushMic's chain hands the plugin a control file (`HUSHMIC_CONTROL`) that
//! stands in for the Mode and Attenuation Limit ports. Driven through the
//! LADSPA descriptor as a host does, with the port set against the file in
//! every phase so the audio says which one the plugin followed:
//! - a valid file wins over the ports from the first cycle, for every
//!   instance in the process, the strength as well as the mode;
//! - an invalid word keeps the last valid one (a mute stays a mute);
//! - a file this plugin cannot honour leaves the ports in charge, says so
//!   once, and leaves `reader` at 0;
//! - a page the kernel would not lock in RAM is still followed, and said
//!   once;
//! - `run()` allocates nothing with the file mapped.
//!
//! The mapping is made once per process, so each case runs in its own
//! subprocess (harness-less, as `host_light_model`). Asset-gated.
//!
//! Realtime suite: the plugin runs at real-time pace here (a worker that
//! falls behind substitutes silence, as in a call), so it needs the CPU a
//! live stream needs. The timing sweep proves it at a 25% CPU quota, not
//! the 6% the other suites get; a failure at 25% is a real finding. A
//! starved engine stepping down to passthrough is not one: the strength is
//! judged on the hops the meter says were filtered. (At 25% most windows
//! pass through; at 50% the engine stays on quality and every check, the
//! stream's level included, runs.)

mod common;

use dpdfnet_ladspa::get_ladspa_descriptor;
use hushmic_shm::control::{ControlWriter, MODE_BYPASS, MODE_MUTE, MODE_PROCESS};
use hushmic_shm::meter::{MeterReader, MODE_PASSTHROUGH};
use ladspa::{Plugin, PluginDescriptor, PortConnection, PortData};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const Q: usize = 480;
/// Port values (the Mode port's codes).
const PROCESS: f32 = 0.0;
const MUTE: f32 = 2.0;

thread_local! {
    /// Set while this thread is inside the plugin's `run()`.
    static IN_RUN: Cell<bool> = const { Cell::new(false) };
}
static RUN_ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

fn count() {
    if IN_RUN.with(Cell::get) {
        RUN_ALLOCS.fetch_add(1, Ordering::Relaxed);
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

fn descriptor() -> PluginDescriptor {
    get_ladspa_descriptor(0).expect("descriptor 0")
}

/// The committed public speech clip (voice the engine keeps in every
/// mode but mute), mono f32.
fn speech() -> Vec<f32> {
    let p = common::repo_root().join("tests/fixtures/noisy_public_48k.flac");
    let mut r = claxon::FlacReader::open(&p).expect("open flac");
    r.samples()
        .map(|s| s.expect("flac sample") as f32 / 32768.0)
        .collect()
}

/// One instance, as a host drives it: a cycle of speech every 10 ms.
struct Host {
    plugin: Box<dyn Plugin + Send>,
    speech: std::rc::Rc<Vec<f32>>,
    /// Feed steady white noise instead of speech.
    noise: bool,
    n: usize,
}

/// The white noise's amplitude (uniform in +-NOISE; RMS NOISE / sqrt 3).
const NOISE: f32 = 0.1;

impl Host {
    fn new() -> Host {
        let d = descriptor();
        let mut plugin = (d.new)(&d, 48_000);
        plugin.activate();
        Host {
            plugin,
            speech: std::rc::Rc::new(speech()),
            noise: false,
            n: 0,
        }
    }

    /// One cycle with the ports at `db` and `mode`; the output.
    fn cycle(&mut self, db: f32, mode: f32) -> Vec<f32> {
        let len = self.speech.len();
        let input: Vec<f32> = (0..Q)
            .map(|k| {
                let i = self.n * Q + k;
                if self.noise {
                    // A fixed hash per sample: deterministic white noise.
                    let h = (i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 40;
                    NOISE * (h as f32 / (1u64 << 24) as f32 * 2.0 - 1.0)
                } else {
                    self.speech[i % len]
                }
            })
            .collect();
        self.n += 1;
        let mut out = vec![0f32; Q];
        {
            let d = descriptor();
            let conns = [
                PortConnection {
                    port: d.ports[0],
                    data: PortData::AudioInput(&input),
                },
                PortConnection {
                    port: d.ports[1],
                    data: PortData::AudioOutput(RefCell::new(&mut out[..])),
                },
                PortConnection {
                    port: d.ports[2],
                    data: PortData::ControlInput(&db),
                },
                PortConnection {
                    port: d.ports[3],
                    data: PortData::ControlInput(&mode),
                },
            ];
            let refs: Vec<&PortConnection> = conns.iter().collect();
            IN_RUN.with(|f| f.set(true));
            self.plugin.run(Q, &refs);
            IN_RUN.with(|f| f.set(false));
        }
        std::thread::sleep(Duration::from_millis(10)); // real-time pacing
        out
    }

    /// `cycles` cycles, all of them exact silence.
    fn silent_for(&mut self, cycles: usize, db: f32, mode: f32, what: &str) {
        for c in 0..cycles {
            let out = self.cycle(db, mode);
            assert!(out.iter().all(|&v| v == 0.0), "{what}: sound in cycle {c}");
        }
    }

    /// Cycles until one carries sound (within `cycles`).
    fn sounds_within(&mut self, cycles: usize, db: f32, mode: f32, what: &str) {
        for _ in 0..cycles {
            if self.cycle(db, mode).iter().any(|v| v.abs() > 0.01) {
                return;
            }
        }
        panic!("{what}: no sound in {cycles} cycles");
    }

    /// Cycles until 20 in a row are exact silence (within `cycles`).
    fn falls_silent_within(&mut self, cycles: usize, db: f32, mode: f32, what: &str) {
        let mut run = 0;
        for _ in 0..cycles {
            run = if self.cycle(db, mode).iter().all(|&v| v == 0.0) {
                run + 1
            } else {
                0
            };
            if run == 20 {
                return;
            }
        }
        panic!("{what}: not silent within {cycles} cycles");
    }
}

/// Store a raw word at `offset` past the writer (which refuses invalid
/// values): what a garbled store would leave in the page.
fn poke(path: &Path, offset: u64, word: u32) {
    use std::os::unix::fs::FileExt;
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open");
    f.write_at(&word.to_le_bytes(), offset).expect("write");
}

/// The `reader` word (offset 24) as the file holds it.
fn reader_word(path: &Path) -> u32 {
    let raw = std::fs::read(path).expect("read control file");
    u32::from_le_bytes(raw[24..28].try_into().unwrap())
}

/// A valid file: it wins over the ports, its last valid word holds.
fn file_case(dir: &Path) {
    let path = dir.join("control-1");
    let w = ControlWriter::create(&path, MODE_MUTE, 100.0).expect("control file");
    std::env::set_var(hushmic_shm::CONTROL_ENV, &path);
    // The engine's own word on each hop: the meter, as the app reads it.
    // With no light model the ladder is quality or passthrough, so a hop
    // the meter calls processed was filtered by the quality model.
    let meter_path = dir.join("meter-1");
    std::env::set_var(hushmic_shm::METER_ENV, &meter_path);
    std::env::set_var(
        "HUSHMIC_FALLBACK_MODEL_PATH",
        dir.join("no-light-model.onnx"),
    );
    let mut h = Host::new();
    assert!(w.honoured(), "the plugin says it honours the file");
    // Muted by the file from the first cycle, whatever the port says.
    h.silent_for(40, 100.0, PROCESS, "file mute, port process");
    // Processing and bypass by the file while the port says mute: the
    // voice comes through.
    assert!(w.set_mode(MODE_PROCESS));
    h.sounds_within(150, 100.0, MUTE, "file process, port mute");
    assert!(w.set_mode(MODE_MUTE));
    h.falls_silent_within(80, 100.0, PROCESS, "file mute, port process");
    assert!(w.set_mode(MODE_BYPASS));
    h.sounds_within(150, 100.0, MUTE, "file bypass, port mute");
    // Muted again; then garbled words keep the mute (never processing).
    assert!(w.set_mode(MODE_MUTE));
    h.falls_silent_within(80, 100.0, PROCESS, "file mute again");
    for bad in [3, 7, u32::MAX] {
        assert!(!w.set_mode(bad), "the writer refuses {bad}");
        poke(&path, 16, bad);
        poke(&path, 20, f32::NAN.to_bits());
        h.silent_for(15, 100.0, PROCESS, &format!("garbled mode {bad}"));
    }
    // Every instance in the process follows the same file.
    assert!(w.set_mode(MODE_MUTE));
    assert!(w.set_attn_db(40.0));
    let mut second = Host::new();
    second.silent_for(40, 100.0, PROCESS, "second instance");
    h.silent_for(5, 100.0, PROCESS, "first instance");
    assert_eq!(
        RUN_ALLOCS.load(Ordering::Relaxed),
        0,
        "run() allocated with the control file mapped"
    );
    // The file's strength reaches the output, not the port's: on steady
    // noise, 0 dB passes it and 100 dB removes it, whatever the port says.
    // Under CPU starvation the ladder steps down to passthrough, which is
    // right, and passthrough has no strength to apply. So the strength is
    // judged by each hop's own input and output on the hops the meter says
    // the quality model filtered; the hops it passed through only show that
    // the file's mode held (no hop is the port's mute). The stream's level
    // is judged as well while every hop of the window was on quality.
    assert!(w.set_mode(MODE_PROCESS));
    h.noise = true;
    let noise_rms = NOISE / 3f32.sqrt();
    let mut meter = MeterReader::open(&meter_path).expect("the plugin's meter");
    let mut hops = Vec::new();
    for (file_db, port_db, loud) in [(0.0, 100.0, true), (100.0, 0.0, false)] {
        assert!(w.set_attn_db(file_db));
        for _ in 0..50 {
            h.cycle(port_db, MUTE); // settle (the port's mute is ignored too)
        }
        meter.read(&mut hops);
        hops.clear();
        let mut sum = 0f64;
        for _ in 0..30 {
            sum += h
                .cycle(port_db, MUTE)
                .iter()
                .map(|&v| (v as f64) * (v as f64))
                .sum::<f64>();
        }
        let rms = (sum / (30 * Q) as f64).sqrt() as f32;
        meter.read(&mut hops);
        let what = format!("file {file_db} dB, port {port_db}");
        assert!(!hops.is_empty(), "{what}: the meter shows no hop");
        assert!(
            hops.iter()
                .all(|h| h.mode == MODE_PROCESS || h.mode == MODE_PASSTHROUGH),
            "{what}: a hop not processing as the file says: {:?}",
            hops.iter().map(|h| h.mode).collect::<Vec<_>>()
        );
        // Hops near a change of tier mix the two; judged are the others.
        let settled = |i: usize| {
            let (lo, hi) = (i.saturating_sub(8), (i + 8).min(hops.len() - 1));
            hops[lo..=hi].iter().all(|h| h.mode == hops[i].mode)
        };
        let ratio = |mode: u32| {
            let (mut i_ms, mut o_ms, mut n) = (0f64, 0f64, 0);
            for (i, h) in hops.iter().enumerate() {
                if h.mode == mode && settled(i) {
                    i_ms += h.in_ms as f64;
                    o_ms += h.out_ms as f64;
                    n += 1;
                }
            }
            (n, o_ms / i_ms.max(f64::MIN_POSITIVE))
        };
        let (filtered, kept) = ratio(MODE_PROCESS);
        if filtered > 0 {
            if loud {
                assert!(kept > 0.25, "{what}: quality kept {kept} of the noise");
            } else {
                assert!(kept < 0.01, "{what}: quality kept {kept} of the noise");
            }
        }
        let (passed, through) = ratio(MODE_PASSTHROUGH);
        if passed > 0 {
            assert!(
                (0.5..2.0).contains(&through),
                "{what}: passthrough kept {through} of the noise"
            );
        }
        if hops.iter().all(|h| h.mode == MODE_PROCESS) {
            if loud {
                assert!(rms > 0.5 * noise_rms, "{what}: {rms}");
            } else {
                assert!(rms < 0.1 * noise_rms, "{what}: {rms}");
            }
        } else {
            eprintln!(
                "{what}: {filtered} of {} hops judged on quality, {passed} passed through",
                hops.len()
            );
        }
        hops.clear();
    }
    assert_eq!(RUN_ALLOCS.load(Ordering::Relaxed), 0, "run() allocated");
    println!("file case ok");
}

/// Whether this process may lock a page in RAM now. A zero memlock limit
/// does not stop a process with CAP_IPC_LOCK (a CI job run as root).
fn may_lock_a_page() -> bool {
    // SAFETY: an anonymous page of our own: mapped, locked, unlocked and
    // unmapped again; nothing else touches it.
    unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        if p == libc::MAP_FAILED {
            return false;
        }
        let locked = libc::mlock(p, 4096) == 0;
        if locked {
            libc::munlock(p, 4096);
        }
        libc::munmap(p, 4096);
        locked
    }
}

/// No memlock allowance: the page cannot be locked (unless the process may
/// lock regardless, which it reports); followed either way.
fn unlocked_case(dir: &Path) {
    let none = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: setrlimit with a valid struct; lowering our own limit.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &none) }, 0);
    // What the kernel does now, for the parent to expect the line or not.
    let may = may_lock_a_page();
    eprintln!(
        "unlocked case: mlock {}",
        if may { "allowed" } else { "refused" }
    );
    let path = dir.join("control-3");
    let w = ControlWriter::create(&path, MODE_MUTE, 100.0).expect("control file");
    std::env::set_var(hushmic_shm::CONTROL_ENV, &path);
    let mut h = Host::new();
    let _second = Host::new(); // one log line per process, not per instance
    assert!(w.honoured(), "followed, locked or not");
    h.silent_for(30, 100.0, PROCESS, "file mute, page unlocked");
    println!("unlocked case ok");
}

/// A file this plugin cannot honour (a newer version): the ports.
fn foreign_case(dir: &Path) {
    let path = dir.join("control-2");
    let mut raw = vec![0u8; 4096];
    raw[0..8].copy_from_slice(b"HMCTRL\0\0");
    raw[8..12].copy_from_slice(&2u32.to_le_bytes());
    raw[12..16].copy_from_slice(&4096u32.to_le_bytes());
    raw[16..20].copy_from_slice(&MODE_MUTE.to_le_bytes());
    raw[20..24].copy_from_slice(&100f32.to_bits().to_le_bytes());
    std::fs::write(&path, raw).expect("write");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::env::set_var(hushmic_shm::CONTROL_ENV, &path);
    let mut h = Host::new();
    let _second = Host::new(); // one log line per process, not per instance
    assert_eq!(reader_word(&path), 0, "reader stays 0");
    // The file says mute; the port says process: the port wins.
    h.sounds_within(150, 100.0, PROCESS, "port process");
    h.falls_silent_within(80, 100.0, MUTE, "port mute");
    println!("foreign case ok");
}

fn main() {
    if let Ok(case) = std::env::var("HUSHMIC_CONTROL_CASE") {
        let dir = PathBuf::from(std::env::var_os("HUSHMIC_CONTROL_DIR").expect("dir"));
        match case.as_str() {
            "file" => file_case(&dir),
            "foreign" => foreign_case(&dir),
            "unlocked" => unlocked_case(&dir),
            other => panic!("unknown case {other}"),
        }
        return;
    }
    let (Some(model), Some(runtime)) = (
        common::model_path("dpdfnet2_48khz_hr.onnx"),
        common::runtime_path(),
    ) else {
        println!("SKIP control_file: dev assets not provisioned");
        return;
    };
    // Earlier runs' directories whose process is gone (a failed case).
    let base = hushmic_shm::test_dir_base();
    for e in std::fs::read_dir(&base).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(pid) = name.strip_prefix("hushmic-control-") {
            if !Path::new("/proc").join(pid).exists() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let dir = base.join(format!("hushmic-control-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    // Removed however this ends, a failed case included.
    struct Cleanup<'a>(&'a Path);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0);
        }
    }
    let _cleanup = Cleanup(&dir);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env = [
        ("HUSHMIC_MODEL_PATH", model.as_os_str()),
        ("ORT_DYLIB_PATH", runtime.as_os_str()),
        ("HUSHMIC_CONTROL_DIR", dir.as_os_str()),
    ];
    let log = case("file", &env);
    assert!(!log.contains("control: not available"), "{log}");
    for l in log.lines().filter(|l| l.contains("hops judged on quality")) {
        println!("note: {l}");
    }
    // Locked where the runner allows a page (the case inherits our limit).
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit fills a valid struct.
    let allowance =
        unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut limit) } == 0 && limit.rlim_cur >= 4096;
    if allowance {
        assert!(!log.contains("not locked in RAM"), "{log}");
    }
    let log = case("unlocked", &env);
    let refused = log.contains("unlocked case: mlock refused");
    assert!(
        refused || log.contains("unlocked case: mlock allowed"),
        "the case reports its mlock probe:\n{log}"
    );
    if !refused {
        println!("note: this runner may lock pages without an allowance (CAP_IPC_LOCK)");
    }
    assert_eq!(
        log.matches("not locked in RAM").count(),
        usize::from(refused),
        "one line per process when the kernel refuses, none when it locks:\n{log}"
    );
    let log = case("foreign", &env);
    assert_eq!(
        log.matches("control: not available").count(),
        1,
        "one line per process:\n{log}"
    );
    assert!(log.contains("version 2"), "{log}");
    println!("control_file: 3 cases ok");
}

/// Run one case in a child and return its stderr.
fn case(name: &str, env: &[(&str, &std::ffi::OsStr)]) -> String {
    let mut cmd = Command::new(std::env::current_exe().expect("current_exe"));
    for v in [
        hushmic_shm::CONTROL_ENV,
        hushmic_shm::METER_ENV,
        "HUSHMIC_FALLBACK_MODEL_PATH",
        "HUSHMIC_DSP_TIER",
    ] {
        cmd.env_remove(v);
    }
    cmd.env("HUSHMIC_CONTROL_CASE", name);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn case");
    let mut stderr = child.stderr.take().expect("stderr pipe");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    // Bounded: a hung case fails with a name, not a stalled job.
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait on case") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("case {name} timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let log = reader.join().expect("stderr reader");
    assert!(status.success(), "case {name} failed:\n{log}");
    log
}
