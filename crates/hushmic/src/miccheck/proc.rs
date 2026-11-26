//! The PipeWire tools Mic check runs: recorders, the keep-alive consumer
//! and the players. Every child is bound to the thread that spawned it
//! (PR_SET_PDEATHSIG) and reaped by it.

use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Kill the child when the spawning thread dies (thread-scoped, see the
/// controller's note on PR_SET_PDEATHSIG).
pub fn pdeathsig(c: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: prctl is async-signal-safe; nothing else runs in the hook.
    unsafe {
        c.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// SIGTERM (pw-cat drains cleanly), a bounded wait, then SIGKILL.
pub fn stop_child(c: &mut Child) {
    // SAFETY: plain kill(2) on our own unreaped child.
    unsafe {
        libc::kill(c.id() as libc::pid_t, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match c.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) if Instant::now() >= deadline => {
                let _ = c.kill();
                let _ = c.wait();
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// The streams Mic check and the level probe record with: what the
/// desktop's microphone indicator and PipeWire tools show for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Mic check's recordings.
    MicCheck,
    /// The window's keep-alive on hushmic_source while a page shows its
    /// meter: in mixers and pw-top as "HushMic window (meter)".
    Meter,
    /// First run's level meter.
    Level,
    /// The Files page playing a file.
    File,
    /// `hushmic test`'s replay.
    MicTest,
}

impl Purpose {
    /// Stream properties (`-P`), where this pw-cat takes them: the app's
    /// name instead of "pw-record", and a node name that says whose
    /// stream it is.
    pub fn props(self) -> Option<String> {
        if !pw_cat_lists("--properties") {
            return None;
        }
        let (media, node) = match self {
            Purpose::MicCheck => ("Mic check", "hushmic-miccheck"),
            Purpose::Meter => ("HushMic window (meter)", "hushmic-meter"),
            Purpose::Level => ("Microphone level", "hushmic-level"),
            Purpose::File => ("File playback", "hushmic-file"),
            Purpose::MicTest => ("Mic test", "hushmic-mictest"),
        };
        Some(format!(
            r#"{{ application.name = "HushMic" media.name = "{media}" node.name = "{node}" }}"#
        ))
    }
}

/// Record `node` (name, or id on old PipeWire) as 48 kHz mono f32 to
/// stdout.
pub fn spawn_record_pipe(node: &str, purpose: Purpose) -> io::Result<Child> {
    let target = crate::pipewire::record_target(node);
    let mut c = Command::new("pw-record");
    if let Some(p) = purpose.props() {
        c.args(["-P", &p]);
    }
    c.args([
        "--target",
        &target,
        "--rate",
        "48000",
        "--channels",
        "1",
        "--format",
        "f32",
        "-",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    pdeathsig(&mut c);
    c.spawn()
}

/// The keep-alive consumer: records `node` into /dev/null so PipeWire
/// keeps the chain running while the page listens (a file target works on
/// every pw-cat, also those that cannot write to a pipe). This is the
/// recording the desktop's microphone indicator shows, which is honest on
/// the Live page.
pub fn spawn_keep_alive(node: &str) -> io::Result<Child> {
    let target = crate::pipewire::record_target(node);
    let mut c = Command::new("pw-record");
    if let Some(p) = Purpose::Meter.props() {
        c.args(["-P", &p]);
    }
    c.args([
        "--target",
        &target,
        "--rate",
        "48000",
        "--channels",
        "1",
        "--format",
        "s16",
        "/dev/null",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    pdeathsig(&mut c);
    c.spawn()
}

/// Whether this pw-cat's help lists `flag` (asked once per process).
fn pw_cat_lists(flag: &str) -> bool {
    static HELP: OnceLock<String> = OnceLock::new();
    HELP.get_or_init(|| {
        Command::new("pw-cat")
            .arg("--help")
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .stdout(Stdio::piped())
            .output()
            .map(|o| String::from_utf8_lossy(&[o.stdout, o.stderr].concat()).into_owned())
            .unwrap_or_default()
    })
    .contains(flag)
}

/// pw-cat names raw mode explicitly (newer PipeWire reads `-` through
/// libsndfile otherwise, which needs a container header).
fn pw_cat_has_raw_flag() -> bool {
    pw_cat_lists("--raw")
}

/// Where HushMic's own players play: while HushMic Speakers runs, a real
/// output named (the default, or another one while the default is HushMic
/// Speakers, so what they play is not cleaned a second time), else None,
/// the system default as any player
/// ([`crate::speakers::graph::own_player_pick`]). Asked at most every two
/// seconds; a player spawns on a thread of its own, never the loop.
#[cfg(feature = "speakers")]
pub fn own_output() -> Option<String> {
    static LAST: std::sync::Mutex<Option<(Instant, Option<String>)>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, target)) = last.as_ref() {
        if at.elapsed() < Duration::from_secs(2) {
            return target.clone();
        }
    }
    let target = crate::speakers::graph::own_player_target_now();
    *last = Some((Instant::now(), target.clone()));
    target
}

/// Where HushMic's own players play: the system default, as any player.
#[cfg(not(feature = "speakers"))]
pub fn own_output() -> Option<String> {
    None
}

/// Play 48 kHz mono f32 samples written to the child's stdin.
pub fn spawn_feed() -> io::Result<Child> {
    spawn_feed_as(Purpose::MicCheck, 1)
}

/// Play 48 kHz f32 frames of `channels` (interleaved) written to the
/// child's stdin, as `purpose`'s stream.
pub fn spawn_feed_as(purpose: Purpose, channels: usize) -> io::Result<Child> {
    let mut c = Command::new("pw-cat");
    if let Some(p) = purpose.props() {
        c.args(["-P", &p]);
    }
    let channels = channels.to_string();
    c.args([
        "--playback",
        "--rate",
        "48000",
        "--channels",
        &channels,
        "--format",
        "f32",
        "--latency",
        "20ms",
    ]);
    if let Some(t) = own_output() {
        c.args(["--target", &t]);
    }
    if pw_cat_has_raw_flag() {
        c.arg("--raw");
    }
    c.arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    pdeathsig(&mut c);
    c.spawn()
}

/// Play a WAV file (the restart fallback).
pub fn spawn_play(path: &Path) -> io::Result<Child> {
    spawn_play_as(path, Purpose::MicCheck)
}

/// Play a WAV file as `purpose`'s stream.
pub fn spawn_play_as(path: &Path, purpose: Purpose) -> io::Result<Child> {
    let mut c = Command::new("pw-play");
    if let Some(p) = purpose.props() {
        c.args(["-P", &p]);
    }
    if let Some(t) = own_output() {
        c.args(["--target", &t]);
    }
    c.arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    pdeathsig(&mut c);
    c.spawn()
}

/// Write mono 48 kHz f32 `samples` as a private WAV file (0600).
pub fn write_wav(path: &Path, samples: &[f32]) -> io::Result<()> {
    write_wav_as(path, samples, 1)
}

/// Write 48 kHz f32 frames of `channels` (interleaved) as a private WAV
/// file (0600).
pub fn write_wav_as(path: &Path, samples: &[f32], channels: usize) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let spec = hound::WavSpec {
        channels: channels as u16,
        sample_rate: 48_000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::new(io::BufWriter::new(file), spec).map_err(io::Error::other)?;
    for &s in samples {
        w.write_sample(s).map_err(io::Error::other)?;
    }
    w.finalize().map_err(io::Error::other)
}

/// Where Mic check keeps its files: the per-user private runtime dir.
pub fn work_dir() -> Option<std::path::PathBuf> {
    crate::meter::runtime_dir().filter(|d| crate::meter::prepare_dir(d).is_ok())
}
