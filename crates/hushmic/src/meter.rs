//! The engine meter, app side (M2): the levels going into HushMic's engine
//! (the raw microphone) and leaving it (what apps hear), in 50 ms columns.
//!
//! The plugin's DSP worker publishes one entry per 10 ms hop into a page
//! named by the daemon (`HUSHMIC_METER`, one file per chain; see
//! [`path_for_generation`]). [`EngineMeter`] maps it read-only and folds
//! five hops into one [`MeterColumn`]. It is pull-only: nothing runs unless
//! the window polls, and a poll costs a few atomic loads and no syscall.
//!
//! Columns only exist while an app uses HushMic: without a consumer
//! PipeWire leaves the chain idle, no hop is published, and the meter
//! reports [`MeterState::Idle`]. Bypass and the engine's passthrough tier
//! show as equal in and out; mute as a silent output.
//!
//! A window that opens mid-call can draw the recent past at once:
//! [`EngineMeter::history`] returns up to about 41 s of the current run
//! (never across an idle gap or an engine reset), and polls continue from
//! there.

use hushmic_shm::meter::{
    Hop, MeterReader, HOP_FRAMES, MODE_BYPASS, MODE_MUTE, MODE_PASSTHROUGH, RATE,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Hops per column (50 ms).
pub const COLUMN_HOPS: u64 = 5;
/// No new hop for this long: the engine stands still.
pub const IDLE_AFTER: Duration = Duration::from_millis(250);
/// A poll this long after the previous one starts at the newest hop
/// instead of replaying the backlog (the window was hidden).
pub const RESYNC_AFTER: Duration = Duration::from_millis(500);
/// How often a missing or malformed meter file is retried.
const RETRY_EVERY: Duration = Duration::from_secs(1);
/// The level scale of the designs (`FxClips`, `LevelMeter`): 0 at
/// -60 dBFS, 1 at 0 dBFS.
pub const LEVEL_FLOOR_DB: f32 = -60.0;

/// What the engine does with the microphone in a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineMode {
    /// Noise suppression on.
    Filtering,
    /// Apps hear the raw microphone (in == out): the user's bypass, or
    /// the engine's passthrough step-down (no filtering while the computer
    /// is too busy).
    Bypass,
    /// Apps hear silence (out silent).
    Muted,
}

impl EngineMode {
    fn from_code(code: u32) -> EngineMode {
        match code {
            MODE_BYPASS | MODE_PASSTHROUGH => EngineMode::Bypass,
            MODE_MUTE => EngineMode::Muted,
            _ => EngineMode::Filtering,
        }
    }
}

/// 50 ms of the engine: the raw microphone (`input`) and what apps get
/// (`output`), of the same sound (the plugin removes the engine latency).
/// Linear amplitudes, full scale 1.0.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeterColumn {
    /// Column number within one chain (hop index / 5); consecutive
    /// columns differ by one unless the reader skipped (window hidden).
    pub index: u64,
    pub input_peak: f32,
    pub output_peak: f32,
    pub input_rms: f32,
    pub output_rms: f32,
    /// The mode of the column's last hop.
    pub mode: EngineMode,
    /// All five hops were read (false: some were overwritten while the
    /// reader stalled; the values cover the ones seen).
    pub complete: bool,
}

impl MeterColumn {
    /// RMS input on the designs' level scale (0..1).
    pub fn input_level(&self) -> f32 {
        level(self.input_rms)
    }
    /// RMS output on the designs' level scale (0..1).
    pub fn output_level(&self) -> f32 {
        level(self.output_rms)
    }
    pub fn input_peak_level(&self) -> f32 {
        level(self.input_peak)
    }
    pub fn output_peak_level(&self) -> f32 {
        level(self.output_peak)
    }
}

/// Linear amplitude -> the designs' level scale: `(dBFS + 60) / 60`,
/// clamped to 0..1; silence and non-finite values are 0.
pub fn level(linear: f32) -> f32 {
    if !(linear.is_finite() && linear > 0.0) {
        return 0.0;
    }
    ((20.0 * linear.log10() - LEVEL_FLOOR_DB) / -LEVEL_FLOOR_DB).clamp(0.0, 1.0)
}

/// Exact digital silence (an RMS at or below this): no microphone in a
/// room sends it, a muted or dead one does (Live's "No sound").
pub const SILENT_RMS: f32 = 1e-7;
/// Silent this long (10 s of columns) while an app uses HushMic, the daemon
/// says so with the window closed (UX panel A4).
pub const SILENT_COLUMNS: usize = 200;

/// The columns are exact digital silence, [`SILENT_COLUMNS`] of them.
pub fn all_silent(cols: &[MeterColumn]) -> bool {
    cols.len() >= SILENT_COLUMNS && cols.iter().all(|c| c.input_rms <= SILENT_RMS)
}

/// The daemon's look at the microphone (UX panel A4): has it sent exact
/// digital silence for the last 10 s? A present mic with its hardware mute
/// switch on, or a dead cable, is reported only by the window otherwise.
/// It reads the meter's recent history when asked (the 5 s tick), nothing
/// in between.
#[derive(Default)]
pub struct SilenceWatch {
    meter: Option<EngineMeter>,
    cols: Vec<MeterColumn>,
}

impl SilenceWatch {
    /// Silent now, from the chain's meter at `path` (None: no chain).
    pub fn silent(&mut self, path: Option<&Path>, now: Instant) -> bool {
        let Some(path) = path else {
            return false;
        };
        let meter = self.meter.get_or_insert_with(EngineMeter::new);
        if meter.path() != Some(path) {
            meter.set_path(Some(path));
        }
        self.cols.clear();
        let state = meter.history(now, SILENT_COLUMNS, &mut self.cols);
        silent_while(state, &self.cols)
    }
}

/// Silent: 10 s of digital silence while hops still arrive. A stopped
/// engine (no app records) is never a silent microphone, whatever its
/// last columns held.
fn silent_while(state: MeterState, cols: &[MeterColumn]) -> bool {
    state == MeterState::Flowing && all_silent(cols)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeterState {
    /// Hops arrive: an app is using HushMic.
    Flowing,
    /// The engine stands still: no app uses HushMic (the normal state
    /// outside calls), or the chain is starting.
    Idle,
    /// No meter to read: no path yet, the file is missing or malformed,
    /// or HushMic is off.
    Unavailable,
}

/// The window's reader. See the module docs.
pub struct EngineMeter {
    path: Option<PathBuf>,
    reader: Option<MeterReader>,
    hops: Vec<Hop>,
    acc: Option<Acc>,
    last_hop_at: Option<Instant>,
    /// The writer's hop count at the last poll: a change means hops came
    /// in even when none were read (backlog skipped, ring overrun).
    last_published: Option<u64>,
    last_poll: Option<Instant>,
    last_try: Option<Instant>,
}

/// The column being filled. Columns follow the hop count, not the
/// engine's epochs: a reset inside a column changes nothing about how loud
/// those 50 ms were.
struct Acc {
    index: u64,
    hops: u64,
    input_peak: f32,
    output_peak: f32,
    input_ms: f32,
    output_ms: f32,
    mode: u32,
}

impl Acc {
    fn new(h: &Hop) -> Acc {
        Acc {
            index: h.index / COLUMN_HOPS,
            hops: 0,
            input_peak: 0.0,
            output_peak: 0.0,
            input_ms: 0.0,
            output_ms: 0.0,
            mode: h.mode,
        }
    }

    fn add(&mut self, h: &Hop) {
        self.hops += 1;
        self.input_peak = self.input_peak.max(finite(h.in_peak));
        self.output_peak = self.output_peak.max(finite(h.out_peak));
        self.input_ms += finite(h.in_ms);
        self.output_ms += finite(h.out_ms);
        self.mode = h.mode;
    }

    fn column(&self) -> MeterColumn {
        let n = self.hops.max(1) as f32;
        MeterColumn {
            index: self.index,
            input_peak: self.input_peak,
            output_peak: self.output_peak,
            input_rms: (self.input_ms / n).sqrt(),
            output_rms: (self.output_ms / n).sqrt(),
            mode: EngineMode::from_code(self.mode),
            complete: self.hops == COLUMN_HOPS,
        }
    }
}

fn finite(x: f32) -> f32 {
    if x.is_finite() {
        x.max(0.0)
    } else {
        0.0
    }
}

impl Default for EngineMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineMeter {
    pub fn new() -> EngineMeter {
        EngineMeter {
            path: None,
            reader: None,
            hops: Vec::with_capacity(hushmic_shm::meter::SLOTS),
            acc: None,
            last_hop_at: None,
            last_published: None,
            last_poll: None,
            last_try: None,
        }
    }

    /// The current chain's meter file (from the daemon's state; `None`
    /// while HushMic is off). A new path is a new chain: the reader starts
    /// over.
    pub fn set_path(&mut self, path: Option<&Path>) {
        if self.path.as_deref() == path {
            return;
        }
        self.path = path.map(Path::to_path_buf);
        self.reader = None;
        self.acc = None;
        self.last_hop_at = None;
        self.last_published = None;
        self.last_try = None;
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The plugin instance publishing, once the file is open.
    pub fn instance(&self) -> Option<u64> {
        self.reader.as_ref().map(MeterReader::instance)
    }

    /// Append the columns completed since the last poll (oldest first) to
    /// `out` and say whether audio is flowing. Call it on the window's
    /// 20 Hz clock while the live view is visible; stop calling it when
    /// the view is hidden (the next poll then skips the backlog and hands
    /// over only the newest complete column).
    pub fn poll(&mut self, now: Instant, out: &mut Vec<MeterColumn>) -> MeterState {
        let stale_poll = self
            .last_poll
            .is_some_and(|t| now.saturating_duration_since(t) > RESYNC_AFTER);
        self.last_poll = Some(now);
        let reopened = self.reader.is_none();
        if !self.open(now) {
            return MeterState::Unavailable;
        }
        let Some(reader) = self.reader.as_mut() else {
            return MeterState::Unavailable;
        };
        if !reader.valid() {
            self.reader = None;
            self.acc = None;
            self.last_published = None;
            return MeterState::Unavailable;
        }
        if reopened || stale_poll {
            // A reader starts at the newest hop, so its first read is
            // empty: the writer's clock says whether audio flows now.
            let last = reader.last_publish_ms();
            if last > 0
                && hushmic_shm::monotonic_ms().saturating_sub(last) <= IDLE_AFTER.as_millis() as u64
            {
                self.last_hop_at = Some(now);
            }
        }
        self.hops.clear();
        let report = reader.read(&mut self.hops);
        let published = reader.published();
        if !self.hops.is_empty() || self.last_published.is_some_and(|p| p != published) {
            self.last_hop_at = Some(now);
        }
        self.last_published = Some(published);
        if report.restarted {
            // A new instance: no backlog.
            self.acc = None;
            self.hops.clear();
        } else if stale_poll {
            // A late look (the window was away, or a slow desktop's frames
            // came late): no backlog, but the newest complete column, what
            // sounds now, so a resting page still sees speech come back.
            self.acc = None;
            let newest = self
                .hops
                .iter()
                .rev()
                .find(|h| h.index % COLUMN_HOPS == COLUMN_HOPS - 1)
                .map(|h| h.index / COLUMN_HOPS);
            match newest {
                Some(col) => self.hops.retain(|h| h.index / COLUMN_HOPS >= col),
                None => self.hops.clear(),
            }
        }
        for h in &self.hops {
            let index = h.index / COLUMN_HOPS;
            if let Some(acc) = &self.acc {
                if acc.index != index {
                    // Its last hop was lost: the column closes when the
                    // next one starts and says so (`complete`).
                    out.push(acc.column());
                    self.acc = None;
                }
            }
            let acc = self.acc.get_or_insert_with(|| Acc::new(h));
            acc.add(h);
            if h.index % COLUMN_HOPS == COLUMN_HOPS - 1 {
                out.push(acc.column());
                self.acc = None;
            }
        }
        match self.last_hop_at {
            Some(t) if now.saturating_duration_since(t) <= IDLE_AFTER => MeterState::Flowing,
            _ => {
                self.acc = None;
                MeterState::Idle
            }
        }
    }

    /// The current run's recent columns, for a view that opens: up to
    /// `max_columns` complete columns (oldest first) are appended to `out`
    /// while audio flows, nothing when the engine stands still (no stale
    /// picture of a call that ended). Call [`EngineMeter::poll`] on the
    /// usual clock afterwards; it continues where this left off.
    pub fn history(
        &mut self,
        now: Instant,
        max_columns: usize,
        out: &mut Vec<MeterColumn>,
    ) -> MeterState {
        self.history_since(now, max_columns, 0, out)
    }

    /// [`EngineMeter::history`] without the hops published before
    /// `began_ms` (CLOCK_MONOTONIC ms; the daemon's
    /// `AppState::engine.use_began_ms`). The engine can run before an app
    /// uses HushMic (a Mic check or a volume meter records it too); what it
    /// heard then reached no app and is not handed out (nor the engine's
    /// latency after the mark: [`MARK_MARGIN_MS`]). 0 cuts nothing.
    pub fn history_since(
        &mut self,
        now: Instant,
        max_columns: usize,
        began_ms: u64,
        out: &mut Vec<MeterColumn>,
    ) -> MeterState {
        if self.open(now) {
            if let Some(r) = self.reader.as_mut() {
                let last = r.last_publish_ms();
                let fresh = r.valid()
                    && last > 0
                    && hushmic_shm::monotonic_ms().saturating_sub(last)
                        <= IDLE_AFTER.as_millis() as u64;
                if fresh {
                    // One extra column: the oldest may start mid-column.
                    // Saturating: "all there are" is usize::MAX; the ring
                    // bounds the rewind anyway.
                    let mut hops = (max_columns as u64)
                        .saturating_add(1)
                        .saturating_mul(COLUMN_HOPS);
                    if began_ms > 0 {
                        hops = hops.min(hops_since(last, began_ms.saturating_add(MARK_MARGIN_MS)));
                    }
                    r.rewind(hops);
                    self.acc = None;
                    self.last_published = None;
                    self.last_poll = Some(now);
                }
            }
        }
        let start = out.len();
        let state = self.poll(now, out);
        if out.get(start).is_some_and(|c| !c.complete) {
            out.remove(start);
        }
        let got = out.len() - start;
        if got > max_columns {
            out.drain(start..start + got - max_columns);
        }
        state
    }

    /// Open the file if needed (at most once a second when it fails).
    fn open(&mut self, now: Instant) -> bool {
        if self.reader.is_some() {
            return true;
        }
        let Some(path) = &self.path else {
            return false;
        };
        if self
            .last_try
            .is_some_and(|t| now.saturating_duration_since(t) < RETRY_EVERY)
        {
            return false;
        }
        self.last_try = Some(now);
        match MeterReader::open(path) {
            Ok(r) => {
                self.reader = Some(r);
                self.last_published = None;
                true
            }
            Err(_) => false,
        }
    }
}

/// What a mark cuts beyond itself (ms): a hop carries sound captured up
/// to the engine's latency before it was published, and a writer that
/// catches up after a stall publishes faster than one hop per 10 ms, so
/// its older hops are dated too late. Erring late, as the mark does.
const MARK_MARGIN_MS: u64 = crate::controller::LATENCY_SAMPLES as u64 * 1000 / RATE as u64 + 2 * 10;

/// How many of the newest hops were published at or after `began_ms`,
/// the newest at `last_ms` and one every hop's length (10 ms) before it.
fn hops_since(last_ms: u64, began_ms: u64) -> u64 {
    let hop_ms = u64::from(HOP_FRAMES) * 1000 / u64::from(RATE);
    match last_ms.checked_sub(began_ms) {
        Some(d) => d / hop_ms + 1,
        None => 0,
    }
}

// ---------------------------------------------------------------- daemon side

/// The directory the meter files, control files and tap requests live in.
/// The plugin's DSP worker writes the meter (and the tap) every hop and its
/// audio thread reads the control file every cycle: a page fault on a
/// disk-backed mapping under memory pressure would stall them. So: the
/// per-user runtime dir when it is memory-backed (logind makes it tmpfs;
/// inside Flatpak the sandbox's /run/user/<uid> is tmpfs too), else a
/// private directory in /dev/shm, else none: no meter, no Mic check tap, no
/// control file (mode and strength go to the ports, as in other hosts).
/// Memory-backed only rules out writeback and stable-page waits: tmpfs pages
/// can still be swapped out, so the audio thread's guarantee comes from the
/// reader's best-effort mlock of the control page, not from this choice.
/// Chosen once per process; a fallback is logged once.
pub fn dir() -> Option<PathBuf> {
    static CHOSEN: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    CHOSEN
        .get_or_init(|| {
            let runtime = runtime_base();
            match choose(runtime.as_deref(), Path::new(SHM), uid(), &on_memory_fs) {
                Choice::Runtime(d) => Some(d),
                Choice::Shm(d) => {
                    eprintln!(
                        "[hushmic] XDG_RUNTIME_DIR is not memory-backed; the engine meter and \
                         control files live in {}",
                        d.display()
                    );
                    Some(d)
                }
                Choice::Off(why) => {
                    eprintln!(
                        "[hushmic] no memory-backed directory for the engine meter and control \
                         file ({why}): the level meter and Mic check's tap are off, mode and \
                         strength go through PipeWire"
                    );
                    None
                }
            }
        })
        .clone()
}

/// [`dir`] for the runtime dir `runtime`, not cached and not logged: what a
/// daemon started with it uses (tests).
pub fn dir_for(runtime: Option<&Path>) -> Option<PathBuf> {
    match choose(runtime, Path::new(SHM), uid(), &on_memory_fs) {
        Choice::Runtime(d) | Choice::Shm(d) => Some(d),
        Choice::Off(_) => None,
    }
}

/// The per-user runtime dir's `hushmic` directory, whatever backs it:
/// where Mic check keeps its files (not mapped by the audio path).
pub fn runtime_dir() -> Option<PathBuf> {
    runtime_base().map(|b| b.join("hushmic"))
}

fn runtime_base() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?;
    let base = PathBuf::from(base);
    base.is_dir().then_some(base)
}

const SHM: &str = "/dev/shm";

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions.
    unsafe { libc::getuid() }
}

#[derive(Debug, PartialEq)]
enum Choice {
    Runtime(PathBuf),
    Shm(PathBuf),
    Off(String),
}

/// See [`dir`]. `memory` tells a memory-backed file system.
fn choose(runtime: Option<&Path>, shm: &Path, uid: u32, memory: &dyn Fn(&Path) -> bool) -> Choice {
    if let Some(r) = runtime.filter(|r| memory(r)) {
        return Choice::Runtime(r.join("hushmic"));
    }
    if !(shm.is_dir() && memory(shm)) {
        return Choice::Off(match runtime {
            Some(_) => "the runtime dir is disk-backed and there is no /dev/shm".into(),
            None => "no runtime dir and no /dev/shm".into(),
        });
    }
    // One per runtime dir: two daemons of one user with different runtime
    // dirs (a container, a test) must not share chain files.
    let name = match runtime {
        Some(r) => {
            use std::os::unix::ffi::OsStrExt;
            format!("hushmic-{uid}-{:016x}", fnv1a(r.as_os_str().as_bytes()))
        }
        None => format!("hushmic-{uid}"),
    };
    let d = shm.join(name);
    match private_dir(&d, uid) {
        Ok(()) => Choice::Shm(d),
        Err(e) => Choice::Off(format!("{}: {e}", d.display())),
    }
}

/// `d` exists as a directory of ours that nobody else may enter (made here
/// if missing). /dev/shm is world-writable: a name someone else made first,
/// a symlink, or a looser mode is refused, never repaired.
fn private_dir(d: &Path, uid: u32) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    if let Err(e) = std::fs::DirBuilder::new().mode(0o700).create(d) {
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(e);
        }
    }
    let m = std::fs::symlink_metadata(d)?;
    if !m.file_type().is_dir() {
        return Err(std::io::Error::other("not a directory"));
    }
    if m.uid() != uid {
        return Err(std::io::Error::other(format!("owned by uid {}", m.uid())));
    }
    if m.mode() & 0o077 != 0 {
        return Err(std::io::Error::other(format!(
            "mode {:o}",
            m.mode() & 0o777
        )));
    }
    Ok(())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// `path` is on a file system kept in memory (tmpfs, ramfs).
fn on_memory_fs(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: a valid C string and an out-pointer to a zeroed statfs.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return false;
    }
    // linux/magic.h; libc has no RAMFS_MAGIC.
    const RAMFS_MAGIC: u32 = 0x8584_58f6;
    st.f_type == libc::TMPFS_MAGIC || st.f_type as u32 == RAMFS_MAGIC
}

/// The meter file of the chain spawned under `generation`.
pub fn path_for_generation(dir: &Path, generation: u64) -> PathBuf {
    dir.join(format!("meter-{generation}"))
}

/// The control file of the chain spawned under `generation`
/// (`hushmic_shm::control`).
pub fn control_path_for_generation(dir: &Path, generation: u64) -> PathBuf {
    dir.join(format!("control-{generation}"))
}

/// The speakers chain's files carry this prefix: its generations count
/// apart from the microphone's, so the bare names would collide.
const SPEAKERS_PREFIX: &str = "speakers-";

/// [`path_for_generation`] for either chain.
pub fn path_for(dir: &Path, chain: crate::diagnostics::Chain, generation: u64) -> PathBuf {
    match chain {
        crate::diagnostics::Chain::Mic => path_for_generation(dir, generation),
        crate::diagnostics::Chain::Speakers => {
            dir.join(format!("{SPEAKERS_PREFIX}meter-{generation}"))
        }
    }
}

/// [`control_path_for_generation`] for either chain.
pub fn control_path_for(dir: &Path, chain: crate::diagnostics::Chain, generation: u64) -> PathBuf {
    match chain {
        crate::diagnostics::Chain::Mic => control_path_for_generation(dir, generation),
        crate::diagnostics::Chain::Speakers => {
            dir.join(format!("{SPEAKERS_PREFIX}control-{generation}"))
        }
    }
}

/// Whether the plugin has published a hop to the meter at `path`: proof
/// that it has run. False while there is no such file yet.
pub fn has_published(path: &Path) -> bool {
    MeterReader::open(path).is_ok_and(|r| r.published() > 0)
}

/// Create the meter directory (0700). The plugin creates the file itself.
pub fn prepare_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Remove a chain's meter file and any tap request left next to it,
/// including a request a window was still writing (`.tap-new-*`).
pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
    let request = hushmic_shm::tap::request_path(path);
    let _ = std::fs::remove_file(&request);
    let (Some(dir), Some(name)) = (path.parent(), request.file_name()) else {
        return;
    };
    let mut prefix = name.to_os_string();
    prefix.push("-new-");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    use std::os::unix::ffi::OsStrExt;
    for e in entries.flatten() {
        if e.file_name().as_bytes().starts_with(prefix.as_bytes()) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// At quit, with the chain gone: the private /dev/shm directory goes too
/// when nothing of anyone's is left in it (UX panel round 2, A24). The
/// runtime dir's `hushmic` stays: Mic check keeps files there and logind
/// empties it at logout.
pub fn leave() {
    if let Some(d) = dir().filter(|d| d.starts_with(SHM)) {
        sweep(&d);
        let _ = std::fs::remove_dir(&d);
    }
}

/// Remove every meter file, control file and tap request in `dir` (daemon
/// start: a previous run that died left them behind). Only names this
/// module makes are touched.
pub fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_ours(name) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn is_ours(name: &str) -> bool {
    let name = name.strip_prefix(SPEAKERS_PREFIX).unwrap_or(name);
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    if let Some(rest) = name.strip_prefix("control-") {
        return digits(rest) > 0 && digits(rest) == rest.len();
    }
    let Some(rest) = name.strip_prefix("meter-") else {
        return false;
    };
    let n = digits(rest);
    let tail = &rest[n..];
    n > 0 && (tail.is_empty() || tail == ".tap" || tail.starts_with(".tap-new-"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hushmic_shm::meter::{MeterWriter, MODE_PROCESS};

    fn scratch(tag: &str) -> PathBuf {
        let d = hushmic_shm::test_dir_base()
            .join(format!("hushmic-meter-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        prepare_dir(&d).unwrap();
        d
    }

    /// A writer clock (ms) ahead of the real one: every hop stamped from
    /// it counts as just published, however long the runner stalls.
    fn ahead() -> u64 {
        hushmic_shm::monotonic_ms() + 600_000
    }

    fn hop(v: f32) -> [f32; 480] {
        [v; 480]
    }

    #[test]
    fn levels_follow_the_design_scale() {
        assert_eq!(level(1.0), 1.0);
        assert_eq!(level(0.0), 0.0);
        assert_eq!(level(f32::NAN), 0.0);
        assert!((level(0.001) - 0.0).abs() < 1e-6, "-60 dBFS is the floor");
        assert!((level(10f32.powf(-27.0 / 20.0)) - 0.55).abs() < 1e-4);
        assert!((level(10f32.powf(-12.0 / 20.0)) - 0.8).abs() < 1e-4);
        assert_eq!(level(4.0), 1.0, "clipped above full scale");
    }

    #[test]
    fn a_meter_proves_the_plugin_ran_only_once_it_holds_a_hop() {
        let d = scratch("ran");
        let p = path_for_generation(&d, 4);
        assert!(!has_published(&p), "no file yet");
        let mut w = MeterWriter::create(&p, 1, 0).unwrap();
        assert!(!has_published(&p), "created, nothing published");
        w.publish(&hop(0.1), &hop(0.1), MODE_PROCESS);
        assert!(has_published(&p));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn five_hops_make_a_column_with_peak_and_rms() {
        let d = scratch("cols");
        let p = path_for_generation(&d, 3);
        let mut w = MeterWriter::create(&p, 1, 0).unwrap();
        let mut m = EngineMeter::new();
        let t0 = Instant::now();
        let mut out = Vec::new();
        assert_eq!(m.poll(t0, &mut out), MeterState::Unavailable, "no path");
        m.set_path(Some(&p));
        assert_eq!(m.poll(t0, &mut out), MeterState::Idle, "nothing yet");
        for v in [0.1, 0.2, 0.4, 0.2, 0.1, 0.5, 0.5] {
            w.publish(&hop(v), &hop(v / 2.0), MODE_PROCESS);
        }
        assert_eq!(m.poll(t0, &mut out), MeterState::Flowing);
        assert_eq!(out.len(), 1, "the second column is not complete yet");
        let c = out[0];
        assert_eq!(c.index, 0);
        assert!(c.complete);
        assert_eq!(c.input_peak, 0.4);
        assert_eq!(c.output_peak, 0.2);
        let rms = ((0.01 + 0.04 + 0.16 + 0.04 + 0.01) / 5.0f32).sqrt();
        assert!((c.input_rms - rms).abs() < 1e-6);
        assert!((c.output_rms - rms / 2.0).abs() < 1e-6);
        assert_eq!(c.mode, EngineMode::Filtering);
        for v in [0.5, 0.5, 0.5] {
            w.publish(&hop(v), &hop(v), MODE_BYPASS);
        }
        out.clear();
        m.poll(t0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].index, out[0].mode), (1, EngineMode::Bypass));
        // The passthrough step-down draws as unfiltered too.
        for _ in 0..5 {
            w.publish(&hop(0.5), &hop(0.5), hushmic_shm::meter::MODE_PASSTHROUGH);
        }
        out.clear();
        m.poll(t0, &mut out);
        assert_eq!((out[0].index, out[0].mode), (2, EngineMode::Bypass));
        assert_eq!(out[0].input_peak, out[0].output_peak, "bypass: in == out");
        // No hop for a while: idle, and no column comes out of it.
        out.clear();
        assert_eq!(
            m.poll(t0 + IDLE_AFTER * 2, &mut out),
            MeterState::Idle,
            "idle after the counter stops"
        );
        assert!(out.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_reset_keeps_the_column_and_a_hidden_window_skips_the_backlog() {
        let d = scratch("reset");
        let p = path_for_generation(&d, 4);
        let mut w = MeterWriter::create(&p, 1, 0).unwrap();
        let mut m = EngineMeter::new();
        m.set_path(Some(&p));
        let t0 = Instant::now();
        let mut out = Vec::new();
        m.poll(t0, &mut out);
        for _ in 0..3 {
            w.publish(&hop(0.3), &hop(0.0), MODE_MUTE);
        }
        w.discontinuity();
        for _ in 0..2 {
            w.publish(&hop(0.3), &hop(0.0), MODE_MUTE);
        }
        m.poll(t0, &mut out);
        assert_eq!(out.len(), 1, "a reset inside a column does not split it");
        assert!(out[0].complete);
        assert_eq!(out[0].mode, EngineMode::Muted);
        assert_eq!(out[0].output_peak, 0.0, "mute: silent output");
        // The window was hidden for a second: no stale backlog animates,
        // only the newest column (what sounds now) comes.
        for _ in 0..50 {
            w.publish(&hop(0.3), &hop(0.3), MODE_PROCESS);
        }
        out.clear();
        m.poll(t0 + Duration::from_secs(1), &mut out);
        assert_eq!(out.len(), 1, "backlog skipped but the newest column");
        assert!(out[0].complete);
        for _ in 0..10 {
            w.publish(&hop(0.3), &hop(0.3), MODE_PROCESS);
        }
        m.poll(t0 + Duration::from_millis(1050), &mut out);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|c| c.complete));
        assert_eq!(out[1].index, out[0].index + 1);
        assert_eq!(out[2].index, out[1].index + 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_late_look_still_sees_what_sounds_now() {
        // A resting Live page on a slow desktop: every look comes later
        // than the resync gap. Each skips the backlog, and each still
        // hands over the newest column: speech is seen when it comes.
        let d = scratch("late");
        let p = path_for_generation(&d, 10);
        let mut w = MeterWriter::create(&p, 1, 0).unwrap();
        let mut m = EngineMeter::new();
        m.set_path(Some(&p));
        let t0 = Instant::now();
        let mut out = Vec::new();
        let mut clock = ahead();
        m.poll(t0, &mut out);
        let late = RESYNC_AFTER + Duration::from_millis(100);
        for look in 1..=4u32 {
            let v = if look < 3 { 0.0 } else { 0.5 };
            for _ in 0..(late.as_millis() / 10) {
                clock += 10;
                w.publish_at(&hop(v), &hop(v), MODE_PROCESS, clock);
            }
            out.clear();
            let state = m.poll(t0 + late * look, &mut out);
            assert_eq!(state, MeterState::Flowing, "look {look}");
            assert_eq!(out.len(), 1, "look {look}: the newest column");
            assert!(out[0].complete);
            assert_eq!(out[0].output_peak > 0.0, look >= 3, "look {look}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn audio_that_already_flows_is_flowing_from_the_first_poll() {
        let d = scratch("open");
        let p = path_for_generation(&d, 5);
        let mut w = MeterWriter::create(&p, 1, 0).unwrap();
        let mut m = EngineMeter::new();
        m.set_path(Some(&p));
        let t0 = Instant::now();
        let mut out = Vec::new();
        let mut clock = ahead();
        let mut publish = |n: usize| {
            for _ in 0..n {
                clock += 10;
                w.publish_at(&hop(0.3), &hop(0.3), MODE_PROCESS, clock);
            }
        };
        publish(10);
        assert_eq!(m.poll(t0, &mut out), MeterState::Flowing, "open");
        publish(5);
        let at = |ms| t0 + Duration::from_millis(ms);
        assert_eq!(m.poll(at(50), &mut out), MeterState::Flowing);
        publish(100);
        assert_eq!(
            m.poll(at(1050), &mut out),
            MeterState::Flowing,
            "back after a hidden second"
        );
        publish(5);
        assert_eq!(m.poll(at(1100), &mut out), MeterState::Flowing);
        // A page that checks once a second while idle sees the call start.
        let mut slow = EngineMeter::new();
        slow.set_path(Some(&p));
        let t1 = t0 + Duration::from_secs(10);
        for s in 0..5 {
            publish(100);
            let state = slow.poll(t1 + Duration::from_secs(s), &mut out);
            assert_eq!(state, MeterState::Flowing, "1 Hz poll {s}");
        }
        // And still goes idle when the hops stop.
        assert_eq!(
            slow.poll(t1 + Duration::from_millis(4300), &mut out),
            MeterState::Idle
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_view_that_opens_mid_call_gets_the_recent_past() {
        let d = scratch("history");
        let p = path_for_generation(&d, 6);
        let mut w = MeterWriter::create(&p, 1, 0).unwrap();
        let t0 = Instant::now();
        // 20 s of a call on a clock of its own (never a gap).
        let mut clock = ahead();
        let mut publish = |w: &mut MeterWriter, v: f32| {
            clock += 10;
            w.publish_at(&hop(v), &hop(v / 2.0), MODE_PROCESS, clock);
        };
        for i in 0..2_003u32 {
            publish(&mut w, (i % 7) as f32 / 10.0);
        }
        let mut m = EngineMeter::new();
        m.set_path(Some(&p));
        let mut out = Vec::new();
        assert_eq!(m.history(t0, 138, &mut out), MeterState::Flowing);
        assert_eq!(out.len(), 138, "a 960 px picture's worth");
        assert!(out.iter().all(|c| c.complete));
        assert!(out.windows(2).all(|c| c[1].index == c[0].index + 1));
        assert_eq!(out.last().unwrap().index, 2_003 / 5 - 1, "up to the newest");
        // More than the ring holds: as much as it has.
        let mut m2 = EngineMeter::new();
        m2.set_path(Some(&p));
        let mut all = Vec::new();
        m2.history(t0, 10_000, &mut all);
        assert_eq!(all.len(), 400, "all the complete columns there are");
        // "All there are", as pages ask for it.
        let mut m3 = EngineMeter::new();
        m3.set_path(Some(&p));
        let mut every = Vec::new();
        assert_eq!(m3.history(t0, usize::MAX, &mut every), MeterState::Flowing);
        assert_eq!(every, all);
        // Polling continues seamlessly.
        for _ in 0..7 {
            publish(&mut w, 0.2);
        }
        let mut next = Vec::new();
        m.poll(t0 + Duration::from_millis(50), &mut next);
        assert_eq!(
            next.first().map(|c| c.index),
            Some(out.last().unwrap().index + 1)
        );
        assert!(next[0].complete, "the column across the seam is whole");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn history_since_leaves_out_what_came_before_the_mark() {
        let d = scratch("history-since");
        let p = path_for_generation(&d, 8);
        let mut w = MeterWriter::create(&p, 1, 0).unwrap();
        // 3 s of a run (a Mic check, say), then 1 s after an app joined;
        // the newest hop counts as just published.
        let start = ahead() - 4_000;
        let mut clock = start;
        for i in 0..400u64 {
            clock = start + i * 10;
            let v = if i < 300 { 0.1 } else { 0.8 };
            w.publish_at(&hop(v), &hop(v), MODE_PROCESS, clock);
        }
        let began = start + 3_000;
        let mut m = EngineMeter::new();
        m.set_path(Some(&p));
        let mut out = Vec::new();
        assert_eq!(
            m.history_since(Instant::now(), 138, began, &mut out),
            MeterState::Flowing
        );
        // Less the margin: 100 ms, two columns.
        assert_eq!(MARK_MARGIN_MS, 100);
        assert_eq!(out.len(), 18, "the second since the mark");
        assert!(out.iter().all(|c| c.complete && c.input_peak == 0.8));
        // A mark mid-column: that column is not whole after it, so it goes.
        let mut m = EngineMeter::new();
        m.set_path(Some(&p));
        out.clear();
        m.history_since(Instant::now(), 138, began - 20, &mut out);
        assert_eq!(out.len(), 18);
        // No cut, or a mark before the run: the run's history.
        for since in [0, start - 5_000] {
            let mut m = EngineMeter::new();
            m.set_path(Some(&p));
            out.clear();
            m.history_since(Instant::now(), 138, since, &mut out);
            assert_eq!(out.len(), 80, "{since}");
        }
        // A mark after the newest hop: nothing yet.
        let mut m = EngineMeter::new();
        m.set_path(Some(&p));
        out.clear();
        m.history_since(Instant::now(), 138, clock + 1, &mut out);
        assert!(out.is_empty());
        assert_eq!(hops_since(1_000, 1_000), 1);
        assert_eq!(hops_since(1_000, 991), 1);
        assert_eq!(hops_since(1_000, 990), 2);
        assert_eq!(hops_since(1_000, 1_001), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ten_seconds_of_digital_silence_is_a_silent_mic() {
        let d = scratch("silent");
        let p = path_for_generation(&d, 9);
        let mut w = MeterWriter::create(&p, 1, 0).unwrap();
        let mut watch = SilenceWatch::default();
        assert!(!watch.silent(None, Instant::now()), "no chain: not silent");
        assert!(!watch.silent(Some(&p), Instant::now()), "nothing published");
        let mut clock = ahead();
        let mut publish = |w: &mut MeterWriter, hops: u64, v: f32| {
            for _ in 0..hops {
                clock += 10;
                w.publish_at(&hop(v), &hop(v), MODE_PROCESS, clock);
            }
        };
        // a room's quiet is not silence
        publish(&mut w, SILENT_COLUMNS as u64 * COLUMN_HOPS + 10, 1e-4);
        assert!(!watch.silent(Some(&p), Instant::now()));
        // exact silence, but not for 10 s yet
        publish(&mut w, (SILENT_COLUMNS as u64 - 20) * COLUMN_HOPS, 0.0);
        assert!(!watch.silent(Some(&p), Instant::now()), "9 s only");
        publish(&mut w, 30 * COLUMN_HOPS, 0.0);
        assert!(watch.silent(Some(&p), Instant::now()), "10 s of silence");
        // one word ends it
        publish(&mut w, COLUMN_HOPS, 0.05);
        assert!(!watch.silent(Some(&p), Instant::now()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_silent_mic_that_no_app_records_is_not_reported() {
        let d = scratch("silent-idle");
        let p = path_for_generation(&d, 10);
        let mut w = MeterWriter::create(&p, 1, 0).unwrap();
        // silence that ended 15 s ago: the engine stands still since
        let mut clock = hushmic_shm::monotonic_ms().saturating_sub(15_000);
        for _ in 0..(SILENT_COLUMNS as u64 + 5) * COLUMN_HOPS {
            clock += 1;
            w.publish_at(&hop(0.0), &hop(0.0), MODE_PROCESS, clock);
        }
        let mut watch = SilenceWatch::default();
        assert!(!watch.silent(Some(&p), Instant::now()));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Review N-M2: silence counts only while hops arrive.
    #[test]
    fn a_stopped_engines_silence_is_no_silent_mic() {
        let quiet = vec![
            MeterColumn {
                index: 0,
                input_peak: 0.0,
                output_peak: 0.0,
                input_rms: 0.0,
                output_rms: 0.0,
                mode: EngineMode::Filtering,
                complete: true,
            };
            SILENT_COLUMNS
        ];
        assert!(silent_while(MeterState::Flowing, &quiet));
        assert!(!silent_while(MeterState::Idle, &quiet));
        assert!(!silent_while(MeterState::Unavailable, &quiet));
    }

    #[test]
    fn all_silent_needs_every_column_and_all_of_them() {
        let col = |rms: f32| MeterColumn {
            index: 0,
            input_peak: rms,
            output_peak: rms,
            input_rms: rms,
            output_rms: rms,
            mode: EngineMode::Filtering,
            complete: true,
        };
        let quiet = vec![col(0.0); SILENT_COLUMNS];
        assert!(all_silent(&quiet));
        assert!(!all_silent(&quiet[1..]), "fewer columns: not yet");
        let mut one = quiet.clone();
        one[SILENT_COLUMNS / 2] = col(1e-5);
        assert!(!all_silent(&one));
        assert!(all_silent(&vec![col(SILENT_RMS); SILENT_COLUMNS]));
    }

    #[test]
    fn a_view_that_opens_between_calls_gets_no_history() {
        let d = scratch("history-idle");
        let p = path_for_generation(&d, 7);
        let mut w = MeterWriter::create(&p, 1, 0).unwrap();
        // A call that ended 10 s ago.
        let mut clock = hushmic_shm::monotonic_ms().saturating_sub(15_000);
        for _ in 0..500 {
            clock += 10;
            w.publish_at(&hop(0.3), &hop(0.3), MODE_PROCESS, clock);
        }
        let mut m = EngineMeter::new();
        m.set_path(Some(&p));
        let mut out = Vec::new();
        assert_eq!(m.history(Instant::now(), 138, &mut out), MeterState::Idle);
        assert!(out.is_empty(), "no picture of a call that ended");
        // The next call: only its own hops, never the old call's.
        let mut clock = ahead();
        for _ in 0..50 {
            clock += 10;
            w.publish_at(&hop(0.6), &hop(0.6), MODE_PROCESS, clock);
        }
        let mut m = EngineMeter::new();
        m.set_path(Some(&p));
        assert_eq!(
            m.history(Instant::now(), 138, &mut out),
            MeterState::Flowing
        );
        assert!(!out.is_empty() && out.len() <= 10);
        assert!(out.iter().all(|c| c.input_peak == 0.6));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_new_chain_path_starts_over_and_bad_files_are_unavailable() {
        let d = scratch("path");
        let mut m = EngineMeter::new();
        let t0 = Instant::now();
        let mut out = Vec::new();
        let bad = path_for_generation(&d, 1);
        std::fs::write(&bad, b"junk").unwrap();
        m.set_path(Some(&bad));
        assert_eq!(m.poll(t0, &mut out), MeterState::Unavailable);
        let good = path_for_generation(&d, 2);
        let mut w = MeterWriter::create(&good, 77, 5).unwrap();
        m.set_path(Some(&good));
        assert_eq!(m.poll(t0, &mut out), MeterState::Idle);
        assert_eq!(m.instance(), Some(77));
        for _ in 0..5 {
            w.publish(&hop(0.1), &hop(0.1), MODE_PROCESS);
        }
        assert_eq!(m.poll(t0, &mut out), MeterState::Flowing);
        m.set_path(None);
        assert_eq!(m.poll(t0, &mut out), MeterState::Unavailable);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sweep_removes_only_meter_and_control_files_and_tap_requests() {
        let d = scratch("sweep");
        for f in [
            "meter-1",
            "meter-22.tap",
            "meter-3.tap-new-55-0",
            "meter-x",
            "meter-",
            "control-4",
            "control-4.x",
            "control-",
            "speakers-meter-2",
            "speakers-meter-2.tap",
            "speakers-control-2",
            "speakers-control-",
            "speakers-x",
            "abtest-raw.wav",
            "hushmic.lock",
        ] {
            std::fs::write(d.join(f), b"").unwrap();
        }
        sweep(&d);
        let mut left: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "abtest-raw.wav",
                "control-",
                "control-4.x",
                "hushmic.lock",
                "meter-",
                "meter-x",
                "speakers-control-",
                "speakers-x"
            ]
        );
        // the two chains' files never share a name
        use crate::diagnostics::Chain;
        assert_ne!(
            path_for(&d, Chain::Mic, 3),
            path_for(&d, Chain::Speakers, 3)
        );
        assert_ne!(
            control_path_for(&d, Chain::Mic, 3),
            control_path_for(&d, Chain::Speakers, 3)
        );
        assert_eq!(path_for(&d, Chain::Mic, 3), path_for_generation(&d, 3));
        let p = path_for_generation(&d, 9);
        std::fs::write(&p, b"").unwrap();
        std::fs::write(hushmic_shm::tap::request_path(&p), b"").unwrap();
        std::fs::write(d.join("meter-9.tap-new-77-1"), b"").unwrap();
        std::fs::write(d.join("meter-90.tap-new-77-1"), b"").unwrap();
        remove(&p);
        assert!(!p.exists() && !hushmic_shm::tap::request_path(&p).exists());
        assert!(!d.join("meter-9.tap-new-77-1").exists());
        assert!(d.join("meter-90.tap-new-77-1").exists(), "another chain's");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn shared_files_live_only_on_a_memory_backed_file_system() {
        use std::os::unix::fs::PermissionsExt;
        let me = uid();
        let base = scratch("choose");
        let runtime = base.join("run");
        let shm = base.join("shm");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::create_dir_all(&shm).unwrap();
        let only =
            |memory: &'static [&'static str]| move |p: &Path| memory.iter().any(|m| p.ends_with(m));
        // A memory-backed runtime dir: as before.
        assert_eq!(
            choose(Some(&runtime), &shm, me, &only(&["run", "shm"])),
            Choice::Runtime(runtime.join("hushmic"))
        );
        // A disk-backed one: a private directory in /dev/shm, one per
        // runtime dir, made 0700 and reused.
        let fallback = match choose(Some(&runtime), &shm, me, &only(&["shm"])) {
            Choice::Shm(d) => d,
            other => panic!("{other:?}"),
        };
        assert!(fallback.starts_with(&shm));
        let mode = std::fs::metadata(&fallback).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert_eq!(
            choose(Some(&runtime), &shm, me, &only(&["shm"])),
            Choice::Shm(fallback.clone())
        );
        let other_runtime = base.join("run2");
        std::fs::create_dir_all(&other_runtime).unwrap();
        assert_ne!(
            choose(Some(&other_runtime), &shm, me, &only(&["shm"])),
            Choice::Shm(fallback.clone()),
            "another runtime dir, another directory"
        );
        assert_eq!(
            choose(None, &shm, me, &only(&["shm"])),
            Choice::Shm(shm.join(format!("hushmic-{me}")))
        );
        // Someone loosened it, or put something else under the name: off,
        // never repaired or followed.
        std::fs::set_permissions(&fallback, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            choose(Some(&runtime), &shm, me, &only(&["shm"])),
            Choice::Off(_)
        ));
        assert_eq!(
            std::fs::metadata(&fallback).unwrap().permissions().mode() & 0o777,
            0o755,
            "not repaired"
        );
        std::fs::remove_dir(&fallback).unwrap();
        std::os::unix::fs::symlink(&runtime, &fallback).unwrap();
        assert!(matches!(
            choose(Some(&runtime), &shm, me, &only(&["shm"])),
            Choice::Off(_)
        ));
        std::fs::remove_file(&fallback).unwrap();
        // Another user's: off.
        assert!(matches!(
            choose(Some(&runtime), &shm, me + 1, &only(&["shm"])),
            Choice::Off(_)
        ));
        // Nothing memory-backed: off.
        assert!(matches!(
            choose(Some(&runtime), &shm, me, &only(&[])),
            Choice::Off(_)
        ));
        assert!(matches!(
            choose(Some(&runtime), &base.join("absent"), me, &only(&["absent"])),
            Choice::Off(_)
        ));
        // The real check: procfs is not memory-backed storage.
        assert!(!on_memory_fs(Path::new("/proc")));
        let _ = std::fs::remove_dir_all(&base);
    }
}
