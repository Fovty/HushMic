//! `pipewire::pw_dump_within` against fake `pw-dump` scripts on PATH: the
//! status path calls it on the main loop, so a wedged daemon must cost the
//! budget and read as unknown, never hang.
//!
//! Single #[test]: PATH is process-global, parallel test fns would race it.

use hushmic::pipewire::pw_dump_within;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

fn fake(dir: &Path, body: &str) {
    let p = dir.join("pw-dump");
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn pw_dump_within_answers_kills_and_reports_failure() {
    let dir = std::env::temp_dir().join(format!("hm-pwdump-budget-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Resolved before PATH narrows to the fakes.
    let sleep = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v sleep"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let sleep = sleep.trim();
    assert!(!sleep.is_empty(), "no sleep binary");
    std::env::set_var("PATH", &dir);

    // A prompt answer passes through.
    fake(&dir, r#"echo '[ { "id": 1 } ]'"#);
    let out = pw_dump_within(Duration::from_secs(5)).expect("fast dump");
    assert!(out.contains("\"id\": 1"), "{out}");

    // A hung daemon: the budget is spent, the child killed, the answer unknown.
    fake(&dir, &format!("exec {sleep} 30"));
    let t = Instant::now();
    assert_eq!(pw_dump_within(Duration::from_millis(300)), None);
    let took = t.elapsed();
    assert!(
        took >= Duration::from_millis(250) && took < Duration::from_secs(3),
        "took {took:?}"
    );

    // A failing probe and non-JSON output read as unknown too.
    fake(&dir, "exit 1");
    assert_eq!(pw_dump_within(Duration::from_secs(5)), None);
    fake(&dir, "echo 'not json'");
    assert_eq!(pw_dump_within(Duration::from_secs(5)), None);

    let _ = std::fs::remove_dir_all(&dir);
}
