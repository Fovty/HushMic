//! Contract log lines: one `write(2)` per line.
//!
//! The host's stderr is a pipe shared with PipeWire's own log output from
//! other threads (xrun warnings above all, which cluster exactly under the
//! overload these lines report). `eprintln!` issues several writes per
//! formatted line, so two writers can interleave mid-line; a single
//! `write_all` of a prebuilt line keeps the head `[dpdfnet-ladspa] engine:`
//! intact for the parsers in the app and in CI.

use std::io::Write;

/// Write `[dpdfnet-ladspa] {body}\n` to stderr in one call.
pub fn contract_line(body: &str) {
    let line = format!("[dpdfnet-ladspa] {body}\n");
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}
