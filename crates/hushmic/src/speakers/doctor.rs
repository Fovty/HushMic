//! The speakers cleaner's section of `hushmic --doctor` (and Copy
//! diagnostics): facts gathered once, rendered as lines the report marks
//! as facts, problems or warnings. Judged only while the cleaner should
//! run (switched on, HushMic on, an instance running); otherwise every
//! line is a plain fact.

use super::graph::{own_player_target, Graph};
use super::{PLAYBACK, SINK};
use crate::pipewire::StreamControls;

/// Every fact the section prints.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// The saved switch, output and strength.
    pub enabled: bool,
    pub output: Option<String>,
    pub attn_limit: f32,
    /// HushMic itself is on (`enabled` in the config).
    pub hushmic_on: bool,
    /// The graph (None: PipeWire did not answer).
    pub graph: Option<Graph>,
    /// What the running chain declares to PipeWire (None: nothing).
    pub latency_reported: Option<u32>,
    /// The filter-chain can declare latency (PipeWire 1.6+).
    pub latency_supported: bool,
    /// Mixer controls on its internal output stream.
    pub controls: Vec<StreamControls>,
    /// From the running instance: failed starts in a row and the last error.
    pub failures: u32,
    pub error: Option<String>,
    /// Apps a restart left on another output.
    pub left: Vec<String>,
    /// From its own log: the worker's scheduling, the engine line, the
    /// inference lines, and the tail.
    pub worker_rt: Option<Result<String, String>>,
    pub engine_cost: Option<(String, f32)>,
    pub inference: Vec<crate::diagnostics::Inference>,
    pub log_tail: Option<String>,
}

/// How a line counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Fact,
    Problem,
    Warning,
    /// Raw text (the log tail), not a check.
    Text,
}

/// Gather the facts: the config, `dump` (the report's one pw-dump), the
/// running instance's `status --json` (None: none answered), the log.
pub fn collect(cfg: &crate::config::Config, dump: Option<&str>, status: Option<&str>) -> Report {
    let sp = cfg.speakers();
    let graph = dump.map(Graph::read);
    let log = std::fs::read_to_string(crate::diagnostics::log_path_of(
        crate::diagnostics::Chain::Speakers,
    ))
    .ok();
    let running: Option<serde_json::Value> = status.and_then(|s| serde_json::from_str(s).ok());
    let block = running.as_ref().map(|v| &v["speakers"]);
    Report {
        enabled: sp.enabled,
        output: sp.output,
        attn_limit: sp.attn_limit,
        hushmic_on: cfg.enabled,
        latency_reported: graph
            .as_ref()
            .filter(|g| g.sink_up)
            .and_then(|_| reported_latency(dump?)),
        graph,
        latency_supported: crate::pipewire::supports_latency_report(),
        controls: dump
            .map(|d| crate::pipewire::stream_controls_of(d, &[PLAYBACK]))
            .unwrap_or_default(),
        failures: block.and_then(|b| b["failures"].as_u64()).unwrap_or(0) as u32,
        error: block.and_then(|b| b["error"].as_str()).map(str::to_string),
        left: block
            .and_then(|b| b["left"].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        worker_rt: log
            .as_deref()
            .and_then(crate::diagnostics::worker_rt_from_log),
        engine_cost: log
            .as_deref()
            .and_then(crate::diagnostics::engine_cost_from_log),
        inference: log
            .as_deref()
            .map(crate::diagnostics::inference_from_log)
            .unwrap_or_default(),
        log_tail: log.as_deref().map(|s| crate::diagnostics::tail(s, 20)),
    }
}

/// `pw-cli enum-params <hushmic_sink> ProcessLatency`, read back.
fn reported_latency(dump: &str) -> Option<u32> {
    let id = crate::pipewire::parse_node_id(dump, SINK)?;
    let out = std::process::Command::new("pw-cli")
        .args(["enum-params", &id.to_string(), "ProcessLatency"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    crate::pipewire::parse_process_latency(&String::from_utf8_lossy(&out.stdout))
}

/// The doctor's line for an app a restart left on another output.
pub fn left_line(app: &str) -> String {
    format!("  {app} left HushMic Speakers when it restarted. Pick HushMic Speakers for it again.")
}

/// The section's lines. `instance_running`: a HushMic runs (the chain
/// can only be up then). Pure.
pub fn render(r: &Report, instance_running: bool) -> Vec<(Kind, String)> {
    use Kind::*;
    let mut out: Vec<(Kind, String)> = Vec::new();
    if !r.enabled {
        out.push((Fact, "speakers: off".into()));
        return out;
    }
    let judge = r.hushmic_on && instance_running;
    let state = match (r.hushmic_on, instance_running) {
        (false, _) => "on (stopped while HushMic is off)",
        (true, false) => "on (HushMic is not running)",
        (true, true) => "on",
    };
    out.push((Fact, format!("speakers: {state}")));
    let g = r.graph.as_ref();
    // with HushMic Speakers as the system output, "the system output" it
    // falls back to is the last real default
    let fallback = if g.is_some_and(Graph::user_default_is_ours) {
        "the last real default"
    } else {
        "the system output"
    };
    let output = match (&r.output, g) {
        (None, _) => "default (the system output)".to_string(),
        (Some(o), Some(g)) if g.is_feeder(o) => {
            format!("{o} (it feeds the microphone HushMic cleans: it plays to {fallback} instead)")
        }
        (Some(o), Some(g)) if !g.has_sink(o) => {
            format!("{o} (not plugged in: it plays to {fallback} meanwhile)")
        }
        (Some(o), Some(g)) => g
            .description_of(o)
            .filter(|d| d != o)
            .map(|d| format!("{d} ({o})"))
            .unwrap_or_else(|| o.clone()),
        (Some(o), None) => o.clone(),
    };
    out.push((Fact, format!("  output: {output}")));
    out.push((
        Fact,
        format!("  strength: {}", super::db_words(r.attn_limit)),
    ));
    if let Some(e) = r.error.as_deref().filter(|_| judge) {
        out.push((Problem, format!("  could not start: {e}")));
    } else if r.failures > 0 && judge {
        out.push((
            Warning,
            format!(
                "speakers: {} failed start(s) in a row; HushMic keeps trying",
                r.failures
            ),
        ));
    }
    let Some(g) = g else {
        out.push((Fact, "  outputs: unavailable".into()));
        return finish(out, r);
    };
    if g.sinks.is_empty() {
        out.push((Fact, "  outputs: none found".into()));
    } else {
        let names: Vec<&str> = g.sinks.iter().map(|s| s.description.as_str()).collect();
        out.push((
            Fact,
            format!("  outputs: {} ({})", names.len(), names.join(", ")),
        ));
    }
    out.push((
        Fact,
        match (
            g.default_effective.as_deref(),
            g.default_configured.as_deref(),
        ) {
            (_, Some(SINK)) | (Some(SINK), None) => match r.output.as_deref() {
                // it plays to the chosen output, while that is there
                Some(o) if g.has_sink(o) => "  default output: HushMic Speakers (everything \
                     heard is cleaned; it plays to the chosen output)"
                    .to_string(),
                _ => "  default output: HushMic Speakers (everything heard is cleaned; it \
                      plays to the last real default)"
                    .to_string(),
            },
            (Some(e), Some(c)) if e == c => format!("  default output: {e}"),
            (Some(e), c) => format!(
                "  default output: {e} (configured: {})",
                c.unwrap_or("none set")
            ),
            (None, c) => format!(
                "  default output: (unknown) (configured: {})",
                c.unwrap_or("none set")
            ),
        },
    ));
    out.push((
        if !g.sink_up && judge { Problem } else { Fact },
        format!(
            "  hushmic_sink present: {}",
            if g.sink_up { "yes" } else { "no" }
        ),
    ));
    if g.sink_up {
        out.push(if g.playing_to.is_empty() {
            (
                if judge { Problem } else { Fact },
                "  plays to: nothing (no output other than HushMic Speakers? it cannot play \
                 to itself)"
                    .to_string(),
            )
        } else if g.plays_into_mic() {
            (
                if judge { Problem } else { Fact },
                format!(
                    "  plays to: {} (it feeds the microphone HushMic cleans: muted until \
                     another output is there)",
                    g.playing_to.join(", ")
                ),
            )
        } else {
            (Fact, format!("  plays to: {}", g.playing_to.join(", ")))
        });
        out.push((
            Fact,
            if g.fed_by.is_empty() {
                "  playing through it: nothing (pick HushMic Speakers as an app's output)"
                    .to_string()
            } else {
                format!("  playing through it: {}", g.fed_by.join(", "))
            },
        ));
        for app in &r.left {
            out.push((if judge { Problem } else { Fact }, left_line(app)));
        }
        if let Some(st) = &g.sink_state {
            out.push((
                Fact,
                if crate::pipewire::state_is_idle(st) {
                    format!("  state: {st} (nothing plays through it, no CPU used)")
                } else {
                    format!(
                        "  state: {st} (the output it plays to runs at {} samples per quantum \
                         meanwhile)",
                        crate::controller::PINNED_QUANTUM
                    )
                },
            ));
        }
        use crate::controller::{LATENCY_SAMPLES, PINNED_QUANTUM};
        out.push(match g.sink_quantum {
            Some(q) if q == PINNED_QUANTUM => (Fact, format!("  quantum pin: {q}")),
            Some(0) | None => (
                if judge { Problem } else { Fact },
                "  quantum pin: missing (a hushmic_sink this HushMic did not make — restart \
                 HushMic)"
                    .to_string(),
            ),
            Some(q) => (
                if judge { Problem } else { Fact },
                format!("  quantum pin: {q} (from a different HushMic version — restart HushMic)"),
            ),
        });
        out.push(if !r.latency_supported {
            (
                Fact,
                "  reported to PipeWire: no (PipeWire 1.6+ required; about 90 ms arrive \
                 late, which calls and videos do not notice)"
                    .to_string(),
            )
        } else {
            match r.latency_reported {
                Some(n) if n == LATENCY_SAMPLES => {
                    (Fact, format!("  reported to PipeWire: yes ({n})"))
                }
                // Declared once sound flows: while nothing plays PipeWire
                // suspends the sink and nothing is reported yet, a fact.
                // Missing while it plays is the fault, as for the mic.
                None | Some(0)
                    if g.sink_state
                        .as_deref()
                        .is_some_and(crate::pipewire::state_is_idle) =>
                {
                    (
                        Fact,
                        "  reported to PipeWire: not yet (declared once sound plays through it)"
                            .to_string(),
                    )
                }
                None | Some(0) => (
                    if judge { Problem } else { Fact },
                    "  reported to PipeWire: missing (declaration not in effect)".to_string(),
                ),
                Some(n) => (
                    if judge { Problem } else { Fact },
                    format!("  reported to PipeWire: mismatch ({n}, declared {LATENCY_SAMPLES})"),
                ),
            }
        });
        if let Some(t) = own_player_target(g) {
            out.push((Fact, format!("  HushMic's own players play to: {t}")));
        }
    }
    for c in &r.controls {
        let pct = (c.volume * 100.0).round();
        if c.muted {
            out.push((
                Problem,
                format!(
                    "  {} is muted in a mixer: HushMic Speakers is silent",
                    c.name
                ),
            ));
        } else if pct < 100.0 {
            out.push((Problem, format!("  {} volume in a mixer: {pct} %", c.name)));
        }
    }
    finish(out, r)
}

/// The engine lines from its log, then the log's tail.
fn finish(mut out: Vec<(Kind, String)>, r: &Report) -> Vec<(Kind, String)> {
    use Kind::*;
    match &r.worker_rt {
        Some(Ok(how)) => out.push((Fact, format!("  engine worker: realtime priority {how}"))),
        Some(Err(why)) => out.push((Fact, format!("  engine worker: not realtime ({why})"))),
        None => {}
    }
    if let Some((tier, cost)) = &r.engine_cost {
        out.push((Fact, format!("  engine: {tier} (cost {cost:.2})")));
    }
    for i in &r.inference {
        out.push((Fact, format!("  inference: {i}")));
    }
    match &r.log_tail {
        Some(t) => {
            out.push((Text, "  speakers log (tail):".into()));
            for l in t.lines() {
                out.push((Text, format!("    {l}")));
            }
        }
        None => out.push((Text, "  speakers log: no log file yet".into())),
    }
    out
}
