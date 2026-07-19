use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub name: String,
    pub description: String,
}

/// Parse `pw-dump` JSON into the audio capture sources PipeWire exposes
/// (`media.class == "Audio/Source"` nodes).
///
/// hushmic is PipeWire-native: source enumeration and the watchdog liveness
/// check use `pw-dump` (and defaults use `pw-metadata`) — both ship in the
/// `pipewire`/`pipewire-bin` package set the project already depends on. They do
/// NOT use PulseAudio's `pactl`, which lives in the separate `pulseaudio-utils`
/// package that is absent on minimal installs and the Ubuntu live image; relying
/// on it would make `hushmic_source_present()` silently return false there, and
/// the watchdog would re-instantiate a perfectly healthy node forever.
///
/// Returns EVERY Audio/Source node (including monitors and our own
/// `hushmic_source`); callers filter as needed. Pure function — no I/O.
pub fn parse_pwdump_nodes(stdout: &str) -> Vec<Source> {
    let v: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for o in arr {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(props) = o.get("info").and_then(|i| i.get("props")) else {
            continue;
        };
        if props.get("media.class").and_then(|c| c.as_str()) != Some("Audio/Source") {
            continue;
        }
        let name = match props.get("node.name").and_then(|n| n.as_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        // Friendly label for the tray: node.description, else node.nick, else the
        // node name (mirrors the human-readable name pactl's Description gave).
        let description = props
            .get("node.description")
            .and_then(|d| d.as_str())
            .or_else(|| props.get("node.nick").and_then(|d| d.as_str()))
            .map(|s| s.to_string())
            .unwrap_or_else(|| name.clone());
        out.push(Source { name, description });
    }
    out
}

/// Resolve a node NAME to its numeric PipeWire global id from a `pw-dump`
/// snapshot. Pure — no I/O. `None` if the JSON is unparseable or no
/// `Audio` node carries that `node.name`.
///
/// Older pw-cat (< 0.3.64, e.g. Ubuntu 22.04's 0.3.48) rejects a node NAME as
/// `--target` — it prints `bad target option "<name>"` and exits before
/// emitting a byte, which strands the A/B window at −∞ with a "failed to fill
/// whole buffer" parse error. The numeric id is pw-cat's original,
/// always-accepted target form (what `--list-targets` prints), so resolving
/// name→id makes the recorder target the chosen node on every PipeWire version.
pub fn parse_node_id(stdout: &str, name: &str) -> Option<u32> {
    let v: serde_json::Value = serde_json::from_str(stdout).ok()?;
    for o in v.as_array()? {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let node_name = o
            .get("info")
            .and_then(|i| i.get("props"))
            .and_then(|p| p.get("node.name"))
            .and_then(|n| n.as_str());
        if node_name == Some(name) {
            return o.get("id").and_then(|i| i.as_u64()).map(|n| n as u32);
        }
    }
    None
}

/// Resolve a node name to its numeric PipeWire id via `pw-dump`, for
/// `pw-record --target`. `None` if the probe fails or the node is absent — the
/// caller then falls back to passing the name (accepted by modern pw-cat).
pub fn node_id(name: &str) -> Option<u32> {
    parse_node_id(&pw_dump()?, name)
}

/// The string to pass to `pw-record --target` for a node NAME.
///
/// Modern pw-cat (>= 0.3.64) accepts the NAME and RE-RESOLVES it at connect
/// time, so it is immune to global-id churn: on newer PipeWire an idle source
/// (a USB mic) is suspended and RECREATED with a new id when a capture wakes
/// it, and a numeric id captured a few ms earlier by `pw-dump` is then stale —
/// pw-cat treats `--target` as a hint, doesn't error, and the session manager
/// silently reroutes the stream to the SYSTEM DEFAULT source (which is how the
/// A/B window's raw leg ended up capturing hushmic_source). So prefer the name.
///
/// Only legacy pw-cat (< 0.3.64) needs the numeric id — it rejects a name
/// outright ("bad target option") — and there the id does not churn. Does I/O
/// (`pipewire --version`, and `pw-dump` on the legacy branch).
pub fn record_target(name: &str) -> String {
    if supports_target_object() {
        name.to_string()
    } else {
        node_id(name)
            .map(|id| id.to_string())
            .unwrap_or_else(|| name.to_string())
    }
}

/// Minimum bytes proving pw-cat actually wrote a capture stream to the pipe.
/// A container header alone is > 40 bytes; 512 is comfortably past any header
/// yet trivially reached in a few ms of real 48 kHz f32 audio.
const PIPE_PROBE_BYTES: usize = 512;

/// Whether pw-cat can stream a capture to a PIPE on this system.
///
/// pw-cat before the mid-2022 rework (Ubuntu 22.04 ships 0.3.48) can only
/// write a *seekable container file* and rejects `-`/pipes outright with
/// `failed to open audio file "-": this file format does not support pipe
/// write` — but a pipe is exactly how the live A/B window reads audio, so the
/// window is unusable there (both capture legs die at the first sample write
/// and the meters sit at −∞ with "failed to fill whole buffer"). When this is
/// false, callers fall back to the file-based recording test, which pw-cat
/// writes to a real file happily on every version.
///
/// Probes empirically rather than by version number: briefly pipe-capture
/// `hushmic_source` (present whenever the A/B window is meaningful) and see if
/// any stream bytes arrive. Returns as soon as the verdict is known — modern
/// pw-cat emits the header+samples in a few ms; old pw-cat's recorder exits
/// without writing a byte.
///
/// Optimistic on an inconclusive probe (source absent, pw-record missing,
/// stall): returns true so a system we could not measure is never degraded out
/// of the live window it might well support.
pub fn supports_pipe_capture() -> bool {
    let Some(id) = node_id("hushmic_source") else {
        return true; // nothing to probe against — don't degrade blindly
    };
    let mut child = match Command::new("pw-record")
        .args([
            "--target",
            &id.to_string(),
            "--rate",
            "48000",
            "--channels",
            "1",
            "--format",
            "f32",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return true, // pw-record unavailable is not "old pw-cat"
    };
    let mut stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    // Reader on its own thread: read(2) has no timeout, and old pw-cat exits
    // without ever writing (clean EOF), so we cannot block the caller on it.
    std::thread::spawn(move || {
        let mut buf = [0u8; PIPE_PROBE_BYTES];
        let mut got = 0usize;
        while got < PIPE_PROBE_BYTES {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break, // EOF (old pw-cat) or error
                Ok(n) => got += n,
            }
        }
        let _ = tx.send(got);
    });
    // Distinguish a finished read from a stall. The reader sends a count only
    // once it hits the threshold or EOF: old pw-cat's clean 0-byte EOF → false;
    // a real stream → true. A recv timeout means neither happened in the budget
    // (a cold-resuming USB/Bluetooth-HFP source on modern pw-cat can be that
    // slow) — that is INCONCLUSIVE, so stay optimistic and don't degrade a
    // system that likely does support pipes.
    let supported = match rx.recv_timeout(Duration::from_millis(800)) {
        Ok(got) => got >= PIPE_PROBE_BYTES,
        Err(_) => true,
    };
    let _ = child.kill();
    let _ = child.wait();
    supported
}

/// Extract the node name from a pw-metadata value line: value:'{"name":"X"}'.
pub fn parse_metadata_value(stdout: &str) -> Option<String> {
    // pw-metadata prints: update: id:0 key:'…' value:'{"name":"X"}' type:'…'.
    // The value is single-quoted with no escaping, so slice from after
    // "value:'" to the LAST "' type:" and hand the payload to a real JSON
    // parser — node names containing double quotes/backslashes then round-trip
    // instead of being truncated by naive quote-splitting.
    let after = stdout.split("value:'").nth(1)?;
    let json = match after.rfind("' type:") {
        Some(i) => &after[..i],
        None => after.split('\'').next()?,
    };
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    Some(v.get("name")?.as_str()?.to_string())
}

/// Run `pw-dump`. `None` means the PROBE failed (binary missing, daemon
/// unreachable, non-zero exit, or output that isn't JSON even after the
/// old-pw-dump repair below) — callers must treat that as "unknown", never
/// as "no nodes": tearing down a healthy child on a failed probe is exactly
/// the watchdog misfire this distinction prevents.
///
/// Pub (raw JSON) because the mic test also traces the *link graph*, which
/// `parse_pwdump_nodes` drops.
pub fn pw_dump() -> Option<String> {
    let o = Command::new("pw-dump").output().ok()?;
    if !o.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&o.stdout).into_owned();
    // Fast path: valid JSON passes through untouched (IgnoredAny validates
    // without building a tree).
    if serde_json::from_str::<serde::de::IgnoredAny>(&raw).is_ok() {
        return Some(raw);
    }
    let repaired = repair_keyless_members(&raw);
    if serde_json::from_str::<serde::de::IgnoredAny>(&repaired).is_ok() {
        return Some(repaired);
    }
    None
}

/// Salvage the invalid JSON old pw-dump emits when the graph contains a node
/// param type it has no name for: `pw-dump` before ~0.3.80 prints such a
/// param as a KEYLESS `[ ]` object member — `"ProcessLatency": [ ], [ ] }` —
/// which no JSON parser accepts. Any client linking a modern libpipewire
/// triggers it (every Flatpak app with native PipeWire nodes attaches the
/// 0.3.79+ `Tag` param; observed live on Debian 12's pw-dump 0.3.65 while
/// the sandboxed HushMic filter-chain ran). Without the repair every
/// consumer's parse fails and the dump reads as an EMPTY graph — see
/// `pw_dump` for why that must instead surface as a failed probe.
///
/// Only called on input that already failed to parse, and only removes empty
/// `[ ]` members whose enclosing container is an OBJECT (keyless members are
/// impossible in valid JSON); empty arrays nested in arrays or sitting as a
/// key's value are legitimate and preserved. String-aware so brackets inside
/// node names can't derail the scan. Pub for testability.
pub fn repair_keyless_members(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    // Container stack: true = object, false = array.
    let mut stack: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'"' => {
                // Copy the whole string literal, honoring escapes.
                let start = i;
                i += 1;
                while i < b.len() {
                    match b[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                let end = i.min(b.len());
                match s.get(start..end) {
                    Some(chunk) => out.push_str(chunk),
                    // An escape overshooting a non-ASCII boundary: not
                    // structurally salvageable — hand back the original so
                    // the caller's re-parse fails and the probe reads as
                    // failed. Never panic under the tray.
                    None => return s.to_string(),
                }
                continue;
            }
            b'{' => stack.push(true),
            b'}' | b']' => {
                stack.pop();
            }
            b'[' => {
                // An empty `[ <ws> ]` directly inside an OBJECT that does
                // NOT follow a `:` is the keyless-member emission (a key's
                // empty-array VALUE follows its colon; a keyless member can
                // only follow `,` or the opening `{`). Drop it together
                // with ONE adjacent comma (the preceding one when present,
                // else the following one for a leading member).
                if stack.last() == Some(&true) {
                    let mut j = i + 1;
                    while j < b.len() && (b[j] as char).is_whitespace() {
                        j += 1;
                    }
                    let prev = out.trim_end().chars().last();
                    if j < b.len() && b[j] == b']' && matches!(prev, Some(',') | Some('{')) {
                        if prev == Some(',') {
                            // Trim back through whitespace and the comma.
                            let keep = out.trim_end().len() - 1;
                            out.truncate(keep);
                        } else {
                            // Leading member: consume a following comma.
                            let mut k = j + 1;
                            while k < b.len() && (b[k] as char).is_whitespace() {
                                k += 1;
                            }
                            if k < b.len() && b[k] == b',' {
                                i = k + 1;
                                continue;
                            }
                        }
                        i = j + 1;
                        continue;
                    }
                }
                stack.push(false);
            }
            _ => {}
        }
        out.push(c as char);
        i += 1;
    }
    out
}

/// One consistent snapshot of every Audio/Source node; `None` = probe failed.
pub fn sources_snapshot() -> Option<Vec<Source>> {
    pw_dump().map(|s| parse_pwdump_nodes(&s))
}

/// The predicate `list_real_sources` applies: everything except our own
/// virtual node and `.monitor` loopbacks.
pub fn filter_real(sources: &[Source]) -> Vec<Source> {
    sources
        .iter()
        .filter(|s| s.name != "hushmic_source" && !s.name.ends_with(".monitor"))
        .cloned()
        .collect()
}

/// List real capture sources, excluding our own `hushmic_source` and any
/// `.monitor` monitor sources. Empty on probe failure.
pub fn list_real_sources() -> Vec<Source> {
    sources_snapshot()
        .map(|v| filter_real(&v))
        .unwrap_or_default()
}

/// The mic to actually pin in the filter-chain, given the saved choice and a
/// source snapshot. A saved mic that no longer matches any live source is
/// dropped (`None` → follow the system default) so a stale or re-enumerated
/// device can't pin a dead `target.object`, which links to nothing and leaves
/// the chain silent. A failed probe (`None` snapshot) keeps the saved mic —
/// unknown is not the same as gone. A mic still re-enumerating (autostart,
/// resume-from-suspend) can be dropped on that single snapshot, and the chain
/// then follows the default until it is re-selected — an accepted tradeoff for
/// auto-healing a permanently-stale saved device. Pure — no I/O.
pub fn resolve_effective_mic(cfg_mic: Option<&str>, snapshot: Option<&[Source]>) -> Option<String> {
    match (cfg_mic, snapshot) {
        (None, _) => None,
        (Some(m), None) => Some(m.to_string()),
        (Some(m), Some(srcs)) => srcs.iter().any(|s| s.name == m).then(|| m.to_string()),
    }
}

/// Whether the running PipeWire honors `target.object` in a filter-chain
/// capture. The key was added in PipeWire 0.3.64; older releases silently
/// ignore it (the capture then follows the system default), so they need the
/// legacy `node.target` to pin a specific mic. A failed probe is treated as
/// modern — old PipeWire is rare and `target.object`-only is the safe default.
///
/// What must be versioned here is the HOST side (its session manager
/// interprets the key). Outside a sandbox `pipewire --version` is the host;
/// inside a Flatpak that binary is the BUNDLED daemon — always modern, even
/// against an old host session — so there the version comes from the daemon's
/// own Core object in a `pw-dump` snapshot instead.
pub fn supports_target_object() -> bool {
    if crate::sandbox::is_flatpak() {
        return match pw_dump().as_deref().and_then(parse_core_version) {
            Some(v) => pw_version_at_least(&v, (0, 3, 64)),
            None => true, // probe failed -> same optimistic default as below
        };
    }
    match Command::new("pipewire").arg("--version").output() {
        Ok(o) => pw_version_at_least(&String::from_utf8_lossy(&o.stdout), (0, 3, 64)),
        Err(_) => true,
    }
}

/// The HOST daemon's version from a `pw-dump` snapshot: the
/// `PipeWire:Interface:Core` object's `info.version`. This is the daemon
/// being talked to, regardless of which pw-* binaries do the talking. Pure.
pub fn parse_core_version(stdout: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stdout).ok()?;
    for o in v.as_array()? {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Core") {
            continue;
        }
        if let Some(ver) = o
            .get("info")
            .and_then(|i| i.get("version"))
            .and_then(|s| s.as_str())
        {
            return Some(ver.to_string());
        }
    }
    None
}

/// Parse the first `X.Y.Z` version triple out of `pipewire --version` output
/// ("Compiled with libpipewire 0.3.48") and compare it to `min`. No triple
/// found → true (assume modern). Pure.
pub fn pw_version_at_least(text: &str, min: (u32, u32, u32)) -> bool {
    for tok in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let parts: Vec<&str> = tok.split('.').collect();
        if parts.len() >= 3 {
            if let (Ok(a), Ok(b), Ok(c)) = (
                parts[0].parse::<u32>(),
                parts[1].parse::<u32>(),
                parts[2].parse::<u32>(),
            ) {
                return (a, b, c) >= min;
            }
        }
    }
    true
}

/// Whether our virtual mic node `hushmic_source` is currently a live PipeWire
/// source. Used by the watchdog: the host child can linger after a daemon
/// restart while the node itself is gone, so node presence (not child PID) is
/// the real liveness signal. `None` = the probe itself failed (unknown).
/// Retry a definitive-answer probe across transient failures. `pw-dump`
/// exits non-zero (or emits unparseable JSON) under graph churn — several
/// concurrent clients plus node registration at once, which is exactly
/// when the A/B window gets (re)opened — and one transient probe failure
/// must not decline a user-visible action (observed live: tag pipeline
/// 3158's declined window reopen). First `Some` wins; `delay` between
/// attempts; all-`None` after `attempts` stays `None` (unknown is still
/// not a verdict). Pure over the injected probe — unit-tested.
pub fn retry_probe<F: FnMut() -> Option<bool>>(
    mut probe: F,
    attempts: u32,
    delay: std::time::Duration,
) -> Option<bool> {
    for i in 0..attempts {
        if let Some(v) = probe() {
            return Some(v);
        }
        if i + 1 < attempts {
            std::thread::sleep(delay);
        }
    }
    None
}

pub fn hushmic_source_present() -> Option<bool> {
    sources_snapshot().map(|v| v.iter().any(|s| s.name == "hushmic_source"))
}

/// Poll until `hushmic_source` is definitively present, up to `timeout`.
/// Returns false on timeout or persistent probe failure.
pub fn wait_for_hushmic_source(timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if hushmic_source_present() == Some(true) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Get the currently configured default source node name.
pub fn get_default_source() -> Option<String> {
    let out = Command::new("pw-metadata")
        .args(["-n", "default", "0", "default.configured.audio.source"])
        .output()
        .ok()?;
    parse_metadata_value(&String::from_utf8_lossy(&out.stdout))
}

/// The flatpak-manifest-installed drop-in that gives the bundled pw-* tools
/// `media.category = Manager` context properties. WirePlumber grants plain
/// flatpak-flagged clients `rx` only and silently discards their metadata
/// writes (`pw-metadata` exits 0, the key never changes — verified on
/// PipeWire 1.6 / WirePlumber 0.5.14); Manager-flagged clients get full
/// `rwxm` (the sanctioned mechanism EasyEffects and pavucontrol use, also
/// verified live). The path must stay in lockstep with
/// packaging/flatpak/io.github.fovty.HushMic.yml.
const FLATPAK_MANAGER_DROPIN: &str = "/app/share/pipewire/client.conf.d/50-hushmic-manager.conf";

/// Whether this process is able to change the system default source.
/// Native installs always can. Inside a Flatpak it works exactly when the
/// Manager drop-in ships next to the bundled tools — a rebuild that strips
/// it would otherwise leave every takeover to fail invisibly, so the whole
/// take-the-default feature (menu entry included) is gated on the actual
/// capability rather than attempted blindly.
pub fn can_set_default() -> bool {
    !crate::sandbox::is_flatpak() || std::path::Path::new(FLATPAK_MANAGER_DROPIN).exists()
}

/// Set the default source node name via pw-metadata.
pub fn set_default_source(node_name: &str) -> std::io::Result<()> {
    // serde_json handles escaping; a node name containing `"` or `\` must not
    // produce malformed metadata JSON (a silently failed restore on disable).
    let val = serde_json::json!({ "name": node_name }).to_string();
    let st = Command::new("pw-metadata")
        .args([
            "-n",
            "default",
            "0",
            "default.configured.audio.source",
            &val,
            "Spa:String:JSON",
        ])
        .status()?;
    if !st.success() {
        return Err(std::io::Error::other("pw-metadata set failed"));
    }
    // Exit status alone is not proof: the daemon acks metadata writes it has
    // no intention of applying (a permission-restricted client's set is
    // dropped server-side with pw-metadata still exiting 0). Success is the
    // key actually holding the value — callers persist prior-default state
    // on this result, so a false Ok would strand the user's real device.
    // One retried read guards the OTHER false verdict: a transiently failed
    // read-back must not report an applied write as dropped.
    for attempt in 0..2 {
        if get_default_source().as_deref() == Some(node_name) {
            return Ok(());
        }
        if attempt == 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Err(std::io::Error::other(
        "pw-metadata set was not applied (insufficient permissions on the metadata object?)",
    ))
}

/// Delete the `default.configured.audio.source` metadata key entirely.
///
/// Used on teardown when this run set the default but there was no prior
/// configured default to restore: leaving the key pointed at the now-dead
/// `hushmic_source` would strand the system default on a vanished node, so we
/// delete the key instead, returning PipeWire to "no configured default".
pub fn clear_default_source() -> std::io::Result<()> {
    let st = Command::new("pw-metadata")
        .args([
            "-n",
            "default",
            "-d",
            "0",
            "default.configured.audio.source",
        ])
        .status()?;
    if !st.success() {
        return Err(std::io::Error::other("pw-metadata delete failed"));
    }
    // Same silent-drop hazard as set_default_source, but the goal here is
    // narrower: the key must no longer strand the default on OUR dead node.
    // Absent is success; someone ELSE's value appearing concurrently (a
    // session manager restoring its persisted default the moment ours is
    // deleted) is success too — treating that as failure would loop the
    // teardown against an actor that keeps winning. Only the key still
    // reading `hushmic_source` (after one retried look) proves the delete
    // was dropped.
    for attempt in 0..2 {
        if get_default_source().as_deref() != Some("hushmic_source") {
            return Ok(());
        }
        if attempt == 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Err(std::io::Error::other(
        "pw-metadata delete was not applied (insufficient permissions on the metadata object?)",
    ))
}
