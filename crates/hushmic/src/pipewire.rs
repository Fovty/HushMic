use std::process::Command;

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
/// unreachable, non-zero exit) — callers must treat that as "unknown", never
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
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
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

/// Whether our virtual mic node `hushmic_source` is currently a live PipeWire
/// source. Used by the watchdog: the host child can linger after a daemon
/// restart while the node itself is gone, so node presence (not child PID) is
/// the real liveness signal. `None` = the probe itself failed (unknown).
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
    if st.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("pw-metadata set failed"))
    }
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
    if st.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("pw-metadata delete failed"))
    }
}
