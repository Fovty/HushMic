//! Shared diagnostics: `hushmic --doctor` and the About window's
//! "Copy diagnostics" button both render the same plain-text report.
//! Read-only probes, no audio, no network; everything in the report is
//! safe to paste into a public issue (device/model names only).
//!
//! Split: probe functions fill a [`Report`] (I/O), [`render`] turns it
//! into text + a problem count (pure — what the unit tests exercise).

/// One resolved asset path (plugin / model / ONNX Runtime).
pub struct AssetFact {
    pub what: &'static str,
    pub path: String,
    pub exists: bool,
}

/// Every fact the report prints. `Option` = the probe could not run
/// (rendered as `unavailable`, counted as a problem only where noted).
pub struct Report {
    pub version: String,
    pub install_type: String,
    /// None = pw-dump/daemon unreachable (problem).
    pub pw_version: Option<String>,
    pub pw_target_object: bool,
    pub config_path: String,
    pub enabled: bool,
    pub preferred_mic: Option<String>,
    pub active_mic: Option<String>,
    /// The model and strength in effect (the active profile's, see
    /// `Config::active_profile`) and the device whose saved profile that
    /// is, by display name; None = the defaults apply.
    pub model: String,
    pub attn_limit: f32,
    pub profile: Option<String>,
    pub set_default: bool,
    /// Saved per-mic profiles (config `mic_prefs` entries).
    pub mic_profiles: usize,
    /// None = probe failed; Some = real capture sources by description.
    pub sources: Option<Vec<String>>,
    pub default_source: Option<String>,
    /// None = probe failed. Some(false) is a problem only while
    /// `enabled` AND `instance_running` (otherwise a plain fact).
    pub hushmic_present: Option<bool>,
    pub can_set_default: bool,
    /// Which client the global shortcuts use in this session.
    pub shortcuts_backend: crate::shortcuts::Backend,
    pub instance_running: bool,
    pub assets: Vec<AssetFact>,
    /// (command, on PATH). Missing = problem.
    pub commands: Vec<(&'static str, bool)>,
    /// Tail of the filter-chain log; None = no log file yet.
    pub log_tail: Option<String>,
    /// The configured engine choice (config `inference`).
    pub inference_setting: String,
    /// What the last chain's plugin ran each model on, from the whole
    /// filter-chain log; None = no log file yet.
    pub inference: Option<Vec<Inference>>,
    /// The filter-chain binary can declare latency (PipeWire >= 1.6).
    pub latency_supported: bool,
    /// Live read-back of what the running chain reports; None = nothing
    /// reported (or no chain/probe). Only judged when a chain is up on a
    /// supporting host.
    pub latency_reported: Option<u32>,
    /// Node names feeding the chain's capture stream, from the live link
    /// graph. None = probe failed; empty = no links (yet). A running chain
    /// fed by anything OTHER than the effective target is the issue-#5
    /// signature (another tool re-routed the stream) — the one failure
    /// where every presence check stays green while the mic is silent.
    pub capture_feeders: Option<Vec<String>>,
    /// The persisted pre-takeover default: the feeder expectation while
    /// "Set as default microphone" makes our own node the default.
    pub prior_default: Option<String>,
    /// The `node.force-quantum` value on the running chain's source
    /// node (issue #10); 0 = node present but unpinned (pre-pin chain).
    /// None = chain not visible in the dump.
    pub chain_quantum_pin: Option<u32>,
    /// PipeWire's scheduling state for `hushmic_source` (`running`,
    /// `idle`, `suspended`, ...); None = no node or no dump. Idle is normal:
    /// with no active consumer PipeWire does not run the chain at all.
    pub chain_node_state: Option<String>,
    /// System-wide `clock.force-quantum` from the settings metadata (the
    /// manual override); None = not forced.
    pub forced_quantum: Option<u32>,
}

/// Gather every fact — the I/O half. Never panics: anything un-probeable
/// lands as `None`/`false` and renders as such. Works with no tray running
/// and with PipeWire down (both are then facts in the report).
pub fn collect() -> Report {
    let cfg = crate::config::Config::load();
    let paths = crate::controller::Paths::resolve();
    let dump = crate::pipewire::pw_dump();
    let snapshot = dump.as_deref().map(crate::pipewire::parse_pwdump_nodes);
    let prefix = std::env::current_exe()
        .ok()
        .and_then(|e| crate::controller::prefix_of(&e));
    let prior_default = crate::controller::persisted_prior_default();
    // With our own node as the default, a follow-default chain runs on the
    // pre-takeover device — its profile is the one in effect.
    let target =
        crate::pipewire::resolve_chain_target(cfg.mic.as_deref(), prior_default.as_deref());
    let profile = cfg.profile_for(target.profile_device.as_deref());
    let profile_name = profile
        .device
        .as_deref()
        .filter(|_| profile.saved)
        .map(|d| {
            target
                .sources
                .as_deref()
                .and_then(|v| crate::pipewire::description_of(v, d))
                .unwrap_or(d)
                .to_string()
        });
    let model_path = paths.model_dir.join(format!("{}.onnx", profile.model));
    // Momentarily acquiring the lock is harmless (LOCK_NB; drop releases):
    // acquired = nothing was holding it. Err (unreadable path, foreign
    // owner) reads as "not running" — conservative, since the node-absent
    // problem only fires while an instance IS running.
    let log = std::fs::read_to_string(log_path()).ok();
    let instance_running = matches!(
        crate::lock::try_lock(&crate::lock::default_lock_path()),
        Ok(None)
    );
    Report {
        version: env!("CARGO_PKG_VERSION").into(),
        install_type: install_type(
            std::env::var_os("APPIMAGE").is_some(),
            crate::sandbox::is_flatpak(),
            prefix.as_ref().and_then(|p| p.to_str()),
        ),
        pw_version: dump
            .as_deref()
            .and_then(crate::pipewire::parse_core_version),
        pw_target_object: crate::pipewire::supports_target_object(),
        config_path: crate::config::Config::path().display().to_string(),
        enabled: cfg.enabled,
        preferred_mic: cfg.mic.clone(),
        active_mic: crate::pipewire::resolve_effective_mic(cfg.mic.as_deref(), snapshot.as_deref()),
        model: profile.model.clone(),
        attn_limit: profile.attn_limit,
        profile: profile_name,
        set_default: cfg.set_default,
        mic_profiles: cfg.mic_prefs.len(),
        sources: snapshot.as_deref().map(|v| {
            crate::pipewire::filter_real(v)
                .into_iter()
                .map(|s| s.description)
                .collect()
        }),
        default_source: crate::pipewire::get_default_source(),
        hushmic_present: snapshot
            .as_deref()
            .map(|v| v.iter().any(|s| s.name == "hushmic_source")),
        can_set_default: crate::pipewire::can_set_default(),
        shortcuts_backend: crate::shortcuts::detect_backend(std::time::Duration::ZERO),
        instance_running,
        assets: vec![
            AssetFact {
                what: "LADSPA plugin",
                path: paths.plugin_so.display().to_string(),
                exists: paths.plugin_so.exists(),
            },
            AssetFact {
                what: "model file",
                path: model_path.display().to_string(),
                exists: model_path.exists(),
            },
            AssetFact {
                what: "ONNX Runtime",
                path: paths.dylib.display().to_string(),
                exists: paths.dylib.exists(),
            },
        ],
        commands: ["pw-dump", "pw-cli", "pw-metadata", "pw-record", "pw-play"]
            .into_iter()
            .map(|c| (c, on_path(c)))
            .collect(),
        log_tail: log.as_deref().map(|s| tail(s, 40)),
        inference_setting: cfg.inference.as_str().to_string(),
        inference: log.as_deref().map(inference_from_log),
        latency_supported: crate::pipewire::supports_latency_report(),
        latency_reported: crate::pipewire::chain_reported_latency(),
        capture_feeders: crate::pipewire::pw_dump()
            .map(|d| crate::pipewire::parse_feeders(&d, "hushmic_input")),
        prior_default,
        chain_quantum_pin: dump
            .as_deref()
            .and_then(crate::pipewire::chain_pins_quantum),
        chain_node_state: dump
            .as_deref()
            .and_then(|d| crate::pipewire::node_state(d, "hushmic_source")),
        forced_quantum: crate::pipewire::settings_force_quantum(),
    }
}

/// Whether `cmd` resolves to a file in any `$PATH` directory.
fn on_path(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

/// Render the report: plain `key: value` text, failing checks prefixed
/// `!!`, last line the problem summary. Returns (text, problem count);
/// `--doctor` exits 1 when the count is nonzero.
pub fn render(r: &Report) -> (String, usize) {
    let mut out = String::new();
    let mut problems = 0usize;
    // A line whose check failed: `!!`-prefixed and counted.
    let mut line = |out: &mut String, bad: bool, s: String| {
        if bad {
            problems += 1;
            out.push_str("!! ");
        }
        out.push_str(&s);
        out.push('\n');
    };

    out.push_str("hushmic diagnostics\n");
    line(&mut out, false, format!("version: {}", r.version));
    line(&mut out, false, format!("install: {}", r.install_type));
    match &r.pw_version {
        Some(v) => line(
            &mut out,
            false,
            format!(
                "pipewire: {v} (target.object: {})",
                if r.pw_target_object { "yes" } else { "no" }
            ),
        ),
        None => line(
            &mut out,
            true,
            "pipewire: unavailable (daemon unreachable or pw-dump failed)".into(),
        ),
    }
    line(&mut out, false, format!("config: {}", r.config_path));
    line(&mut out, false, format!("  enabled: {}", r.enabled));
    line(
        &mut out,
        false,
        format!(
            "  preferred mic: {}",
            r.preferred_mic.as_deref().unwrap_or("(system default)")
        ),
    );
    line(
        &mut out,
        false,
        format!(
            "  active mic: {}",
            r.active_mic.as_deref().unwrap_or("(system default)")
        ),
    );
    line(&mut out, false, format!("  model: {}", r.model));
    line(
        &mut out,
        false,
        format!("  attenuation limit: {} dB", r.attn_limit),
    );
    line(
        &mut out,
        false,
        match &r.profile {
            Some(p) => format!("  settings: {p} profile"),
            None => "  settings: defaults".to_string(),
        },
    );
    line(&mut out, false, format!("  set default: {}", r.set_default));
    line(
        &mut out,
        false,
        format!("  per-mic profiles: {}", r.mic_profiles),
    );
    line(
        &mut out,
        false,
        format!("  inference: {}", r.inference_setting),
    );
    line(
        &mut out,
        false,
        format!(
            "instance running: {}",
            if r.instance_running { "yes" } else { "no" }
        ),
    );
    {
        use crate::controller::LATENCY_SAMPLES;
        line(
            &mut out,
            false,
            format!(
                "chain latency: {} ms ({} samples @ 48 kHz)",
                LATENCY_SAMPLES * 1000 / 48_000,
                LATENCY_SAMPLES
            ),
        );
        // The read-back is a verdict only when there is a chain to ask on
        // a host that can carry the declaration; otherwise a plain fact.
        let chain_up = r.hushmic_present == Some(true);
        let (bad, s) = if !chain_up {
            (false, "  reported to PipeWire: (chain not running)".into())
        } else if !r.latency_supported {
            (
                false,
                "  reported to PipeWire: no (PipeWire 1.6+ required)".into(),
            )
        } else {
            match r.latency_reported {
                Some(n) if n == LATENCY_SAMPLES => {
                    (false, format!("  reported to PipeWire: yes ({n})"))
                }
                Some(n) => (
                    true,
                    format!("  reported to PipeWire: mismatch ({n}, declared {LATENCY_SAMPLES})"),
                ),
                None => (
                    true,
                    "  reported to PipeWire: missing (declaration not in effect)".into(),
                ),
            }
        };
        line(&mut out, bad, s);
    }
    // Issue #10: the chain must pin the graph quantum — the async DSP's
    // output margin and the declared latency are sized for exactly the
    // pinned value. Judged only for a visible chain.
    {
        use crate::controller::PINNED_QUANTUM;
        let (bad, s) = match (r.hushmic_present == Some(true), r.chain_quantum_pin) {
            (true, Some(q)) if q == PINNED_QUANTUM => {
                (false, format!("quantum pin: yes ({PINNED_QUANTUM})"))
            }
            (true, Some(0)) => (
                true,
                "quantum pin: missing (chain from an older hushmic — restart HushMic)".to_string(),
            ),
            // A live pin from a DIFFERENT hushmic version: the declared
            // latency assumes PINNED_QUANTUM, so the figure is wrong
            // until the chain restarts with the current pin.
            (true, Some(q)) => (
                true,
                format!("quantum pin: {q} (from a different hushmic version — restart HushMic)"),
            ),
            _ => (false, "quantum pin: (chain not running)".to_string()),
        };
        line(&mut out, bad, s);
        if let Some(q) = r.forced_quantum {
            // The manual system-wide force wins over the node pin. With
            // the async DSP, smaller values are harmless (the RT path is
            // a memcpy); a value ABOVE the pin outgrows the output
            // margin, so real latency exceeds the declared figure.
            let s = if q > PINNED_QUANTUM {
                format!(
                    "  clock.force-quantum: {q} (system-wide manual override; \
                     chain latency exceeds the declared value)"
                )
            } else {
                format!("  clock.force-quantum: {q} (system-wide manual override)")
            };
            line(&mut out, q > PINNED_QUANTUM, s);
        }
    }
    // The link-graph fact: who actually feeds the capture stream. Judged
    // only for a running chain with a known expectation — a mismatch there
    // means the mic the user picked is NOT what the chain hears.
    if let Some(feeders) = &r.capture_feeders {
        let chain_up = r.instance_running && r.hushmic_present == Some(true);
        // Same expectation the watchdog's healer uses: pinned mic, else the
        // default — except when the default is our own node ("Set as
        // default microphone" on), where the persisted pre-takeover default
        // is what the chain follows. No expectation = plain fact.
        let expected = crate::pipewire::repin_want(
            r.active_mic.as_deref(),
            r.default_source.clone(),
            r.prior_default.as_deref(),
        );
        let (bad, s) = if feeders.is_empty() {
            (false, "capture fed by: (no links)".to_string())
        } else if chain_up && feeders.iter().any(|f| f == "hushmic_source") {
            // Needs no expectation: the chain hearing its own output is a
            // silent loop under every configuration.
            (
                true,
                format!(
                    "capture fed by: {} (the chain's own output)",
                    feeders.join(", ")
                ),
            )
        } else if let (true, Some(want)) = (chain_up, expected.as_deref()) {
            if feeders.iter().any(|f| f == want) {
                (false, format!("capture fed by: {}", feeders.join(", ")))
            } else {
                (
                    true,
                    format!("capture fed by: {} (expected {want})", feeders.join(", ")),
                )
            }
        } else {
            (false, format!("capture fed by: {}", feeders.join(", ")))
        };
        line(&mut out, bad, s);
    }
    // Which engine the last chain ran each model on (issue #18). A fact,
    // never a problem: ONNX is a supported engine, and the reason says
    // what would change it.
    match &r.inference {
        Some(v) if v.is_empty() => line(
            &mut out,
            false,
            "inference: (not in the filter-chain log)".into(),
        ),
        Some(v) => {
            for i in v {
                line(&mut out, false, format!("inference: {i}"));
            }
        }
        None => {}
    }
    match &r.sources {
        Some(s) if s.is_empty() => line(&mut out, false, "sources: 0".into()),
        Some(s) => line(
            &mut out,
            false,
            format!("sources: {} ({})", s.len(), s.join(", ")),
        ),
        None => line(&mut out, false, "sources: unavailable".into()),
    }
    line(
        &mut out,
        false,
        format!(
            "default source: {}",
            r.default_source.as_deref().unwrap_or("(none set)")
        ),
    );
    // Some(false) is a problem only while an enabled instance is running:
    // that is precisely "the chain should be up but the node is gone".
    // Absent-with-nothing-running is a plain fact.
    let node_bad = r.hushmic_present == Some(false) && r.enabled && r.instance_running;
    match r.hushmic_present {
        Some(p) => line(
            &mut out,
            node_bad,
            format!("hushmic_source present: {}", if p { "yes" } else { "no" }),
        ),
        None => line(
            &mut out,
            false,
            "hushmic_source present: unavailable".into(),
        ),
    }
    // A fact, never a problem: idle is PipeWire not scheduling the chain,
    // normally because no stream reads from it (the capture side is passive).
    if let (Some(true), Some(st)) = (r.hushmic_present, r.chain_node_state.as_deref()) {
        let s = if crate::pipewire::state_is_idle(st) {
            format!("  state: {st} (PipeWire is not running the chain right now)")
        } else {
            format!("  state: {st}")
        };
        line(&mut out, false, s);
    }
    line(
        &mut out,
        false,
        format!(
            "can set default: {}",
            if r.can_set_default { "yes" } else { "no" }
        ),
    );
    line(
        &mut out,
        false,
        format!(
            "global shortcuts: {}",
            match r.shortcuts_backend {
                crate::shortcuts::Backend::Portal => "desktop portal",
                crate::shortcuts::Backend::KGlobalAccel => "KDE kglobalaccel (Plasma before 6.4)",
                crate::shortcuts::Backend::Unsupported => "unavailable (sandbox on Plasma 5)",
            }
        ),
    );
    out.push_str("assets:\n");
    for a in &r.assets {
        line(
            &mut out,
            !a.exists,
            format!(
                "  {}: {} ({})",
                a.what,
                a.path,
                if a.exists { "ok" } else { "MISSING" }
            ),
        );
    }
    for (cmd, found) in &r.commands {
        line(
            &mut out,
            !found,
            format!(
                "command {}: {}",
                cmd,
                if *found { "ok" } else { "NOT ON PATH" }
            ),
        );
    }
    match &r.log_tail {
        Some(t) => {
            out.push_str("filter-chain log (tail):\n");
            for l in t.lines() {
                out.push_str("  ");
                out.push_str(l);
                out.push('\n');
            }
        }
        None => out.push_str("filter-chain log: no log file yet\n"),
    }
    if problems == 0 {
        out.push_str("no problems found\n");
    } else {
        out.push_str(&format!("{problems} problem(s) found\n"));
    }
    (out, problems)
}

/// The last `n` lines of `text` (all of it when shorter).
pub fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].concat()
}

/// Hard cap on the filter-chain log: a pathologically chatty child must
/// not fill a disk. Reached → one `[log capped]` marker, appends stop
/// (stderr forwarding continues).
pub const LOG_CAP_BYTES: u64 = 256 * 1024;

/// Where the filter-chain child's stderr is persisted so `--doctor` and
/// the About window (separate processes — and post-mortem runs after a
/// crash) can include its tail.
pub fn log_path() -> std::path::PathBuf {
    let dirs = directories::ProjectDirs::from("io", "hushmic", "hushmic").expect("home");
    dirs.state_dir()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| dirs.data_local_dir().to_path_buf())
        .join("filter-chain.log")
}

/// Tee the filter-chain child's stderr: every line goes to our own stderr
/// unchanged (today's visible behavior) AND is appended to `log`, which is
/// created/truncated here — each chain spawn starts a fresh log. The
/// thread exits when the pipe closes.
pub fn spawn_stderr_tee(
    reader: impl std::io::Read + Send + 'static,
    log: std::path::PathBuf,
    generation: u64,
) -> std::thread::JoinHandle<()> {
    tee_with_cap(reader, log, LOG_CAP_BYTES, generation)
}

/// The engine tier the running chain reports (issue #14). The plugin writes
/// one contract line per transition to stderr (`[dpdfnet-ladspa] engine:
/// <word> ...`); the tee below is the only reader, and this atomic is the
/// only channel from a `module-filter-chain` plugin to the app.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EngineTier {
    Quality,
    Light,
    Passthrough,
}

impl EngineTier {
    pub fn word(self) -> &'static str {
        match self {
            EngineTier::Quality => "quality",
            EngineTier::Light => "light",
            EngineTier::Passthrough => "passthrough",
        }
    }
}

/// The reported tier and the chain generation that reported it in one
/// word: the low 8 bits hold the tier code (0 = nothing reported), the
/// rest the generation. Packing them means a tee left over from a killed
/// chain cannot land its last line on top of the fresh chain's tier: its
/// generation no longer matches and the store is dropped. Joining that tee
/// is best effort (`Controller::disable` waits a couple of seconds), so
/// this cannot rest on the join.
static ENGINE_TIER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn tier_code(t: EngineTier) -> u64 {
    match t {
        EngineTier::Quality => 1,
        EngineTier::Light => 2,
        EngineTier::Passthrough => 3,
    }
}

pub fn engine_tier() -> Option<EngineTier> {
    match ENGINE_TIER.load(std::sync::atomic::Ordering::Acquire) & 0xff {
        1 => Some(EngineTier::Quality),
        2 => Some(EngineTier::Light),
        3 => Some(EngineTier::Passthrough),
        _ => None,
    }
}

/// Retire the current chain's reporting: the tier is forgotten and every
/// tee started under an earlier generation is ignored from here on. Called
/// before a chain spawn (the returned token is what that chain's tee
/// reports under) and on disable (where the token is dropped). Only the
/// main thread calls this, in step with enable/disable.
pub fn new_engine_generation() -> u64 {
    let mut cur = ENGINE_TIER.load(std::sync::atomic::Ordering::Acquire);
    loop {
        let next = ((cur >> 8) + 1) << 8;
        match ENGINE_TIER.compare_exchange_weak(
            cur,
            next,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        ) {
            Ok(_) => return next >> 8,
            Err(c) => cur = c,
        }
    }
}

/// Record a tier on behalf of `generation`. A retired tee's line is
/// dropped: the value it carries describes a chain that is already gone.
fn store_engine_tier(generation: u64, t: EngineTier) {
    let want = (generation << 8) | tier_code(t);
    let mut cur = ENGINE_TIER.load(std::sync::atomic::Ordering::Acquire);
    while cur >> 8 == generation {
        match ENGINE_TIER.compare_exchange_weak(
            cur,
            want,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(c) => cur = c,
        }
    }
}

/// The contract head is `[dpdfnet-ladspa] engine: <word>` followed by a
/// space, a newline or the end of the line; anything else (a torn line, a
/// foreign message) is ignored.
///
/// The head is looked for anywhere in the line, not only at the start: the
/// chain host and the plugin share one stderr, so a line the tee reads can
/// begin with the tail of somebody else's write. The last occurrence wins,
/// because a line that carries two heads carries the older report first.
pub fn parse_engine_line(line: &[u8]) -> Option<EngineTier> {
    const HEAD: &[u8] = b"[dpdfnet-ladspa] engine: ";
    let head_at = (0..line.len().saturating_sub(HEAD.len()) + 1)
        .rev()
        .find(|&i| line[i..].starts_with(HEAD))?;
    let rest = &line[head_at + HEAD.len()..];
    let end = rest
        .iter()
        .position(|&b| b == b' ' || b == b'\n' || b == b'\r')
        .unwrap_or(rest.len());
    match &rest[..end] {
        b"quality" => Some(EngineTier::Quality),
        b"light" => Some(EngineTier::Light),
        b"passthrough" => Some(EngineTier::Passthrough),
        _ => None,
    }
}

/// What the plugin runs one model on (issue #18), from its contract line
/// `[dpdfnet-ladspa] inference: <engine>[ (<reason>)] for <model>`, e.g.
/// `native int8` or `onnx (native weights not installed (...))`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inference {
    pub model: String,
    pub engine: String,
    /// Why ONNX runs; None for a native engine.
    pub reason: Option<String>,
}

impl std::fmt::Display for Inference {
    /// The plugin's own wording, without the head.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reason {
            Some(r) => write!(f, "{} ({r}) for {}", self.engine, self.model),
            None => write!(f, "{} for {}", self.engine, self.model),
        }
    }
}

/// Parse an inference contract line. Like the tier line, the head is found
/// anywhere in the line (a torn line can start with someone else's
/// write) and the last occurrence wins.
pub fn parse_inference_line(line: &[u8]) -> Option<Inference> {
    const HEAD: &[u8] = b"[dpdfnet-ladspa] inference: ";
    let head_at = (0..line.len().saturating_sub(HEAD.len()) + 1)
        .rev()
        .find(|&i| line[i..].starts_with(HEAD))?;
    let rest = std::str::from_utf8(&line[head_at + HEAD.len()..]).ok()?;
    let rest = rest.trim_end_matches(['\n', '\r']);
    // `<engine> for <model>` or `<engine> (<reason>) for <model>`: the
    // engine has no parentheses and the reason's are balanced, so the
    // model is whatever follows the first " for " outside them (a model
    // id may contain spaces or the word "for"; a reason may say "for").
    let (engine, reason, model) = match rest.split_once(" (") {
        Some((e, r)) if !e.contains(" for ") => {
            let mut depth = 1usize;
            let close = r.char_indices().find_map(|(i, c)| {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                (depth == 0).then_some(i)
            })?;
            let model = r[close + 1..].strip_prefix(" for ")?;
            (e, Some(r[..close].to_string()), model)
        }
        _ => {
            let (e, m) = rest.split_once(" for ")?;
            (e, None, m)
        }
    };
    if model.trim().is_empty() {
        return None;
    }
    if engine.is_empty() {
        return None;
    }
    Some(Inference {
        model: model.to_string(),
        engine: engine.to_string(),
        reason,
    })
}

/// The running chain's inference lines, one per model, stamped with the
/// generation that reported them (the same generations as the tier: a
/// retired chain's entries are simply never read again).
static INFERENCE: std::sync::Mutex<(u64, Vec<Inference>)> = std::sync::Mutex::new((0, Vec::new()));

fn current_generation() -> u64 {
    ENGINE_TIER.load(std::sync::atomic::Ordering::Acquire) >> 8
}

fn store_inference(generation: u64, i: Inference) {
    let mut g = INFERENCE.lock().unwrap_or_else(|e| e.into_inner());
    if generation != current_generation() {
        return;
    }
    if g.0 != generation {
        *g = (generation, Vec::new());
    }
    g.1.retain(|old| old.model != i.model);
    g.1.push(i);
}

/// What the running chain reports running `model` on; None before the
/// plugin said so (or with a plugin that predates the line).
pub fn inference_for(model: &str) -> Option<Inference> {
    let g = INFERENCE.lock().unwrap_or_else(|e| e.into_inner());
    if g.0 != current_generation() {
        return None;
    }
    g.1.iter().find(|i| i.model == model).cloned()
}

/// Every model's last inference line in a log, in first-seen order.
pub fn inference_from_log(text: &str) -> Vec<Inference> {
    let mut out: Vec<Inference> = Vec::new();
    for line in text.lines() {
        if let Some(i) = parse_inference_line(line.as_bytes()) {
            match out.iter_mut().find(|o| o.model == i.model) {
                Some(o) => *o = i,
                None => out.push(i),
            }
        }
    }
    out
}

fn tee_with_cap(
    reader: impl std::io::Read + Send + 'static,
    log: std::path::PathBuf,
    cap: u64,
    generation: u64,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        // Best-effort: a failed log open must not lose the stderr
        // forwarding (and must never take the audio chain down).
        if let Some(d) = log.parent() {
            let _ = std::fs::create_dir_all(d);
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log)
            .ok();
        let mut written = 0u64;
        let mut capped = false;
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        // read_until, not lines(): the child's output is not guaranteed
        // UTF-8, and raw bytes must pass through unmangled.
        while {
            line.clear();
            matches!(reader.read_until(b'\n', &mut line), Ok(n) if n > 0)
        } {
            let _ = std::io::stderr().write_all(&line);
            if let Some(t) = parse_engine_line(&line) {
                store_engine_tier(generation, t);
            }
            if let Some(i) = parse_inference_line(&line) {
                store_inference(generation, i);
            }
            if let Some(f) = file.as_mut() {
                if written + line.len() as u64 <= cap {
                    if f.write_all(&line).is_ok() {
                        written += line.len() as u64;
                    }
                } else if !capped {
                    capped = true;
                    let _ = f.write_all(b"[log capped]\n");
                }
            }
        }
    })
}

/// Classify the install from environment facts. Pure; the caller feeds
/// `$APPIMAGE` presence, `/.flatpak-info` existence, and the install
/// prefix implied by the binary's location (`controller::prefix_of`).
pub fn install_type(appimage: bool, flatpak: bool, prefix: Option<&str>) -> String {
    if appimage {
        return "AppImage".into();
    }
    if flatpak {
        return "Flatpak".into();
    }
    match prefix {
        Some("/usr") => "system package (/usr)".into(),
        Some(p) => format!("prefix install ({p})"),
        None => "development build (no install prefix)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Report {
        Report {
            version: "0.3.0".into(),
            install_type: "prefix install (/usr/local)".into(),
            pw_version: Some("1.2.0".into()),
            pw_target_object: true,
            config_path: "/home/u/.config/hushmic/config.toml".into(),
            enabled: true,
            preferred_mic: None,
            active_mic: None,
            model: "dpdfnet8_48khz_hr".into(),
            attn_limit: 100.0,
            profile: Some("RODE NT-USB".into()),
            set_default: true,
            mic_profiles: 1,
            sources: Some(vec!["RODE NT-USB".into(), "Webcam C920".into()]),
            default_source: Some("hushmic_source".into()),
            hushmic_present: Some(true),
            can_set_default: true,
            shortcuts_backend: crate::shortcuts::Backend::Portal,
            instance_running: true,
            assets: vec![AssetFact {
                what: "LADSPA plugin",
                path: "/usr/lib/ladspa/libdpdfnet_ladspa.so".into(),
                exists: true,
            }],
            commands: vec![("pw-dump", true), ("pw-cli", true)],
            log_tail: Some("[hushmic] chain up\n".into()),
            latency_supported: true,
            latency_reported: Some(3840),
            capture_feeders: Some(vec!["alsa_input.usb-mic".into()]),
            prior_default: Some("alsa_input.usb-mic".into()),
            chain_quantum_pin: Some(crate::controller::PINNED_QUANTUM),
            chain_node_state: Some("running".into()),
            forced_quantum: None,
            inference_setting: "auto".into(),
            inference: Some(vec![]),
        }
    }

    #[test]
    fn inference_lines_parse_with_and_without_a_reason() {
        assert_eq!(
            parse_inference_line(
                b"[dpdfnet-ladspa] inference: native int8 for dpdfnet8_48khz_hr\n"
            ),
            Some(Inference {
                model: "dpdfnet8_48khz_hr".into(),
                engine: "native int8".into(),
                reason: None,
            })
        );
        let i = parse_inference_line(
            b"[pw] xrun[dpdfnet-ladspa] inference: onnx (native weights not installed \
              (/usr/share/hushmic/models/dpdfnet2_48khz_hr.weights.f32)) for dpdfnet2_48khz_hr\r\n",
        )
        .unwrap();
        assert_eq!(i.engine, "onnx");
        assert_eq!(i.model, "dpdfnet2_48khz_hr");
        assert_eq!(
            i.reason.as_deref(),
            Some("native weights not installed (/usr/share/hushmic/models/dpdfnet2_48khz_hr.weights.f32)")
        );
        // A reason may itself say "for".
        let i = parse_inference_line(
            b"[dpdfnet-ladspa] inference: onnx (CPU lacks AVX2/FMA for native int8) for m\n",
        )
        .unwrap();
        assert_eq!(
            i.reason.as_deref(),
            Some("CPU lacks AVX2/FMA for native int8")
        );
        assert_eq!(
            i.to_string(),
            "onnx (CPU lacks AVX2/FMA for native int8) for m"
        );
        // A model id with spaces, or with "for" in it.
        let i = parse_inference_line(
            b"[dpdfnet-ladspa] inference: onnx (no native build of this model file) for my model for calls\n",
        )
        .unwrap();
        assert_eq!(i.model, "my model for calls");
        assert_eq!(
            i.reason.as_deref(),
            Some("no native build of this model file")
        );
        let i = parse_inference_line(b"[dpdfnet-ladspa] inference: native int8 for a b\n").unwrap();
        assert_eq!(
            (i.engine.as_str(), i.model.as_str()),
            ("native int8", "a b")
        );
        for torn in [
            &b"[dpdfnet-ladspa] inference: native int8"[..],
            b"[dpdfnet-ladspa] inference: native int8 for \n",
            b"[dpdfnet-ladspa] inference: onnx (no close for m\n",
            b"[dpdfnet-ladspa] engine: quality (cost 0.42)\n",
            b"inference: native int8 for m\n",
        ] {
            assert_eq!(
                parse_inference_line(torn),
                None,
                "{}",
                String::from_utf8_lossy(torn)
            );
        }
        // The two contracts never read each other's lines.
        assert_eq!(
            parse_engine_line(b"[dpdfnet-ladspa] inference: native int8 for m\n"),
            None
        );
    }

    #[test]
    fn the_log_keeps_each_models_last_inference_line() {
        let log = "[dpdfnet-ladspa] inference: native int8 for q\n\
                   [dpdfnet-ladspa] inference: onnx (requested) for l\n\
                   [dpdfnet-ladspa] engine: quality (cost 0.30)\n\
                   [dpdfnet-ladspa] inference: onnx (requested) for q\n";
        let v = inference_from_log(log);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].model.as_str(), v[0].engine.as_str()), ("q", "onnx"));
        assert_eq!(v[1].model, "l");
        assert!(inference_from_log("[hushmic] chain up\n").is_empty());
    }

    #[test]
    fn doctor_reports_the_setting_and_each_models_engine_as_facts() {
        let mut r = healthy();
        r.inference = Some(vec![
            Inference {
                model: "dpdfnet8_48khz_hr".into(),
                engine: "native int8".into(),
                reason: None,
            },
            Inference {
                model: "dpdfnet2_48khz_hr".into(),
                engine: "onnx".into(),
                reason: Some("CPU lacks AVX2/FMA for native int8".into()),
            },
        ]);
        let (text, problems) = render(&r);
        assert_eq!(problems, 0, "{text}");
        assert!(text.contains("  inference: auto\n"), "{text}");
        assert!(
            text.contains("inference: native int8 for dpdfnet8_48khz_hr\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "inference: onnx (CPU lacks AVX2/FMA for native int8) for dpdfnet2_48khz_hr\n"
            ),
            "{text}"
        );
        r.inference = Some(vec![]);
        assert!(render(&r)
            .0
            .contains("inference: (not in the filter-chain log)"));
        r.inference = None;
        let (text, _) = render(&r);
        assert!(!text.contains("inference: ("), "{text}");
    }

    #[test]
    fn capture_feeder_matching_the_target_is_a_plain_fact() {
        // Follow-default mode: the expected feeder is the default source.
        let mut r = healthy();
        r.default_source = Some("alsa_input.usb-mic".into());
        r.capture_feeders = Some(vec!["alsa_input.usb-mic".into()]);
        let (text, problems) = render(&r);
        assert_eq!(problems, 0, "{text}");
        assert!(
            text.contains("capture fed by: alsa_input.usb-mic"),
            "{text}"
        );
    }

    #[test]
    fn an_idle_chain_is_a_plain_fact_worded_as_normal() {
        let mut r = healthy();
        let (text, problems) = render(&r);
        assert_eq!(problems, 0, "{text}");
        assert!(text.contains("  state: running\n"), "{text}");
        for st in ["idle", "suspended"] {
            r.chain_node_state = Some(st.into());
            let (text, problems) = render(&r);
            assert_eq!(problems, 0, "idle is never a problem: {text}");
            assert!(
                text.contains(&format!(
                    "  state: {st} (PipeWire is not running the chain right now)"
                )),
                "{text}"
            );
        }
        // No node, or no state: no state line.
        r.hushmic_present = Some(false);
        assert!(!render(&r).0.contains("  state: "));
        r.hushmic_present = Some(true);
        r.chain_node_state = None;
        assert!(!render(&r).0.contains("  state: "));
    }

    #[test]
    fn missing_quantum_pin_on_a_running_chain_is_a_problem() {
        // Issue #10: a pre-pin chain still running after an upgrade.
        let mut r = healthy();
        r.chain_quantum_pin = Some(0);
        let (text, problems) = render(&r);
        assert_eq!(problems, 1, "{text}");
        assert!(text.contains("quantum pin: missing"), "{text}");
    }

    #[test]
    fn stale_pin_value_on_a_running_chain_is_a_problem() {
        // v0.7.0's chain pinned 1024; after an upgrade the declared
        // latency assumes 480 — the doctor must say restart, not "yes".
        let mut r = healthy();
        r.chain_quantum_pin = Some(1024);
        let (text, problems) = render(&r);
        assert_eq!(problems, 1, "{text}");
        assert!(text.contains("quantum pin: 1024"), "{text}");
        assert!(text.contains("restart HushMic"), "{text}");
    }

    #[test]
    fn large_manual_force_quantum_is_a_problem_small_is_a_fact() {
        // Async DSP (issue #10): a small forced quantum only means more,
        // smaller memcpys — harmless. A value above the pin outgrows the
        // output margin: audio survives, latency exceeds the declared
        // figure, so the doctor must flag it.
        let mut r = healthy();
        r.forced_quantum = Some(2048);
        let (text, problems) = render(&r);
        assert_eq!(problems, 1, "{text}");
        assert!(text.contains("clock.force-quantum: 2048"), "{text}");
        assert!(text.contains("exceeds the declared value"), "{text}");

        let mut r = healthy();
        r.forced_quantum = Some(256);
        let (text, problems) = render(&r);
        assert_eq!(problems, 0, "{text}");
        assert!(text.contains("clock.force-quantum: 256"), "{text}");
    }

    #[test]
    fn capture_fed_by_a_foreign_node_is_a_problem() {
        // Issue #5's signature: chain healthy on paper, but another tool
        // (EasyEffects) re-routed the capture stream onto its own source.
        let mut r = healthy();
        r.active_mic = Some("alsa_input.usb-mic".into());
        r.capture_feeders = Some(vec!["easyeffects_source".into()]);
        let (text, problems) = render(&r);
        assert_eq!(problems, 1, "{text}");
        assert!(
            text.contains("capture fed by: easyeffects_source (expected alsa_input.usb-mic)"),
            "{text}"
        );
    }

    #[test]
    fn capture_feeder_facts_stay_quiet_when_unjudgeable() {
        // No links yet (snapshot mid-spawn), probe failure, or no chain:
        // plain facts, never a false alarm.
        let mut r = healthy();
        r.active_mic = Some("alsa_input.usb-mic".into());
        r.capture_feeders = Some(vec![]);
        let (text, problems) = render(&r);
        assert_eq!(problems, 0, "{text}");
        assert!(text.contains("capture fed by: (no links)"), "{text}");

        let mut r = healthy();
        r.capture_feeders = None;
        assert_eq!(render(&r).1, 0);

        let mut r = healthy();
        r.hushmic_present = Some(false);
        r.capture_feeders = Some(vec!["easyeffects_source".into()]);
        // instance not running / chain down: the feeder is stale info, and
        // the missing node is already the reported problem.
        let (_, problems) = render(&r);
        assert!(problems >= 1); // the node problem, not a feeder one
    }

    #[test]
    fn chain_fed_by_its_own_output_is_always_a_problem() {
        // No expectation needed: hushmic_source feeding hushmic_input is a
        // silent loop under every configuration.
        let mut r = healthy();
        r.active_mic = None;
        r.prior_default = None;
        r.capture_feeders = Some(vec!["hushmic_source".into()]);
        let (text, problems) = render(&r);
        assert_eq!(problems, 1, "{text}");
        assert!(text.contains("the chain's own output"), "{text}");
    }

    #[test]
    fn set_default_theft_is_caught_via_the_prior_default_breadcrumb() {
        // The set-default + follow-default config: the default source is
        // our own node, but the persisted pre-takeover default tells us
        // what the chain should follow — a re-routed stream is flagged.
        let mut r = healthy();
        r.active_mic = None;
        r.default_source = Some("hushmic_source".into());
        r.prior_default = Some("alsa_input.usb-mic".into());
        r.capture_feeders = Some(vec!["easyeffects_source".into()]);
        let (text, problems) = render(&r);
        assert_eq!(problems, 1, "{text}");
        assert!(
            text.contains("capture fed by: easyeffects_source (expected alsa_input.usb-mic)"),
            "{text}"
        );
    }

    #[test]
    fn set_default_users_never_get_a_false_feeder_alarm() {
        // Follow-default + set-default: the default source is our own node,
        // so there is no outside-knowable expectation — plain fact only.
        let mut r = healthy();
        r.active_mic = None;
        r.default_source = Some("hushmic_source".into());
        r.prior_default = None; // breadcrumb missing: no expectation at all
        r.capture_feeders = Some(vec!["alsa_input.usb-mic".into()]);
        let (text, problems) = render(&r);
        assert_eq!(problems, 0, "{text}");
        assert!(
            text.contains("capture fed by: alsa_input.usb-mic"),
            "{text}"
        );
    }

    #[test]
    fn latency_facts_render_and_verify() {
        let (text, problems) = render(&healthy());
        assert_eq!(problems, 0);
        assert!(
            text.contains("chain latency: 80 ms (3840 samples @ 48 kHz)"),
            "{text}"
        );
        assert!(text.contains("reported to PipeWire: yes (3840)"), "{text}");
    }

    #[test]
    fn latency_declaration_not_in_effect_is_a_problem() {
        // Supported host, chain up — but the read-back is missing or wrong:
        // the declaration is not in effect, which is exactly the bug class
        // --doctor exists to catch.
        let mut r = healthy();
        r.latency_reported = None;
        let (text, problems) = render(&r);
        assert_eq!(problems, 1, "{text}");
        let line = text.lines().find(|l| l.contains("reported to")).unwrap();
        assert!(line.starts_with("!!"), "{line}");

        let mut r = healthy();
        r.latency_reported = Some(480);
        let (text, problems) = render(&r);
        assert_eq!(problems, 1, "{text}");
        assert!(
            text.contains("480"),
            "mismatch shows the wrong value: {text}"
        );
    }

    #[test]
    fn latency_unsupported_host_is_a_plain_fact() {
        let mut r = healthy();
        r.latency_supported = false;
        r.latency_reported = None;
        let (text, problems) = render(&r);
        assert_eq!(problems, 0, "{text}");
        assert!(text.contains("no (PipeWire 1.6+ required)"), "{text}");
    }

    #[test]
    fn latency_readback_skipped_without_a_chain() {
        let mut r = healthy();
        r.hushmic_present = Some(false);
        r.enabled = false; // absent node while disabled is a plain fact
        r.latency_reported = None;
        let (text, problems) = render(&r);
        assert_eq!(problems, 0, "{text}");
        assert!(text.contains("(chain not running)"), "{text}");
    }

    #[test]
    fn healthy_report_has_no_problems() {
        let (text, problems) = render(&healthy());
        assert_eq!(problems, 0);
        assert!(text.contains("no problems found"), "{text}");
        assert!(!text.contains("!!"), "{text}");
        // Spot-check the facts are all present.
        for needle in [
            "0.3.0",
            "prefix install (/usr/local)",
            "1.2.0",
            "dpdfnet8_48khz_hr",
            "RODE NT-USB",
            "libdpdfnet_ladspa.so",
            "pw-dump",
            "chain up",
            "per-mic profiles: 1",
            "settings: RODE NT-USB profile",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }

    #[test]
    fn settings_line_says_whose_settings_apply() {
        let mut r = healthy();
        r.profile = None;
        let (text, problems) = render(&r);
        assert_eq!(problems, 0);
        assert!(text.contains("  settings: defaults\n"), "{text}");
    }

    #[test]
    fn missing_asset_is_a_problem() {
        let mut r = healthy();
        r.assets[0].exists = false;
        let (text, problems) = render(&r);
        assert_eq!(problems, 1);
        assert!(text.contains("1 problem(s) found"), "{text}");
        // The failing line itself carries the marker.
        let line = text
            .lines()
            .find(|l| l.contains("libdpdfnet_ladspa.so"))
            .unwrap();
        assert!(line.starts_with("!!"), "{line}");
    }

    #[test]
    fn unreachable_pipewire_is_a_problem() {
        let mut r = healthy();
        r.pw_version = None;
        let (text, problems) = render(&r);
        assert_eq!(problems, 1);
        let line = text.lines().find(|l| l.contains("pipewire")).unwrap();
        assert!(line.starts_with("!!"), "{line}");
        assert!(line.contains("unavailable"), "{line}");
    }

    #[test]
    fn missing_command_is_a_problem() {
        let mut r = healthy();
        r.commands.push(("pw-metadata", false));
        let (_, problems) = render(&r);
        assert_eq!(problems, 1);
    }

    #[test]
    fn absent_node_is_a_problem_only_with_running_enabled_instance() {
        let mut r = healthy();
        r.hushmic_present = Some(false);
        let (text, problems) = render(&r);
        assert_eq!(problems, 1, "{text}");

        r.instance_running = false;
        let (text, problems) = render(&r);
        assert_eq!(problems, 0, "{text}");

        r.instance_running = true;
        r.enabled = false;
        let (text, problems) = render(&r);
        assert_eq!(problems, 0, "{text}");
    }

    #[test]
    fn several_problems_are_all_counted() {
        let mut r = healthy();
        r.pw_version = None;
        r.assets[0].exists = false;
        r.commands.push(("pw-play", false));
        let (text, problems) = render(&r);
        assert_eq!(problems, 3);
        assert!(text.contains("3 problem(s) found"), "{text}");
    }

    #[test]
    fn empty_sources_render_without_trailing_parens() {
        let mut r = healthy();
        r.sources = Some(vec![]);
        let (text, _) = render(&r);
        assert!(text.contains("sources: 0\n"), "{text}");
    }

    #[test]
    fn missing_log_renders_a_note_not_a_problem() {
        let mut r = healthy();
        r.log_tail = None;
        let (text, problems) = render(&r);
        assert_eq!(problems, 0);
        assert!(text.contains("no log"), "{text}");
    }

    #[test]
    fn tee_writes_lines_and_truncates_the_previous_log() {
        let dir = std::env::temp_dir().join(format!("hushmic-diag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("tee-basic.log");
        std::fs::write(&log, "stale content from the previous spawn\n").unwrap();
        let handle = tee_with_cap("one\ntwo\n".as_bytes(), log.clone(), LOG_CAP_BYTES, 0);
        handle.join().unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "one\ntwo\n");
        // The log's directory is private, like the mictest recording dir.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "log dir mode {mode:o}");
    }

    #[test]
    fn tee_caps_the_log_with_a_marker() {
        let dir = std::env::temp_dir().join(format!("hushmic-diag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("tee-cap.log");
        // 5 lines of 11 bytes; cap 25 fits two of them.
        let input = "0123456789\n".repeat(5);
        let handle = tee_with_cap(std::io::Cursor::new(input.into_bytes()), log.clone(), 25, 0);
        handle.join().unwrap();
        let got = std::fs::read_to_string(&log).unwrap();
        assert!(got.ends_with("[log capped]\n"), "{got:?}");
        assert_eq!(got.matches("0123456789").count(), 2, "{got:?}");
    }

    #[test]
    fn tee_survives_non_utf8_bytes() {
        let dir = std::env::temp_dir().join(format!("hushmic-diag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("tee-raw.log");
        let handle = tee_with_cap(
            std::io::Cursor::new(vec![0xff, 0xfe, b'\n']),
            log.clone(),
            LOG_CAP_BYTES,
            0,
        );
        handle.join().unwrap();
        assert_eq!(std::fs::read(&log).unwrap(), vec![0xff, 0xfe, b'\n']);
    }

    #[test]
    fn install_type_classification() {
        // Sandbox/bundle formats win over any path inference.
        assert_eq!(install_type(true, false, Some("/usr")), "AppImage");
        assert_eq!(install_type(false, true, Some("/app")), "Flatpak");
        // Plain prefixes: /usr = distro package, others named as-is.
        assert_eq!(
            install_type(false, false, Some("/usr")),
            "system package (/usr)"
        );
        assert_eq!(
            install_type(false, false, Some("/usr/local")),
            "prefix install (/usr/local)"
        );
        // target/release & co: no <p>/bin layout.
        assert_eq!(
            install_type(false, false, None),
            "development build (no install prefix)"
        );
    }

    #[test]
    fn tail_returns_last_lines() {
        assert_eq!(tail("a\nb\nc\nd\n", 2), "c\nd\n");
        assert_eq!(tail("a\nb\n", 5), "a\nb\n");
        assert_eq!(tail("", 3), "");
        // No trailing newline on the input: preserved as-is.
        assert_eq!(tail("a\nb\nc", 2), "b\nc");
    }

    #[test]
    fn engine_lines_parse_by_head_only() {
        use super::{parse_engine_line, EngineTier};
        assert_eq!(
            parse_engine_line(
                b"[dpdfnet-ladspa] engine: light (cpu tight, 9.2 ms per 10 ms hop)\n"
            ),
            Some(EngineTier::Light)
        );
        assert_eq!(
            parse_engine_line(
                b"[dpdfnet-ladspa] engine: passthrough (cpu overloaded, lag 3 hops)\n"
            ),
            Some(EngineTier::Passthrough)
        );
        assert_eq!(
            parse_engine_line(b"[dpdfnet-ladspa] engine: quality (cost 0.42)\n"),
            Some(EngineTier::Quality)
        );
        assert_eq!(
            parse_engine_line(b"[dpdfnet-ladspa] engine: quality"),
            Some(EngineTier::Quality)
        );
        // Torn by another writer, unrelated, or the worker line: ignored.
        assert_eq!(
            parse_engine_line(b"[dpdfnet-ladspa] engine: li[pw] xrun\n"),
            None
        );
        assert_eq!(
            parse_engine_line(b"[dpdfnet-ladspa] worker: realtime priority 10\n"),
            None
        );
        assert_eq!(parse_engine_line(b"engine: light\n"), None);
        // Another writer's unterminated line in front of an intact
        // contract line: the report is still read.
        assert_eq!(
            parse_engine_line(b"[pw] xrun of 12 ms[dpdfnet-ladspa] engine: passthrough (cpu overloaded, lag 3 hops)\n"),
            Some(EngineTier::Passthrough)
        );
        // Two heads in one line: the later report is the current one.
        assert_eq!(
            parse_engine_line(
                b"[dpdfnet-ladspa] engine: passthrough[dpdfnet-ladspa] engine: light\n"
            ),
            Some(EngineTier::Light)
        );
        // Shorter than the head: no panic, no match.
        assert_eq!(parse_engine_line(b"e\n"), None);
        assert_eq!(parse_engine_line(b""), None);
    }

    #[test]
    fn a_retired_tee_cannot_overwrite_the_next_chain() {
        // Sequential, so it asserts the rule rather than racing on it:
        // the generation a tee reports under is the only thing that
        // decides, no matter when its line arrives.
        let old_gen = new_engine_generation();
        store_engine_tier(old_gen, EngineTier::Quality);
        assert_eq!(engine_tier(), Some(EngineTier::Quality));
        // The chain is replaced: a new generation, nothing reported yet.
        let new_gen = new_engine_generation();
        assert_ne!(old_gen, new_gen);
        assert_eq!(engine_tier(), None);
        // The old tee drains its last line now.
        store_engine_tier(old_gen, EngineTier::Passthrough);
        assert_eq!(engine_tier(), None, "a retired tee wrote the live tier");
        // The new chain reports, and the old tee still cannot take it back.
        store_engine_tier(new_gen, EngineTier::Light);
        store_engine_tier(old_gen, EngineTier::Passthrough);
        assert_eq!(engine_tier(), Some(EngineTier::Light));

        // A real tee reports under the generation it was started with.
        let dir = std::env::temp_dir().join(format!("hushmic-diag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("tee-engine.log");
        let live = new_engine_generation();
        let handle = tee_with_cap(
            std::io::Cursor::new(b"[dpdfnet-ladspa] engine: quality (cost 0.42)\n".to_vec()),
            log,
            LOG_CAP_BYTES,
            live,
        );
        handle.join().unwrap();
        assert_eq!(engine_tier(), Some(EngineTier::Quality));
        store_engine_tier(new_gen, EngineTier::Passthrough);
        assert_eq!(engine_tier(), Some(EngineTier::Quality));

        // Inference lines follow the same generations.
        let line = |engine: &str| Inference {
            model: "q".into(),
            engine: engine.into(),
            reason: None,
        };
        store_inference(live, line("native int8"));
        assert_eq!(inference_for("q"), Some(line("native int8")));
        assert_eq!(inference_for("l"), None);
        store_inference(new_gen, line("onnx"));
        assert_eq!(inference_for("q"), Some(line("native int8")));
        let next = new_engine_generation();
        assert_eq!(inference_for("q"), None, "a new chain starts unreported");
        store_inference(live, line("onnx"));
        assert_eq!(
            inference_for("q"),
            None,
            "a retired tee wrote the live engine"
        );
        let handle = tee_with_cap(
            std::io::Cursor::new(b"[dpdfnet-ladspa] inference: native fp32 for q\n".to_vec()),
            dir.join("tee-inference.log"),
            LOG_CAP_BYTES,
            next,
        );
        handle.join().unwrap();
        assert_eq!(inference_for("q"), Some(line("native fp32")));
    }
}
