//! "Test my mic": record a few seconds of the raw microphone and of the
//! cleaned `hushmic_source` side by side, then play both back — an audible
//! is-it-even-working check, driven from the tray.
//!
//! The recording/playback helpers are `pw-record`/`pw-play` (pw-cat), which
//! ship in the same PipeWire tool package as the `pw-dump`/`pw-metadata`
//! binaries the app already requires. The flow runs on a dedicated worker
//! thread; progress is reported via desktop notifications. Every blocking
//! point is bounded (deadline or cancellation flag): the worker must always
//! terminate and report back, or the tray's "test running" state would
//! wedge forever.

use crate::notify::{self, Slot};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const RECORD_SECS: u64 = 5;

/// The worker's result when the test was aborted because the filter-chain
/// was reconfigured/restarted mid-test (any chain mutation invalidates the
/// cleaned leg). Compared by the main loop to soften the notification.
pub const CANCELLED_MSG: &str = "Stopped — the audio settings changed during the test.";

const ICON: &str = "audio-input-microphone";
const RAW_NAME: &str = "mictest-raw.wav";
const CLEAN_NAME: &str = "mictest-cleaned.wav";

/// Whether a mic test can start right now; the error is user-facing
/// (surfaced as a notification). Requires the chain to be up: the point of
/// the test is the raw-vs-cleaned comparison, and only a running chain says
/// which physical mic is actually in use.
pub fn precondition(
    enabled: bool,
    node_present: Option<bool>,
    already_running: bool,
) -> Result<(), &'static str> {
    if already_running {
        Err("A mic test is already running.")
    } else if !enabled {
        Err(
            "Turn on noise suppression first — the test compares the raw microphone \
             with the cleaned HushMic output.",
        )
    } else {
        // None = the pw-dump probe itself failed: unknown, NOT "node gone".
        match node_present {
            Some(true) => Ok(()),
            Some(false) => {
                Err("The HushMic virtual microphone is not up yet — try again in a few seconds.")
            }
            None => Err("Could not query PipeWire for the microphones — is PipeWire running?"),
        }
    }
}

/// The node feeding `sink_node`, traced through the link graph of a
/// `pw-dump` snapshot: find `sink_node`'s object id, the Link whose
/// `input-node-id` is that id, and the Node behind the link's
/// `output-node-id`. Pure function — no I/O.
///
/// This is the ground truth for the raw-mic leg: when no mic is configured
/// the chain follows the system default, and when "set as default" is
/// active the system default *is* `hushmic_source` — only the live link
/// says which physical device actually feeds the chain.
pub fn find_feeding_node(pwdump_json: &str, sink_node: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(pwdump_json).ok()?;
    let arr = v.as_array()?;

    let mut sink_id = None;
    for o in arr {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let props = o.get("info").and_then(|i| i.get("props"));
        if props
            .and_then(|p| p.get("node.name"))
            .and_then(|n| n.as_str())
            == Some(sink_node)
        {
            sink_id = o.get("id").and_then(|i| i.as_u64());
            break;
        }
    }
    let sink_id = sink_id?;

    let mut feeder_id = None;
    for o in arr {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Link") {
            continue;
        }
        let Some(info) = o.get("info") else { continue };
        if info.get("input-node-id").and_then(|i| i.as_u64()) == Some(sink_id) {
            feeder_id = info.get("output-node-id").and_then(|i| i.as_u64());
            break;
        }
    }
    let feeder_id = feeder_id?;

    for o in arr {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        if o.get("id").and_then(|i| i.as_u64()) != Some(feeder_id) {
            continue;
        }
        return o
            .get("info")
            .and_then(|i| i.get("props"))
            .and_then(|p| p.get("node.name"))
            .and_then(|n| n.as_str())
            .map(String::from);
    }
    None
}

/// The node to record the raw leg from: the traced live feeder wins (it is
/// what the user actually hears through HushMic right now); the configured
/// mic is the fallback when the trace comes up empty.
pub fn raw_target(traced: Option<String>, cfg_mic: Option<&str>) -> Option<String> {
    traced.or_else(|| cfg_mic.map(String::from))
}

/// The raw-leg source for the A/B window, in priority order: the live traced
/// feeder, then the tray-configured mic, then the system default source.
///
/// The default-source fallback is what stops the window from showing a false
/// "no microphone" when the chain is up but not linked to any mic and the
/// tray is on "System default": there is a real default mic to record the
/// raw leg from even though nothing currently feeds `hushmic_input`. It must
/// skip our own `hushmic_source` (when HushMic is itself the configured
/// default) and any `.monitor` — neither is a real capture device to compare
/// against. Empty string only when nothing resolves. Pure — no I/O.
pub fn resolve_raw(
    traced: Option<String>,
    cfg_mic: Option<&str>,
    default_source: Option<String>,
) -> String {
    raw_target(traced, cfg_mic)
        .or_else(|| default_source.filter(|n| n != "hushmic_source" && !n.ends_with(".monitor")))
        .unwrap_or_default()
}

/// `pw-record` invocation for one leg: mono 48 kHz WAV (the chain's native
/// format) pinned to an explicit source node.
pub fn record_command(target: &str, out: &Path) -> Command {
    let mut c = Command::new("pw-record");
    c.args(["--target", target, "--rate", "48000", "--channels", "1"])
        .arg(out);
    c
}

/// `pw-play` invocation: default output device (the user must hear it).
pub fn play_command(file: &Path) -> Command {
    let mut c = Command::new("pw-play");
    c.arg(file);
    c
}

/// The directories a recording could live in (no I/O, no creation).
fn candidate_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR") {
        if !d.is_empty() {
            v.push(Path::new(&d).join("hushmic"));
        }
    }
    let uid = unsafe { libc::geteuid() };
    v.push(std::env::temp_dir().join(format!("hushmic-mictest-{uid}")));
    v
}

/// Best-effort removal of recordings a previous run may have left behind:
/// run()'s own cleanup is skipped when the process exits mid-test (Quit,
/// SIGTERM, crash), and the user's voice must not outlive the app. Called
/// at startup and on the way out.
pub fn remove_recordings() {
    remove_recordings_from(&candidate_dirs());
}

fn remove_recordings_from(dirs: &[PathBuf]) {
    // Includes the A/B window's transient playback WAVs: its backend deletes
    // them on clean exit, but an abnormal death (crash, SIGKILL) skips that
    // path just like the mic test's own.
    for d in dirs {
        for f in [
            RAW_NAME,
            CLEAN_NAME,
            "abtest-raw.wav",
            "abtest-filtered.wav",
        ] {
            let _ = std::fs::remove_file(d.join(f));
        }
    }
}

/// Where the recordings live. Recordings are the user's voice: the runtime
/// dir is per-user 0700; the /tmp fallback is created 0700,
/// ownership-checked (same squat defense as the lock file), and its mode is
/// re-asserted when it already exists.
pub(crate) fn work_dir() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        let base = Path::new(&d);
        if base.is_dir() {
            let dir = base.join("hushmic");
            std::fs::create_dir_all(&dir)?;
            // The runtime dir is per-user 0700 by spec, but recordings live
            // in here: assert our subdir too (parity with the /tmp path).
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            return Ok(dir);
        }
    }
    let uid = unsafe { libc::geteuid() };
    let dir = std::env::temp_dir().join(format!("hushmic-mictest-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let md = std::fs::symlink_metadata(&dir)?;
            if !md.is_dir() || md.uid() != uid {
                return Err(std::io::Error::other(format!(
                    "refusing to record into {}: not a directory owned by this user",
                    dir.display()
                )));
            }
            // A leftover dir may carry a laxer mode (older runs, umask
            // accidents); the recordings inside must stay private.
            let mut perm = md.permissions();
            perm.set_mode(0o700);
            std::fs::set_permissions(&dir, perm)?;
        }
        Err(e) => return Err(e),
    }
    Ok(dir)
}

/// Spawn a helper bound to this worker thread's lifetime via
/// PR_SET_PDEATHSIG (thread-scoped, like the controller's filter-chain
/// spawn): if hushmic dies mid-test, an orphaned pw-record must not keep
/// capturing the microphone. On every normal path the worker outlives its
/// helpers (it waits on them), so the binding only fires on abnormal
/// teardown. stderr is piped so failures can be quoted in the error
/// message (pw-cat prints little — the pipe cannot fill up).
fn spawn_bound(mut cmd: Command) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;
    cmd.stderr(Stdio::piped());
    unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()
}

/// Stop a helper: SIGTERM first (pw-cat drains and finalizes the WAV
/// header on it), bounded wait, SIGKILL as a last resort.
fn stop_helper(c: &mut Child) {
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

/// The last stderr line of an exited helper, as an error-message suffix
/// (" — <line>"), or empty. Only meaningful after the child has exited.
fn stderr_suffix(c: &mut Child) -> String {
    let mut s = String::new();
    if let Some(mut e) = c.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut s);
    }
    match s.trim().lines().last() {
        Some(line) if !line.is_empty() => {
            let line: String = line.chars().take(200).collect();
            format!(" — {line}")
        }
        _ => String::new(),
    }
}

/// Play `file` to the default output, bounded: the file is RECORD_SECS
/// long, so anything beyond that plus a margin means the stream is stuck
/// (output device vanished mid-test) and must be reaped, not waited on.
fn play(file: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let mut child = spawn_bound(play_command(file))
        .map_err(|e| format!("could not start pw-play ({e}) — it ships with the PipeWire tools"))?;
    let deadline = Instant::now() + Duration::from_secs(RECORD_SECS + 5);
    loop {
        if cancel.load(Ordering::Relaxed) {
            stop_helper(&mut child);
            return Err(CANCELLED_MSG.to_string());
        }
        match child.try_wait() {
            Ok(Some(st)) if st.success() => return Ok(()),
            Ok(Some(st)) => {
                let detail = stderr_suffix(&mut child);
                return Err(format!(
                    "pw-play failed ({st}){detail} — is an output device available?"
                ));
            }
            Ok(None) if Instant::now() >= deadline => {
                stop_helper(&mut child);
                return Err("playback never finished — is an output device available?".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("pw-play did not finish: {e}")),
        }
    }
}

/// The full record-then-play flow. Runs on a dedicated worker thread.
/// `cancel` is set by the main loop when the filter-chain is mutated
/// mid-test (which invalidates the cleaned leg).
pub fn run(raw_node: &str, cancel: &AtomicBool) -> Result<(), String> {
    let dir = work_dir().map_err(|e| format!("no private directory for the recordings: {e}"))?;
    let raw_wav = dir.join(RAW_NAME);
    let clean_wav = dir.join(CLEAN_NAME);
    let result = record_and_play(raw_node, &raw_wav, &clean_wav, cancel);
    // The recordings are the user's voice: never leave them behind. (Exits
    // that skip this — Quit/SIGTERM/crash mid-test — are swept by
    // remove_recordings() at startup and on the way out.)
    let _ = std::fs::remove_file(&raw_wav);
    let _ = std::fs::remove_file(&clean_wav);
    result
}

fn record_and_play(
    raw_node: &str,
    raw_wav: &Path,
    clean_wav: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    notify::send_transient(
        Slot::MicTest,
        ICON,
        "Mic test",
        &format!("Recording {RECORD_SECS} seconds — speak into your microphone…"),
    );
    let started = Instant::now();
    // pw-cat < 0.3.64 (e.g. Ubuntu 22.04's 0.3.48) rejects a node NAME as
    // `--target` and the recorder dies at start; resolve name→numeric id (see
    // pipewire::record_target). Falls back to the name on modern pw-cat.
    let raw_target = crate::pipewire::record_target(raw_node);
    let clean_target = crate::pipewire::record_target("hushmic_source");
    let mut raw = spawn_bound(record_command(&raw_target, raw_wav)).map_err(|e| {
        format!("could not start pw-record ({e}) — it ships with the PipeWire tools")
    })?;
    let mut clean = match spawn_bound(record_command(&clean_target, clean_wav)) {
        Ok(c) => c,
        Err(e) => {
            stop_helper(&mut raw);
            return Err(format!("could not start the second pw-record: {e}"));
        }
    };

    // A recorder that dies instantly (bad target, no permission) must not
    // cost the user the full speak window before being reported.
    std::thread::sleep(Duration::from_millis(400));
    if let Ok(Some(st)) = raw.try_wait() {
        let detail = stderr_suffix(&mut raw);
        stop_helper(&mut clean);
        return Err(format!(
            "recording the raw microphone failed at start ({st}){detail}"
        ));
    }
    if let Ok(Some(st)) = clean.try_wait() {
        let detail = stderr_suffix(&mut clean);
        stop_helper(&mut raw);
        return Err(format!(
            "recording the cleaned microphone failed at start ({st}){detail}"
        ));
    }

    while started.elapsed() < Duration::from_secs(RECORD_SECS) {
        if cancel.load(Ordering::Relaxed) {
            stop_helper(&mut raw);
            stop_helper(&mut clean);
            return Err(CANCELLED_MSG.to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop_helper(&mut raw);
    stop_helper(&mut clean);

    for (what, p, rec) in [
        ("raw", raw_wav, &mut raw),
        ("cleaned", clean_wav, &mut clean),
    ] {
        // 44 bytes = an empty canonical WAV (header only).
        let has_audio = std::fs::metadata(p).map(|m| m.len() > 44).unwrap_or(false);
        if !has_audio {
            let detail = stderr_suffix(rec);
            return Err(format!(
                "the {what} recording came out empty{detail} — is the microphone capturing?"
            ));
        }
    }

    notify::send_transient(
        Slot::MicTest,
        ICON,
        "Mic test",
        "Playing the raw recording (without HushMic)…",
    );
    play(raw_wav, cancel)?;
    notify::send_transient(
        Slot::MicTest,
        ICON,
        "Mic test",
        "Playing the cleaned recording (what your apps hear)…",
    );
    play(clean_wav, cancel)?;
    notify::send_transient(
        Slot::MicTest,
        ICON,
        "Mic test",
        "Mic test finished — the second playback should have had much less background noise.",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precondition_gating() {
        assert!(precondition(true, Some(true), false).is_ok());
        assert!(precondition(true, Some(true), true).is_err()); // already running
        assert!(precondition(false, Some(true), false).is_err()); // disabled
        assert!(precondition(true, Some(false), false).is_err()); // node gone
        assert!(precondition(true, None, false).is_err()); // probe failed
    }

    #[test]
    fn precondition_distinguishes_probe_failure_from_absence() {
        // None = pw-dump itself failed: the message must not claim the node
        // is down (None-means-unknown invariant), and the two cases need
        // different advice.
        let probe_failed = precondition(true, None, false).unwrap_err();
        let node_absent = precondition(true, Some(false), false).unwrap_err();
        assert_ne!(probe_failed, node_absent);
        assert!(probe_failed.contains("PipeWire"), "{probe_failed}");
        assert!(!probe_failed.contains("not up yet"), "{probe_failed}");
    }

    // Shape mirrors real pw-dump output: Node objects carry node.name under
    // info.props, Link objects carry output-node-id/input-node-id under info.
    const DUMP: &str = r#"[
      {"id": 30, "type": "PipeWire:Interface:Node",
       "info": {"props": {"node.name": "alsa_input.usb-mic", "media.class": "Audio/Source"}}},
      {"id": 42, "type": "PipeWire:Interface:Node",
       "info": {"props": {"node.name": "hushmic_input", "media.class": "Stream/Input/Audio"}}},
      {"id": 43, "type": "PipeWire:Interface:Node",
       "info": {"props": {"node.name": "hushmic_source", "media.class": "Audio/Source"}}},
      {"id": 77, "type": "PipeWire:Interface:Port", "info": {}},
      {"id": 50, "type": "PipeWire:Interface:Link",
       "info": {"output-node-id": 43, "input-node-id": 99}},
      {"id": 51, "type": "PipeWire:Interface:Link",
       "info": {"output-node-id": 30, "input-node-id": 42}}
    ]"#;

    #[test]
    fn traces_the_feeder_through_the_link_graph() {
        assert_eq!(
            find_feeding_node(DUMP, "hushmic_input").as_deref(),
            Some("alsa_input.usb-mic")
        );
    }

    #[test]
    fn trace_misses_yield_none() {
        assert_eq!(find_feeding_node(DUMP, "no_such_node"), None);
        assert_eq!(find_feeding_node("not json", "hushmic_input"), None);
        assert_eq!(find_feeding_node("[]", "hushmic_input"), None);
        // node exists but nothing links INTO it (hushmic_source only links out)
        assert_eq!(find_feeding_node(DUMP, "hushmic_source"), None);
    }

    #[test]
    fn stereo_mic_double_link_resolves_to_one_feeder() {
        // A stereo capture feeding the mono chain produces one link per
        // channel — both from the same node. The trace must settle on it.
        const STEREO: &str = r#"[
          {"id": 30, "type": "PipeWire:Interface:Node",
           "info": {"props": {"node.name": "alsa_input.usb-mic", "media.class": "Audio/Source"}}},
          {"id": 42, "type": "PipeWire:Interface:Node",
           "info": {"props": {"node.name": "hushmic_input", "media.class": "Stream/Input/Audio"}}},
          {"id": 50, "type": "PipeWire:Interface:Link",
           "info": {"output-node-id": 30, "input-node-id": 42}},
          {"id": 51, "type": "PipeWire:Interface:Link",
           "info": {"output-node-id": 30, "input-node-id": 42}}
        ]"#;
        assert_eq!(
            find_feeding_node(STEREO, "hushmic_input").as_deref(),
            Some("alsa_input.usb-mic")
        );
    }

    #[test]
    fn raw_target_prefers_the_live_trace() {
        assert_eq!(
            raw_target(Some("traced".into()), Some("configured")).as_deref(),
            Some("traced")
        );
        assert_eq!(
            raw_target(None, Some("configured")).as_deref(),
            Some("configured")
        );
        assert_eq!(raw_target(None, None), None);
    }

    #[test]
    fn resolve_raw_falls_back_to_the_default_source() {
        // Trace and configured mic win ahead of the default, in that order.
        assert_eq!(
            resolve_raw(Some("traced".into()), Some("cfg"), Some("dflt".into())),
            "traced"
        );
        assert_eq!(resolve_raw(None, Some("cfg"), Some("dflt".into())), "cfg");
        // The real fix: chain up but unlinked + "System default" selected ->
        // record the raw leg from the actual default mic instead of "" (which
        // rendered a false "no microphone detected").
        assert_eq!(
            resolve_raw(None, None, Some("alsa_input.mic".into())),
            "alsa_input.mic"
        );
        // But never fall back to our own node or a monitor — those are not a
        // real capture device to compare against.
        assert_eq!(resolve_raw(None, None, Some("hushmic_source".into())), "");
        assert_eq!(
            resolve_raw(None, None, Some("alsa_output.x.monitor".into())),
            ""
        );
        // Nothing to resolve.
        assert_eq!(resolve_raw(None, None, None), "");
    }

    #[test]
    fn record_command_pins_target_and_format() {
        let c = record_command("alsa_input.x", Path::new("/run/user/1000/hushmic/raw.wav"));
        assert_eq!(c.get_program(), "pw-record");
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--target",
                "alsa_input.x",
                "--rate",
                "48000",
                "--channels",
                "1",
                "/run/user/1000/hushmic/raw.wav"
            ]
        );
    }

    #[test]
    fn play_command_uses_default_output() {
        let c = play_command(Path::new("/tmp/x.wav"));
        assert_eq!(c.get_program(), "pw-play");
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["/tmp/x.wav"]); // no --target: default sink
    }

    #[test]
    fn remove_recordings_sweeps_only_the_recordings() {
        let scratch =
            std::env::temp_dir().join(format!("hushmic-sweeptest-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let victim = scratch.join(RAW_NAME);
        let victim2 = scratch.join(CLEAN_NAME);
        let bystander = scratch.join("keep.txt");
        std::fs::write(&victim, b"x").unwrap();
        std::fs::write(&victim2, b"x").unwrap();
        std::fs::write(&bystander, b"x").unwrap();
        remove_recordings_from(std::slice::from_ref(&scratch));
        assert!(!victim.exists(), "stale raw recording must be swept");
        assert!(!victim2.exists(), "stale cleaned recording must be swept");
        assert!(bystander.exists(), "unrelated files must survive");
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
