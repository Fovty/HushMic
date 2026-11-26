//! `hushmic config`: the keys, their value grammar and validation, the
//! renderers, and the diff the daemon uses to decide what to apply. Pure
//! apart from the file-fallback helpers at the bottom — the daemon (live)
//! and the CLI (no daemon) both go through here, so `set` behaves the same
//! whether or not HushMic is running.

use crate::config::{Appearance, Config, Inference, TrayIcon};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Mic,
    Model,
    AttnLimit,
    SetDefault,
    Autostart,
    Tray,
    TrayIcon,
    Notifications,
    Inference,
    VoiceHold,
    Appearance,
    Enabled,
    ShortcutsSetup,
    MicPrefs,
    HiddenNotices,
    CloseNoticeSent,
    /// The speakers cleaner's switch (`[speakers]` `enabled`).
    #[cfg(feature = "speakers")]
    SpeakersEnabled,
    /// The output it plays to (`[speakers]` `output`).
    #[cfg(feature = "speakers")]
    SpeakersOutput,
    /// Its strength (`[speakers]` `attn_limit`).
    #[cfg(feature = "speakers")]
    SpeakersAttn,
}

/// How many of the keys are HushMic Speakers' (none in a build without it).
const SPEAKERS_KEYS: usize = 3 * cfg!(feature = "speakers") as usize;

/// The one key table: listing order (what a user asks about first,
/// app-managed keys last), the file/CLI name, and whether `set` takes it.
const KEYS: [(Key, &str, bool); 16 + SPEAKERS_KEYS] = [
    (Key::Enabled, "enabled", false),
    (Key::Mic, "mic", true),
    (Key::Model, "model", true),
    (Key::AttnLimit, "attn_limit", true),
    (Key::SetDefault, "set_default", true),
    (Key::Autostart, "autostart", true),
    (Key::Tray, "tray", true),
    (Key::TrayIcon, "tray_icon", true),
    (Key::Notifications, "notifications", true),
    (Key::Inference, "inference", true),
    (Key::VoiceHold, "voice_hold", true),
    (Key::Appearance, "appearance", true),
    #[cfg(feature = "speakers")]
    (Key::SpeakersEnabled, "speakers.enabled", true),
    #[cfg(feature = "speakers")]
    (Key::SpeakersOutput, "speakers.output", true),
    #[cfg(feature = "speakers")]
    (Key::SpeakersAttn, "speakers.attn_limit", true),
    (Key::ShortcutsSetup, "shortcuts_setup", false),
    (Key::MicPrefs, "mic_prefs", false),
    (Key::HiddenNotices, "hidden_notices", false),
    (Key::CloseNoticeSent, "close_notice_sent", false),
];

/// Every key, in listing order.
pub fn listed() -> impl Iterator<Item = Key> {
    KEYS.iter().map(|(k, _, _)| *k)
}

/// The keys `config` lists: the settings, and each microphone's own
/// (`mic_prefs`). The app's bookkeeping (whether the mic is on, which
/// notices were sent or hidden) stays out of the listing; `config get`
/// still reads it.
pub fn shown() -> impl Iterator<Item = Key> {
    listed().filter(|k| !k.internal())
}

fn names(settable_only: bool) -> String {
    KEYS.iter()
        .filter(|(k, _, s)| if settable_only { *s } else { !k.internal() })
        .map(|(_, n, _)| *n)
        .collect::<Vec<_>>()
        .join(" ")
}

impl Key {
    pub fn name(self) -> &'static str {
        KEYS.iter()
            .find(|(k, _, _)| *k == self)
            .map(|(_, n, _)| *n)
            .expect("every Key is in KEYS")
    }
    pub fn settable(self) -> bool {
        KEYS.iter()
            .find(|(k, _, _)| *k == self)
            .is_some_and(|(_, _, s)| *s)
    }
    /// The app's own bookkeeping, not a setting (see [`shown`]).
    pub fn internal(self) -> bool {
        matches!(
            self,
            Key::Enabled | Key::ShortcutsSetup | Key::HiddenNotices | Key::CloseNoticeSent
        )
    }
}

pub fn parse_key(s: &str) -> Result<Key, String> {
    KEYS.iter()
        .find(|(_, n, _)| *n == s)
        .map(|(k, _, _)| *k)
        .ok_or_else(|| format!("unknown key '{s}' (keys: {})", names(false)))
}

pub fn parse_settable_key(s: &str) -> Result<Key, String> {
    match parse_key(s)? {
        Key::Enabled => {
            Err("'enabled' is set with: hushmic mode suppress|bypass|mute|off".to_string())
        }
        k if !k.settable() => Err(format!(
            "'{s}' is managed by HushMic itself (settable keys: {})",
            names(true)
        )),
        k => Ok(k),
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    Mic(Option<String>),
    /// An output by node name; None = the system default.
    #[cfg(feature = "speakers")]
    Output(Option<String>),
    Model(String),
    Attn(f32),
    Bool(bool),
    Icon(TrayIcon),
    Inference(Inference),
    Appearance(Appearance),
}

/// Named strengths — the tray's presets (tray.rs ATTN_PRESETS). The
/// window calls 6 dB "gentle"; "light" stays accepted.
const ATTN_WORDS: [(&str, f32); 5] = [
    ("maximum", 100.0),
    ("strong", 24.0),
    ("medium", 12.0),
    ("gentle", 6.0),
    ("light", 6.0),
];

/// Validate a value for `key` exactly as the daemon will accept it. A
/// model must exist as `<model_dir>/<id>.onnx` NOW: `Controller::enable`
/// tears the chain down before its own preflight, so letting it discover
/// a typo would leave the mic dead and the typo in the file.
pub fn parse_value(key: Key, raw: &str, model_dir: &Path) -> Result<Value, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("{} needs a value", key.name()));
    }
    match key {
        Key::Mic => Ok(Value::Mic(if raw == "default" {
            None
        } else {
            Some(raw.to_string())
        })),
        Key::Model => {
            // the window's words for the two models
            let typed = raw;
            let raw = match raw.to_ascii_lowercase().as_str() {
                "quality" => crate::appstate::QUALITY_MODEL,
                "light" => crate::controller::LIGHT_MODEL,
                _ => raw,
            };
            if raw.contains('/') || raw.contains("..") {
                return Err(format!(
                    "'{raw}' is not a model id (a file name without .onnx under {})",
                    model_dir.display()
                ));
            }
            let p = model_dir.join(format!("{raw}.onnx"));
            if !p.exists() {
                if typed != raw {
                    return Err(format!(
                        "the {} model is not installed: {} is not there",
                        typed.to_ascii_lowercase(),
                        p.display()
                    ));
                }
                return Err(format!(
                    "model must be quality or light, not '{raw}' (or the id of an installed \
                     model file: {}; {} is not there)",
                    installed_models(model_dir).join(" "),
                    p.display(),
                ));
            }
            Ok(Value::Model(raw.to_string()))
        }
        #[cfg(feature = "speakers")]
        Key::SpeakersOutput => match raw {
            "default" => Ok(Value::Output(None)),
            crate::speakers::SINK => Err(format!(
                "speakers.output is a real output (see hushmic devices outputs), not {} itself",
                crate::speakers::SINK
            )),
            _ => Ok(Value::Output(Some(raw.to_string()))),
        },
        Key::AttnLimit => parse_attn(key, raw),
        #[cfg(feature = "speakers")]
        Key::SpeakersAttn => parse_attn(key, raw),
        Key::TrayIcon => TrayIcon::parse(&raw.to_ascii_lowercase())
            .map(Value::Icon)
            .ok_or_else(|| {
                format!(
                    "tray_icon must be {}, not '{raw}'",
                    TrayIcon::VALUES.join(" or ")
                )
            }),
        Key::Inference => Inference::parse(&raw.to_ascii_lowercase())
            .map(Value::Inference)
            .ok_or_else(|| {
                format!(
                    "inference must be {}, not '{raw}'",
                    Inference::VALUES.join(" or ")
                )
            }),
        Key::Appearance => Appearance::parse(&raw.to_ascii_lowercase())
            .map(Value::Appearance)
            .ok_or_else(|| {
                format!(
                    "appearance must be {}, not '{raw}'",
                    Appearance::VALUES.join(" or ")
                )
            }),
        Key::SetDefault | Key::Autostart | Key::Tray | Key::Notifications | Key::VoiceHold => {
            parse_bool(key, raw)
        }
        #[cfg(feature = "speakers")]
        Key::SpeakersEnabled => parse_bool(key, raw),
        Key::Enabled
        | Key::ShortcutsSetup
        | Key::MicPrefs
        | Key::HiddenNotices
        | Key::CloseNoticeSent => Err(format!("{} is read-only here", key.name())),
    }
}

/// A strength: a number of dB or one of the [`ATTN_WORDS`].
fn parse_attn(key: Key, raw: &str) -> Result<Value, String> {
    if let Some((_, v)) = ATTN_WORDS.iter().find(|(w, _)| *w == raw) {
        return Ok(Value::Attn(*v));
    }
    let v: f32 = raw
        .parse()
        .ok()
        .filter(|v: &f32| v.is_finite())
        .filter(|v| (0.0..=100.0).contains(v))
        .ok_or_else(|| {
            format!(
                "{} must be 0-100 (dB) or maximum|strong|medium|gentle, not '{raw}'",
                key.name()
            )
        })?;
    Ok(Value::Attn(v))
}

/// A switch: true or false, in the words people type.
fn parse_bool(key: Key, raw: &str) -> Result<Value, String> {
    match raw.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(Value::Bool(true)),
        "false" | "off" | "no" | "0" => Ok(Value::Bool(false)),
        _ => Err(format!("{} must be true or false, not '{raw}'", key.name())),
    }
}

fn installed_models(model_dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(model_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.strip_suffix(".onnx").map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    if v.is_empty() {
        v.push("none".into());
    }
    v
}

/// Same rules as the tray commands: `model` and `attn_limit` change the
/// settings in effect for the device in use, `device` (see
/// [`Config::edit_target`]).
pub fn apply(cfg: &mut Config, key: Key, value: Value, device: Option<&str>) {
    match (key, value) {
        (Key::Mic, Value::Mic(m)) => cfg.apply_mic_selection(m),
        (Key::Model, Value::Model(m)) => cfg.set_model_for(device, m),
        (Key::AttnLimit, Value::Attn(v)) => cfg.set_attn_for(device, v),
        (Key::SetDefault, Value::Bool(b)) => cfg.set_default = b,
        (Key::Autostart, Value::Bool(b)) => cfg.autostart = b,
        (Key::Tray, Value::Bool(b)) => cfg.tray = b,
        (Key::TrayIcon, Value::Icon(v)) => cfg.tray_icon = v,
        (Key::Notifications, Value::Bool(b)) => cfg.notifications = b,
        (Key::Inference, Value::Inference(v)) => cfg.inference = v,
        (Key::VoiceHold, Value::Bool(b)) => cfg.voice_hold = b,
        (Key::Appearance, Value::Appearance(v)) => cfg.appearance = v,
        #[cfg(feature = "speakers")]
        (Key::SpeakersEnabled, Value::Bool(b)) => cfg.speakers_mut().enabled = b,
        #[cfg(feature = "speakers")]
        (Key::SpeakersOutput, Value::Output(o)) => cfg.speakers_mut().output = o,
        #[cfg(feature = "speakers")]
        (Key::SpeakersAttn, Value::Attn(v)) => cfg.speakers_mut().attn_limit = v,
        (k, v) => unreachable!("parse_value produced {v:?} for {k:?}"),
    }
}

fn attn_text(v: f32) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// The plain-text value of one key. `model` and `attn_limit` are the ones
/// `device` runs with — what `set` on the same device changes.
pub fn get(cfg: &Config, key: Key, device: Option<&str>) -> String {
    match key {
        Key::Mic => cfg.mic.clone().unwrap_or_else(|| "default".into()),
        Key::Model => cfg.profile_for(device).model,
        Key::AttnLimit => attn_text(cfg.profile_for(device).attn_limit),
        Key::SetDefault => cfg.set_default.to_string(),
        Key::Autostart => cfg.autostart.to_string(),
        Key::Tray => cfg.tray.to_string(),
        Key::TrayIcon => cfg.tray_icon.as_str().to_string(),
        Key::Notifications => cfg.notifications.to_string(),
        Key::Inference => cfg.inference.as_str().to_string(),
        Key::VoiceHold => cfg.voice_hold.to_string(),
        Key::Appearance => cfg.appearance.as_str().to_string(),
        #[cfg(feature = "speakers")]
        Key::SpeakersEnabled => cfg.speakers().enabled.to_string(),
        #[cfg(feature = "speakers")]
        Key::SpeakersOutput => cfg.speakers().output.unwrap_or_else(|| "default".into()),
        #[cfg(feature = "speakers")]
        Key::SpeakersAttn => attn_text(cfg.speakers().attn_limit),
        Key::Enabled => cfg.enabled.to_string(),
        Key::ShortcutsSetup => cfg.shortcuts_setup.to_string(),
        Key::CloseNoticeSent => cfg.close_notice_sent.to_string(),
        Key::MicPrefs => {
            if cfg.mic_prefs.is_empty() {
                "(none)".into()
            } else {
                cfg.mic_prefs
                    .iter()
                    .map(|(m, p)| format!("{m}: {} {} dB", p.model, attn_text(p.attn_limit)))
                    .collect::<Vec<_>>()
                    .join("; ")
            }
        }
        Key::HiddenNotices => {
            if cfg.hidden_notices.is_empty() {
                "(none)".into()
            } else {
                cfg.hidden_notices
                    .iter()
                    .map(|h| h.app.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            }
        }
    }
}

/// `key = value` for every listed key — including the ones the file
/// omits at their default, which are exactly what a new user asks about.
pub fn render_all(cfg: &Config, device: Option<&str>) -> String {
    shown()
        .map(|k| format!("{} = {}\n", k.name(), get(cfg, k, device)))
        .collect()
}

fn json_value(cfg: &Config, key: Key, device: Option<&str>) -> serde_json::Value {
    use serde_json::json;
    match key {
        // the word `set` takes, as the text listing says it
        Key::Mic => json!(cfg.mic.as_deref().unwrap_or("default")),
        Key::Model => json!(cfg.profile_for(device).model),
        Key::AttnLimit => json!(cfg.profile_for(device).attn_limit),
        Key::SetDefault => json!(cfg.set_default),
        Key::Autostart => json!(cfg.autostart),
        Key::Tray => json!(cfg.tray),
        Key::TrayIcon => json!(cfg.tray_icon.as_str()),
        Key::Notifications => json!(cfg.notifications),
        Key::Inference => json!(cfg.inference.as_str()),
        Key::VoiceHold => json!(cfg.voice_hold),
        Key::Appearance => json!(cfg.appearance.as_str()),
        #[cfg(feature = "speakers")]
        Key::SpeakersEnabled => json!(cfg.speakers().enabled),
        #[cfg(feature = "speakers")]
        Key::SpeakersOutput => json!(cfg.speakers().output.as_deref().unwrap_or("default")),
        #[cfg(feature = "speakers")]
        Key::SpeakersAttn => json!(cfg.speakers().attn_limit),
        Key::Enabled => json!(cfg.enabled),
        Key::ShortcutsSetup => json!(cfg.shortcuts_setup),
        Key::CloseNoticeSent => json!(cfg.close_notice_sent),
        Key::MicPrefs => serde_json::Value::Object(
            cfg.mic_prefs
                .iter()
                .map(|(m, p)| {
                    (
                        m.clone(),
                        json!({"model": p.model, "attn_limit": p.attn_limit}),
                    )
                })
                .collect(),
        ),
        Key::HiddenNotices => json!(cfg.hidden_notices),
    }
}

pub fn render_all_json(cfg: &Config, device: Option<&str>) -> String {
    let mut map = serde_json::Map::new();
    for k in shown() {
        map.insert(k.name().to_string(), json_value(cfg, k, device));
    }
    serde_json::Value::Object(map).to_string()
}

pub fn render_one_json(cfg: &Config, key: Key, device: Option<&str>) -> String {
    let mut map = serde_json::Map::new();
    map.insert(key.name().to_string(), json_value(cfg, key, device));
    serde_json::Value::Object(map).to_string()
}

/// A model by the word `set` takes for it ("quality", "light"), else its id.
pub fn model_word(id: &str) -> &str {
    match id {
        crate::appstate::QUALITY_MODEL => "quality",
        crate::controller::LIGHT_MODEL => "light",
        other => other,
    }
}

/// `config set`'s answer, before any qualifier: the key and its value in
/// the words `set` takes ("model = light", not the model's id), and where
/// `model`/`attn_limit` went: the microphone in use, or the defaults
/// (`defaults`: `config set --defaults`). F13.
pub fn set_echo(cfg: &Config, k: Key, device: Option<&str>, defaults: bool) -> String {
    let at = if defaults { None } else { device };
    let value = |device| match k {
        Key::Model => model_word(&cfg.profile_for(device).model).to_string(),
        _ => get(cfg, k, device),
    };
    let place = match (k, cfg.edit_target(at)) {
        (Key::Model | Key::AttnLimit, Some(d)) => {
            format!(" (saved for {d}; the defaults stay {})", value(None))
        }
        (Key::Model | Key::AttnLimit, None) if !cfg.mic_prefs.is_empty() => {
            " (the defaults; microphones with their own settings keep theirs)".to_string()
        }
        _ => String::new(),
    };
    format!("{} = {}{place}", k.name(), value(at))
}

/// `line` with `note` (" (…)") in its own closing parenthesis, if it has
/// one: "attn_limit = 24 (saved for usbmic; the defaults stay 30; applies
/// when HushMic starts)", never two in a row, "saved" once (A106 N-C1).
pub fn fold(line: &str, note: &str) -> String {
    let inner = note.strip_prefix(" (").and_then(|n| n.strip_suffix(')'));
    match (line.strip_suffix(')'), inner) {
        (Some(head), Some(inner)) if line.contains(" (") => {
            // "saved" once
            let inner = match head.contains("(saved") {
                true => inner.strip_prefix("saved; ").unwrap_or(inner),
                false => inner,
            };
            format!("{head}; {inner})")
        }
        _ => format!("{line}{note}"),
    }
}

/// The suffix `set` prints after `key = value`.
pub fn set_qualifier(key: Key, daemon_running: bool) -> &'static str {
    if !daemon_running {
        " (saved; applies when HushMic starts)"
    } else if key == Key::Tray {
        " (takes effect on the next start)"
    } else {
        ""
    }
}

/// What changed between two configs, grouped by what the daemon must do
/// about it. Also the shape a file reload will use (every chain-relevant
/// field, not just the ones `set` can touch).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Diff {
    /// enabled / mic / model / attn_limit / set_default / mic_prefs /
    /// inference / voice_hold: the filter chain re-renders.
    pub chain: bool,
    pub autostart: bool,
    pub notifications: bool,
}

pub fn diff(old: &Config, new: &Config) -> Diff {
    Diff {
        chain: old.enabled != new.enabled
            || old.mic != new.mic
            || old.model != new.model
            || old.attn_limit != new.attn_limit
            || old.set_default != new.set_default
            || old.mic_prefs != new.mic_prefs
            || old.inference != new.inference
            || old.voice_hold != new.voice_hold,
        autostart: old.autostart != new.autostart,
        notifications: old.notifications != new.notifications,
    }
}

// --- `config get` / `config set` without a daemon ---------------------------

/// `config` / `config get KEY` rendered from a Config (the daemon's live
/// copy or the file), `device` being the one whose settings apply.
pub fn offline_get(
    cfg: &Config,
    key: Option<&str>,
    json: bool,
    device: Option<&str>,
) -> Result<String, String> {
    Ok(match (key, json) {
        (None, false) => render_all(cfg, device),
        (None, true) => render_all_json(cfg, device),
        (Some(k), false) => format!("{}\n", get(cfg, parse_key(k)?, device)),
        (Some(k), true) => render_one_json(cfg, parse_key(k)?, device),
    })
}

/// The device a start would take its settings from — what `config get` and
/// `set` refer to without a daemon (see
/// [`crate::pipewire::resolve_chain_target`]). A crashed run's takeover
/// breadcrumb counts, as in `--doctor`.
pub fn offline_device(cfg: &Config) -> Option<String> {
    let prior = crate::controller::persisted_prior_default();
    crate::pipewire::resolve_chain_target(cfg.mic.as_deref(), prior.as_deref()).profile_device
}

/// The no-daemon path: validate exactly like the daemon, write the file,
/// and do the autostart side effect (the entry IS what "autostart" means).
pub fn offline_set(
    key: &str,
    raw: &str,
    model_dir: &Path,
    defaults: bool,
) -> Result<String, String> {
    let k = parse_settable_key(key)?;
    let v = parse_value(k, raw, model_dir)?;
    let mut cfg = Config::load();
    let device = offline_device(&cfg).filter(|_| !defaults);
    apply(&mut cfg, k, v, device.as_deref());
    cfg.save()
        .map_err(|e| format!("could not write {}: {e}", Config::path().display()))?;
    let mut note = String::new();
    if k == Key::Autostart {
        if crate::sandbox::is_flatpak() {
            match crate::portal::request_background_blocking(cfg.autostart) {
                Ok(granted) if cfg.autostart && !granted => {
                    note.push_str(" (the desktop denied the autostart request)")
                }
                Ok(_) => {}
                Err(e) => note.push_str(&format!(" (autostart request failed: {e})")),
            }
        } else {
            crate::autostart::set_autostart(cfg.autostart)
                .map_err(|e| format!("saved, but the autostart entry could not be written: {e}"))?;
            if cfg.autostart && crate::service::unit_enabled() {
                note.push_str(
                    " (note: the systemd unit hushmic.service is enabled too; use one or the other)",
                );
            }
        }
    }
    // the login entry is written (or asked for) now, not at the next start
    let qualifier = match (k, cfg.autostart) {
        (Key::Autostart, true) => " (saved; HushMic starts when you log in)",
        (Key::Autostart, false) => " (saved; HushMic no longer starts when you log in)",
        _ => set_qualifier(k, false),
    };
    let echo = set_echo(&cfg, k, device.as_deref(), defaults);
    Ok(format!("{}\n", fold(&fold(&echo, qualifier), &note)))
}

/// After `config set mic NAME`: a word when NAME is not a microphone
/// HushMic can capture from now (`usable`: what `devices` lists; None when
/// PipeWire could not be asked, and nothing is said). It is saved all the
/// same: a mic that is unplugged now is picked up when it comes back.
pub fn mic_warning(name: &str, usable: Option<&[crate::pipewire::Source]>) -> Option<String> {
    let usable = usable?;
    if name == "default" || usable.iter().any(|s| s.name == name) {
        return None;
    }
    Some(format!(
        "warning: {name} is not a microphone HushMic can use now (see hushmic devices); \
         until it is, HushMic follows the system default"
    ))
}

/// `hushmic devices`: the real capture sources, the configured one marked.
pub fn render_devices(
    sources: &[crate::pipewire::Source],
    configured: Option<&str>,
    json: bool,
) -> String {
    if json {
        return serde_json::Value::Array(
            sources
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "name": s.name,
                        "description": s.description,
                        "selected": configured == Some(s.name.as_str()),
                    })
                })
                .collect(),
        )
        .to_string();
    }
    if sources.is_empty() {
        // PipeWire answered (a failed look is the caller's error)
        return "(no microphones found)\n".to_string();
    }
    let mut out = String::new();
    for s in sources {
        let mark = if configured == Some(s.name.as_str()) {
            "*"
        } else {
            " "
        };
        out.push_str(&format!("{mark} {}\t{}\n", s.name, s.description));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway model dir, removed on drop.
    struct ModelDir(std::path::PathBuf);
    impl Drop for ModelDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn model_dir() -> ModelDir {
        let d = std::env::temp_dir().join(format!(
            "hushmic-cfgcli-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("dpdfnet2_48khz_hr.onnx"), b"x").unwrap();
        ModelDir(d)
    }

    #[test]
    fn keys_parse_and_the_readonly_ones_are_not_settable() {
        assert_eq!(parse_key("attn_limit"), Ok(Key::AttnLimit));
        assert_eq!(parse_key("enabled"), Ok(Key::Enabled));
        assert!(parse_key("volume").unwrap_err().contains("unknown key"));
        #[cfg(not(feature = "speakers"))]
        assert!(parse_key("speakers.enabled")
            .is_err_and(|e| e.contains("unknown key") && !e.contains("speakers.output")));
        let e = parse_settable_key("enabled").unwrap_err();
        assert!(e.contains("hushmic mode"), "{e}");
        assert!(parse_settable_key("mic_prefs").is_err());
        assert!(parse_settable_key("shortcuts_setup").is_err());
        assert_eq!(listed().count(), 16 + SPEAKERS_KEYS);
        assert!(parse_settable_key("close_notice_sent").is_err());
        assert_eq!(
            listed().filter(|k| k.settable()).count(),
            11 + SPEAKERS_KEYS
        );
        assert_eq!(parse_settable_key("voice_hold"), Ok(Key::VoiceHold));
        assert_eq!(parse_settable_key("appearance"), Ok(Key::Appearance));
        assert!(parse_settable_key("hidden_notices").is_err());
        assert_eq!(parse_settable_key("inference"), Ok(Key::Inference));
        assert_eq!(parse_key("tray_icon"), Ok(Key::TrayIcon));
        assert_eq!(parse_settable_key("tray_icon"), Ok(Key::TrayIcon));
        for k in listed() {
            assert_eq!(parse_key(k.name()), Ok(k));
        }
        let e = parse_key("nope").unwrap_err();
        assert!(e.contains("keys: mic model"), "{e}");
        assert!(!e.contains("close_notice_sent"), "{e}");
    }

    #[cfg(feature = "speakers")]
    #[test]
    fn the_speakers_keys_set_their_own_section() {
        let md = model_dir();
        let mut cfg = Config::default();
        assert_eq!(get(&cfg, Key::SpeakersEnabled, None), "false");
        assert_eq!(get(&cfg, Key::SpeakersOutput, None), "default");
        assert_eq!(get(&cfg, Key::SpeakersAttn, None), "100");
        // listed, but nothing written until used
        assert!(render_all(&cfg, None).contains("speakers.enabled = false\n"));
        assert!(cfg.speakers.is_none());
        for (k, raw) in [
            ("speakers.enabled", "on"),
            ("speakers.output", "alsa_output.usb-dac"),
            ("speakers.attn_limit", "medium"),
        ] {
            let key = parse_settable_key(k).unwrap();
            let v = parse_value(key, raw, &md.0).unwrap();
            apply(&mut cfg, key, v, Some("alsa_input.x"));
        }
        let sp = cfg.speakers();
        assert!(sp.enabled);
        assert_eq!(sp.output.as_deref(), Some("alsa_output.usb-dac"));
        assert_eq!(sp.attn_limit, 12.0);
        // the microphone's settings are untouched (no profile created)
        assert!(cfg.mic_prefs.is_empty());
        assert_eq!(cfg.attn_limit, 100.0);
        assert_eq!(
            set_echo(&cfg, Key::SpeakersAttn, Some("alsa_input.x"), false),
            "speakers.attn_limit = 12"
        );
        let v: serde_json::Value = serde_json::from_str(&render_all_json(&cfg, None)).unwrap();
        assert_eq!(v["speakers.enabled"], true);
        assert_eq!(v["speakers.output"], "alsa_output.usb-dac");
        assert_eq!(v["speakers.attn_limit"], 12.0);
        // back to the default output; never the sink itself
        let v = parse_value(Key::SpeakersOutput, "default", &md.0).unwrap();
        apply(&mut cfg, Key::SpeakersOutput, v, None);
        assert_eq!(cfg.speakers().output, None);
        let e = parse_value(Key::SpeakersOutput, "hushmic_sink", &md.0).unwrap_err();
        assert!(e.contains("real output"), "{e}");
        let e = parse_value(Key::SpeakersAttn, "300", &md.0).unwrap_err();
        assert!(e.starts_with("speakers.attn_limit must be 0-100"), "{e}");
        let e = parse_value(Key::SpeakersEnabled, "maybe", &md.0).unwrap_err();
        assert!(e.contains("true or false"), "{e}");
        // the microphone's chain is not restarted for them
        let old = Config::default();
        assert!(!diff(&old, &cfg).chain);
    }

    #[test]
    fn values_validate_and_out_of_range_is_refused() {
        let md_guard = model_dir();
        let md = md_guard.0.clone();
        assert_eq!(parse_value(Key::Mic, "default", &md), Ok(Value::Mic(None)));
        assert_eq!(
            parse_value(Key::Mic, " alsa_input.x ", &md),
            Ok(Value::Mic(Some("alsa_input.x".into())))
        );
        assert!(parse_value(Key::Mic, "", &md).is_err());
        assert_eq!(
            parse_value(Key::Model, "dpdfnet2_48khz_hr", &md),
            Ok(Value::Model("dpdfnet2_48khz_hr".into()))
        );
        // the window's words
        let light_only = parse_value(Key::Model, "quality", &md).unwrap_err();
        assert!(
            light_only.starts_with("the quality model is not installed"),
            "{light_only}"
        );
        std::fs::write(md.join("dpdfnet8_48khz_hr.onnx"), b"x").unwrap();
        for (w, id) in [
            ("quality", "dpdfnet8_48khz_hr"),
            ("light", "dpdfnet2_48khz_hr"),
            ("Light", "dpdfnet2_48khz_hr"),
        ] {
            assert_eq!(
                parse_value(Key::Model, w, &md),
                Ok(Value::Model(id.into())),
                "{w}"
            );
        }
        let e = parse_value(Key::Model, "nope", &md).unwrap_err();
        assert!(e.contains("quality or light"), "{e}");
        assert!(e.contains("nope.onnx"), "{e}");
        assert!(e.contains("dpdfnet2_48khz_hr"), "lists installed: {e}");
        assert!(parse_value(Key::Model, "../etc", &md).is_err());
        assert!(parse_value(Key::Model, "a/b", &md).is_err());
        assert_eq!(
            parse_value(Key::AttnLimit, "24", &md),
            Ok(Value::Attn(24.0))
        );
        assert_eq!(
            parse_value(Key::AttnLimit, "24.5", &md),
            Ok(Value::Attn(24.5))
        );
        assert_eq!(
            parse_value(Key::AttnLimit, "strong", &md),
            Ok(Value::Attn(24.0))
        );
        assert_eq!(
            parse_value(Key::AttnLimit, "maximum", &md),
            Ok(Value::Attn(100.0))
        );
        // out of range is refused, as a wrong word is (not clamped)
        for bad in ["250", "-5", "100.5", "9999"] {
            let e = parse_value(Key::AttnLimit, bad, &md).unwrap_err();
            assert!(e.contains("0-100"), "{bad}: {e}");
        }
        assert_eq!(parse_value(Key::AttnLimit, "0", &md), Ok(Value::Attn(0.0)));
        assert_eq!(
            parse_value(Key::AttnLimit, "100", &md),
            Ok(Value::Attn(100.0))
        );
        assert!(parse_value(Key::AttnLimit, "nan", &md).is_err());
        assert!(parse_value(Key::AttnLimit, "inf", &md).is_err());
        assert!(parse_value(Key::AttnLimit, "loud", &md).is_err());
        for (raw, want) in [
            ("true", true),
            ("on", true),
            ("yes", true),
            ("1", true),
            ("TRUE", true),
            ("false", false),
            ("off", false),
            ("no", false),
            ("0", false),
        ] {
            assert_eq!(
                parse_value(Key::Autostart, raw, &md),
                Ok(Value::Bool(want)),
                "{raw}"
            );
        }
        assert!(parse_value(Key::Tray, "maybe", &md).is_err());
        for (raw, want) in [
            ("auto", TrayIcon::Auto),
            ("color", TrayIcon::Color),
            ("symbolic", TrayIcon::Symbolic),
            ("Symbolic", TrayIcon::Symbolic),
            (" AUTO ", TrayIcon::Auto),
        ] {
            assert_eq!(
                parse_value(Key::TrayIcon, raw, &md),
                Ok(Value::Icon(want)),
                "{raw}"
            );
        }
        let e = parse_value(Key::TrayIcon, "mono", &md).unwrap_err();
        assert!(e.contains("auto or color or symbolic"), "{e}");
        assert!(e.contains("mono"), "{e}");
        assert!(parse_value(Key::TrayIcon, "", &md).is_err());
        for (raw, want) in [
            ("auto", Inference::Auto),
            ("onnx", Inference::Onnx),
            (" ONNX ", Inference::Onnx),
        ] {
            assert_eq!(
                parse_value(Key::Inference, raw, &md),
                Ok(Value::Inference(want)),
                "{raw}"
            );
        }
        // The native engines are debugging overrides (HUSHMIC_INFERENCE),
        // not settings.
        let e = parse_value(Key::Inference, "native", &md).unwrap_err();
        assert!(e.contains("auto or onnx"), "{e}");
        assert!(parse_value(Key::Enabled, "true", &md).is_err());
    }

    #[test]
    fn apply_follows_the_tray_profile_rules() {
        let mut c = Config {
            mic: Some("rode".into()),
            ..Config::default()
        };
        let rode = Some("rode");
        apply(&mut c, Key::AttnLimit, Value::Attn(24.0), rode);
        assert_eq!(c.mic_prefs["rode"].attn_limit, 24.0);
        assert_eq!(c.attn_limit, 100.0, "the defaults stay");
        apply(
            &mut c,
            Key::Model,
            Value::Model("dpdfnet2_48khz_hr".into()),
            rode,
        );
        assert_eq!(c.mic_prefs["rode"].model, "dpdfnet2_48khz_hr");
        assert_eq!(get(&c, Key::AttnLimit, rode), "24");
        assert_eq!(get(&c, Key::Model, rode), "dpdfnet2_48khz_hr");
        apply(&mut c, Key::Mic, Value::Mic(None), rode);
        assert_eq!(c.mic, None);
        // Follow-default: the RODE still has its profile, so edits keep
        // landing there…
        apply(&mut c, Key::AttnLimit, Value::Attn(18.0), rode);
        assert_eq!(c.mic_prefs["rode"].attn_limit, 18.0);
        // …and a device without one gets its own on the first edit (UX
        // panel B2), the defaults stay.
        apply(&mut c, Key::AttnLimit, Value::Attn(6.0), Some("jabra"));
        assert_eq!(c.mic_prefs["jabra"].attn_limit, 6.0);
        assert_eq!(c.attn_limit, 100.0);
        assert_eq!(get(&c, Key::AttnLimit, Some("jabra")), "6");
        c.mic_prefs.remove("jabra");
        // No device can own it (unknown default, our own node): the
        // defaults change.
        apply(&mut c, Key::AttnLimit, Value::Attn(12.0), None);
        apply(
            &mut c,
            Key::AttnLimit,
            Value::Attn(12.0),
            Some("hushmic_source"),
        );
        assert_eq!(c.attn_limit, 12.0);
        assert_eq!(c.mic_prefs.len(), 1);
        assert_eq!(get(&c, Key::AttnLimit, None), "12");
        apply(&mut c, Key::TrayIcon, Value::Icon(TrayIcon::Symbolic), None);
        assert_eq!(c.tray_icon, TrayIcon::Symbolic);
        apply(&mut c, Key::Tray, Value::Bool(false), None);
        apply(&mut c, Key::Notifications, Value::Bool(false), None);
        apply(&mut c, Key::SetDefault, Value::Bool(true), None);
        apply(&mut c, Key::Autostart, Value::Bool(true), None);
        assert!(!c.tray && !c.notifications && c.set_default && c.autostart);
    }

    #[test]
    fn rendering_lists_every_key_plainly_and_as_json() {
        let c = Config::default();
        let plain = render_all(&c, None);
        for k in listed() {
            let name = k.name();
            assert_eq!(
                plain.lines().any(|l| l.starts_with(&format!("{name} = "))),
                !k.internal(),
                "{name}:\n{plain}"
            );
        }
        // the app's bookkeeping is out of the listing, and still read
        assert_eq!(shown().count(), 12 + SPEAKERS_KEYS);
        assert_eq!(get(&c, Key::CloseNoticeSent, None), "false");
        assert!(plain.contains("mic = default\n"), "{plain}");
        assert!(plain.contains("tray = true\n"), "{plain}");
        assert!(plain.contains("tray_icon = auto\n"), "{plain}");
        assert!(plain.contains("inference = auto\n"), "{plain}");
        assert!(plain.contains("voice_hold = false\n"), "{plain}");
        assert!(plain.contains("attn_limit = 100\n"), "{plain}");
        assert!(plain.contains("mic_prefs = (none)\n"), "{plain}");
        let v: serde_json::Value = serde_json::from_str(&render_all_json(&c, None)).unwrap();
        // the same word as the text (and `set`)
        assert_eq!(v["mic"], "default");
        assert!(v.get("enabled").is_none() && v.get("hidden_notices").is_none());
        assert_eq!(v["attn_limit"], 100.0);
        assert_eq!(v["tray"], true);
        assert_eq!(v["tray_icon"], "auto");
        assert_eq!(v["inference"], "auto");
        assert_eq!(v["voice_hold"], false);
        assert!(v["mic_prefs"].is_object());
        let one: serde_json::Value =
            serde_json::from_str(&render_one_json(&c, Key::Model, None)).unwrap();
        assert_eq!(one["model"], "dpdfnet8_48khz_hr");
        assert_eq!(get(&c, Key::AttnLimit, None), "100");
        let mut c2 = c.clone();
        c2.attn_limit = 24.5;
        c2.mic_prefs.insert(
            "rode".into(),
            crate::config::MicPrefs {
                model: "dpdfnet2_48khz_hr".into(),
                attn_limit: 12.0,
            },
        );
        assert_eq!(get(&c2, Key::AttnLimit, None), "24.5");
        assert_eq!(
            get(&c2, Key::MicPrefs, None),
            "rode: dpdfnet2_48khz_hr 12 dB"
        );
        let v: serde_json::Value = serde_json::from_str(&render_all_json(&c2, None)).unwrap();
        assert_eq!(v["mic_prefs"]["rode"]["attn_limit"], 12.0);
        // With the RODE in use, model and attn_limit are its profile's.
        let rode = Some("alsa_input.rode");
        let rode_cfg = {
            let mut r = c2.clone();
            r.mic_prefs
                .insert("alsa_input.rode".into(), c2.mic_prefs["rode"].clone());
            r
        };
        let plain = render_all(&rode_cfg, rode);
        assert!(plain.contains("model = dpdfnet2_48khz_hr\n"), "{plain}");
        assert!(plain.contains("attn_limit = 12\n"), "{plain}");
        let v: serde_json::Value = serde_json::from_str(&render_all_json(&rode_cfg, rode)).unwrap();
        assert_eq!(v["attn_limit"], 12.0);
        assert_eq!(v["model"], "dpdfnet2_48khz_hr");
        assert_eq!(
            offline_get(&c, Some("model"), false, None).unwrap(),
            "dpdfnet8_48khz_hr\n"
        );
        assert_eq!(
            offline_get(&rode_cfg, Some("attn_limit"), true, rode).unwrap(),
            r#"{"attn_limit":12.0}"#
        );
        assert!(offline_get(&c, Some("volume"), false, None).is_err());
    }

    #[test]
    fn diff_and_qualifiers() {
        let a = Config::default();
        let mut b = a.clone();
        b.attn_limit = 12.0;
        let d = diff(&a, &b);
        assert!(d.chain && !d.autostart && !d.notifications);
        let mut h = a.clone();
        apply(&mut h, Key::VoiceHold, Value::Bool(true), None);
        assert!(
            h.voice_hold && diff(&a, &h).chain,
            "the chain restarts for it"
        );
        assert_eq!(get(&h, Key::VoiceHold, None), "true");
        let mut c = a.clone();
        c.autostart = true;
        c.notifications = false;
        c.tray = false;
        let d = diff(&a, &c);
        assert!(!d.chain && d.autostart && d.notifications);
        let mut f = a.clone();
        apply(
            &mut f,
            Key::Inference,
            Value::Inference(Inference::Onnx),
            None,
        );
        assert_eq!(f.inference, Inference::Onnx);
        assert!(diff(&a, &f).chain, "the engine choice restarts the chain");
        assert_eq!(set_qualifier(Key::Inference, true), "");
        let mut e = a.clone();
        e.enabled = false;
        assert!(
            diff(&a, &e).chain,
            "a reload sees enabled as a chain change"
        );
        assert_eq!(
            diff(&a, &a),
            Diff {
                chain: false,
                autostart: false,
                notifications: false
            }
        );
        assert_eq!(
            set_qualifier(Key::Tray, true),
            " (takes effect on the next start)"
        );
        assert_eq!(set_qualifier(Key::TrayIcon, true), "", "the tray repaints");
        assert_eq!(set_qualifier(Key::AttnLimit, true), "");
        assert_eq!(
            set_qualifier(Key::AttnLimit, false),
            " (saved; applies when HushMic starts)"
        );
        assert_eq!(
            set_qualifier(Key::Tray, false),
            " (saved; applies when HushMic starts)"
        );
    }

    #[test]
    fn devices_marks_the_configured_mic() {
        use crate::pipewire::Source;
        let src = vec![
            Source {
                name: "alsa_input.a".into(),
                description: "Webcam".into(),
            },
            Source {
                name: "alsa_input.b".into(),
                description: "Rode".into(),
            },
        ];
        let plain = render_devices(&src, Some("alsa_input.b"), false);
        assert!(plain.contains("  alsa_input.a\tWebcam\n"), "{plain}");
        assert!(plain.contains("* alsa_input.b\tRode\n"), "{plain}");
        let v: serde_json::Value =
            serde_json::from_str(&render_devices(&src, Some("alsa_input.b"), true)).unwrap();
        assert_eq!(v[1]["selected"], true);
        assert_eq!(v[0]["selected"], false);
        assert_eq!(render_devices(&[], None, false), "(no microphones found)\n");
        assert_eq!(render_devices(&[], None, true), "[]");
        assert_eq!(mic_warning("alsa_input.b", Some(&src)), None);
        assert_eq!(mic_warning("default", Some(&src)), None);
        assert_eq!(
            mic_warning("nosuch", None),
            None,
            "PipeWire not asked: no word"
        );
        let w = mic_warning("nosuch", Some(&src)).unwrap();
        assert!(w.starts_with("warning: nosuch is not a microphone"), "{w}");
    }

    /// F13: `config set` says where model/attn_limit went, in the words
    /// it takes; `--defaults` leaves the microphone's own alone.
    #[test]
    fn set_says_where_it_saved() {
        let mut c = Config::default();
        assert_eq!(
            set_echo(&c, Key::AttnLimit, None, false),
            "attn_limit = 100"
        );
        c.set_attn_for(Some("usbmic"), 24.0);
        assert_eq!(
            set_echo(&c, Key::AttnLimit, Some("usbmic"), false),
            "attn_limit = 24 (saved for usbmic; the defaults stay 100)"
        );
        c.set_attn_for(None, 12.0);
        assert_eq!(
            set_echo(&c, Key::AttnLimit, Some("usbmic"), true),
            "attn_limit = 12 (the defaults; microphones with their own settings keep theirs)"
        );
        assert_eq!(c.profile_for(Some("usbmic")).attn_limit, 24.0);
        c.set_model_for(Some("usbmic"), crate::controller::LIGHT_MODEL.into());
        assert_eq!(
            set_echo(&c, Key::Model, Some("usbmic"), false),
            "model = light (saved for usbmic; the defaults stay quality)"
        );
        assert_eq!(
            set_echo(&c, Key::Tray, Some("usbmic"), false),
            "tray = true"
        );
        // A106 N-C1: one parenthesis, never two in a row
        let echo = set_echo(&c, Key::AttnLimit, Some("usbmic"), false);
        assert_eq!(
            fold(&echo, set_qualifier(Key::AttnLimit, false)),
            "attn_limit = 24 (saved for usbmic; the defaults stay 12; \
             applies when HushMic starts)"
        );
        assert_eq!(
            fold("tray = true", set_qualifier(Key::Tray, false)),
            "tray = true (saved; applies when HushMic starts)"
        );
        assert_eq!(fold("tray = true", ""), "tray = true");
    }
}
