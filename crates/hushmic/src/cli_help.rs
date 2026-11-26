//! Each command's own help (`hushmic COMMAND --help`, `hushmic help
//! COMMAND`) and its usage line for a usage error: only its own forms and
//! its own exit codes (F12). `clean`, `open-files` and `--window` keep
//! theirs (`files::cli`, the window's).

/// What a command's exit codes mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codes {
    /// Talks to the running HushMic.
    Control,
    /// Runs here, with or without a running HushMic.
    Local,
}

struct Command {
    word: &'static str,
    /// Its forms, each without the leading "hushmic ".
    forms: &'static [&'static str],
    about: &'static str,
    codes: Codes,
}

const COMMANDS: &[Command] = &[
    Command {
        word: "status",
        forms: &["status [--json]"],
        about: "What the running HushMic is doing: its mode, the microphone and\n\
                the chain. --json: one JSON object (see docs/usage.md).",
        codes: Codes::Control,
    },
    Command {
        word: "mode",
        forms: &["mode [suppress|bypass|mute|off]"],
        about: "Without a state, print the one it is in. suppress: your voice,\n\
                cleaned. bypass: your raw voice, same latency. mute: silence.\n\
                off: the HushMic microphone is removed.",
        codes: Codes::Control,
    },
    Command {
        word: "toggle",
        forms: &["toggle mute|bypass"],
        about: "Go into that state, or back to the one before it: one key can\n\
                serve as a mute button.",
        codes: Codes::Control,
    },
    Command {
        word: "unmute-to",
        forms: &["unmute-to suppress|bypass"],
        about: "What unmuting returns to (not muted: the next mute's).",
        codes: Codes::Control,
    },
    Command {
        word: "quit",
        forms: &["quit [--force]"],
        about: "Stop HushMic. Returns once it is gone and the previous default\n\
                microphone is back. Refuses while files are cleaning, or cleaned\n\
                and not saved; --force quits anyway.",
        codes: Codes::Control,
    },
    Command {
        word: "watch",
        forms: &["watch"],
        about: "Its state as JSON lines, one per change, until HushMic stops.",
        codes: Codes::Control,
    },
    Command {
        word: "refresh",
        forms: &["refresh"],
        about: "Read PipeWire again now.",
        codes: Codes::Control,
    },
    Command {
        word: "retry",
        forms: &["retry"],
        about: "Try a microphone that is down again, now.",
        codes: Codes::Control,
    },
    Command {
        word: "shortcuts",
        forms: &["shortcuts setup"],
        about: "Open the desktop's dialog for HushMic's global shortcuts.",
        codes: Codes::Control,
    },
    Command {
        word: "miccheck",
        forms: &["miccheck"],
        about: "Record a few seconds, then play them back raw, then cleaned (no\n\
                window needed: notifications guide). Returns when both have\n\
                played.",
        codes: Codes::Control,
    },
    Command {
        word: "notice",
        forms: &["notice hide APP [NAME]", "notice show APP"],
        about: "Leave APP (its PipeWire application name) out of the apps listed\n\
                as recording the microphone directly, or list it again; NAME is\n\
                how a hidden one is shown.",
        codes: Codes::Control,
    },
    Command {
        word: "profile",
        forms: &["profile reset DEVICE"],
        about: "Forget a microphone's own strength and model: it uses the\n\
                defaults again.",
        codes: Codes::Control,
    },
    Command {
        word: "config",
        forms: &[
            "config [--json]",
            "config get KEY [--json]",
            "config set [--defaults] KEY VALUE",
            "config path",
        ],
        about: "The settings. Applied at once while HushMic runs, else saved to\n\
                the file for its next start. model and attn_limit are those of\n\
                the microphone in use, and set saves them for it; --defaults\n\
                sets them for every microphone without its own.",
        codes: Codes::Local,
    },
    #[cfg(not(feature = "speakers"))]
    Command {
        word: "devices",
        forms: &["devices [--json]"],
        about: "The microphones HushMic can use, by the names `config set mic`\n\
                takes; the one chosen is marked.",
        codes: Codes::Local,
    },
    #[cfg(feature = "speakers")]
    Command {
        word: "devices",
        forms: &["devices [outputs] [--json]"],
        about: "The microphones HushMic can use, by the names `config set mic`\n\
                takes; the one chosen is marked. outputs: the outputs HushMic\n\
                Speakers can play to, by the names `speakers output` takes.",
        codes: Codes::Local,
    },
    #[cfg(feature = "speakers")]
    Command {
        word: "speakers",
        forms: &[
            "speakers",
            "speakers on|off",
            "speakers strength maximum|strong|medium|gentle|DB",
            "speakers output default|NAME",
        ],
        about: "HushMic Speakers: an output apps can pick whose sound reaches your\n\
                speakers or headphones cleaned, about 0.1 s later. Without\n\
                arguments, what it does. on|off: switch it (saved). strength:\n\
                how strongly it cleans. output: where it plays (default: the\n\
                system's output). The same as `config set speakers.enabled`,\n\
                `speakers.attn_limit` and `speakers.output`.",
        codes: Codes::Local,
    },
    Command {
        word: "service",
        forms: &["service install|uninstall"],
        about: "A systemd user unit that runs this install headless at login\n\
                (it replaces the login autostart entry; see docs/headless.md).",
        codes: Codes::Local,
    },
];

/// `config --help`'s keys: name, values, default.
const KEYS: &[(&str, &str, &str)] = &[
    (
        "mic",
        "a name from `hushmic devices`, or default",
        "default",
    ),
    ("model", "quality or light", "quality"),
    (
        "attn_limit",
        "0 to 100 (dB), or maximum, strong, medium, gentle",
        "100",
    ),
    (
        "set_default",
        "true or false: HushMic is the system's default input",
        "false",
    ),
    ("autostart", "true or false: start when you log in", "false"),
    (
        "tray",
        "true or false: a tray icon (from the next start)",
        "true",
    ),
    ("tray_icon", "auto, color or symbolic", "auto"),
    ("notifications", "true or false", "true"),
    (
        "inference",
        "auto or onnx: the engine that runs the model",
        "auto",
    ),
    (
        "voice_hold",
        "true or false: keep voices farther from the microphone out",
        "false",
    ),
    (
        "appearance",
        "system, dark or light: the window's colors",
        "system",
    ),
    #[cfg(feature = "speakers")]
    (
        "speakers.enabled",
        "true or false: the HushMic Speakers output",
        "false",
    ),
    #[cfg(feature = "speakers")]
    (
        "speakers.output",
        "a name from `hushmic devices outputs`, or default",
        "default",
    ),
    #[cfg(feature = "speakers")]
    (
        "speakers.attn_limit",
        "0 to 100 (dB), or maximum, strong, medium, gentle",
        "100",
    ),
];

fn command(word: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.word == word)
}

/// The usage a usage error of `word` prints: its forms only. None for a
/// word that is not one of these commands.
pub fn usage(word: &str) -> Option<String> {
    let c = command(word)?;
    let mut out = String::new();
    for (i, f) in c.forms.iter().enumerate() {
        let lead = if i == 0 { "usage: " } else { "       " };
        out.push_str(&format!("{lead}hushmic {f}\n"));
    }
    out.pop();
    Some(out)
}

/// `hushmic COMMAND --help`: its forms, what it does, `config`'s keys, and
/// its own exit codes.
pub fn help(word: &str) -> Option<String> {
    let c = command(word)?;
    let mut out = String::from("Usage:");
    for (i, f) in c.forms.iter().enumerate() {
        let lead = if i == 0 { " " } else { "       " };
        out.push_str(&format!("{lead}hushmic {f}\n"));
    }
    out.push('\n');
    out.push_str(c.about);
    out.push_str("\n\n");
    if word == "config" {
        out.push_str("Keys (value, then the default):\n");
        let w = KEYS.iter().map(|k| k.0.len()).max().unwrap_or(0);
        for (k, values, default) in KEYS {
            out.push_str(&format!("  {k:<w$}  {values} ({default})\n"));
        }
        out.push('\n');
    }
    out.push_str(match c.codes {
        Codes::Control => {
            "Exit codes: 0 ok; 1 a usage error, or the command failed; 2 HushMic is\n\
             not running.\n"
        }
        Codes::Local => "Exit codes: 0 ok; 1 a usage error, or the command failed.\n",
    });
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_has_its_own_usage_and_exit_codes() {
        assert_eq!(usage("status").unwrap(), "usage: hushmic status [--json]");
        assert_eq!(
            usage("notice").unwrap(),
            "usage: hushmic notice hide APP [NAME]\n       hushmic notice show APP"
        );
        let h = help("status").unwrap();
        assert!(h.starts_with("Usage: hushmic status [--json]\n"), "{h}");
        assert!(h.contains("2 HushMic is\nnot running"), "{h}");
        assert!(!h.contains("already running"), "{h}");
        let c = help("config").unwrap();
        assert!(!c.contains("not running"), "config works without it: {c}");
        assert!(c.contains("config set [--defaults] KEY VALUE"), "{c}");
        assert!(usage("clean").is_none() && help("nope").is_none());
    }

    /// Every key `config set` takes is in `config --help`, and no other.
    #[test]
    fn config_help_lists_every_settable_key() {
        let settable: Vec<&str> = crate::config_cli::listed()
            .filter(|k| k.settable())
            .map(|k| k.name())
            .collect();
        let listed: Vec<&str> = KEYS.iter().map(|k| k.0).collect();
        assert_eq!(listed, settable);
        let c = help("config").unwrap();
        #[cfg(feature = "speakers")]
        assert!(c.contains("  attn_limit           0 to 100 (dB)"), "{c}");
        #[cfg(feature = "speakers")]
        assert!(c.contains("  speakers.output      a name from"), "{c}");
        #[cfg(not(feature = "speakers"))]
        assert!(c.contains("  attn_limit     0 to 100 (dB)"), "{c}");
        #[cfg(not(feature = "speakers"))]
        assert!(!c.contains("speakers") && help("speakers").is_none(), "{c}");
    }
}
