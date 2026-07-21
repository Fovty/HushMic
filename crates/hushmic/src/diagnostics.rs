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
    pub model: String,
    pub attn_limit: f32,
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
    pub instance_running: bool,
    pub assets: Vec<AssetFact>,
    /// (command, on PATH). Missing = problem.
    pub commands: Vec<(&'static str, bool)>,
    /// Tail of the filter-chain log; None = no log file yet.
    pub log_tail: Option<String>,
    /// The filter-chain binary can declare latency (PipeWire >= 1.6).
    pub latency_supported: bool,
    /// Live read-back of what the running chain reports; None = nothing
    /// reported (or no chain/probe). Only judged when a chain is up on a
    /// supporting host.
    pub latency_reported: Option<u32>,
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
    let model_path = paths.model_dir.join(format!("{}.onnx", cfg.model));
    // Momentarily acquiring the lock is harmless (LOCK_NB; drop releases):
    // acquired = nothing was holding it. Err (unreadable path, foreign
    // owner) reads as "not running" — conservative, since the node-absent
    // problem only fires while an instance IS running.
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
        model: cfg.model.clone(),
        attn_limit: cfg.attn_limit,
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
        log_tail: std::fs::read_to_string(log_path())
            .ok()
            .map(|s| tail(&s, 40)),
        latency_supported: crate::pipewire::supports_latency_report(),
        latency_reported: crate::pipewire::chain_reported_latency(),
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
    line(&mut out, false, format!("  set default: {}", r.set_default));
    line(
        &mut out,
        false,
        format!("  per-mic profiles: {}", r.mic_profiles),
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
    line(
        &mut out,
        false,
        format!(
            "can set default: {}",
            if r.can_set_default { "yes" } else { "no" }
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
) -> std::thread::JoinHandle<()> {
    tee_with_cap(reader, log, LOG_CAP_BYTES)
}

fn tee_with_cap(
    reader: impl std::io::Read + Send + 'static,
    log: std::path::PathBuf,
    cap: u64,
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
            set_default: true,
            mic_profiles: 1,
            sources: Some(vec!["RODE NT-USB".into(), "Webcam C920".into()]),
            default_source: Some("hushmic_source".into()),
            hushmic_present: Some(true),
            can_set_default: true,
            instance_running: true,
            assets: vec![AssetFact {
                what: "LADSPA plugin",
                path: "/usr/lib/ladspa/libdpdfnet_ladspa.so".into(),
                exists: true,
            }],
            commands: vec![("pw-dump", true), ("pw-cli", true)],
            log_tail: Some("[hushmic] chain up\n".into()),
            latency_supported: true,
            latency_reported: Some(2880),
        }
    }

    #[test]
    fn latency_facts_render_and_verify() {
        let (text, problems) = render(&healthy());
        assert_eq!(problems, 0);
        assert!(
            text.contains("chain latency: 60 ms (2880 samples @ 48 kHz)"),
            "{text}"
        );
        assert!(text.contains("reported to PipeWire: yes (2880)"), "{text}");
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
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
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
        let handle = tee_with_cap("one\ntwo\n".as_bytes(), log.clone(), LOG_CAP_BYTES);
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
        let handle = tee_with_cap(std::io::Cursor::new(input.into_bytes()), log.clone(), 25);
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
}
