//! `--help` is a documented entry point: exit 0, stdout, every mode named.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hushmic"))
}

#[test]
fn help_exits_zero_on_stdout_and_names_every_mode() {
    for flag in ["--help", "-h"] {
        let out = bin().arg(flag).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{flag}");
        let s = String::from_utf8(out.stdout).unwrap();
        for word in [
            "--tray",
            "--headless",
            "--enable-once",
            "config set",
            "devices",
            "quit",
            "service install",
        ] {
            assert!(s.contains(word), "{flag} output lacks {word}:\n{s}");
        }
        assert!(out.stderr.is_empty());
    }
}

#[test]
fn headless_and_tray_together_is_a_usage_error() {
    let out = bin().args(["--headless", "--tray"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.starts_with("usage:"), "{err}");
}
