//! `hushmic config`: the keys, their value grammar and validation, the
//! renderers, and the diff the daemon uses to decide what to apply. Pure
//! apart from the file-fallback helpers at the bottom — the daemon (live)
//! and the CLI (no daemon) both go through here, so `set` behaves the same
//! whether or not HushMic is running.

use crate::config::Config;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Mic,
    Model,
    AttnLimit,
    SetDefault,
    Autostart,
    Tray,
    Notifications,
    Enabled,
    ShortcutsSetup,
    MicPrefs,
}

/// The one key table: listing order (what a user asks about first,
/// app-managed keys last), the file/CLI name, and whether `set` takes it.
const KEYS: [(Key, &str, bool); 10] = [
    (Key::Enabled, "enabled", false),
    (Key::Mic, "mic", true),
    (Key::Model, "model", true),
    (Key::AttnLimit, "attn_limit", true),
    (Key::SetDefault, "set_default", true),
    (Key::Autostart, "autostart", true),
    (Key::Tray, "tray", true),
    (Key::Notifications, "notifications", true),
    (Key::ShortcutsSetup, "shortcuts_setup", false),
    (Key::MicPrefs, "mic_prefs", false),
];

/// Every key, in listing order.
pub fn listed() -> impl Iterator<Item = Key> {
    KEYS.iter().map(|(k, _, _)| *k)
}

fn names(settable_only: bool) -> String {
    KEYS.iter()
        .filter(|(_, _, s)| !settable_only || *s)
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
    Model(String),
    Attn(f32),
    Bool(bool),
}

/// Named strengths — the tray's presets (tray.rs ATTN_PRESETS).
const ATTN_WORDS: [(&str, f32); 4] = [
    ("maximum", 100.0),
    ("strong", 24.0),
    ("medium", 12.0),
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
            if raw.contains('/') || raw.contains("..") {
                return Err(format!(
                    "'{raw}' is not a model id (a file name without .onnx under {})",
                    model_dir.display()
                ));
            }
            let p = model_dir.join(format!("{raw}.onnx"));
            if !p.exists() {
                return Err(format!(
                    "model file not found: {} (installed: {})",
                    p.display(),
                    installed_models(model_dir).join(" ")
                ));
            }
            Ok(Value::Model(raw.to_string()))
        }
        Key::AttnLimit => {
            if let Some((_, v)) = ATTN_WORDS.iter().find(|(w, _)| *w == raw) {
                return Ok(Value::Attn(*v));
            }
            let v: f32 = raw
                .parse()
                .ok()
                .filter(|v: &f32| v.is_finite())
                .ok_or_else(|| {
                    format!(
                        "attn_limit must be 0-100 (dB) or maximum|strong|medium|light, not '{raw}'"
                    )
                })?;
            Ok(Value::Attn(v.clamp(0.0, 100.0)))
        }
        Key::SetDefault | Key::Autostart | Key::Tray | Key::Notifications => {
            match raw.to_ascii_lowercase().as_str() {
                "true" | "on" | "yes" | "1" => Ok(Value::Bool(true)),
                "false" | "off" | "no" | "0" => Ok(Value::Bool(false)),
                _ => Err(format!("{} must be true or false, not '{raw}'", key.name())),
            }
        }
        Key::Enabled | Key::ShortcutsSetup | Key::MicPrefs => {
            Err(format!("{} is read-only here", key.name()))
        }
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

/// Same rules as the tray commands: a mic pick loads its saved profile; a
/// model or strength change is remembered under the selected mic.
pub fn apply(cfg: &mut Config, key: Key, value: Value) {
    match (key, value) {
        (Key::Mic, Value::Mic(m)) => cfg.apply_mic_selection(m),
        (Key::Model, Value::Model(m)) => {
            cfg.model = m;
            cfg.remember_selected_prefs();
        }
        (Key::AttnLimit, Value::Attn(v)) => {
            cfg.attn_limit = v;
            cfg.remember_selected_prefs();
        }
        (Key::SetDefault, Value::Bool(b)) => cfg.set_default = b,
        (Key::Autostart, Value::Bool(b)) => cfg.autostart = b,
        (Key::Tray, Value::Bool(b)) => cfg.tray = b,
        (Key::Notifications, Value::Bool(b)) => cfg.notifications = b,
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

/// The plain-text value of one key.
pub fn get(cfg: &Config, key: Key) -> String {
    match key {
        Key::Mic => cfg.mic.clone().unwrap_or_else(|| "default".into()),
        Key::Model => cfg.model.clone(),
        Key::AttnLimit => attn_text(cfg.attn_limit),
        Key::SetDefault => cfg.set_default.to_string(),
        Key::Autostart => cfg.autostart.to_string(),
        Key::Tray => cfg.tray.to_string(),
        Key::Notifications => cfg.notifications.to_string(),
        Key::Enabled => cfg.enabled.to_string(),
        Key::ShortcutsSetup => cfg.shortcuts_setup.to_string(),
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
    }
}

/// `key = value` for every listed key — including the ones the file
/// omits at their default, which are exactly what a new user asks about.
pub fn render_all(cfg: &Config) -> String {
    listed()
        .map(|k| format!("{} = {}\n", k.name(), get(cfg, k)))
        .collect()
}

fn json_value(cfg: &Config, key: Key) -> serde_json::Value {
    use serde_json::json;
    match key {
        Key::Mic => json!(cfg.mic),
        Key::Model => json!(cfg.model),
        Key::AttnLimit => json!(cfg.attn_limit),
        Key::SetDefault => json!(cfg.set_default),
        Key::Autostart => json!(cfg.autostart),
        Key::Tray => json!(cfg.tray),
        Key::Notifications => json!(cfg.notifications),
        Key::Enabled => json!(cfg.enabled),
        Key::ShortcutsSetup => json!(cfg.shortcuts_setup),
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
    }
}

pub fn render_all_json(cfg: &Config) -> String {
    let mut map = serde_json::Map::new();
    for k in listed() {
        map.insert(k.name().to_string(), json_value(cfg, k));
    }
    serde_json::Value::Object(map).to_string()
}

pub fn render_one_json(cfg: &Config, key: Key) -> String {
    let mut map = serde_json::Map::new();
    map.insert(key.name().to_string(), json_value(cfg, key));
    serde_json::Value::Object(map).to_string()
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
    /// enabled / mic / model / attn_limit / set_default / mic_prefs: the
    /// filter chain re-renders.
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
            || old.mic_prefs != new.mic_prefs,
        autostart: old.autostart != new.autostart,
        notifications: old.notifications != new.notifications,
    }
}

// --- `config get` / `config set` without a daemon ---------------------------

/// `config` / `config get KEY` rendered from a Config (the daemon's live
/// copy or the file).
pub fn offline_get(cfg: &Config, key: Option<&str>, json: bool) -> Result<String, String> {
    Ok(match (key, json) {
        (None, false) => render_all(cfg),
        (None, true) => render_all_json(cfg),
        (Some(k), false) => format!("{}\n", get(cfg, parse_key(k)?)),
        (Some(k), true) => render_one_json(cfg, parse_key(k)?),
    })
}

/// The no-daemon path: validate exactly like the daemon, write the file,
/// and do the autostart side effect (the entry IS what "autostart" means).
pub fn offline_set(key: &str, raw: &str, model_dir: &Path) -> Result<String, String> {
    let k = parse_settable_key(key)?;
    let v = parse_value(k, raw, model_dir)?;
    let mut cfg = Config::load();
    apply(&mut cfg, k, v);
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
    Ok(format!(
        "{} = {}{}{}\n",
        k.name(),
        get(&cfg, k),
        set_qualifier(k, false),
        note
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
        return "(no microphones found — is PipeWire running?)\n".to_string();
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
        let e = parse_settable_key("enabled").unwrap_err();
        assert!(e.contains("hushmic mode"), "{e}");
        assert!(parse_settable_key("mic_prefs").is_err());
        assert!(parse_settable_key("shortcuts_setup").is_err());
        assert_eq!(listed().count(), 10);
        assert_eq!(listed().filter(|k| k.settable()).count(), 7);
        for k in listed() {
            assert_eq!(parse_key(k.name()), Ok(k));
        }
        let e = parse_key("nope").unwrap_err();
        assert!(e.contains("enabled mic model"), "{e}");
    }

    #[test]
    fn values_validate_and_clamp() {
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
        let e = parse_value(Key::Model, "nope", &md).unwrap_err();
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
        assert_eq!(
            parse_value(Key::AttnLimit, "250", &md),
            Ok(Value::Attn(100.0))
        );
        assert_eq!(parse_value(Key::AttnLimit, "-5", &md), Ok(Value::Attn(0.0)));
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
        assert!(parse_value(Key::Enabled, "true", &md).is_err());
    }

    #[test]
    fn apply_follows_the_tray_profile_rules() {
        let mut c = Config {
            mic: Some("rode".into()),
            ..Config::default()
        };
        apply(&mut c, Key::AttnLimit, Value::Attn(24.0));
        assert_eq!(c.attn_limit, 24.0);
        assert_eq!(c.mic_prefs["rode"].attn_limit, 24.0);
        apply(&mut c, Key::Model, Value::Model("dpdfnet2_48khz_hr".into()));
        assert_eq!(c.mic_prefs["rode"].model, "dpdfnet2_48khz_hr");
        apply(&mut c, Key::Mic, Value::Mic(None));
        assert_eq!(c.mic, None);
        c.model = "dpdfnet8_48khz_hr".into();
        apply(&mut c, Key::Mic, Value::Mic(Some("rode".into())));
        assert_eq!(c.model, "dpdfnet2_48khz_hr", "profile loaded on pick");
        apply(&mut c, Key::Tray, Value::Bool(false));
        apply(&mut c, Key::Notifications, Value::Bool(false));
        apply(&mut c, Key::SetDefault, Value::Bool(true));
        apply(&mut c, Key::Autostart, Value::Bool(true));
        assert!(!c.tray && !c.notifications && c.set_default && c.autostart);
    }

    #[test]
    fn rendering_lists_every_key_plainly_and_as_json() {
        let c = Config::default();
        let plain = render_all(&c);
        for k in listed() {
            let k = k.name();
            assert!(
                plain.lines().any(|l| l.starts_with(&format!("{k} = "))),
                "{k} missing:\n{plain}"
            );
        }
        assert!(plain.contains("mic = default\n"), "{plain}");
        assert!(plain.contains("tray = true\n"), "{plain}");
        assert!(plain.contains("attn_limit = 100\n"), "{plain}");
        assert!(plain.contains("mic_prefs = (none)\n"), "{plain}");
        let v: serde_json::Value = serde_json::from_str(&render_all_json(&c)).unwrap();
        assert!(v["mic"].is_null());
        assert_eq!(v["attn_limit"], 100.0);
        assert_eq!(v["tray"], true);
        assert!(v["mic_prefs"].is_object());
        let one: serde_json::Value =
            serde_json::from_str(&render_one_json(&c, Key::Model)).unwrap();
        assert_eq!(one["model"], "dpdfnet8_48khz_hr");
        assert_eq!(get(&c, Key::AttnLimit), "100");
        let mut c2 = c.clone();
        c2.attn_limit = 24.5;
        c2.mic_prefs.insert(
            "rode".into(),
            crate::config::MicPrefs {
                model: "dpdfnet2_48khz_hr".into(),
                attn_limit: 12.0,
            },
        );
        assert_eq!(get(&c2, Key::AttnLimit), "24.5");
        assert_eq!(get(&c2, Key::MicPrefs), "rode: dpdfnet2_48khz_hr 12 dB");
        let v: serde_json::Value = serde_json::from_str(&render_all_json(&c2)).unwrap();
        assert_eq!(v["mic_prefs"]["rode"]["attn_limit"], 12.0);
        assert_eq!(
            offline_get(&c, Some("model"), false).unwrap(),
            "dpdfnet8_48khz_hr\n"
        );
        assert!(offline_get(&c, Some("volume"), false).is_err());
    }

    #[test]
    fn diff_and_qualifiers() {
        let a = Config::default();
        let mut b = a.clone();
        b.attn_limit = 12.0;
        let d = diff(&a, &b);
        assert!(d.chain && !d.autostart && !d.notifications);
        let mut c = a.clone();
        c.autostart = true;
        c.notifications = false;
        c.tray = false;
        let d = diff(&a, &c);
        assert!(!d.chain && d.autostart && d.notifications);
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
        assert!(render_devices(&[], None, false).contains("no microphones"));
        assert_eq!(render_devices(&[], None, true), "[]");
    }
}
