//! `hushmic --doctor` end-to-end: prints the diagnostics report and exits
//! 0 (healthy) or 1 (problems found) — never a usage error or a crash,
//! even on a machine with no PipeWire at all (the CI runner).

use std::process::Command;

#[test]
fn doctor_prints_a_report_and_exits_zero_or_one() {
    let out = Command::new(env!("CARGO_BIN_EXE_hushmic"))
        .arg("--doctor")
        .output()
        .expect("spawn hushmic --doctor");
    let code = out.status.code();
    assert!(
        code == Some(0) || code == Some(1),
        "exit {code:?}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("hushmic diagnostics"), "{stdout}");
    // The summary line is always last: "no problems found" or "N problem(s) found".
    assert!(stdout.trim_end().ends_with("found"), "{stdout}");
}

#[test]
fn doctor_rejects_extra_flags_as_usage_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_hushmic"))
        .args(["--doctor", "--version"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(2));
}
