//! CLI control surface: the request/response protocol spoken over the
//! control socket, the toggle state machine, and the status renderers.
//! The tray's main loop owns all state; this module is pure except for
//! the socket plumbing of the listener/client.

use crate::controller::RunMode;
pub use crate::diagnostics::EngineTier;

/// A validated control request. `SetMode(None)` = Off (the persisted
/// disable path, exactly like the tray radio).
#[derive(Clone, PartialEq, Debug)]
pub enum Request {
    Status {
        json: bool,
    },
    GetMode,
    SetMode(Option<RunMode>),
    Toggle(RunMode), // Bypass or Mute only
    /// What unmuting returns to (Suppress or Bypass): while muted, now,
    /// staying muted; otherwise kept for the next mute (UX panel round 2,
    /// A11).
    UnmuteTo(RunMode),
    /// Stop the daemon (orderly: the previous default mic is restored).
    /// Without `force` it refuses while files are cleaning or cleaned and
    /// not saved (`quit --force` quits all the same; the window's Quit
    /// comes after its own question, so it forces).
    Quit {
        force: bool,
    },
    /// `config` / `config get KEY`, plain or JSON.
    ConfigGet {
        key: Option<String>,
        json: bool,
    },
    /// `config set KEY VALUE…` — the value is the rest of the line.
    ConfigSet {
        key: String,
        value: String,
        /// Answered once the change is on disk too, so a failed write is
        /// reported ([`WRITTEN`], which the CLI sends). Without it the
        /// answer comes once the change is applied (the window: its next
        /// action, a Mute, must not wait behind a slow disk).
        written: bool,
        /// `config set --defaults`: `model`/`attn_limit` for every
        /// microphone without its own, not the one in use (F13).
        defaults: bool,
    },
    /// Keep the connection open and stream `AppState` snapshots (see
    /// `watch.rs`); answered by the listener itself, never the main loop.
    Watch,
    /// Re-read PipeWire now (the device list, who uses HushMic) instead of
    /// at the next tick.
    Refresh,
    /// A chain that should run but does not: try again now.
    Retry,
    /// Open the desktop's shortcut dialog (the tray's "Set up shortcuts…").
    SetupShortcuts,
    /// The notification-driven mic check: record a few seconds, play them
    /// back raw, then cleaned. For setups where no window can open.
    MicCheck,
    /// `miccheck state`: whether the mic check still runs (`running`), or
    /// how the last one ended (`done`, `cancelled`, `failed MESSAGE`). What
    /// `hushmic miccheck` waits on.
    MicCheckState,
    /// `notice hide APP [NAME…]`: no more direct-mic notices for APP.
    HideNotice {
        app: String,
        name: Option<String>,
    },
    /// `notice show APP`: undo a hide.
    ShowNotice {
        app: String,
    },
    /// `profile reset DEVICE`: forget a microphone's own settings.
    ResetProfile {
        device: String,
    },
    /// `speakers`: what the speakers cleaner does (its changes go through
    /// `config set speakers.*`; the window's `speakers on|off`, `speakers
    /// output …` and `speakers strength …` read as those).
    #[cfg(feature = "speakers")]
    Speakers,
    /// `speakers hold on|off`: "Hold to hear the original" (never saved).
    /// `on` starts or renews a lease the daemon ends by itself when no
    /// renewal came for 2.5 s (the window renews it every second).
    #[cfg(feature = "speakers")]
    SpeakersHold(bool),
    /// `speakers retry`: start a speakers chain that is down now.
    #[cfg(feature = "speakers")]
    SpeakersRetry,
    /// The window's own state, which the daemon keeps for it.
    Ui(UiRequest),
    /// The Files page's jobs.
    Files(FilesRequest),
}

/// `files …`: the jobs of the Files page (see `files::jobs`).
#[derive(Clone, PartialEq, Debug)]
pub enum FilesRequest {
    /// `files add "PATH"`: the path as a JSON string.
    Add(String),
    Remove(u64),
    /// `files play ID FRAME`
    Play {
        id: u64,
        at: u64,
    },
    Retry(u64),
    /// `files strength DB [ID]`: the file's strength (a dB cap) and the
    /// page's for files added next; without an id the page's only.
    Strength {
        db: f32,
        id: Option<u64>,
    },
    /// `files track ID N`: clean the file's `N`th sound track (1 the
    /// first; held as 0 the first).
    Track {
        id: u64,
        track: usize,
    },
    /// `files clean-anyway`: clean while an app uses HushMic on a busy
    /// computer, until no app uses it.
    CleanAnyway,
    /// `files clean-next ID`: this waiting file goes before the others.
    CleanNext(u64),
    /// `files save ID ["FOLDER"]`: next to the original, else into the
    /// folder (a JSON string).
    Save {
        id: u64,
        dir: Option<String>,
    },
    CancelSave(u64),
    /// `files replace ID "PATH"`: the file found again elsewhere.
    Replace {
        id: u64,
        path: String,
    },
    /// `files clean-first ID`: a damaged file's part before the damage.
    CleanFirst(u64),
    /// `files quit-stop`: the answer to the tray's Quit while files clean:
    /// stop them and quit.
    QuitStop,
    /// `files quit-keep`: keep cleaning; the question goes.
    QuitKeep,
    /// `files quit-save`: the quit question's "Save all and quit": every
    /// file it would lose is saved (once cleaned), then HushMic quits.
    QuitSave,
    /// `files quit-cancel`: Cancel on "Quits once 2 files are saved".
    QuitCancel,
    /// `files quit-ask`: Quit in the window: ask as for the tray's Quit
    /// while files clean, quit otherwise (or on a second Quit).
    QuitAsk,
    /// `files folder "PATH"`: where cleaned files go when not next to the
    /// original.
    Folder(String),
    /// `files folder none`: next to the original again (Settings › Files).
    NextToOriginal,
    /// `files open`: the window, on the Files page ("Open with").
    Open,
    /// `files clear`: Clear all (undoable for a moment).
    Clear,
    /// `files undo-clear`: the files Clear all took, back.
    UndoClear,
}

/// The JSON string that is the rest of the raw request line after `words`
/// (spaces inside it kept exactly).
fn json_tail(line: &str, words: &[&str]) -> Option<String> {
    let mut rest = line.trim_start();
    for word in words {
        rest = rest.strip_prefix(word)?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        rest = rest.trim_start();
    }
    serde_json::from_str::<String>(rest.trim_end())
        .ok()
        .filter(|p| !p.is_empty())
}

/// `files add "PATH"`: the path, decoded from the JSON string that is the
/// rest of the raw request line.
pub fn files_add_path(line: &str) -> Option<String> {
    json_tail(line, &["files", "add"])
}

/// `files save ID "FOLDER"`: the folder, read from the raw request line
/// like [`files_add_path`].
pub fn files_save_dir(line: &str) -> Option<String> {
    let id = line.split_whitespace().nth(2)?;
    json_tail(line, &["files", "save", id])
}

/// `files folder "PATH"`: the folder, likewise.
pub fn files_folder_path(line: &str) -> Option<String> {
    json_tail(line, &["files", "folder"])
}

/// `files replace ID "PATH"`: the path, likewise.
pub fn files_replace_path(line: &str) -> Option<String> {
    let id = line.split_whitespace().nth(2)?;
    json_tail(line, &["files", "replace", id])
}

fn parse_files(words: &[&str]) -> Option<FilesRequest> {
    let id = |w: &str| w.parse::<u64>().ok();
    match words {
        ["add", rest @ ..] if !rest.is_empty() => {
            files_add_path(&format!("files add {}", rest.join(" "))).map(FilesRequest::Add)
        }
        ["remove", i] => id(i).map(FilesRequest::Remove),
        ["play", i, at] => Some(FilesRequest::Play {
            id: id(i)?,
            at: id(at)?,
        }),
        ["retry", i] => id(i).map(FilesRequest::Retry),
        ["strength", db, rest @ ..] if rest.len() <= 1 => {
            let db = db
                .parse::<f32>()
                .ok()
                .filter(|d| d.is_finite() && *d >= 0.0)?;
            let id = match rest {
                [i] => Some(id(i)?),
                _ => None,
            };
            Some(FilesRequest::Strength { db, id })
        }
        ["track", i, n] => Some(FilesRequest::Track {
            id: id(i)?,
            track: n.parse::<usize>().ok().filter(|&n| n >= 1)? - 1,
        }),
        ["clean-anyway"] => Some(FilesRequest::CleanAnyway),
        ["clean-next", i] => id(i).map(FilesRequest::CleanNext),
        ["save", i] => Some(FilesRequest::Save {
            id: id(i)?,
            dir: None,
        }),
        ["save", i, rest @ ..] => {
            let line = format!("files save {i} {}", rest.join(" "));
            Some(FilesRequest::Save {
                id: id(i)?,
                dir: Some(files_save_dir(&line)?),
            })
        }
        ["cancel-save", i] => id(i).map(FilesRequest::CancelSave),
        ["replace", i, rest @ ..] if !rest.is_empty() => {
            let line = format!("files replace {i} {}", rest.join(" "));
            Some(FilesRequest::Replace {
                id: id(i)?,
                path: files_replace_path(&line)?,
            })
        }
        ["clean-first", i] => id(i).map(FilesRequest::CleanFirst),
        ["quit-stop"] => Some(FilesRequest::QuitStop),
        ["quit-keep"] => Some(FilesRequest::QuitKeep),
        ["quit-save"] => Some(FilesRequest::QuitSave),
        ["quit-cancel"] => Some(FilesRequest::QuitCancel),
        ["quit-ask"] => Some(FilesRequest::QuitAsk),
        ["open"] => Some(FilesRequest::Open),
        ["clear"] => Some(FilesRequest::Clear),
        ["undo-clear"] => Some(FilesRequest::UndoClear),
        // a path comes as a JSON string, so a bare word is not one
        ["folder", "none"] => Some(FilesRequest::NextToOriginal),
        ["folder", rest @ ..] if !rest.is_empty() => {
            files_folder_path(&format!("files folder {}", rest.join(" "))).map(FilesRequest::Folder)
        }
        _ => None,
    }
}

/// `hushmic files …` (the window's requests; scripts may use them too):
/// every form [`parse_files`] takes. On the socket PATH and FOLDER are
/// JSON strings; the command line quotes them itself.
pub const FILES_USAGE: &str = "usage: hushmic files add \"PATH\" | files remove ID | \
     files play ID FRAME | files retry ID | files strength DB [ID] | files track ID N | \
     files clean-anyway | files clean-next ID | files save ID [\"FOLDER\"] | \
     files cancel-save ID | files replace ID \"PATH\" | files clean-first ID | \
     files quit-stop | files quit-keep | files quit-save | files quit-cancel | \
     files quit-ask | files open | files clear | files undo-clear | \
     files folder \"FOLDER\"|none";

/// `ui …`: what the window asks the daemon to remember for it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum UiRequest {
    Page(crate::appstate::Page),
    /// First run at this step (1 to 3); None = done.
    FirstRun(Option<u8>),
    BeamUsed,
    /// The window's size in logical points.
    Size(u16, u16),
    /// The window gained (true) or lost (false) keyboard focus.
    Focused(bool),
    /// The problem Live's Status card shows; None = none of the
    /// notification ones.
    Shows(Option<crate::notify::Problem>),
    /// The file selected on the Files page and its playhead.
    File(Option<(u64, u64)>),
    /// The answer to the question the daemon had the window ask
    /// (`AppState::ask`): yes acts (Quit, Turn off).
    Answer(bool),
}

fn parse_ui(words: &[&str]) -> Option<UiRequest> {
    match words {
        ["page", p] => crate::appstate::Page::parse(p).map(UiRequest::Page),
        ["first-run", "done"] => Some(UiRequest::FirstRun(None)),
        ["first-run", n] => n
            .parse::<u8>()
            .ok()
            .filter(|n| (1..=3).contains(n))
            .map(|n| UiRequest::FirstRun(Some(n))),
        ["beam-used"] => Some(UiRequest::BeamUsed),
        ["size", w, h] => Some(UiRequest::Size(w.parse().ok()?, h.parse().ok()?)),
        ["focused", "on"] => Some(UiRequest::Focused(true)),
        ["focused", "off"] => Some(UiRequest::Focused(false)),
        ["shows", "none"] => Some(UiRequest::Shows(None)),
        ["answer", "yes"] => Some(UiRequest::Answer(true)),
        ["answer", "no"] => Some(UiRequest::Answer(false)),
        ["shows", w] => crate::notify::Problem::parse(w).map(|p| UiRequest::Shows(Some(p))),
        ["file", "none"] => Some(UiRequest::File(None)),
        ["file", id, at] => Some(UiRequest::File(Some((id.parse().ok()?, at.parse().ok()?)))),
        _ => None,
    }
}

/// Parse subcommand words (CLI argv tail or a socket request line) into a
/// `Request`. `Err` carries a one-line usage message (CLI exit 1).
pub fn parse_request(words: &[&str]) -> Result<Request, String> {
    // a known command's own usage, else every command's (F12)
    let all = || {
        let all = ALL_USAGE.to_string();
        #[cfg(feature = "speakers")]
        let all = all + " | speakers";
        all
    };
    let own = words
        .first()
        .and_then(|w| crate::cli_help::usage(w))
        .unwrap_or_else(all);
    const ALL_USAGE: &str = "usage: hushmic status [--json] | mode [suppress|bypass|mute|off] | \
                         toggle mute|bypass | unmute-to suppress|bypass | quit [--force] | config [get KEY|set [--defaults] KEY VALUE] [--json] | \
                         watch | refresh | retry | shortcuts setup | miccheck | notice hide APP [NAME] | \
                         notice show APP | profile reset DEVICE";
    match words {
        ["watch"] => Ok(Request::Watch),
        #[cfg(feature = "speakers")]
        ["speakers"] => Ok(Request::Speakers),
        #[cfg(feature = "speakers")]
        ["speakers", "hold", "on"] => Ok(Request::SpeakersHold(true)),
        #[cfg(feature = "speakers")]
        ["speakers", "hold", "off"] => Ok(Request::SpeakersHold(false)),
        #[cfg(feature = "speakers")]
        ["speakers", "retry"] => Ok(Request::SpeakersRetry),
        #[cfg(feature = "speakers")]
        ["speakers", word @ ("on" | "off")] => Ok(Request::ConfigSet {
            key: "speakers.enabled".into(),
            value: (if *word == "on" { "true" } else { "false" }).into(),
            written: false,
            defaults: false,
        }),
        #[cfg(feature = "speakers")]
        ["speakers", "output", name] => Ok(Request::ConfigSet {
            key: "speakers.output".into(),
            value: name.to_string(),
            written: false,
            defaults: false,
        }),
        #[cfg(feature = "speakers")]
        ["speakers", "strength", db] => Ok(Request::ConfigSet {
            key: "speakers.attn_limit".into(),
            value: db.to_string(),
            written: false,
            defaults: false,
        }),
        #[cfg(feature = "speakers")]
        ["speakers", ..] => Err(own),
        ["refresh"] => Ok(Request::Refresh),
        ["retry"] => Ok(Request::Retry),
        ["shortcuts", "setup"] => Ok(Request::SetupShortcuts),
        ["miccheck"] => Ok(Request::MicCheck),
        ["miccheck", "state"] => Ok(Request::MicCheckState),
        ["notice", "hide", app, name @ ..] => Ok(Request::HideNotice {
            app: app.to_string(),
            name: (!name.is_empty()).then(|| name.join(" ")),
        }),
        ["notice", "show", app] => Ok(Request::ShowNotice {
            app: app.to_string(),
        }),
        ["profile", "reset", device] => Ok(Request::ResetProfile {
            device: device.to_string(),
        }),
        ["ui", rest @ ..] => parse_ui(rest)
            .map(Request::Ui)
            .ok_or_else(|| format!("unknown ui request\n{}", all())),
        ["files", rest @ ..] => parse_files(rest)
            .map(Request::Files)
            .ok_or_else(|| FILES_USAGE.to_string()),
        ["status"] => Ok(Request::Status { json: false }),
        ["status", "--json"] => Ok(Request::Status { json: true }),
        ["quit"] => Ok(Request::Quit { force: false }),
        ["quit", "--force"] => Ok(Request::Quit { force: true }),
        ["config"] => Ok(Request::ConfigGet {
            key: None,
            json: false,
        }),
        ["config", "--json"] => Ok(Request::ConfigGet {
            key: None,
            json: true,
        }),
        ["config", "get", key] => Ok(Request::ConfigGet {
            key: Some(key.to_string()),
            json: false,
        }),
        ["config", "get", key, "--json"] => Ok(Request::ConfigGet {
            key: Some(key.to_string()),
            json: true,
        }),
        ["config", "set", WRITTEN, DEFAULTS, key, rest @ ..] if !rest.is_empty() => {
            Ok(Request::ConfigSet {
                key: key.to_string(),
                value: rest.join(" "),
                written: true,
                defaults: true,
            })
        }
        ["config", "set", DEFAULTS, key, rest @ ..] if !rest.is_empty() => Ok(Request::ConfigSet {
            key: key.to_string(),
            value: rest.join(" "),
            written: false,
            defaults: true,
        }),
        // a flag is never a key: `set --defaults attn_limit` (no value) is
        // a usage error, not "unknown key '--defaults'" (A106 N-C2)
        ["config", "set", WRITTEN, key, rest @ ..]
            if !rest.is_empty() && !key.starts_with("--") =>
        {
            Ok(Request::ConfigSet {
                key: key.to_string(),
                value: rest.join(" "),
                written: true,
                defaults: false,
            })
        }
        ["config", "set", key, rest @ ..] if !rest.is_empty() && !key.starts_with("--") => {
            Ok(Request::ConfigSet {
                key: key.to_string(),
                value: rest.join(" "),
                written: false,
                defaults: false,
            })
        }
        ["mode"] => Ok(Request::GetMode),
        ["mode", state] => match *state {
            "suppress" => Ok(Request::SetMode(Some(RunMode::Suppress))),
            "bypass" => Ok(Request::SetMode(Some(RunMode::Bypass))),
            "mute" => Ok(Request::SetMode(Some(RunMode::Mute))),
            "off" => Ok(Request::SetMode(None)),
            other => Err(format!("unknown mode '{other}'\n{own}")),
        },
        ["toggle", which] => match *which {
            "mute" => Ok(Request::Toggle(RunMode::Mute)),
            "bypass" => Ok(Request::Toggle(RunMode::Bypass)),
            other => Err(format!("cannot toggle '{other}' (mute|bypass)\n{own}")),
        },
        ["unmute-to", which] => match *which {
            "suppress" => Ok(Request::UnmuteTo(RunMode::Suppress)),
            "bypass" => Ok(Request::UnmuteTo(RunMode::Bypass)),
            other => Err(format!(
                "cannot unmute to '{other}' (suppress|bypass)\n{own}"
            )),
        },
        _ => Err(own),
    }
}

/// `config set KEY VALUE…`: the value exactly as typed after the key
/// (interior whitespace kept), from the raw request line. None when the
/// line is not a `config set` with a value.
/// The word after `config set` that asks for the answer once the change
/// is written (the CLI's `config set` sends it; see
/// [`Request::ConfigSet`]).
pub const WRITTEN: &str = "--written";

/// `config set --defaults KEY VALUE`: the defaults, not the microphone in
/// use (F13).
pub const DEFAULTS: &str = "--defaults";

/// A request that sets a mode outright (`mode mute`, `mode suppress` …):
/// the latest says what the user wants, and running it again changes
/// nothing. It is never dropped for being late (a push-to-talk release
/// that waited behind a busy loop must still mute): the loop runs it when
/// it gets there, in order, and the client hears that it was late.
pub fn runs_late(line: &str) -> bool {
    matches!(
        line.split_whitespace().collect::<Vec<_>>()[..],
        ["mode", "suppress" | "bypass" | "mute" | "off"]
    )
}

pub fn config_set_value(line: &str) -> Option<String> {
    let mut rest = line.trim_start();
    for word in ["config", "set"] {
        rest = rest.strip_prefix(word)?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        rest = rest.trim_start();
    }
    for flag in [WRITTEN, DEFAULTS] {
        if let Some(r) = rest
            .strip_prefix(flag)
            .filter(|r| r.starts_with(char::is_whitespace))
        {
            rest = r.trim_start();
        }
    }
    // skip the key
    let key_end = rest.find(char::is_whitespace)?;
    let value = rest[key_end..].trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// The state word for a mode selection: suppress | bypass | mute | off.
pub fn mode_word(sel: Option<RunMode>) -> &'static str {
    match sel {
        Some(RunMode::Suppress) => "suppress",
        Some(RunMode::Bypass) => "bypass",
        Some(RunMode::Mute) => "mute",
        None => "off",
    }
}

/// The toggle transition: entering `target` unless already there, in which
/// case return to the remembered previous AUDIBLE state. Never yields Off
/// (a mute hotkey must not kill the mic entirely), and leaving `target`
/// when the memory IS `target` falls to suppression — the return leg must
/// always change something.
pub fn toggle_next(
    current: Option<RunMode>,
    prev_alive: RunMode,
    target: RunMode,
) -> Option<RunMode> {
    if current == Some(target) {
        Some(if prev_alive == target {
            RunMode::Suppress
        } else {
            prev_alive
        })
    } else {
        Some(target)
    }
}

/// Update the remembered previous AUDIBLE state after a transition
/// `old -> new` (from ANY path — tray radio, CLI or hotkey — so all of
/// them round-trip). Only suppress/bypass are ever remembered: every
/// return leg (untoggle, push-to-talk hold, push-to-mute release) exists
/// to make the user AUDIBLE, so remembering Mute would turn toggle-bypass
/// into a mute/bypass loop — and Off must never be resurrected either
/// (coming out of Off remembers suppress).
pub fn update_prev_alive(prev: RunMode, old: Option<RunMode>, new: Option<RunMode>) -> RunMode {
    match (old, new) {
        (None, Some(_)) => RunMode::Suppress,
        (Some(m), Some(n)) if m != n && m != RunMode::Mute => m,
        (Some(m), None) if m != RunMode::Mute => m,
        _ => prev,
    }
}

/// [`update_prev_alive`], then a preference `unmute-to` stored while not
/// muted takes over as the mute begins (and is used up). Returns the new
/// memory and what stays stored (UX panel round 2, A11).
///
/// `hold`: the mute is push-to-mute's press. Its release returns to the
/// mode before the press whatever is stored, and what is stored waits for
/// the next mute (A115 SF-U1: the cough key with `unmute-to bypass`
/// stored let go into Bypass).
pub fn after_transition(
    prev: RunMode,
    stored: Option<RunMode>,
    old: Option<RunMode>,
    new: Option<RunMode>,
    hold: bool,
) -> (RunMode, Option<RunMode>) {
    let prev = update_prev_alive(prev, old, new);
    match stored {
        Some(s) if new == Some(RunMode::Mute) && old != new && !hold => (s, None),
        _ => (prev, stored),
    }
}

/// Everything `status` reports, snapshotted by the main loop.
#[derive(Clone, Debug)]
pub struct Status {
    pub version: String,
    pub mode: Option<RunMode>,
    pub mic_configured: Option<String>,
    pub mic_active: Option<String>,
    /// The two above by display name, for the human form (UX panel round
    /// 2, A11); None where none is known.
    pub mic_names: [Option<String>; 2],
    pub fallback_active: bool,
    /// The model and strength in effect: the active profile's (see
    /// `Config::active_profile`), not necessarily the defaults below.
    pub model: String,
    pub attn_limit: f32,
    /// The device whose saved profile is in effect (node.name, and its
    /// display name); None = the defaults apply.
    pub profile: Option<String>,
    pub profile_name: Option<String>,
    /// The defaults for devices without a profile (config `model` /
    /// `attn_limit`).
    pub defaults_model: String,
    pub defaults_attn_limit: f32,
    pub chain_running: bool,
    /// A microphone feeds the chain's capture; false once none has for
    /// [`NOT_LINKED_AFTER`] (UX panel round 2, A11); None before the
    /// first graph read.
    pub linked: Option<bool>,
    pub node_present: Option<bool>,
    /// PipeWire's scheduling state for `hushmic_source` (`running`,
    /// `idle`, `suspended`, ...); None = unknown or the chain is down. Idle
    /// means PipeWire is not running the chain, so it costs no CPU (see
    /// `pipewire::state_is_idle`); it says nothing about which apps hold a
    /// stream open (a corked one does not count).
    pub node_state: Option<String>,
    /// Whether the chain's plugin takes mode and strength from its control
    /// file (`Some(true)`), has not mapped it (yet: it does when it first
    /// starts; an older plugin never does) or None: the chain has no
    /// control file (no memory-backed directory) or does not run.
    pub control_file: Option<bool>,
    /// The tray icon: registered, waiting for a tray to appear, or
    /// disabled (--headless, tray = false). The words are `watch`'s.
    pub tray: crate::appstate::TrayHost,
    /// The engine tier the chain last reported (issue #14); None until the
    /// plugin has said anything, or when the chain is not running.
    pub engine: Option<EngineTier>,
    /// The configured model is already the light one, so a `light` tier is
    /// not a degradation.
    pub configured_light: bool,
    /// What the model on the live tier runs on (issue #18), see
    /// [`live_model`]; None before the plugin said so, in passthrough, or
    /// with the chain down.
    pub inference: Option<crate::diagnostics::Inference>,
    /// The apps recording HushMic, and the apps recording the microphone
    /// it cleans directly, as `watch` has them (UX panel A1).
    pub apps_using: Vec<crate::appstate::AppRef>,
    pub apps_direct: Vec<crate::appstate::DirectUse>,
    /// App playbacks HushMic keeps off its microphone input: they keep
    /// being linked into it (`pipewire::IntruderGuard`).
    pub apps_kept_off: Vec<String>,
    /// The speakers cleaner.
    #[cfg(feature = "speakers")]
    pub speakers: crate::speakers::Status,
}

/// The model file the live tier runs: the chain's own model, or the light
/// model it falls back to; None in passthrough.
pub fn live_model(
    tier: Option<EngineTier>,
    chain_model: &str,
    configured_light: bool,
) -> Option<String> {
    match tier {
        Some(EngineTier::Passthrough) => None,
        Some(EngineTier::Light) if !configured_light => {
            Some(crate::controller::LIGHT_MODEL.to_string())
        }
        _ => Some(chain_model.to_string()),
    }
}

/// The human wording of the engine line, e.g. `quality model (native
/// w7a8)` or `light model (fallback, onnx)`.
fn engine_words(
    tier: EngineTier,
    configured_light: bool,
    inference: Option<&crate::diagnostics::Inference>,
) -> String {
    let (base, note) = match tier {
        EngineTier::Quality => ("quality model", None),
        EngineTier::Light if configured_light => ("light model", None),
        EngineTier::Light => ("light model", Some("fallback")),
        EngineTier::Passthrough => ("passthrough", Some("fallback")),
    };
    let notes: Vec<&str> = note
        .into_iter()
        .chain(
            inference
                .filter(|_| tier != EngineTier::Passthrough)
                .map(|i| i.engine.as_str()),
        )
        .collect();
    if notes.is_empty() {
        base.to_string()
    } else {
        format!("{base} ({})", notes.join(", "))
    }
}

/// The `apps:` line's words: who records HushMic, and who records the
/// microphone directly, e.g. `Zoom, Teams (direct: Slack)` or `none`.
fn apps_words(s: &Status) -> String {
    let name = |a: &crate::appstate::AppRef| a.name.clone().unwrap_or_else(|| "an app".into());
    let using: Vec<String> = s.apps_using.iter().map(name).collect();
    let direct: Vec<String> = s.apps_direct.iter().map(|d| name(&d.app)).collect();
    let mut w = if using.is_empty() {
        "none".to_string()
    } else {
        using.join(", ")
    };
    if !direct.is_empty() {
        w.push_str(&format!(" (direct: {})", direct.join(", ")));
    }
    if !s.apps_kept_off.is_empty() {
        w.push_str(&format!(
            " (kept off HushMic's microphone: {})",
            s.apps_kept_off.join(", ")
        ));
    }
    w
}

/// The device the chain captures from, and why when it is not the one
/// chosen: `status`'s and the doctor's words, e.g. `usbmic (fallback;
/// headset is unplugged)` (UX panel round 2, A9).
pub fn mic_words(configured: Option<&str>, active: Option<&str>, fallback: bool) -> String {
    match (configured, active, fallback) {
        (Some(m), Some(a), true) => format!("{a} (fallback; {m} is unplugged)"),
        (Some(m), None, true) => format!("system default (fallback; {m} is unplugged)"),
        (Some(m), Some(a), false) if a != m => format!("{a} (chosen: {m})"),
        (Some(m), _, false) => m.to_string(),
        (None, Some(a), _) => format!("{a} (system default)"),
        (None, None, _) => "system default".to_string(),
    }
}

/// How long the chain's capture may have no microphone on its link
/// before `status` says so (a relink takes a moment).
pub const NOT_LINKED_AFTER: Duration = Duration::from_secs(5);

pub fn render_status_human(s: &Status) -> String {
    let mode = match s.mode {
        Some(RunMode::Suppress) => "noise suppression",
        Some(RunMode::Bypass) => "bypass",
        Some(RunMode::Mute) => "mute",
        None => "off",
    };
    // by display name (the node names are in --json)
    let [cname, aname] = &s.mic_names;
    let configured = cname.as_deref().or(s.mic_configured.as_deref());
    let active = if s.mic_active.is_some() && s.mic_active == s.mic_configured {
        configured
    } else {
        aname.as_deref().or(s.mic_active.as_deref())
    };
    let mic = mic_words(configured, active, s.fallback_active);
    let chain = if !s.chain_running {
        "not running"
    } else {
        match (s.node_present, s.node_state.as_deref()) {
            // no microphone reaches it: not "idle", which reads as fine
            _ if s.linked == Some(false) => {
                "running, not linked (no microphone is connected to it)"
            }
            (Some(true), Some(st)) if crate::pipewire::state_is_idle(st) => {
                "running, idle (not processing audio right now)"
            }
            (Some(true), _) => "running",
            (Some(false), _) => "running, node missing",
            (None, _) => "running (node state unknown)",
        }
    };
    let engine = match (s.chain_running, s.engine) {
        (true, Some(t)) => format!(
            "engine: {}\n",
            engine_words(t, s.configured_light, s.inference.as_ref())
        ),
        // The inference line arrives at load, the first tier line a moment
        // later: the chain starts on its own model, said in the same words.
        (true, None) => s
            .inference
            .as_ref()
            .map(|i| {
                let tier = if s.configured_light {
                    EngineTier::Light
                } else {
                    EngineTier::Quality
                };
                format!(
                    "engine: {}\n",
                    engine_words(tier, s.configured_light, Some(i))
                )
            })
            .unwrap_or_default(),
        _ => String::new(),
    };
    let out = format!(
        "hushmic {} — mode: {}\nmic: {}\nmodel: {}  strength: {} dB ({})\n{}latency: {} ms added\ntray: {}\nchain: {}\napps: {}\n",
        s.version,
        mode,
        mic,
        s.model,
        s.attn_limit,
        match &s.profile {
            Some(p) => format!("{} profile", s.profile_name.as_deref().unwrap_or(p)),
            None => "defaults".to_string(),
        },
        engine,
        crate::controller::LATENCY_SAMPLES * 1000 / 48_000,
        tray_word(s.tray),
        chain,
        apps_words(s)
    );
    #[cfg(feature = "speakers")]
    let out = format!("{out}speakers: {}\n", s.speakers.words());
    out
}

/// `watch`'s words for the tray, in `status` too.
fn tray_word(tray: crate::appstate::TrayHost) -> &'static str {
    use crate::appstate::TrayHost;
    match tray {
        TrayHost::Registered => "registered",
        TrayHost::Waiting => "waiting",
        TrayHost::Disabled => "disabled",
    }
}

pub fn render_status_json(s: &Status) -> String {
    let v = serde_json::json!({
        "running": true,
        "version": s.version,
        "mode": mode_word(s.mode),
        "enabled": s.mode.is_some(),
        "mic": {
            "configured": s.mic_configured,
            "active": s.mic_active,
            "fallback_active": s.fallback_active,
            // by display name, as the human form says them (an unplugged
            // mic keeps the name it had: the doctor asks for it)
            "configured_name": s.mic_names[0],
            "active_name": s.mic_names[1],
        },
        "model": s.model,
        "attn_limit": s.attn_limit,
        "profile": s.profile,
        "defaults": {
            "model": s.defaults_model,
            "attn_limit": s.defaults_attn_limit,
        },
        "engine": s.chain_running.then_some(s.engine).flatten().map(EngineTier::word),
        "inference": s.inference.as_ref().filter(|_| s.chain_running).map(|i| serde_json::json!({
            "model": i.model,
            "engine": i.engine,
            "reason": i.reason,
        })),
        "latency_samples": crate::controller::LATENCY_SAMPLES,
        "tray": tray_word(s.tray),
        "apps": {
            "using": s.apps_using,
            "direct": s.apps_direct,
            "kept_off": s.apps_kept_off,
        },
        "chain": {
            "running": s.chain_running,
            "linked": s.linked.filter(|_| s.chain_running),
            "node_present": s.node_present,
            "node_state": s.node_state.as_ref().filter(|_| s.chain_running),
            "idle": s.node_state.as_deref()
                .filter(|_| s.chain_running)
                .map(crate::pipewire::state_is_idle),
            "control": s.control_file
                .filter(|_| s.chain_running)
                .map(|mapped| if mapped { "file" } else { "unmapped" }),
        },
    });
    #[cfg(feature = "speakers")]
    let v = {
        let mut v = v;
        v["speakers"] = s.speakers.json();
        v
    };
    v.to_string()
}

/// What starts a line of a reply that the command line writes to stderr
/// rather than stdout: `mode off` says which apps now hear the raw
/// microphone (UX panel A1), and a script reading the mode word is not
/// thrown by it.
const NOTE: &str = "note: ";

/// `payload` with `note` on lines of its own (see [`split_notes`]).
pub fn with_note(payload: &str, note: Option<&str>) -> String {
    match note {
        Some(n) => format!("{payload}\n{NOTE}{n}"),
        None => payload.to_string(),
    }
}

/// A reply's payload and its notes, apart (see [`with_note`]).
pub fn split_notes(out: &str) -> (String, Vec<String>) {
    let mut notes = Vec::new();
    let mut rest = Vec::new();
    for line in out.lines() {
        match line.strip_prefix(NOTE) {
            Some(n) => notes.push(n.to_string()),
            None => rest.push(line),
        }
    }
    (rest.join("\n"), notes)
}

/// Wire format: `ok\n<payload>` / `err <message>\n`. The connection close
/// delimits the payload — no framing.
pub fn encode_ok(payload: &str) -> String {
    format!("ok\n{payload}")
}

pub fn encode_err(msg: &str) -> String {
    format!("err {msg}\n")
}

/// Split a raw response into (ok, payload-or-message). Anything that is
/// not a well-formed `ok` decodes as an error — a foreign peer's bytes
/// must never read as success.
pub fn decode_response(raw: &str) -> (bool, String) {
    if let Some(payload) = raw.strip_prefix("ok\n") {
        (true, payload.to_string())
    } else if let Some(msg) = raw.strip_prefix("err ") {
        (false, msg.trim_end_matches('\n').to_string())
    } else {
        (false, format!("unrecognized response: {raw}"))
    }
}

// --- socket plumbing --------------------------------------------------------

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

/// One request from a CLI client, delivered to the main loop; the reply
/// string (already `encode_ok`/`encode_err`-framed) goes back through
/// `reply` and the listener writes it to the peer.
pub struct ControlReq {
    pub line: String,
    /// The sender's pid (SO_PEERCRED; 0 when not visible from here).
    pub peer: u32,
    pub reply: Sender<String>,
    /// Fires once the listener has written the reply to the peer. Only
    /// `quit` waits on it: the loop breaks right after answering, and the
    /// process must not exit before the bytes are on the wire.
    pub written: std::sync::mpsc::Receiver<()>,
    /// Who has the request: see [`ControlReq::claim`].
    state: Arc<AtomicU8>,
    /// The connection, to see whether the client still waits.
    conn: Option<UnixStream>,
    /// See [`runs_late`]: claimed even after the listener said it was late
    /// or its client hung up.
    late_ok: bool,
}

/// Nobody has started the request.
const PENDING: u8 = 0;
/// The main loop runs it; the listener waits for the answer.
const RUNNING: u8 = 1;
/// The listener told the client it timed out; it must never run.
const ABANDONED: u8 = 2;

impl ControlReq {
    /// Take the request to run it. False when its client is no longer
    /// waiting (the listener already told it "did not answer in time", or
    /// it hung up): such a request must not change anything now. Once
    /// claimed, the listener waits for the answer (up to [`RUN_LIMIT`]).
    pub fn claim(&self) -> bool {
        if self.late_ok {
            self.state.store(RUNNING, Ordering::Release);
            return true;
        }
        if self.conn.as_ref().is_some_and(hung_up) {
            self.state.store(ABANDONED, Ordering::Release);
            return false;
        }
        self.state
            .compare_exchange(PENDING, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// The other end closed the connection (a client that gave up).
fn hung_up(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut p = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: 0,
        revents: 0,
    };
    // SAFETY: one valid pollfd, zero timeout.
    let n = unsafe { libc::poll(&mut p, 1, 0) };
    n > 0 && p.revents & (libc::POLLHUP | libc::POLLERR) != 0
}

/// Per-connection socket I/O budget. A peer that connects and stalls gets
/// dropped after this, so the (serial) listener can serve the next client.
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the listener waits for the main loop to take a request. A
/// request not taken by then is answered "did not answer in time" and never
/// runs (see [`ControlReq::claim`]).
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the listener waits for a request the loop has taken. The loop
/// bounds every PipeWire call it makes; a restart stays well inside this.
const RUN_LIMIT: Duration = Duration::from_secs(20);
/// Answers waited for on threads of their own at once. Past it a request
/// is answered "busy" at once (a loop that is stuck while a status bar
/// keeps asking must not pile up threads); a mode set outright still goes
/// to the loop ([`runs_late`]).
pub const MAX_WAITING: usize = 32;
/// The answer past [`MAX_WAITING`].
const BUSY: &str = "HushMic is busy; try again in a moment";
/// The answer to a mode set outright that the loop has not taken in time:
/// it runs once the loop gets there ([`runs_late`]).
const LATE: &str = "HushMic is busy; it applies this as soon as it can";
/// Longest request line we accept; real requests are tens of bytes.
const MAX_REQUEST: usize = 4096;

/// Serve the control socket on a background thread: one request line per
/// connection, delivered as `ControlReq` into `tx`; exits when the main
/// loop's receiver is gone. No `watch` (see [`spawn_listener_with`]).
pub fn spawn_listener(listener: UnixListener, tx: Sender<ControlReq>) {
    spawn_listener_with(listener, tx, None)
}

/// [`spawn_listener`] with its own wait for the loop to take a request
/// (tests).
pub fn spawn_listener_timed(
    listener: UnixListener,
    tx: Sender<ControlReq>,
    reply_timeout: Duration,
) {
    serve(listener, tx, None, reply_timeout)
}

/// [`spawn_listener`], plus `watch`: such a connection is handed to `hub`
/// (which keeps it open and streams snapshots) instead of the main loop.
pub fn spawn_listener_with(
    listener: UnixListener,
    tx: Sender<ControlReq>,
    hub: Option<crate::watch::Hub>,
) {
    serve(listener, tx, hub, REPLY_TIMEOUT)
}

fn serve(
    listener: UnixListener,
    tx: Sender<ControlReq>,
    hub: Option<crate::watch::Hub>,
    reply_timeout: Duration,
) {
    std::thread::spawn(move || {
        let waiting = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for conn in listener.incoming() {
            let Ok(mut stream) = conn else { continue };
            // Only our own user's processes (in the /tmp fallback the
            // socket is reachable by anyone the umask lets in).
            let Some(peer) = peer_of_ours(&stream) else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
            let mut raw = Vec::new();
            let mut buf = [0u8; 512];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break, // EOF, timeout, or error
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if raw.contains(&b'\n') || raw.len() > MAX_REQUEST {
                            break;
                        }
                    }
                }
            }
            let text = String::from_utf8_lossy(&raw);
            let line = text.lines().next().unwrap_or("").trim().to_string();
            if line.is_empty() {
                continue; // stalled or empty peer: drop it, serve the next
            }
            if line.split_whitespace().eq(["watch"]) {
                match &hub {
                    Some(hub) => hub.subscribe(stream),
                    None => {
                        let _ = stream.write_all(encode_err("watch is not available").as_bytes());
                    }
                }
                continue;
            }
            let late_ok = runs_late(&line);
            let busy = waiting.load(Ordering::Acquire) >= MAX_WAITING;
            if busy && !late_ok {
                let _ = stream.write_all(encode_err(BUSY).as_bytes());
                continue;
            }
            let (rtx, rrx) = std::sync::mpsc::channel();
            let (wtx, wrx) = std::sync::mpsc::channel();
            let state = Arc::new(AtomicU8::new(PENDING));
            if tx
                .send(ControlReq {
                    line,
                    peer,
                    reply: rtx,
                    written: wrx,
                    state: state.clone(),
                    conn: stream.try_clone().ok(),
                    late_ok,
                })
                .is_err()
            {
                return; // main loop gone — tray shutting down
            }
            if busy {
                // the loop runs it when it gets there
                let _ = stream.write_all(encode_err(LATE).as_bytes());
                continue;
            }
            // Each answer is waited for on its own thread, so the next
            // request is read at once: an answer that comes late (a
            // `config set` answered after its write, which a busy disk can
            // hold for seconds) never holds up a mute or a push-to-talk
            // release behind it. Reading and enqueueing stay here, on
            // this one thread, in accept order: a release never reaches
            // the loop before its press. Only the wait moves.
            let mut answer = move || {
                let late = || encode_err("HushMic did not answer in time");
                let reply = match rrx.recv_timeout(reply_timeout) {
                    Ok(r) => r,
                    // A mode set outright is never dropped: not taken yet,
                    // the loop runs it when it gets there, and the client
                    // hears so; taken, its answer is worth the wait.
                    Err(_) if late_ok => {
                        if state.load(Ordering::Acquire) == PENDING {
                            encode_err(LATE)
                        } else {
                            rrx.recv_timeout(RUN_LIMIT).unwrap_or_else(|_| late())
                        }
                    }
                    // Not taken yet: it never will be. Taken: the loop is
                    // on it, and its answer is worth the wait.
                    Err(_) => match state.compare_exchange(
                        PENDING,
                        ABANDONED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => late(),
                        Err(_) => rrx.recv_timeout(RUN_LIMIT).unwrap_or_else(|_| late()),
                    },
                };
                let _ = stream.write_all(reply.as_bytes());
                let _ = stream.flush();
                let _ = wtx.send(());
            };
            waiting.fetch_add(1, Ordering::AcqRel);
            let done = waiting.clone();
            if let Err(e) = std::thread::Builder::new()
                .name("hushmic-answer".into())
                .stack_size(128 * 1024)
                .spawn(move || {
                    answer();
                    done.fetch_sub(1, Ordering::AcqRel);
                })
            {
                // no thread to be had: the peer sees the connection close
                waiting.fetch_sub(1, Ordering::AcqRel);
                eprintln!("[hushmic] control: could not wait for an answer: {e}");
            }
        }
    });
}

/// The pid on the other end, if it runs as our user (SO_PEERCRED).
fn peer_of_ours(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (rc == 0 && cred.uid == unsafe { libc::getuid() }).then_some(cred.pid as u32)
}

/// `client_run`'s output when the tray took the request but never
/// answered (exit 1, like a refused request — this tells them apart).
pub const NO_REPLY: &str = "no reply from HushMic";

/// Run one CLI command against the socket at `path`. Returns
/// `(exit code, output)`; the caller prints the output (stdout on 0,
/// stderr otherwise) and exits with the code. Usage errors exit 1 without
/// touching the socket; connect failure is exit 2 ("tray not running").
/// What a client says when no instance answers (UX panel round 2, A11:
/// the unit's start too).
pub const NOT_RUNNING: &str = "hushmic is not running (start it with hushmic --tray or hushmic \
     --headless, or with systemctl --user start hushmic when its service is installed)";

pub fn client_run_at(path: &Path, words: &[String]) -> (i32, String) {
    let refs: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
    if let Err(usage) = parse_request(&refs) {
        return (1, usage);
    }
    let mut stream = match UnixStream::connect(path) {
        Ok(s) => s,
        Err(_) => return (2, NOT_RUNNING.to_string()),
    };
    // The listener answers within REPLY_TIMEOUT, or RUN_LIMIT more once
    // the loop has taken the request, counted from when it read the line;
    // the margin covers the read (a request taken at the last moment must
    // not be reported as unanswered after it ran).
    let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT + RUN_LIMIT + IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let line = format!("{}\n", refs.join(" "));
    if stream.write_all(line.as_bytes()).is_err() {
        return (2, NOT_RUNNING.to_string());
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut raw = String::new();
    if stream.read_to_string(&mut raw).is_err() || raw.is_empty() {
        return (1, NO_REPLY.to_string());
    }
    let (ok, payload) = decode_response(&raw);
    (if ok { 0 } else { 1 }, payload)
}

/// The socket at `path` is ours to talk to: our uid, a real socket (no
/// symlink). The show socket's check (in the /tmp fallback the predictable
/// path could be squatted by another local user).
pub fn socket_is_ours(path: &Path) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    matches!(std::fs::symlink_metadata(path),
        Ok(md) if md.uid() == unsafe { libc::getuid() } && md.file_type().is_socket())
}

/// `client_run_at` on the default socket path, with the same
/// ownership check the show socket does before connecting.
/// The command line's words as sent: its `config set` waits for the
/// write, to report one that fails (the window's doesn't: see
/// [`Request::ConfigSet`]).
pub fn cli_words(words: &[String]) -> Vec<String> {
    match words {
        // a key and a value: anything less stays the usage error it is
        [c, s, rest @ ..]
            if c == "config" && s == "set" && rest.len() >= 2 && rest[0] != WRITTEN =>
        {
            [&words[..2], &[WRITTEN.to_string()], rest].concat()
        }
        _ => words.to_vec(),
    }
}

/// [`client_run_at`] for the command line: its `config set` asks for the
/// answer once written ([`cli_words`]). A daemon from before that flag
/// (still running after an upgrade) refuses it as an unknown key: asked
/// again without it, as it would have been then.
pub fn cli_run_at(path: &Path, words: &[String]) -> (i32, String) {
    let marked = cli_words(words);
    let (code, out) = client_run_at(path, &marked);
    if marked.len() != words.len() && code == 1 && out.contains(&format!("unknown key '{WRITTEN}'"))
    {
        return client_run_at(path, words);
    }
    (code, out)
}

/// A read-only request to the running instance on the default socket,
/// answered within `limit` or not at all (the doctor's: a stuck instance
/// is what it may be run to look at). None: nothing answered in time, or
/// it refused.
pub fn ask_within(words: &[&str], limit: Duration) -> Option<String> {
    let path = crate::lock::default_control_socket_path();
    if !socket_is_ours(&path) {
        return None;
    }
    let mut stream = UnixStream::connect(&path).ok()?;
    stream.set_read_timeout(Some(limit)).ok()?;
    stream.set_write_timeout(Some(limit)).ok()?;
    stream
        .write_all(format!("{}\n", words.join(" ")).as_bytes())
        .ok()?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    match decode_response(&raw) {
        (true, payload) => Some(payload),
        _ => None,
    }
}

pub fn client_run(words: &[String]) -> (i32, String) {
    let path = crate::lock::default_control_socket_path();
    if !socket_is_ours(&path) {
        // still validate usage locally so `hushmic frobnicate` is exit 1
        let refs: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
        if let Err(usage) = parse_request(&refs) {
            return (1, usage);
        }
        return (2, NOT_RUNNING.to_string());
    }
    cli_run_at(&path, words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_goes_apart_from_the_answer() {
        let out = with_note("off", Some("Zoom now hears Jabra directly."));
        assert_eq!(
            split_notes(&out),
            (
                "off".to_string(),
                vec!["Zoom now hears Jabra directly.".to_string()]
            )
        );
        assert_eq!(with_note("off", None), "off");
        assert_eq!(split_notes("off\n"), ("off".to_string(), vec![]));
    }

    #[test]
    fn parse_request_accepts_the_documented_surface() {
        assert_eq!(
            parse_request(&["status"]),
            Ok(Request::Status { json: false })
        );
        assert_eq!(
            parse_request(&["status", "--json"]),
            Ok(Request::Status { json: true })
        );
        assert_eq!(parse_request(&["mode"]), Ok(Request::GetMode));
        assert_eq!(
            parse_request(&["mode", "suppress"]),
            Ok(Request::SetMode(Some(RunMode::Suppress)))
        );
        assert_eq!(
            parse_request(&["mode", "bypass"]),
            Ok(Request::SetMode(Some(RunMode::Bypass)))
        );
        assert_eq!(
            parse_request(&["mode", "mute"]),
            Ok(Request::SetMode(Some(RunMode::Mute)))
        );
        assert_eq!(parse_request(&["mode", "off"]), Ok(Request::SetMode(None)));
        assert_eq!(
            parse_request(&["toggle", "mute"]),
            Ok(Request::Toggle(RunMode::Mute))
        );
        assert_eq!(
            parse_request(&["toggle", "bypass"]),
            Ok(Request::Toggle(RunMode::Bypass))
        );
        assert_eq!(
            parse_request(&["unmute-to", "suppress"]),
            Ok(Request::UnmuteTo(RunMode::Suppress))
        );
        assert_eq!(
            parse_request(&["unmute-to", "bypass"]),
            Ok(Request::UnmuteTo(RunMode::Bypass))
        );
        for bad in [
            &["unmute-to", "mute"][..],
            &["unmute-to", "off"],
            &["unmute-to"],
        ] {
            assert!(parse_request(bad).is_err(), "{bad:?}");
        }
        assert_eq!(parse_request(&["quit"]), Ok(Request::Quit { force: false }));
        assert_eq!(
            parse_request(&["quit", "--force"]),
            Ok(Request::Quit { force: true })
        );
    }

    #[test]
    fn window_requests_parse() {
        use crate::appstate::Page;
        assert_eq!(parse_request(&["watch"]), Ok(Request::Watch));
        assert_eq!(parse_request(&["miccheck"]), Ok(Request::MicCheck));
        assert_eq!(
            parse_request(&["miccheck", "state"]),
            Ok(Request::MicCheckState)
        );
        assert_eq!(parse_request(&["refresh"]), Ok(Request::Refresh));
        assert_eq!(parse_request(&["retry"]), Ok(Request::Retry));
        assert_eq!(
            parse_request(&["shortcuts", "setup"]),
            Ok(Request::SetupShortcuts)
        );
        assert_eq!(
            parse_request(&["notice", "hide", "us.zoom.Zoom", "Zoom", "Workplace"]),
            Ok(Request::HideNotice {
                app: "us.zoom.Zoom".into(),
                name: Some("Zoom Workplace".into())
            })
        );
        assert_eq!(
            parse_request(&["notice", "hide", "chromium"]),
            Ok(Request::HideNotice {
                app: "chromium".into(),
                name: None
            })
        );
        assert_eq!(
            parse_request(&["notice", "show", "obs"]),
            Ok(Request::ShowNotice { app: "obs".into() })
        );
        assert_eq!(
            parse_request(&["profile", "reset", "alsa_input.x"]),
            Ok(Request::ResetProfile {
                device: "alsa_input.x".into()
            })
        );
        assert_eq!(
            parse_request(&["ui", "page", "files"]),
            Ok(Request::Ui(UiRequest::Page(Page::Files)))
        );
        #[cfg(feature = "speakers")]
        {
            // the Speakers page: its settings are config sets, answered once
            // applied (the window's next action must not wait for the disk)
            assert_eq!(parse_request(&["speakers"]), Ok(Request::Speakers));
            let set = |key: &str, value: &str| {
                Ok(Request::ConfigSet {
                    key: key.into(),
                    value: value.into(),
                    written: false,
                    defaults: false,
                })
            };
            assert_eq!(
                parse_request(&["speakers", "on"]),
                set("speakers.enabled", "true")
            );
            assert_eq!(
                parse_request(&["speakers", "off"]),
                set("speakers.enabled", "false")
            );
            assert_eq!(
                parse_request(&["speakers", "output", "default"]),
                set("speakers.output", "default")
            );
            assert_eq!(
                parse_request(&["speakers", "output", "alsa_output.usb-dac"]),
                set("speakers.output", "alsa_output.usb-dac")
            );
            assert_eq!(
                parse_request(&["speakers", "strength", "12"]),
                set("speakers.attn_limit", "12")
            );
            assert_eq!(
                parse_request(&["speakers", "hold", "on"]),
                Ok(Request::SpeakersHold(true))
            );
            assert_eq!(
                parse_request(&["speakers", "hold", "off"]),
                Ok(Request::SpeakersHold(false))
            );
            assert_eq!(
                parse_request(&["speakers", "retry"]),
                Ok(Request::SpeakersRetry)
            );
            for junk in [
                &["speakers", "hold"][..],
                &["speakers", "hold", "yes"],
                &["speakers", "output"],
                &["speakers", "loud"],
            ] {
                assert!(parse_request(junk).is_err(), "{junk:?}");
            }
        }
        #[cfg(not(feature = "speakers"))]
        assert!(parse_request(&["speakers"]).is_err());
        // Mic check is part of Live
        assert_eq!(
            parse_request(&["ui", "page", "mic-check"]),
            Ok(Request::Ui(UiRequest::Page(Page::Live)))
        );
        assert_eq!(
            parse_request(&["ui", "first-run", "2"]),
            Ok(Request::Ui(UiRequest::FirstRun(Some(2))))
        );
        assert_eq!(
            parse_request(&["ui", "first-run", "done"]),
            Ok(Request::Ui(UiRequest::FirstRun(None)))
        );
        assert_eq!(
            parse_request(&["ui", "beam-used"]),
            Ok(Request::Ui(UiRequest::BeamUsed))
        );
        assert_eq!(
            parse_request(&["ui", "size", "1024", "700"]),
            Ok(Request::Ui(UiRequest::Size(1024, 700)))
        );
        assert_eq!(
            parse_request(&["ui", "focused", "off"]),
            Ok(Request::Ui(UiRequest::Focused(false)))
        );
        assert_eq!(
            parse_request(&["ui", "focused", "on"]),
            Ok(Request::Ui(UiRequest::Focused(true)))
        );
        assert_eq!(
            parse_request(&["ui", "shows", "rerouted"]),
            Ok(Request::Ui(UiRequest::Shows(Some(
                crate::notify::Problem::Rerouted
            ))))
        );
        assert_eq!(
            parse_request(&["ui", "shows", "none"]),
            Ok(Request::Ui(UiRequest::Shows(None)))
        );
        assert_eq!(
            parse_request(&["ui", "answer", "yes"]),
            Ok(Request::Ui(UiRequest::Answer(true)))
        );
        assert_eq!(
            parse_request(&["ui", "answer", "no"]),
            Ok(Request::Ui(UiRequest::Answer(false)))
        );
        assert_eq!(
            parse_request(&["ui", "file", "3", "48000"]),
            Ok(Request::Ui(UiRequest::File(Some((3, 48_000)))))
        );
        assert_eq!(
            parse_request(&["ui", "file", "none"]),
            Ok(Request::Ui(UiRequest::File(None)))
        );
        assert_eq!(
            parse_request(&["files", "add", "\"/a", "b.wav\""]),
            Ok(Request::Files(FilesRequest::Add("/a b.wav".into())))
        );
        assert_eq!(
            files_add_path("files add \"/a  b.wav\""),
            Some("/a  b.wav".into()),
            "the raw line keeps the spaces"
        );
        assert_eq!(
            parse_request(&["files", "remove", "3"]),
            Ok(Request::Files(FilesRequest::Remove(3)))
        );
        assert_eq!(
            parse_request(&["files", "play", "3", "96000"]),
            Ok(Request::Files(FilesRequest::Play { id: 3, at: 96_000 }))
        );
        assert_eq!(
            parse_request(&["files", "retry", "3"]),
            Ok(Request::Files(FilesRequest::Retry(3)))
        );
        assert_eq!(
            parse_request(&["files", "strength", "12.5"]),
            Ok(Request::Files(FilesRequest::Strength {
                db: 12.5,
                id: None
            }))
        );
        assert_eq!(
            parse_request(&["files", "strength", "6", "3"]),
            Ok(Request::Files(FilesRequest::Strength {
                db: 6.0,
                id: Some(3)
            }))
        );
        assert_eq!(
            parse_request(&["files", "track", "3", "2"]),
            Ok(Request::Files(FilesRequest::Track { id: 3, track: 1 }))
        );
        assert!(parse_request(&["files", "track", "3", "0"]).is_err());
        assert_eq!(
            parse_request(&["files", "clean-anyway"]),
            Ok(Request::Files(FilesRequest::CleanAnyway))
        );
        assert_eq!(
            parse_request(&["files", "clean-next", "3"]),
            Ok(Request::Files(FilesRequest::CleanNext(3)))
        );
        assert_eq!(
            parse_request(&["files", "save", "3"]),
            Ok(Request::Files(FilesRequest::Save { id: 3, dir: None }))
        );
        assert_eq!(
            parse_request(&["files", "save", "3", "\"/x\""]),
            Ok(Request::Files(FilesRequest::Save {
                id: 3,
                dir: Some("/x".into())
            }))
        );
        assert_eq!(
            files_save_dir("files save 3 \"/my  music\""),
            Some("/my  music".into())
        );
        assert_eq!(
            parse_request(&["files", "cancel-save", "3"]),
            Ok(Request::Files(FilesRequest::CancelSave(3)))
        );
        assert_eq!(
            parse_request(&["files", "replace", "3", "\"/a.wav\""]),
            Ok(Request::Files(FilesRequest::Replace {
                id: 3,
                path: "/a.wav".into()
            }))
        );
        assert_eq!(
            files_replace_path("files replace 3 \"/a  b.wav\""),
            Some("/a  b.wav".into())
        );
        assert_eq!(
            parse_request(&["files", "clean-first", "3"]),
            Ok(Request::Files(FilesRequest::CleanFirst(3)))
        );
        assert_eq!(
            parse_request(&["files", "quit-stop"]),
            Ok(Request::Files(FilesRequest::QuitStop))
        );
        assert_eq!(
            parse_request(&["files", "quit-keep"]),
            Ok(Request::Files(FilesRequest::QuitKeep))
        );
        assert_eq!(
            parse_request(&["files", "quit-save"]),
            Ok(Request::Files(FilesRequest::QuitSave))
        );
        assert_eq!(
            parse_request(&["files", "quit-ask"]),
            Ok(Request::Files(FilesRequest::QuitAsk))
        );
        assert_eq!(
            parse_request(&["files", "open"]),
            Ok(Request::Files(FilesRequest::Open))
        );
        assert_eq!(
            parse_request(&["files", "clear"]),
            Ok(Request::Files(FilesRequest::Clear))
        );
        assert_eq!(
            parse_request(&["files", "undo-clear"]),
            Ok(Request::Files(FilesRequest::UndoClear))
        );
        assert_eq!(
            parse_request(&["files", "folder", "\"/a\""]),
            Ok(Request::Files(FilesRequest::Folder("/a".into())))
        );
        assert_eq!(
            parse_request(&["files", "folder", "none"]),
            Ok(Request::Files(FilesRequest::NextToOriginal))
        );
        assert_eq!(
            parse_request(&["files", "folder", "\"none\""]),
            Ok(Request::Files(FilesRequest::Folder("none".into())))
        );
        assert_eq!(
            files_folder_path("files folder \"/a  b\""),
            Some("/a  b".into())
        );
        for bad in [
            &["files", "strength", "-1"][..],
            &["files", "strength", "NaN"][..],
            &["files", "save", "3", "/x"][..],
            &["files", "save", "x"][..],
            &["files", "replace", "3"][..],
            &["files", "replace", "3", "/a.wav"][..],
        ] {
            assert!(parse_request(bad).is_err(), "{bad:?}");
        }
        for bad in [
            &["watch", "now"][..],
            &["shortcuts"][..],
            &["notice", "hide"][..],
            &["notice", "show"][..],
            &["profile", "reset"][..],
            &["ui"][..],
            &["ui", "page", "nowhere"][..],
            &["ui", "first-run", "4"][..],
            &["ui", "first-run", "0"][..],
            // no take is handed over (nor any file named)
            &["ui", "take", "none"][..],
            &["ui", "take", "../../etc/passwd"][..],
            &["ui", "size", "big", "700"][..],
            &["ui", "size", "-1", "700"][..],
            &["ui", "focused", "maybe"][..],
            // only the Status card's problems
            &["ui", "shows", "default-released"][..],
            &["ui", "shows"][..],
            &["ui", "file", "3"][..],
            &["files"][..],
            &["files", "add"][..],
            // not a JSON string, or an empty one
            &["files", "add", "/a.wav"][..],
            &["files", "add", "\"\""][..],
            &["files", "remove", "x"][..],
            &["files", "play", "3"][..],
            &["files", "play", "3", "-1"][..],
        ] {
            assert!(parse_request(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn our_own_processes_may_talk_to_the_socket() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(peer_of_ours(&a), Some(std::process::id()));
    }

    #[test]
    fn config_set_value_keeps_the_value_as_typed() {
        assert_eq!(
            config_set_value("config set mic My  Mic \n"),
            Some("My  Mic".into())
        );
        assert_eq!(
            config_set_value("  config\tset attn_limit 24"),
            Some("24".into())
        );
        assert_eq!(config_set_value("config set mic"), None);
        assert_eq!(config_set_value("config set mic   "), None);
        assert_eq!(config_set_value("configset mic x"), None);
        assert_eq!(config_set_value("mode mute"), None);
        assert_eq!(
            config_set_value("config set --written mic My  Mic").as_deref(),
            Some("My  Mic")
        );
        // only a mode set outright runs late
        for l in ["mode mute", "mode suppress", " mode  bypass ", "mode off"] {
            assert!(runs_late(l), "{l}");
        }
        for l in [
            "mode",
            "toggle mute",
            "mode loud",
            "config set mic x",
            "mode mute now",
        ] {
            assert!(!runs_late(l), "{l}");
        }
    }

    #[test]
    fn config_words_parse_with_rest_of_line_values() {
        assert_eq!(
            parse_request(&["config"]),
            Ok(Request::ConfigGet {
                key: None,
                json: false
            })
        );
        assert_eq!(
            parse_request(&["config", "--json"]),
            Ok(Request::ConfigGet {
                key: None,
                json: true
            })
        );
        assert_eq!(
            parse_request(&["config", "get", "mic"]),
            Ok(Request::ConfigGet {
                key: Some("mic".into()),
                json: false
            })
        );
        assert_eq!(
            parse_request(&["config", "get", "mic", "--json"]),
            Ok(Request::ConfigGet {
                key: Some("mic".into()),
                json: true
            })
        );
        assert_eq!(
            parse_request(&["config", "set", "mic", "My", "Mic"]),
            Ok(Request::ConfigSet {
                key: "mic".into(),
                value: "My Mic".into(),
                written: false,
                defaults: false,
            })
        );
        // the command line's: answered once written
        assert_eq!(
            parse_request(&["config", "set", WRITTEN, "mic", "My", "Mic"]),
            Ok(Request::ConfigSet {
                key: "mic".into(),
                value: "My Mic".into(),
                written: true,
                defaults: false,
            })
        );
        // A106 N-H1: the files usage names every form parse_files takes
        for w in [
            "add",
            "remove",
            "play",
            "retry",
            "strength",
            "track",
            "clean-anyway",
            "clean-next",
            "save",
            "cancel-save",
            "replace",
            "clean-first",
            "quit-stop",
            "quit-keep",
            "quit-save",
            "quit-cancel",
            "quit-ask",
            "open",
            "clear",
            "undo-clear",
            "folder",
        ] {
            assert!(FILES_USAGE.contains(&format!("files {w} ")), "{w}");
        }
        assert_eq!(
            parse_request(&["files", "bogus"]),
            Err(FILES_USAGE.to_string())
        );
        // F12: a known command's bad word gets its own usage only
        assert_eq!(
            parse_request(&["status", "--bogus"]),
            Err("usage: hushmic status [--json]".to_string())
        );
        assert_eq!(
            parse_request(&["mode", "loud"]),
            Err("unknown mode 'loud'\nusage: hushmic mode [suppress|bypass|mute|off]".to_string())
        );
        // F13: the defaults, not the microphone in use
        assert_eq!(
            parse_request(&["config", "set", DEFAULTS, "attn_limit", "12"]),
            Ok(Request::ConfigSet {
                key: "attn_limit".into(),
                value: "12".into(),
                written: false,
                defaults: true,
            })
        );
        // A106 N-C2: no value after --defaults: config's own usage
        let e = parse_request(&["config", "set", DEFAULTS, "attn_limit"]).unwrap_err();
        assert!(e.starts_with("usage: hushmic config [--json]"), "{e}");
        assert!(e.contains("config set [--defaults] KEY VALUE"), "{e}");
        assert_eq!(
            parse_request(&["config", "set", WRITTEN, DEFAULTS, "attn_limit", "12"]),
            Ok(Request::ConfigSet {
                key: "attn_limit".into(),
                value: "12".into(),
                written: true,
                defaults: true,
            })
        );
        assert_eq!(
            config_set_value("config set --written --defaults model  light ").as_deref(),
            Some("light")
        );
        let w = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            cli_words(&w(&["config", "set", "attn_limit", "24"])),
            w(&["config", "set", WRITTEN, "attn_limit", "24"])
        );
        assert_eq!(cli_words(&w(&["mode", "mute"])), w(&["mode", "mute"]));
        assert_eq!(
            cli_words(&w(&["config", "set", "tray"])),
            w(&["config", "set", "tray"])
        );
        assert_eq!(
            cli_words(&w(&["config", "get", "mic"])),
            w(&["config", "get", "mic"])
        );
        assert_eq!(
            cli_words(&w(&["config", "set", WRITTEN, "mic", "x"])),
            w(&["config", "set", WRITTEN, "mic", "x"])
        );
        // client-only words never parse as socket requests
        for bad in [
            &["config", "set"][..],
            &["config", "set", "mic"][..],
            &["config", "get"][..],
            &["config", "frob"][..],
            &["config", "path"][..],
            &["devices"][..],
            &["service", "install"][..],
            &["quit", "now"][..],
        ] {
            assert!(parse_request(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parse_request_rejects_junk_with_usage_messages() {
        for bad in [
            &["mode", "loud"][..],
            &["toggle", "suppress"][..], // only mute|bypass toggle
            &["toggle", "off"][..],
            &["toggle"][..],
            &["frobnicate"][..],
            &[][..],
            &["status", "--yaml"][..],
            &["mode", "mute", "extra"][..],
        ] {
            let e = parse_request(bad).unwrap_err();
            assert!(!e.is_empty(), "{bad:?} must carry a usage message");
        }
    }

    #[test]
    fn mode_words_round_trip_with_the_parser() {
        for sel in [
            Some(RunMode::Suppress),
            Some(RunMode::Bypass),
            Some(RunMode::Mute),
            None,
        ] {
            let w = mode_word(sel);
            assert_eq!(parse_request(&["mode", w]), Ok(Request::SetMode(sel)));
        }
    }

    #[test]
    fn toggle_enters_and_leaves_as_an_overlay() {
        use RunMode::*;
        // enter mute from suppress
        assert_eq!(toggle_next(Some(Suppress), Suppress, Mute), Some(Mute));
        // leave mute back to where we came from — INCLUDING bypass
        assert_eq!(toggle_next(Some(Mute), Bypass, Mute), Some(Bypass));
        assert_eq!(toggle_next(Some(Mute), Suppress, Mute), Some(Suppress));
        // from Off a toggle ENABLES into the target (born-muted spawn)
        assert_eq!(toggle_next(None, Suppress, Mute), Some(Mute));
        assert_eq!(toggle_next(None, Suppress, Bypass), Some(Bypass));
        // toggling bypass while muted switches to bypass (not a no-op)
        assert_eq!(toggle_next(Some(Mute), Suppress, Bypass), Some(Bypass));
    }

    #[test]
    fn prev_alive_tracks_transitions_and_never_remembers_off() {
        use RunMode::*;
        // ordinary chain-alive change remembers the old state
        assert_eq!(
            update_prev_alive(Suppress, Some(Suppress), Some(Mute)),
            Suppress
        );
        assert_eq!(
            update_prev_alive(Suppress, Some(Bypass), Some(Mute)),
            Bypass
        );
        // no-op transition keeps the memory
        assert_eq!(update_prev_alive(Bypass, Some(Mute), Some(Mute)), Bypass);
        // coming out of Off resets to suppress (never resurrect Off)
        assert_eq!(update_prev_alive(Bypass, None, Some(Mute)), Suppress);
        // going Off keeps the last live state (harmless, next enable resets)
        assert_eq!(update_prev_alive(Suppress, Some(Bypass), None), Bypass);
        // Mute is never a return address: it is the state toggles LEAVE,
        // and every return leg (untoggle, PTT hold, PtM release) exists to
        // make the user audible again.
        assert_eq!(
            update_prev_alive(Suppress, Some(Mute), Some(Bypass)),
            Suppress
        );
        assert_eq!(update_prev_alive(Bypass, Some(Mute), None), Bypass);
    }

    #[test]
    fn toggle_bypass_alternates_with_suppression_never_mute() {
        use RunMode::*;
        // Muted, then toggle-bypass repeatedly: the result must
        // alternate bypass <-> suppression — never fall back into mute.
        let (mut sel, mut prev) = (Some(Suppress), Suppress);
        for (target, want) in [
            (Mute, Mute),       // mute first
            (Bypass, Bypass),   // toggle-bypass: audible in bypass
            (Bypass, Suppress), // again: the OTHER processing mode, not mute
            (Bypass, Bypass),   // and back
        ] {
            let next = toggle_next(sel, prev, target);
            assert_eq!(next, Some(want), "toggle {target:?} from {sel:?}");
            prev = update_prev_alive(prev, sel, next);
            sel = next;
        }
        // Degenerate corner: bypass -> mute -> bypass leaves prev == Bypass;
        // toggling bypass out of bypass must not be a no-op.
        assert_eq!(toggle_next(Some(Bypass), Bypass, Bypass), Some(Suppress));
    }

    /// UX panel round 2, A11: `unmute-to` while not muted is kept, and the
    /// next mute returns to it, once.
    #[test]
    fn unmute_to_while_not_muted_waits_for_the_next_mute() {
        use RunMode::*;
        let s = Some(Suppress);
        // stored while suppressing: entering Mute takes it
        assert_eq!(
            after_transition(Suppress, Some(Bypass), s, Some(Mute), false),
            (Bypass, None)
        );
        // other moves keep it waiting
        assert_eq!(
            after_transition(Suppress, Some(Bypass), s, Some(Bypass), false),
            (Suppress, Some(Bypass))
        );
        assert_eq!(
            after_transition(Suppress, Some(Bypass), s, None, false),
            (Suppress, Some(Bypass))
        );
        // nothing stored: as before
        assert_eq!(
            after_transition(Bypass, None, s, Some(Mute), false),
            (Suppress, None)
        );
        // already muted is no new mute
        assert_eq!(
            after_transition(Suppress, Some(Bypass), Some(Mute), Some(Mute), false),
            (Suppress, Some(Bypass))
        );
    }

    /// A115 SF-U1: `unmute-to` is for toggling and unmuting. Push to
    /// mute's release always returns to the mode before the press, and
    /// what is stored waits for the next mute.
    #[test]
    fn the_cough_key_lets_go_to_the_mode_before_its_press() {
        use crate::shortcuts::{shortcut_transition, Action, Holds};
        use RunMode::*;
        /// The main loop's state: the selection, its memory, what is
        /// stored, the keys.
        struct Loop(Option<RunMode>, RunMode, Option<RunMode>, Holds);
        fn step(l: &mut Loop, a: Action, down: bool) {
            if let Some(next) = shortcut_transition(a, down, l.0, l.1, &mut l.3) {
                let hold = crate::shortcuts::holds_mute(a, down);
                (l.1, l.2) = after_transition(l.1, l.2, l.0, next, hold);
                l.0 = next;
            }
        }
        let mut l = Loop(Some(Suppress), Suppress, Some(Bypass), Holds::default());
        step(&mut l, Action::PushToMute, true);
        assert_eq!(l.0, Some(Mute));
        step(&mut l, Action::PushToMute, false);
        assert_eq!(l.0, Some(Suppress), "back to the mode before the press");
        assert_eq!(l.2, Some(Bypass), "kept for the next mute");
        // the next toggle-mute takes it, and unmuting goes there
        step(&mut l, Action::ToggleMute, true);
        step(&mut l, Action::ToggleMute, false);
        assert_eq!((l.0, l.2), (Some(Mute), None));
        step(&mut l, Action::ToggleMute, true);
        assert_eq!(l.0, Some(Bypass));
    }

    #[test]
    fn full_overlay_story_bypass_mute_bypass() {
        use RunMode::*;
        // user in bypass; toggle mute; toggle mute again -> back in bypass
        let (mut sel, mut prev) = (Some(Bypass), Suppress);
        let next = toggle_next(sel, prev, Mute);
        prev = update_prev_alive(prev, sel, next);
        sel = next;
        assert_eq!(sel, Some(Mute));
        let next = toggle_next(sel, prev, Mute);
        prev = update_prev_alive(prev, sel, next);
        sel = next;
        assert_eq!(sel, Some(Bypass));
        let _ = prev;
    }

    fn demo_status() -> Status {
        Status {
            version: "0.5.0".into(),
            mode: Some(RunMode::Mute),
            mic_configured: Some("alsa_input.rode".into()),
            mic_active: Some("alsa_input.rode".into()),
            mic_names: [None, None],
            fallback_active: false,
            model: "dpdfnet8_48khz_hr".into(),
            attn_limit: 100.0,
            profile: Some("alsa_input.rode".into()),
            profile_name: Some("RODE NT-USB".into()),
            defaults_model: "dpdfnet2_48khz_hr".into(),
            defaults_attn_limit: 24.0,
            chain_running: true,
            linked: Some(true),
            node_present: Some(true),
            node_state: Some("running".into()),
            control_file: Some(true),
            tray: crate::appstate::TrayHost::Registered,
            engine: Some(EngineTier::Quality),
            configured_light: false,
            inference: None,
            apps_using: Vec::new(),
            apps_direct: Vec::new(),
            apps_kept_off: Vec::new(),
            #[cfg(feature = "speakers")]
            speakers: demo_speakers(),
        }
    }

    #[cfg(feature = "speakers")]
    fn demo_speakers() -> crate::speakers::Status {
        crate::speakers::Status {
            enabled: true,
            on: true,
            running: true,
            output_configured: None,
            output_target: None,
            output_active: Some("alsa_output.usb-dac".into()),
            output_active_name: Some("USB DAC".into()),
            system_default: false,
            attn_limit: 24.0,
            model: crate::speakers::MODEL,
            engine: Some(EngineTier::Quality),
            inference: None,
            node_present: Some(true),
            node_state: Some("running".into()),
            playing: vec!["Discord".into()],
            bypass: false,
            failures: 0,
            failing: false,
            error: None,
            left: Vec::new(),
        }
    }

    fn inference(engine: &str, reason: Option<&str>) -> crate::diagnostics::Inference {
        crate::diagnostics::Inference {
            model: "dpdfnet8_48khz_hr".into(),
            engine: engine.into(),
            reason: reason.map(str::to_string),
        }
    }

    #[test]
    fn status_names_the_inference_engine_of_the_live_tier() {
        let mut s = demo_status();
        s.inference = Some(inference("native w7a8 avx2", None));
        assert!(
            render_status_human(&s).contains("engine: quality model (native w7a8 avx2)\n"),
            "{}",
            render_status_human(&s)
        );
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert_eq!(v["engine"], "quality");
        assert_eq!(v["inference"]["engine"], "native w7a8 avx2");
        assert_eq!(v["inference"]["model"], "dpdfnet8_48khz_hr");
        assert!(v["inference"]["reason"].is_null());
        s.engine = Some(EngineTier::Light);
        s.inference = Some(inference("onnx", Some("requested")));
        assert!(render_status_human(&s).contains("engine: light model (fallback, onnx)\n"));
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert_eq!(v["inference"]["reason"], "requested");
        s.engine = Some(EngineTier::Passthrough);
        assert!(render_status_human(&s).contains("engine: passthrough (fallback)\n"));
        // Before the first tier line: the chain's own model, in the same
        // words as after it
        s.engine = None;
        s.inference = Some(inference("native w7a8 avx2", None));
        assert!(render_status_human(&s).contains("engine: quality model (native w7a8 avx2)\n"));
        s.configured_light = true;
        assert!(render_status_human(&s).contains("engine: light model (native w7a8 avx2)\n"));
        s.configured_light = false;
        s.chain_running = false;
        assert!(!render_status_human(&s).contains("engine:"));
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert!(v["inference"].is_null());
    }

    #[test]
    fn the_live_model_follows_the_tier() {
        let q = "dpdfnet8_48khz_hr";
        let light = crate::controller::LIGHT_MODEL;
        assert_eq!(
            live_model(Some(EngineTier::Quality), q, false).as_deref(),
            Some(q)
        );
        assert_eq!(live_model(None, q, false).as_deref(), Some(q));
        assert_eq!(
            live_model(Some(EngineTier::Light), q, false).as_deref(),
            Some(light)
        );
        assert_eq!(
            live_model(Some(EngineTier::Light), light, true).as_deref(),
            Some(light)
        );
        assert_eq!(live_model(Some(EngineTier::Passthrough), q, false), None);
    }

    #[test]
    fn status_reports_the_engine_tier() {
        let mut s = demo_status();
        assert!(render_status_human(&s).contains("engine: quality model\n"));
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert_eq!(v["engine"], "quality");
        s.engine = Some(EngineTier::Light);
        assert!(render_status_human(&s).contains("engine: light model (fallback)\n"));
        s.configured_light = true;
        assert!(render_status_human(&s).contains("engine: light model\n"));
        s.engine = Some(EngineTier::Passthrough);
        assert!(render_status_human(&s).contains("engine: passthrough (fallback)\n"));
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert_eq!(v["engine"], "passthrough");
        // Nothing reported yet, or chain down: no line, JSON null.
        s.engine = None;
        assert!(!render_status_human(&s).contains("engine:"));
        s.engine = Some(EngineTier::Light);
        s.chain_running = false;
        assert!(!render_status_human(&s).contains("engine:"));
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert!(v["engine"].is_null());
    }

    #[test]
    fn status_json_is_the_documented_stable_object() {
        let v: serde_json::Value =
            serde_json::from_str(&render_status_json(&demo_status())).expect("valid json");
        assert_eq!(v["version"], "0.5.0");
        assert_eq!(v["mode"], "mute");
        assert_eq!(v["enabled"], true);
        assert_eq!(v["mic"]["configured"], "alsa_input.rode");
        assert_eq!(v["mic"]["active"], "alsa_input.rode");
        assert_eq!(v["mic"]["fallback_active"], false);
        assert_eq!(v["model"], "dpdfnet8_48khz_hr");
        assert_eq!(v["attn_limit"], 100.0);
        assert_eq!(v["latency_samples"], 3840);
        assert_eq!(v["tray"], "registered");
        assert_eq!(v["chain"]["running"], true);
        assert_eq!(v["chain"]["node_present"], true);
        #[cfg(feature = "speakers")]
        {
            // the speakers block (additive)
            let sp = &v["speakers"];
            assert_eq!(sp["enabled"], true);
            assert_eq!(sp["on"], true);
            assert_eq!(sp["running"], true);
            assert!(sp["output"]["configured"].is_null());
            assert_eq!(sp["output"]["active"], "alsa_output.usb-dac");
            assert_eq!(sp["output"]["active_name"], "USB DAC");
            assert_eq!(sp["output"]["system_default"], false);
            assert_eq!(sp["attn_limit"], 24.0);
            assert_eq!(sp["model"], "dpdfnet8_48khz_hr");
            assert_eq!(sp["engine"], "quality");
            assert_eq!(sp["chain"]["node_present"], true);
            assert_eq!(sp["chain"]["idle"], false);
            assert_eq!(sp["playing"][0], "Discord");
            assert_eq!(sp["bypass"], false);
            assert_eq!(sp["failures"], 0);
            assert!(sp["error"].is_null());
        }
        #[cfg(not(feature = "speakers"))]
        assert!(v.get("speakers").is_none());
    }

    #[cfg(feature = "speakers")]
    #[test]
    fn status_says_what_the_speakers_cleaner_does() {
        let mut s = demo_status();
        let h = render_status_human(&s);
        assert!(
            h.ends_with("speakers: on, playing to USB DAC, 1 stream\n"),
            "{h}"
        );
        let mut sp = demo_speakers();
        sp.playing.clear();
        sp.output_configured = Some("alsa_output.headset".into());
        assert_eq!(
            sp.words(),
            "on, playing to USB DAC (chosen: alsa_output.headset, not plugged in), \
             nothing playing through it"
        );
        sp.output_configured = None;
        sp.system_default = true;
        sp.bypass = true;
        sp.playing = vec!["a".into(), "b".into()];
        assert_eq!(
            sp.words(),
            "on, playing to USB DAC (HushMic Speakers is the system output; playing to the \
             last one), 2 streams, bypass held"
        );
        sp.running = false;
        assert_eq!(sp.words(), "starting");
        sp.error = Some("model file not found at /x".into());
        assert_eq!(sp.words(), "not running (model file not found at /x)");
        sp.on = false;
        assert_eq!(sp.words(), "off (HushMic is off)");
        sp.enabled = false;
        assert_eq!(sp.words(), "off");
        // stopped: nothing live in the block
        s.speakers = sp;
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert_eq!(v["speakers"]["enabled"], false);
        assert!(v["speakers"]["engine"].is_null());
        assert!(v["speakers"]["chain"]["node_state"].is_null());
    }

    /// The reported bug: the RODE profile runs, and `status` must say so
    /// instead of showing the defaults.
    #[test]
    fn status_shows_the_settings_in_effect_and_whose_they_are() {
        let mut s = demo_status();
        let h = render_status_human(&s);
        assert!(
            h.contains("model: dpdfnet8_48khz_hr  strength: 100 dB (RODE NT-USB profile)\n"),
            "{h}"
        );
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        // Top-level model/attn_limit are what runs (compatible: they
        // always were what `status` claimed was running).
        assert_eq!(v["model"], "dpdfnet8_48khz_hr");
        assert_eq!(v["attn_limit"], 100.0);
        assert_eq!(v["profile"], "alsa_input.rode");
        assert_eq!(v["defaults"]["model"], "dpdfnet2_48khz_hr");
        assert_eq!(v["defaults"]["attn_limit"], 24.0);
        // No description known: the node name.
        s.profile_name = None;
        assert!(render_status_human(&s).contains("100 dB (alsa_input.rode profile)\n"));
        // The defaults in effect.
        s.profile = None;
        s.model = "dpdfnet2_48khz_hr".into();
        s.attn_limit = 24.0;
        let h = render_status_human(&s);
        assert!(
            h.contains("model: dpdfnet2_48khz_hr  strength: 24 dB (defaults)\n"),
            "{h}"
        );
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert!(v["profile"].is_null());
    }

    #[test]
    fn status_json_null_fields_where_unknown() {
        let mut s = demo_status();
        s.mode = None;
        s.mic_configured = None;
        s.mic_active = None;
        s.node_present = None;
        s.chain_running = false;
        let v: serde_json::Value =
            serde_json::from_str(&render_status_json(&s)).expect("valid json");
        assert_eq!(v["mode"], "off");
        assert_eq!(v["enabled"], false);
        assert!(v["mic"]["configured"].is_null());
        assert!(v["chain"]["node_present"].is_null());
        // A stale state from before the chain went down is not reported.
        assert!(v["chain"]["node_state"].is_null());
        assert!(v["chain"]["idle"].is_null());
    }

    #[test]
    fn status_says_when_nothing_records_from_hushmic() {
        // Running and recorded from: plain "running", idle false.
        let s = demo_status();
        assert!(render_status_human(&s).contains("chain: running\n"));
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert_eq!(v["chain"]["node_state"], "running");
        assert_eq!(v["chain"]["idle"], false);
        // Idle and suspended both read as idle, worded as normal.
        for st in ["idle", "suspended"] {
            let mut i = demo_status();
            i.node_state = Some(st.into());
            let h = render_status_human(&i);
            assert!(
                h.contains("chain: running, idle (not processing audio right now)\n"),
                "{h}"
            );
            let v: serde_json::Value = serde_json::from_str(&render_status_json(&i)).unwrap();
            assert_eq!(v["chain"]["node_state"], st);
            assert_eq!(v["chain"]["idle"], true);
        }
        // Unknown state: the line stays as before, idle is null.
        let mut u = demo_status();
        u.node_state = None;
        assert!(render_status_human(&u).contains("chain: running\n"));
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&u)).unwrap();
        assert!(v["chain"]["idle"].is_null());
        // UX panel round 2, A11: no microphone on the link for a while is
        // "not linked", idle or not, never "idle" alone
        for st in [Some("idle"), Some("running"), None] {
            let mut n = demo_status();
            n.node_state = st.map(str::to_string);
            n.linked = Some(false);
            let h = render_status_human(&n);
            assert!(
                h.contains("chain: running, not linked (no microphone is connected to it)\n"),
                "{h}"
            );
            let v: serde_json::Value = serde_json::from_str(&render_status_json(&n)).unwrap();
            assert_eq!(v["chain"]["linked"], false);
        }
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert_eq!(v["chain"]["linked"], true);
        assert_eq!(v["running"], true);
    }

    #[test]
    fn status_says_whether_the_plugin_follows_its_control_file() {
        let mut s = demo_status();
        let control = |s: &Status| {
            let v: serde_json::Value = serde_json::from_str(&render_status_json(s)).unwrap();
            v["chain"]["control"].clone()
        };
        assert_eq!(control(&s), "file");
        s.control_file = Some(false);
        assert_eq!(control(&s), "unmapped");
        s.control_file = None;
        assert!(control(&s).is_null());
        s.control_file = Some(true);
        s.chain_running = false;
        assert!(control(&s).is_null(), "no chain, nothing to report");
    }

    #[test]
    fn status_reports_the_tray_origin() {
        use crate::appstate::TrayHost;
        let mut t = demo_status();
        // watch's words, in text and JSON alike
        for (host, word) in [
            (TrayHost::Registered, "registered"),
            (TrayHost::Waiting, "waiting"),
            (TrayHost::Disabled, "disabled"),
        ] {
            t.tray = host;
            let v: serde_json::Value = serde_json::from_str(&render_status_json(&t)).unwrap();
            assert_eq!(v["tray"], word);
            assert!(render_status_human(&t).contains(&format!("tray: {word}\n")));
            let w: serde_json::Value = serde_json::to_value(host).unwrap();
            assert_eq!(w, word, "the same as watch");
        }
    }

    /// UX panel A1: `status` says which apps hear HushMic and which the
    /// raw microphone.
    #[test]
    fn status_names_the_apps_on_hushmic_and_on_the_raw_mic() {
        use crate::appstate::{AppRef, DirectUse};
        let app = |id: &str, name: Option<&str>| AppRef {
            id: id.into(),
            name: name.map(str::to_string),
        };
        let mut s = demo_status();
        assert!(render_status_human(&s).contains("\napps: none\n"));
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert_eq!(v["apps"]["using"], serde_json::json!([]));
        s.apps_using = vec![app("zoom", Some("Zoom")), app("x", None)];
        s.apps_direct = vec![DirectUse {
            app: app("slack", Some("Slack")),
            follows_default: false,
        }];
        let h = render_status_human(&s);
        assert!(h.contains("\napps: Zoom, an app (direct: Slack)\n"), "{h}");
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert_eq!(v["apps"]["using"][0]["name"], "Zoom");
        assert_eq!(v["apps"]["direct"][0]["app"]["name"], "Slack");
        assert_eq!(v["apps"]["direct"][0]["follows_default"], false);
        s.apps_using.clear();
        assert!(render_status_human(&s).contains("\napps: none (direct: Slack)\n"));
        assert_eq!(v["apps"]["kept_off"], serde_json::json!([]));
        s.apps_kept_off = vec!["Firefox".into()];
        assert!(render_status_human(&s)
            .contains("\napps: none (direct: Slack) (kept off HushMic's microphone: Firefox)\n"));
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&s)).unwrap();
        assert_eq!(v["apps"]["kept_off"], serde_json::json!(["Firefox"]));
    }

    #[test]
    fn status_human_reads_like_a_report_not_a_dump() {
        let s = demo_status();
        let h = render_status_human(&s);
        assert!(h.contains("mute"), "{h}");
        assert!(h.contains("alsa_input.rode"), "{h}");
        assert!(h.contains("80 ms"), "latency stated: {h}");
        assert!(!h.contains('{'), "no JSON braces in human output: {h}");
        // fallback state is spelled out when engaged, naming the device
        // actually linked
        let mut f = demo_status();
        f.mic_configured = Some("usb_mic".into());
        f.mic_active = Some("laptop_mic".into());
        f.fallback_active = true;
        let h = render_status_human(&f);
        assert!(
            h.contains("mic: laptop_mic (fallback; usb_mic is unplugged)\n"),
            "{h}"
        );
        f.mic_active = None;
        let h = render_status_human(&f);
        assert!(
            h.contains("mic: system default (fallback; usb_mic is unplugged)\n"),
            "{h}"
        );
        // back on the chosen one
        f.mic_active = Some("usb_mic".into());
        f.fallback_active = false;
        assert!(render_status_human(&f).contains("mic: usb_mic\n"));
        // following the default names the device it is
        f.mic_configured = None;
        f.mic_active = Some("laptop_mic".into());
        assert!(render_status_human(&f).contains("mic: laptop_mic (system default)\n"));
        f.mic_active = None;
        assert!(render_status_human(&f).contains("mic: system default\n"));
        // UX panel round 2, A11: by display name, the node names in --json
        let mut n = demo_status();
        n.mic_configured = Some("usb_mic".into());
        n.mic_active = Some("laptop_mic".into());
        n.mic_names = [Some("Jabra Evolve2".into()), Some("Built-in Audio".into())];
        n.fallback_active = true;
        let h = render_status_human(&n);
        assert!(
            h.contains("mic: Built-in Audio (fallback; Jabra Evolve2 is unplugged)\n"),
            "{h}"
        );
        let v: serde_json::Value = serde_json::from_str(&render_status_json(&n)).unwrap();
        assert_eq!(v["mic"]["configured"], "usb_mic");
        assert_eq!(v["mic"]["active"], "laptop_mic");
        // and by name beside them: the doctor names an unplugged mic so
        // (A115 N-D1)
        assert_eq!(
            crate::diagnostics::parse_mic_names(&render_status_json(&n)),
            vec![
                ("usb_mic".to_string(), "Jabra Evolve2".to_string()),
                ("laptop_mic".to_string(), "Built-in Audio".to_string()),
            ]
        );
        n.mic_active = n.mic_configured.clone();
        n.fallback_active = false;
        assert!(render_status_human(&n).contains("mic: Jabra Evolve2\n"));
    }

    #[test]
    fn responses_encode_and_decode() {
        assert_eq!(decode_response(&encode_ok("mute")), (true, "mute".into()));
        assert_eq!(
            decode_response(&encode_ok("line1\nline2")),
            (true, "line1\nline2".into())
        );
        assert_eq!(decode_response(&encode_ok("")), (true, String::new()));
        assert_eq!(
            decode_response(&encode_err("no such state")),
            (false, "no such state".into())
        );
        // garbage from a non-hushmic peer decodes as an error, not a panic
        let (ok, _) = decode_response("HTTP/1.1 400 Bad Request");
        assert!(!ok);
    }
}
