//! CLI control surface: the request/response protocol spoken over the
//! control socket, the toggle state machine, and the status renderers.
//! The tray's main loop owns all state; this module is pure except for
//! the socket plumbing of the listener/client.

use crate::controller::RunMode;

/// A validated control request. `SetMode(None)` = Off (the persisted
/// disable path, exactly like the tray radio).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Request {
    Status { json: bool },
    GetMode,
    SetMode(Option<RunMode>),
    Toggle(RunMode), // Bypass or Mute only
}

/// Parse subcommand words (CLI argv tail or a socket request line) into a
/// `Request`. `Err` carries a one-line usage message (CLI exit 1).
pub fn parse_request(words: &[&str]) -> Result<Request, String> {
    const USAGE: &str =
        "usage: hushmic status [--json] | mode [suppress|bypass|mute|off] | toggle mute|bypass";
    match words {
        ["status"] => Ok(Request::Status { json: false }),
        ["status", "--json"] => Ok(Request::Status { json: true }),
        ["mode"] => Ok(Request::GetMode),
        ["mode", state] => match *state {
            "suppress" => Ok(Request::SetMode(Some(RunMode::Suppress))),
            "bypass" => Ok(Request::SetMode(Some(RunMode::Bypass))),
            "mute" => Ok(Request::SetMode(Some(RunMode::Mute))),
            "off" => Ok(Request::SetMode(None)),
            other => Err(format!("unknown mode '{other}'\n{USAGE}")),
        },
        ["toggle", which] => match *which {
            "mute" => Ok(Request::Toggle(RunMode::Mute)),
            "bypass" => Ok(Request::Toggle(RunMode::Bypass)),
            other => Err(format!("cannot toggle '{other}' (mute|bypass)\n{USAGE}")),
        },
        _ => Err(USAGE.to_string()),
    }
}

/// The state word for a mode selection: suppress | bypass | mute | off.
pub fn mode_word(sel: Option<RunMode>) -> &'static str {
    match sel {
        Some(RunMode::Suppress) => "suppress",
        Some(RunMode::Bypass) => "bypass",
        Some(RunMode::Mute) => "mute",
        None => "off",
    }
}

/// The toggle transition: entering `target` unless already there, in which
/// case return to the remembered previous chain-alive state. Never yields
/// Off — a mute hotkey must not kill the mic entirely.
pub fn toggle_next(
    current: Option<RunMode>,
    prev_alive: RunMode,
    target: RunMode,
) -> Option<RunMode> {
    if current == Some(target) {
        Some(prev_alive)
    } else {
        Some(target)
    }
}

/// Update the remembered previous chain-alive state after a transition
/// `old -> new` (from ANY path — tray radio or CLI — so both round-trip).
/// Coming out of Off remembers suppress: toggle's return leg must never
/// resurrect Off.
pub fn update_prev_alive(prev: RunMode, old: Option<RunMode>, new: Option<RunMode>) -> RunMode {
    match (old, new) {
        (None, Some(_)) => RunMode::Suppress,
        (Some(m), Some(n)) if m != n => m,
        (Some(m), None) => m,
        _ => prev,
    }
}

/// Everything `status` reports, snapshotted by the main loop.
#[derive(Clone, Debug)]
pub struct Status {
    pub version: String,
    pub mode: Option<RunMode>,
    pub mic_configured: Option<String>,
    pub mic_active: Option<String>,
    pub fallback_active: bool,
    pub model: String,
    pub attn_limit: f32,
    pub chain_running: bool,
    pub node_present: Option<bool>,
}

pub fn render_status_human(s: &Status) -> String {
    let mode = match s.mode {
        Some(RunMode::Suppress) => "noise suppression",
        Some(RunMode::Bypass) => "bypass",
        Some(RunMode::Mute) => "mute",
        None => "off",
    };
    let mic = match (&s.mic_configured, s.fallback_active) {
        (Some(m), true) => format!("{m} (unplugged — using system default)"),
        (Some(m), false) => m.clone(),
        (None, _) => "system default".to_string(),
    };
    let chain = if !s.chain_running {
        "not running"
    } else {
        match s.node_present {
            Some(true) => "running",
            Some(false) => "running, node missing",
            None => "running (node state unknown)",
        }
    };
    format!(
        "hushmic {} — mode: {}\nmic: {}\nmodel: {}  strength: {} dB\nlatency: {} ms added\nchain: {}\n",
        s.version,
        mode,
        mic,
        s.model,
        s.attn_limit,
        crate::controller::LATENCY_SAMPLES * 1000 / 48_000,
        chain
    )
}

pub fn render_status_json(s: &Status) -> String {
    serde_json::json!({
        "version": s.version,
        "mode": mode_word(s.mode),
        "enabled": s.mode.is_some(),
        "mic": {
            "configured": s.mic_configured,
            "active": s.mic_active,
            "fallback_active": s.fallback_active,
        },
        "model": s.model,
        "attn_limit": s.attn_limit,
        "latency_samples": crate::controller::LATENCY_SAMPLES,
        "chain": {
            "running": s.chain_running,
            "node_present": s.node_present,
        },
    })
    .to_string()
}

/// Wire format: `ok\n<payload>` / `err <message>\n`. The connection close
/// delimits the payload — no framing.
pub fn encode_ok(payload: &str) -> String {
    format!("ok\n{payload}")
}

pub fn encode_err(msg: &str) -> String {
    format!("err {msg}\n")
}

/// Split a raw response into (ok, payload-or-message). Anything that is
/// not a well-formed `ok` decodes as an error — a foreign peer's bytes
/// must never read as success.
pub fn decode_response(raw: &str) -> (bool, String) {
    if let Some(payload) = raw.strip_prefix("ok\n") {
        (true, payload.to_string())
    } else if let Some(msg) = raw.strip_prefix("err ") {
        (false, msg.trim_end_matches('\n').to_string())
    } else {
        (false, format!("unrecognized response: {raw}"))
    }
}

// --- socket plumbing --------------------------------------------------------

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// One request from a CLI client, delivered to the main loop; the reply
/// string (already `encode_ok`/`encode_err`-framed) goes back through
/// `reply` and the listener writes it to the peer.
pub struct ControlReq {
    pub line: String,
    pub reply: Sender<String>,
}

/// Per-connection socket I/O budget. A peer that connects and stalls gets
/// dropped after this, so the (serial) listener can serve the next client.
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the listener waits for the main loop's reply. A mode change can
/// legitimately take a couple of seconds (restart fallback); 10 s outlasts
/// that without letting a wedged loop pile up connections forever.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest request line we accept; real requests are tens of bytes.
const MAX_REQUEST: usize = 4096;

/// Serve the control socket on a background thread: one request line per
/// connection, delivered as `ControlReq` into `tx`; exits when the main
/// loop's receiver is gone.
pub fn spawn_listener(listener: UnixListener, tx: Sender<ControlReq>) {
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut stream) = conn else { continue };
            let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
            let mut raw = Vec::new();
            let mut buf = [0u8; 512];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break, // EOF, timeout, or error
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if raw.contains(&b'\n') || raw.len() > MAX_REQUEST {
                            break;
                        }
                    }
                }
            }
            let text = String::from_utf8_lossy(&raw);
            let line = text.lines().next().unwrap_or("").trim().to_string();
            if line.is_empty() {
                continue; // stalled or empty peer: drop it, serve the next
            }
            let (rtx, rrx) = std::sync::mpsc::channel();
            if tx.send(ControlReq { line, reply: rtx }).is_err() {
                return; // main loop gone — tray shutting down
            }
            let reply = rrx
                .recv_timeout(REPLY_TIMEOUT)
                .unwrap_or_else(|_| encode_err("tray did not answer in time"));
            let _ = stream.write_all(reply.as_bytes());
        }
    });
}

/// Run one CLI command against the socket at `path`. Returns
/// `(exit code, output)`; the caller prints the output (stdout on 0,
/// stderr otherwise) and exits with the code. Usage errors exit 1 without
/// touching the socket; connect failure is exit 2 ("tray not running").
pub fn client_run_at(path: &Path, words: &[String]) -> (i32, String) {
    let refs: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
    if let Err(usage) = parse_request(&refs) {
        return (1, usage);
    }
    let mut stream = match UnixStream::connect(path) {
        Ok(s) => s,
        Err(_) => {
            return (
                2,
                "hushmic is not running (start the tray first)".to_string(),
            )
        }
    };
    let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let line = format!("{}\n", refs.join(" "));
    if stream.write_all(line.as_bytes()).is_err() {
        return (
            2,
            "hushmic is not running (start the tray first)".to_string(),
        );
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut raw = String::new();
    if stream.read_to_string(&mut raw).is_err() || raw.is_empty() {
        return (1, "no reply from the tray".to_string());
    }
    let (ok, payload) = decode_response(&raw);
    (if ok { 0 } else { 1 }, payload)
}

/// `client_run_at` on the default socket path, with the same
/// ownership check the show socket does before connecting (in the /tmp
/// fallback the predictable path could be squatted by another local user).
pub fn client_run(words: &[String]) -> (i32, String) {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let path = crate::lock::default_control_socket_path();
    match std::fs::symlink_metadata(&path) {
        Ok(md) if md.uid() == unsafe { libc::getuid() } && md.file_type().is_socket() => {}
        _ => {
            // still validate usage locally so `hushmic frobnicate` is exit 1
            let refs: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
            if let Err(usage) = parse_request(&refs) {
                return (1, usage);
            }
            return (
                2,
                "hushmic is not running (start the tray first)".to_string(),
            );
        }
    }
    client_run_at(&path, words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request_accepts_the_documented_surface() {
        assert_eq!(
            parse_request(&["status"]),
            Ok(Request::Status { json: false })
        );
        assert_eq!(
            parse_request(&["status", "--json"]),
            Ok(Request::Status { json: true })
        );
        assert_eq!(parse_request(&["mode"]), Ok(Request::GetMode));
        assert_eq!(
            parse_request(&["mode", "suppress"]),
            Ok(Request::SetMode(Some(RunMode::Suppress)))
        );
        assert_eq!(
            parse_request(&["mode", "bypass"]),
            Ok(Request::SetMode(Some(RunMode::Bypass)))
        );
        assert_eq!(
            parse_request(&["mode", "mute"]),
            Ok(Request::SetMode(Some(RunMode::Mute)))
        );
        assert_eq!(parse_request(&["mode", "off"]), Ok(Request::SetMode(None)));
        assert_eq!(
            parse_request(&["toggle", "mute"]),
            Ok(Request::Toggle(RunMode::Mute))
        );
        assert_eq!(
            parse_request(&["toggle", "bypass"]),
            Ok(Request::Toggle(RunMode::Bypass))
        );
    }

    #[test]
    fn parse_request_rejects_junk_with_usage_messages() {
        for bad in [
            &["mode", "loud"][..],
            &["toggle", "suppress"][..], // only mute|bypass toggle
            &["toggle", "off"][..],
            &["toggle"][..],
            &["frobnicate"][..],
            &[][..],
            &["status", "--yaml"][..],
            &["mode", "mute", "extra"][..],
        ] {
            let e = parse_request(bad).unwrap_err();
            assert!(!e.is_empty(), "{bad:?} must carry a usage message");
        }
    }

    #[test]
    fn mode_words_round_trip_with_the_parser() {
        for sel in [
            Some(RunMode::Suppress),
            Some(RunMode::Bypass),
            Some(RunMode::Mute),
            None,
        ] {
            let w = mode_word(sel);
            assert_eq!(parse_request(&["mode", w]), Ok(Request::SetMode(sel)));
        }
    }

    #[test]
    fn toggle_enters_and_leaves_as_an_overlay() {
        use RunMode::*;
        // enter mute from suppress
        assert_eq!(toggle_next(Some(Suppress), Suppress, Mute), Some(Mute));
        // leave mute back to where we came from — INCLUDING bypass
        assert_eq!(toggle_next(Some(Mute), Bypass, Mute), Some(Bypass));
        assert_eq!(toggle_next(Some(Mute), Suppress, Mute), Some(Suppress));
        // from Off a toggle ENABLES into the target (born-muted spawn)
        assert_eq!(toggle_next(None, Suppress, Mute), Some(Mute));
        assert_eq!(toggle_next(None, Suppress, Bypass), Some(Bypass));
        // toggling bypass while muted switches to bypass (not a no-op)
        assert_eq!(toggle_next(Some(Mute), Suppress, Bypass), Some(Bypass));
    }

    #[test]
    fn prev_alive_tracks_transitions_and_never_remembers_off() {
        use RunMode::*;
        // ordinary chain-alive change remembers the old state
        assert_eq!(
            update_prev_alive(Suppress, Some(Suppress), Some(Mute)),
            Suppress
        );
        assert_eq!(
            update_prev_alive(Suppress, Some(Bypass), Some(Mute)),
            Bypass
        );
        // no-op transition keeps the memory
        assert_eq!(update_prev_alive(Bypass, Some(Mute), Some(Mute)), Bypass);
        // coming out of Off resets to suppress (never resurrect Off)
        assert_eq!(update_prev_alive(Bypass, None, Some(Mute)), Suppress);
        // going Off keeps the last live state (harmless, next enable resets)
        assert_eq!(update_prev_alive(Suppress, Some(Bypass), None), Bypass);
    }

    #[test]
    fn full_overlay_story_bypass_mute_bypass() {
        use RunMode::*;
        // user in bypass; toggle mute; toggle mute again -> back in bypass
        let (mut sel, mut prev) = (Some(Bypass), Suppress);
        let next = toggle_next(sel, prev, Mute);
        prev = update_prev_alive(prev, sel, next);
        sel = next;
        assert_eq!(sel, Some(Mute));
        let next = toggle_next(sel, prev, Mute);
        prev = update_prev_alive(prev, sel, next);
        sel = next;
        assert_eq!(sel, Some(Bypass));
        let _ = prev;
    }

    fn demo_status() -> Status {
        Status {
            version: "0.5.0".into(),
            mode: Some(RunMode::Mute),
            mic_configured: Some("alsa_input.rode".into()),
            mic_active: Some("alsa_input.rode".into()),
            fallback_active: false,
            model: "dpdfnet8_48khz_hr".into(),
            attn_limit: 100.0,
            chain_running: true,
            node_present: Some(true),
        }
    }

    #[test]
    fn status_json_is_the_documented_stable_object() {
        let v: serde_json::Value =
            serde_json::from_str(&render_status_json(&demo_status())).expect("valid json");
        assert_eq!(v["version"], "0.5.0");
        assert_eq!(v["mode"], "mute");
        assert_eq!(v["enabled"], true);
        assert_eq!(v["mic"]["configured"], "alsa_input.rode");
        assert_eq!(v["mic"]["active"], "alsa_input.rode");
        assert_eq!(v["mic"]["fallback_active"], false);
        assert_eq!(v["model"], "dpdfnet8_48khz_hr");
        assert_eq!(v["attn_limit"], 100.0);
        assert_eq!(v["latency_samples"], 2880);
        assert_eq!(v["chain"]["running"], true);
        assert_eq!(v["chain"]["node_present"], true);
    }

    #[test]
    fn status_json_null_fields_where_unknown() {
        let mut s = demo_status();
        s.mode = None;
        s.mic_configured = None;
        s.mic_active = None;
        s.node_present = None;
        s.chain_running = false;
        let v: serde_json::Value =
            serde_json::from_str(&render_status_json(&s)).expect("valid json");
        assert_eq!(v["mode"], "off");
        assert_eq!(v["enabled"], false);
        assert!(v["mic"]["configured"].is_null());
        assert!(v["chain"]["node_present"].is_null());
    }

    #[test]
    fn status_human_reads_like_a_report_not_a_dump() {
        let s = demo_status();
        let h = render_status_human(&s);
        assert!(h.contains("mute"), "{h}");
        assert!(h.contains("alsa_input.rode"), "{h}");
        assert!(h.contains("60 ms"), "latency stated: {h}");
        assert!(!h.contains('{'), "no JSON braces in human output: {h}");
        // fallback state is spelled out when engaged
        let mut f = demo_status();
        f.fallback_active = true;
        f.mic_active = None;
        let h = render_status_human(&f);
        assert!(h.contains("system default"), "{h}");
    }

    #[test]
    fn responses_encode_and_decode() {
        assert_eq!(decode_response(&encode_ok("mute")), (true, "mute".into()));
        assert_eq!(
            decode_response(&encode_ok("line1\nline2")),
            (true, "line1\nline2".into())
        );
        assert_eq!(decode_response(&encode_ok("")), (true, String::new()));
        assert_eq!(
            decode_response(&encode_err("no such state")),
            (false, "no such state".into())
        );
        // garbage from a non-hushmic peer decodes as an error, not a panic
        let (ok, _) = decode_response("HTTP/1.1 400 Bad Request");
        assert!(!ok);
    }
}
