//! PipeWire-facing backend of the A/B window: two live capture children
//! (`pw-record … -` pipes), the 10 s recording buffer, playback via
//! `pw-play`, and a device watch. `Record` restarts the monitors itself
//! when they are down (Sample view), and `Stop` is contextual — active
//! playback first, then an armed recording (the monitors keep running),
//! then the monitors themselves. Runs on its own thread; talks to the UI
//! purely via `Frame`s and a repaint callback (no egui types here).

use crate::abtest::dsp::{LevelMeter, SpectrumAnalyzer};
use crate::abtest::stream::{F32Reader, Header, SampleEndian, StreamInfo};
use crate::abtest::types::{Channel, Command, Frame, DB_FLOOR, RECORD_SECS, SAMPLE_RATE};
use crate::abtest::{metrics, stream};
use std::path::PathBuf;
use std::process::{Child, Command as Proc, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Backend {
    raw_node: String,
    filtered_node: String,
    cmd_rx: Receiver<Command>,
    frame_tx: Sender<Frame>,
}

/// State shared between the backend loop and the two capture threads.
struct Shared {
    /// Latest smoothed levels (raw, filtered) in dBFS.
    levels: Mutex<(f32, f32)>,
    /// Active recording buffers; None when not recording.
    rec: Mutex<Option<RecBuf>>,
    /// Monitor generation whose capture pipe died unexpectedly (node gone /
    /// pw-record failed); 0 = none. The sweep ignores reports from retired
    /// generations, so a straggler thread from a stopped monitor can never
    /// tear down its successor.
    capture_died: AtomicU64,
}

struct RecBuf {
    raw: Vec<f32>,
    filtered: Vec<f32>,
    target: usize,
    /// Monitor generation this take was armed under: only its own capture
    /// threads may append (a stalled thread from an earlier generation
    /// resuming mid-take would otherwise corrupt/misalign the channels).
    generation: u64,
}

struct Monitors {
    stop: Arc<AtomicBool>,
    /// pw-record children of this generation, recorded at spawn so stop can
    /// SIGTERM them: a thread parked in a blocking read(2) never sees the
    /// stop flag, and the child holding the mic node open is the real
    /// resource to release. Reaping stays with the owning capture thread
    /// (the pid cannot be reused before that reap).
    pids: Arc<Mutex<Vec<u32>>>,
    generation: u64,
}

struct Playback {
    child: Child,
    started: Instant,
}

/// The last completed sample: in-memory buffers plus the runtime-dir WAVs
/// that pw-play plays back.
struct Sample {
    raw: Vec<f32>,
    filtered: Vec<f32>,
    raw_wav: PathBuf,
    filtered_wav: PathBuf,
}

impl Backend {
    pub fn new(
        raw_node: String,
        filtered_node: String,
        cmd_rx: Receiver<Command>,
        frame_tx: Sender<Frame>,
    ) -> Self {
        Backend {
            raw_node,
            filtered_node,
            cmd_rx,
            frame_tx,
        }
    }

    /// Spawn the backend thread. `repaint` is invoked after every emitted
    /// frame so the UI wakes up.
    pub fn start(self, repaint: Box<dyn Fn() + Send + Sync>) {
        std::thread::Builder::new()
            .name("abtest-backend".into())
            .spawn(move || run_backend(self, Arc::from(repaint)))
            .expect("spawn backend thread");
    }
}

fn run_backend(b: Backend, repaint: Arc<dyn Fn() + Send + Sync>) {
    let shared = Arc::new(Shared {
        levels: Mutex::new((DB_FLOOR, DB_FLOOR)),
        rec: Mutex::new(None),
        capture_died: AtomicU64::new(0),
    });
    let emit = |f: Frame| {
        let _ = b.frame_tx.send(f);
        repaint();
    };

    let mut monitors: Option<Monitors> = None;
    let mut playback: Option<Playback> = None;
    let mut sample: Option<Sample> = None;
    let mut device_ok = check_device(&b.raw_node, &b.filtered_node);
    let mut last_device_poll = Instant::now();
    emit(Frame::Device {
        ok: device_ok,
        name: b.raw_node.clone(),
    });

    let stop_monitors = |m: &mut Option<Monitors>, shared: &Shared| {
        if let Some(mon) = m.take() {
            mon.stop.store(true, Ordering::Relaxed);
            // A thread parked in a blocking read(2) never re-checks the
            // flag: SIGTERM the children so the pipes close, the reads
            // return, and the mic node is released NOW (not whenever the
            // stream next produces data). Zombies persist until the owning
            // thread reaps, so the pids cannot be reused underneath us.
            for pid in mon.pids.lock().unwrap().iter() {
                unsafe {
                    libc::kill(*pid as libc::pid_t, libc::SIGTERM);
                }
            }
        }
        *shared.rec.lock().unwrap() = None;
    };
    // Shared by StartMonitor and Record (Record brings the monitors back up
    // itself from the Sample view). Callers guard on `is_none() && device_ok`
    // and bump the generation.
    let start_monitors = |m: &mut Option<Monitors>, generation: u64| {
        let stop = Arc::new(AtomicBool::new(false));
        let pids = Arc::new(Mutex::new(Vec::new()));
        for (node, ch) in [
            (b.raw_node.clone(), Channel::Raw),
            (b.filtered_node.clone(), Channel::Filtered),
        ] {
            spawn_capture_thread(
                node,
                ch,
                Arc::clone(&shared),
                b.frame_tx.clone(),
                Arc::clone(&repaint),
                Arc::clone(&stop),
                Arc::clone(&pids),
                generation,
            );
        }
        *m = Some(Monitors {
            stop,
            pids,
            generation,
        });
    };
    let mut generation: u64 = 0;

    loop {
        let cmd = match b.cmd_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break, // window closed
        };

        match cmd {
            Some(Command::StartMonitor) => {
                if monitors.is_none() && device_ok {
                    generation += 1;
                    start_monitors(&mut monitors, generation);
                }
            }
            Some(Command::Stop) => {
                if let Some(mut p) = playback.take() {
                    stop_child(&mut p.child);
                    emit(Frame::PlaybackDone);
                } else if shared.rec.lock().unwrap().take().is_some() {
                    // Cancelled take: drop the buffers only — the monitors
                    // keep running. The frame re-syncs a UI whose optimistic
                    // cancel raced its own RecordStarted (quick
                    // Record→Cancel), which would otherwise re-enter a
                    // Recording that no longer exists.
                    emit(Frame::RecordCancelled);
                } else {
                    stop_monitors(&mut monitors, &shared);
                    // A deliberate stop still needs a terminal frame: a
                    // cancel that raced a completing take lands here (the
                    // rec was already consumed), and without it the UI
                    // would sit in a Live view with the monitors dead.
                    emit(Frame::MonitorStopped);
                }
            }
            Some(Command::Record) => {
                // From the Sample view the monitors are down; bring them
                // back up before arming. With the device gone this stays a
                // no-op — the Device{ok:false} overlay unwedges the UI's
                // optimistic Recording. During playback (script/race only:
                // the button is disabled) recording is refused outright —
                // the mic must not go hot under a playing sample.
                if playback.is_none() && monitors.is_none() && device_ok {
                    generation += 1;
                    start_monitors(&mut monitors, generation);
                }
                if playback.is_none() && monitors.is_some() {
                    let target = (RECORD_SECS * SAMPLE_RATE as f32) as usize;
                    *shared.rec.lock().unwrap() = Some(RecBuf {
                        raw: Vec::with_capacity(target),
                        filtered: Vec::with_capacity(target),
                        target,
                        generation,
                    });
                    // Authoritative take-start: re-syncs a UI whose
                    // optimistic Recording was knocked to the Sample view
                    // by a stale MonitorStopped racing the click (the mic
                    // IS hot again — the view must say so).
                    emit(Frame::RecordStarted);
                }
            }
            Some(Command::Play(ch)) => {
                // Refused while a take is armed (script/race only — the
                // button is disabled): playback stops the monitors, which
                // would silently destroy the recording while the UI stays
                // in Recording.
                if shared.rec.lock().unwrap().is_some() {
                    // no-op; the UI refused the same command
                } else if let Some(s) = &sample {
                    // Playback ends in the Sample view: live monitoring must
                    // not keep the mic hot behind a sample review (and the
                    // mic would only pick the playback back up anyway).
                    stop_monitors(&mut monitors, &shared);
                    if let Some(mut p) = playback.take() {
                        stop_child(&mut p.child);
                    }
                    let wav = match ch {
                        Channel::Raw => &s.raw_wav,
                        Channel::Filtered => &s.filtered_wav,
                    };
                    match spawn_play(wav) {
                        Ok(child) => {
                            playback = Some(Playback {
                                child,
                                started: Instant::now(),
                            });
                        }
                        Err(e) => {
                            emit(Frame::Warn(format!("playback failed to start: {e}")));
                            emit(Frame::PlaybackDone);
                        }
                    }
                }
            }
            Some(Command::RetryDevice) => last_device_poll = Instant::now() - DEVICE_POLL,
            None => {}
        }

        // A capture pipe died: tear everything down and let the device poll
        // decide whether this is a vanished node or a transient failure.
        // MonitorStopped unwedges the UI even when the poll then finds the
        // nodes still present (pw-record OOM-killed, xrun exit): without a
        // terminal frame the window would pulse Recording/Listening forever.
        // Reports are generation-tagged: a straggler thread of a stopped
        // monitor dying late must not tear down its healthy successor.
        let died_gen = shared.capture_died.swap(0, Ordering::Relaxed);
        if died_gen != 0 && monitors.as_ref().is_some_and(|m| m.generation == died_gen) {
            let was_recording = shared.rec.lock().unwrap().is_some();
            stop_monitors(&mut monitors, &shared);
            if was_recording {
                emit(Frame::Warn(
                    "recording aborted — the capture stream ended unexpectedly".into(),
                ));
            }
            emit(Frame::MonitorStopped);
            last_device_poll = Instant::now() - DEVICE_POLL;
        }

        // Recording completion (both channels filled).
        let finished = {
            let rec = shared.rec.lock().unwrap();
            match rec.as_ref() {
                Some(r) if r.raw.len() >= r.target && r.filtered.len() >= r.target => true,
                Some(r) => {
                    emit(Frame::RecordProgress {
                        secs_done: r.raw.len().min(r.target) as f32 / SAMPLE_RATE as f32,
                    });
                    false
                }
                None => false,
            }
        };
        if finished {
            let r = shared.rec.lock().unwrap().take().unwrap();
            let (mut raw, mut filtered) = (r.raw, r.filtered);
            raw.truncate(r.target);
            filtered.truncate(r.target);
            match write_playback_wavs(&raw, &filtered) {
                Ok((raw_wav, filtered_wav)) => {
                    emit(Frame::RecordDone);
                    let m = metrics::compute(&raw, &filtered);
                    emit(Frame::Metrics(m));
                    sample = Some(Sample {
                        raw,
                        filtered,
                        raw_wav,
                        filtered_wav,
                    });
                }
                Err(e) => {
                    emit(Frame::Warn(format!("could not store the sample: {e}")));
                    stop_monitors(&mut monitors, &shared);
                    emit(Frame::MonitorStopped);
                }
            }
        }

        // Playback progress / completion.
        if let Some(p) = &mut playback {
            match p.child.try_wait() {
                Ok(Some(_)) => {
                    emit(Frame::PlaybackDone);
                    playback = None;
                }
                Ok(None) => {
                    let secs = p.started.elapsed().as_secs_f32();
                    // Belt and suspenders: pw-play exits by itself, but a
                    // wedged stream must not play "forever".
                    if secs > RECORD_SECS + 5.0 {
                        stop_child(&mut p.child);
                        emit(Frame::PlaybackDone);
                        playback = None;
                    } else {
                        emit(Frame::PlaybackProgress { secs });
                        // Meters follow the replay: both channels' levels at
                        // the playhead, from the in-memory take (wall clock
                        // tracks pw-play closely enough for a meter).
                        if let Some(s) = &sample {
                            let end = ((secs * SAMPLE_RATE as f32) as usize).min(s.raw.len());
                            let start = end.saturating_sub(SAMPLE_RATE as usize / 10);
                            if end > start {
                                emit(Frame::Level {
                                    raw_db: rms_db(&s.raw[start..end]),
                                    filtered_db: rms_db(&s.filtered[start..end]),
                                });
                            }
                        }
                    }
                }
                Err(_) => {
                    emit(Frame::PlaybackDone);
                    playback = None;
                }
            }
        }

        // Device watch.
        if last_device_poll.elapsed() >= DEVICE_POLL {
            last_device_poll = Instant::now();
            let ok = check_device(&b.raw_node, &b.filtered_node);
            if ok != device_ok {
                device_ok = ok;
                if !ok {
                    stop_monitors(&mut monitors, &shared);
                    if let Some(mut p) = playback.take() {
                        stop_child(&mut p.child);
                    }
                }
                emit(Frame::Device {
                    ok,
                    name: b.raw_node.clone(),
                });
            }
        }
    }

    // Window closed: reap children and drop the transient playback WAVs.
    stop_monitors(&mut monitors, &shared);
    if let Some(mut p) = playback.take() {
        stop_child(&mut p.child);
    }
    if let Some(s) = sample {
        let _ = std::fs::remove_file(s.raw_wav);
        let _ = std::fs::remove_file(s.filtered_wav);
    }
}

const DEVICE_POLL: Duration = Duration::from_millis(1500);

/// Both required nodes are live PipeWire sources. `None` probes (pw-dump
/// failure) keep the last known state rather than flapping the overlay.
fn check_device(raw_node: &str, filtered_node: &str) -> bool {
    match crate::pipewire::sources_snapshot() {
        Some(nodes) => {
            let has = |n: &str| nodes.iter().any(|s| s.name == n);
            has(raw_node) && has(filtered_node)
        }
        None => true,
    }
}

/// One capture channel: spawn `pw-record`, parse the stream, feed the
/// analyzers, forward frames. The child is PDEATHSIG-bound to THIS thread
/// (it owns and reaps it; abnormal process death must not leave a recorder
/// capturing the mic).
#[allow(clippy::too_many_arguments)]
fn spawn_capture_thread(
    node: String,
    ch: Channel,
    shared: Arc<Shared>,
    frame_tx: Sender<Frame>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    stop: Arc<AtomicBool>,
    pids: Arc<Mutex<Vec<u32>>>,
    generation: u64,
) {
    std::thread::Builder::new()
        .name(format!("abtest-cap-{}", ch.label()))
        .spawn(move || {
            // Death reports carry the generation so the sweep can ignore
            // stragglers of retired monitors; a stopped thread reports
            // nothing at all.
            let died = || {
                if !stop.load(Ordering::Relaxed) {
                    shared.capture_died.store(generation, Ordering::Relaxed);
                }
            };
            let mut child = match spawn_record(&node) {
                Ok(c) => c,
                Err(e) => {
                    let _ = frame_tx.send(Frame::Warn(format!(
                        "could not start pw-record for the {} channel: {e}",
                        ch.label()
                    )));
                    repaint();
                    died();
                    return;
                }
            };
            // Registered so stop_monitors can SIGTERM a child whose pipe
            // never produces data (thread parked in read(2)).
            pids.lock().unwrap().push(child.id());
            let mut stdout = child.stdout.take().expect("piped stdout");
            // Bound the startup. read_header does a blocking read(2); a leg that
            // connects but never streams (a USB source cold-resuming, or a
            // target that silently routed nowhere) would park here forever, so
            // died() never fires and that pane stays blank with no recovery.
            // Give it a cold-resume budget, then SIGTERM so the blocked read
            // returns EOF -> read_header errors -> the leg reports death and the
            // window degrades to a recoverable "Go live".
            let resolved = Arc::new(AtomicBool::new(false));
            {
                let resolved = Arc::clone(&resolved);
                let stop = Arc::clone(&stop);
                let pid = child.id();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1500));
                    if !resolved.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
                        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                    }
                });
            }
            // Disarm the instant read_header RETURNS — before any stop_child
            // reaps the pid — so the watchdog targets a child still blocked in
            // the read. A sub-microsecond load-then-kill window remains (a leg
            // whose header lands right at the deadline can still be SIGTERMed),
            // but the worst case is a recoverable "Go live", not a hang or a
            // stray signal to a healthy long-running child.
            let header = stream::read_header(&mut stdout);
            resolved.store(true, Ordering::Relaxed);
            let reader = match header {
                Ok(Header::Parsed(StreamInfo {
                    channels: 1,
                    sample_rate: SAMPLE_RATE,
                    endian,
                })) => F32Reader::new(stdout, endian, 64 * 1024),
                Ok(Header::Parsed(other)) => {
                    let _ = frame_tx.send(Frame::Warn(format!(
                        "unexpected capture format {other:?} on {}",
                        ch.label()
                    )));
                    repaint();
                    stop_child(&mut child);
                    died();
                    return;
                }
                // pw-cat <= 1.2 writes raw headerless samples; we requested
                // f32/48k/mono explicitly, so that is what the bytes are.
                Ok(Header::Raw(prefix)) => {
                    F32Reader::new_with_prefix(stdout, SampleEndian::Le, 64 * 1024, &prefix)
                }
                Err(e) => {
                    let _ = frame_tx.send(Frame::Warn(format!(
                        "could not parse the {} capture stream: {e}",
                        ch.label()
                    )));
                    repaint();
                    stop_child(&mut child);
                    died();
                    return;
                }
            };
            capture_loop(reader, ch, &shared, &frame_tx, &repaint, &stop, generation);
            stop_child(&mut child);
        })
        .expect("spawn capture thread");
}

fn capture_loop(
    mut reader: F32Reader<std::process::ChildStdout>,
    ch: Channel,
    shared: &Shared,
    frame_tx: &Sender<Frame>,
    repaint: &Arc<dyn Fn() + Send + Sync>,
    stop: &AtomicBool,
    generation: u64,
) {
    let mut spectrum = SpectrumAnalyzer::new();
    let mut level = LevelMeter::new();
    let mut samples = Vec::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match reader.read_samples(&mut samples) {
            Ok(true) => {}
            Ok(false) | Err(_) => {
                if !stop.load(Ordering::Relaxed) {
                    let _ = frame_tx.send(Frame::Warn(format!(
                        "the {} capture stream ended unexpectedly",
                        ch.label()
                    )));
                    repaint();
                    shared.capture_died.store(generation, Ordering::Relaxed);
                }
                return;
            }
        }
        // Stopped mid-read (stop_monitors killed the child): drop the tail
        // silently — no stale frames, no appends into a successor's take.
        if stop.load(Ordering::Relaxed) {
            return;
        }

        for bins in spectrum.feed(&samples) {
            let _ = frame_tx.send(Frame::Spectrum {
                ch,
                bins: bins.to_vec(),
            });
            repaint();
        }
        for db in level.feed(&samples) {
            let mut lv = shared.levels.lock().unwrap();
            match ch {
                Channel::Raw => lv.0 = db,
                Channel::Filtered => lv.1 = db,
            }
            // One combined frame per raw-channel reading (~15 Hz total).
            if ch == Channel::Raw {
                let (raw_db, filtered_db) = *lv;
                drop(lv);
                let _ = frame_tx.send(Frame::Level {
                    raw_db,
                    filtered_db,
                });
                repaint();
            }
        }
        if let Some(rec) = shared.rec.lock().unwrap().as_mut() {
            // Only the take's own generation may append: a stalled thread
            // of an earlier monitor resuming mid-take would interleave
            // stale audio and time-misalign the channels.
            if rec.generation == generation {
                let buf = match ch {
                    Channel::Raw => &mut rec.raw,
                    Channel::Filtered => &mut rec.filtered,
                };
                let take = rec.target.saturating_sub(buf.len()).min(samples.len());
                buf.extend_from_slice(&samples[..take]);
            }
        }
    }
}

fn spawn_record(node: &str) -> std::io::Result<Child> {
    // Old pw-cat (< 0.3.64, e.g. Ubuntu 22.04's 0.3.48) rejects a node NAME as
    // `--target` ("bad target option") and exits before emitting a byte, so the
    // A/B window strands at −∞ with a "failed to fill whole buffer" parse error.
    // record_target resolves the name to its numeric id (pw-cat's original,
    // universally accepted target form), falling back to the name on modern
    // pw-cat / probe failure.
    let target = crate::pipewire::record_target(node);
    let mut c = Proc::new("pw-record");
    c.args([
        "--target",
        target.as_str(),
        "--rate",
        "48000",
        "--channels",
        "1",
        "--format",
        "f32",
        "-",
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    pre_exec_pdeathsig(&mut c);
    c.spawn()
}

fn spawn_play(wav: &std::path::Path) -> std::io::Result<Child> {
    let mut c = Proc::new("pw-play");
    c.arg(wav).stdout(Stdio::null()).stderr(Stdio::null());
    pre_exec_pdeathsig(&mut c);
    c.spawn()
}

/// PR_SET_PDEATHSIG is thread-scoped; every caller owns its child on the
/// spawning thread and reaps it there, so the binding only fires on
/// abnormal teardown (same pattern as mictest/controller).
fn pre_exec_pdeathsig(c: &mut Proc) {
    use std::os::unix::process::CommandExt;
    unsafe {
        c.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// SIGTERM (pw-cat drains cleanly), bounded wait, SIGKILL fallback.
fn stop_child(c: &mut Child) {
    unsafe {
        libc::kill(c.id() as libc::pid_t, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match c.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() >= deadline => {
                let _ = c.kill();
                let _ = c.wait();
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return,
        }
    }
}

/// RMS level of a sample window in dBFS, floored at DB_FLOOR (silence and
/// the empty window collapse to the floor, never to NaN/-inf).
fn rms_db(w: &[f32]) -> f32 {
    if w.is_empty() {
        return DB_FLOOR;
    }
    let mean_sq = w.iter().map(|x| x * x).sum::<f32>() / w.len() as f32;
    // 20*log10(rms) == 10*log10(mean_sq)
    (10.0 * mean_sq.log10()).max(DB_FLOOR)
}

/// Transient playback WAVs live in the private runtime dir (user's voice —
/// same policy as the mic test) under fixed names, overwritten per take.
fn write_playback_wavs(raw: &[f32], filtered: &[f32]) -> std::io::Result<(PathBuf, PathBuf)> {
    let dir = crate::mictest::work_dir()?;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let write = |path: &PathBuf, data: &[f32]| -> std::io::Result<()> {
        let mut w = hound::WavWriter::create(path, spec).map_err(std::io::Error::other)?;
        for &x in data {
            w.write_sample((x.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .map_err(std::io::Error::other)?;
        }
        w.finalize().map_err(std::io::Error::other)
    };
    let raw_wav = dir.join("abtest-raw.wav");
    let filtered_wav = dir.join("abtest-filtered.wav");
    write(&raw_wav, raw)?;
    write(&filtered_wav, filtered)?;
    Ok((raw_wav, filtered_wav))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_db_floors_empty_and_silence_and_measures_known_levels() {
        assert_eq!(rms_db(&[]), DB_FLOOR);
        assert_eq!(rms_db(&[0.0; 128]), DB_FLOOR);
        // Full-scale DC: 0 dBFS.
        assert!(rms_db(&[1.0; 128]).abs() < 1e-4);
        // Half scale: −6.02 dB.
        assert!((rms_db(&[0.5; 128]) + 6.0206).abs() < 0.01);
        assert!((rms_db(&[-0.5; 128]) + 6.0206).abs() < 0.01);
    }
}
