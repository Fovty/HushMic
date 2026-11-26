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
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert_eq!(
        err,
        "hushmic: --headless and --tray can't be used together (one at a time)\n"
    );
}

/// UX panel round 2, A11: `status --json` with nothing running still
/// answers on stdout, `{"running":false}`, exit 2; the hint on stderr
/// names the unit's start too.
#[test]
fn status_json_says_not_running_on_stdout() {
    let dir = std::env::temp_dir().join(format!("hushmic-norun-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (args, stdout) in [
        (&["status", "--json"][..], "{\"running\":false}\n"),
        (&["status"][..], ""),
    ] {
        let out = bin()
            .args(args)
            .env("XDG_RUNTIME_DIR", &dir)
            .env("HOME", &dir)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(String::from_utf8(out.stdout).unwrap(), stdout, "{args:?}");
        let err = String::from_utf8(out.stderr).unwrap();
        assert!(
            err.contains("not running") && err.contains("systemctl --user start hushmic"),
            "{args:?}: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Usage errors are exit 1 everywhere, in one line (not the whole help),
/// and 2 stays "not running".
#[test]
fn usage_errors_exit_1_in_one_line() {
    for (args, says) in [
        (
            &["--frobnicate"][..],
            "hushmic: unknown option '--frobnicate' (see hushmic --help)\n",
        ),
        (
            &["frobnicate"][..],
            "hushmic: unknown command 'frobnicate' (see hushmic --help)\n",
        ),
        (
            &["help", "frobnicate"][..],
            "hushmic: no command 'frobnicate' (see hushmic --help)\n",
        ),
        (&["--window", "--page", "nowhere"][..], ""),
        #[cfg(not(feature = "speakers"))]
        (
            &["devices", "--wide"][..],
            "usage: hushmic devices [--json]\n",
        ),
        #[cfg(feature = "speakers")]
        (
            &["devices", "--wide"][..],
            "usage: hushmic devices [outputs] [--json]\n",
        ),
        // a bad word: that command's usage, not every command's (F12)
        (
            &["status", "--bogus"][..],
            "usage: hushmic status [--json]\n",
        ),
    ] {
        let out = bin().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8(out.stderr).unwrap();
        if !says.is_empty() {
            assert_eq!(err, says, "{args:?}");
        }
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn version_has_its_short_forms() {
    let want = bin().arg("--version").output().unwrap().stdout;
    assert!(String::from_utf8_lossy(&want).starts_with("hushmic "));
    for flag in ["-V", "version"] {
        let out = bin().arg(flag).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert_eq!(out.stdout, want, "{flag}");
    }
}

/// `COMMAND --help` and `help COMMAND`: that command's lines and the exit
/// codes, on stdout, and nothing runs (no daemon is asked).
#[test]
fn every_command_has_its_own_help() {
    for (args, has) in [
        (&["status", "--help"][..], "hushmic status [--json]"),
        (&["status", "-h"][..], "hushmic status [--json]"),
        (
            &["help", "mode"][..],
            "hushmic mode [suppress|bypass|mute|off]",
        ),
        (&["quit", "--help"][..], "hushmic quit"),
        (
            &["unmute-to", "--help"][..],
            "What unmuting returns to (not muted: the next mute's).",
        ),
        (
            &["config", "set", "--help"][..],
            "Keys (value, then the default):",
        ),
        #[cfg(not(feature = "speakers"))]
        (&["devices", "--help"][..], "hushmic devices [--json]"),
        #[cfg(feature = "speakers")]
        (
            &["devices", "--help"][..],
            "hushmic devices [outputs] [--json]",
        ),
        #[cfg(feature = "speakers")]
        (
            &["speakers", "--help"][..],
            "hushmic speakers strength maximum|strong|medium|gentle|DB",
        ),
        (
            &["service", "--help"][..],
            "hushmic service install|uninstall",
        ),
        (&["miccheck", "--help"][..], "cleaned (no\nwindow needed"),
        (&["watch", "--help"][..], "hushmic watch"),
        (
            &["open-files", "a.wav", "--help"][..],
            "Usage: hushmic open-files",
        ),
        (&["clean", "--help"][..], "Usage: hushmic clean"),
        (&["help", "clean"][..], "Usage: hushmic clean"),
        (&["--window", "--help"][..], "--reduced-motion"),
    ] {
        let out = bin()
            .args(args)
            // a daemon must not be reached: a socket nowhere
            .env("XDG_RUNTIME_DIR", "/nonexistent")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        let s = String::from_utf8(out.stdout).unwrap();
        assert!(s.contains(has), "{args:?}:\n{s}");
        assert!(out.stderr.is_empty(), "{args:?}");
    }
    let out = bin().args(["status", "--help"]).output().unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(!s.contains("hushmic mode"), "only its own lines:\n{s}");
    // only its own exit codes (F12)
    assert!(
        s.contains("Exit codes:") && !s.contains("already running"),
        "{s}"
    );
    let out = bin().args(["--window", "--help"]).output().unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("one of:"), "the sample states are named:\n{s}");
}

#[test]
fn the_help_documents_the_exit_codes() {
    let out = bin().arg("--help").output().unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("1 usage error or failed, 2 not running"), "{s}");
    assert!(s.contains("3 already running"), "{s}");
    assert!(s.contains("--size WxH"), "{s}");
}

/// A start that finds another instance holding the lock started nothing:
/// from a terminal or a script exit 3, not 0. Started by systemd it exits
/// 0 as before 1.0 and says so for the journal: a unit an earlier version
/// generated (Restart=on-failure, no RestartPreventExitStatus) must not
/// restart every 3 s for as long as a tray instance runs.
#[test]
fn a_second_start_is_already_running_exit_3_or_0_under_systemd() {
    let dir = std::env::temp_dir().join(format!("hushmic-second-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let held = hushmic::lock::try_lock(&dir.join("hushmic.lock"))
        .unwrap()
        .expect("the lock is free");
    hushmic::lock::record_holder(&held, "tray");
    // `exec_pid`: what SYSTEMD_EXEC_PID says; "self" is the started
    // process's own pid (sh's $$, kept across exec), as systemd sets it
    let start = |mode: &str, invocation: bool, exec_pid: Option<&str>| {
        let mut c = Command::new("sh");
        let pid = match exec_pid {
            Some("self") => "SYSTEMD_EXEC_PID=$$ ".to_string(),
            Some(p) => format!("SYSTEMD_EXEC_PID={p} "),
            None => String::new(),
        };
        c.arg("-c")
            .arg(format!("{pid}exec \"$0\" \"$1\""))
            .arg(env!("CARGO_BIN_EXE_hushmic"))
            .arg(mode)
            .env("XDG_RUNTIME_DIR", &dir)
            .env("HOME", &dir)
            .env_remove("SYSTEMD_EXEC_PID")
            .stdin(std::process::Stdio::null());
        if invocation {
            c.env("INVOCATION_ID", "0123456789abcdef0123456789abcdef");
        } else {
            c.env_remove("INVOCATION_ID");
        }
        let out = c.output().unwrap();
        (out.status.code(), String::from_utf8(out.stderr).unwrap())
    };
    let me = std::process::id();
    for mode in ["--headless", "--tray", "--enable-once"] {
        let (code, err) = start(mode, false, None);
        assert_eq!(code, Some(3), "{mode}");
        assert!(
            err.starts_with(&format!("hushmic is already running (pid {me}, tray).")),
            "{mode}: {err}"
        );
    }
    // a shell under a terminal that runs as a systemd service inherits
    // both variables (the pid is the terminal server's), and one started
    // by older systemd's neighbour has no SYSTEMD_EXEC_PID and a parent
    // that isn't the manager: both are typed starts
    for exec_pid in [Some("1"), None] {
        let (code, err) = start("--tray", true, exec_pid);
        assert_eq!(code, Some(3), "{exec_pid:?}: {err}");
    }
    let (code, err) = start("--headless", true, Some("self"));
    assert_eq!(code, Some(0));
    assert_eq!(
        err,
        format!("hushmic is already running (pid {me}, tray); this service instance stops.\n")
    );
    drop(held);
    let _ = std::fs::remove_dir_all(dir);
}
